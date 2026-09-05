"""Benchmarks the Arrow table diff (#38-#42) against two hand-rolled baselines.

Runs `deepdiff_rs.diff_tables`, the DuckDB SQL oracle (`oracle_duckdb.py`), and
an idiomatic polars join-based diff on the same seeded parquet pair, at two
sizes: the 1M-row fixture and the full ~5 GB fixture (`generate_fixtures.py`'s
`DEFAULT_ROWS`). Each measured run is its own subprocess (matching
`crates/onix-py/benchmarks/bench_bindings.py`'s own rationale: `ru_maxrss` is a
whole-process high-water mark, so only a fresh process can attribute peak RSS
to one tool's run), reporting wall clock, peak RSS, and CPU seconds (user +
system). Medians are taken over 11 runs at the 1M size and 5 runs at the full
size (a single full-size run already takes tens of seconds; more runs would
not fit a foreground session). Every run's raw metrics, plus the pair's
SHA-256 checksums, are written to `bench_raw/<size>/<tool>_<run>.json`.
This script's `Measurement`/`_normalize_maxrss`/`_fmt_mb` duplicate rather
than import their `bench_bindings.py` counterparts: it runs from `perf/arrow`
with only that directory on `sys.path`, the same cross-directory-import
constraint `perf/_common.py`'s own docstring documents for this repo's other
single-file perf scripts.

# Correctness before timing

For each size, before any run is timed, all three tools' counts (rows added,
rows removed, changed cells, duplicate keys) are computed once and checked
against `generate_fixtures.py`'s sidecar `manifest.json` (rows_added,
rows_deleted, rows_modified_amount + rows_modified_payload, duplicate_keys).
A mismatch aborts the whole run and prints every tool's differing counts: a
timing number over disagreeing output is meaningless. `cells_changed`, not
`rows_changed`, is the comparison field for all three, since the DuckDB
oracle reports only cell-level counts.

# What the polars baseline does and does not see

`_polars_counts` is the join-based diff a data engineer reaching for polars
alone would write: `b.join(a, on=key, how="anti")` for added rows,
`a.join(b, on=key, how="anti")` for removed rows, and an inner join followed
by a per-column `.ne_missing()` (polars' null-safe inequality, matching the
oracle's `IS DISTINCT FROM`) for changed cells. It reports the same four
counts as `diff_tables` and the oracle, computed fully in memory with no
result written to disk. It cannot see, and does not attempt: per-cell change
*kinds* (`value_changed` vs. `became_null`/`became_non_null`, which
`TableDiff.cells_changed()` labels), or duplicate keys (an inner/anti join on
a non-unique key silently fans out instead of reporting the collision, which
is why this fixture's ids are always unique by construction). Both baselines
also skip work `diff_tables` does not have to: they never see the `category`
column's dictionary retype, since DuckDB's schema reader can't observe it
(`oracle_duckdb.py`'s own module docstring) and this script's polars diff
compares only row/cell counts, never schema.

# Fairness

All three tools read the same two parquet files from disk inside the timed
window (no side gets a warm, pre-loaded table). The DuckDB oracle always
persists its five result tables to a scratch directory as part of its
existing, unmodified contract (`oracle_duckdb.run`); the scratch directory's
creation and deletion are excluded from the timed window (mkdtemp/rmtree are
not part of "how long the diff took"), but the writes themselves are not.
Neither the polars script nor `diff_tables` writes its result to disk.

Usage (from the repo root, using `crates/onix-py`'s own venv, which already
carries deepdiff_rs, pyarrow, polars, and duckdb pinned together for its own
tests)::

    cd crates/onix-py
    uv sync --group test
    uv run --group test maturin develop --release
    uv run --group test python ../../perf/arrow/bench_tables.py \\
        --fixtures-1m /path/to/1m --fixtures-full /path/to/full --key id
"""

import argparse
import hashlib
import json
import resource
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Final

import polars as pl
import pyarrow.parquet as pq

from deepdiff_rs import diff_tables
from oracle_duckdb import run as oracle_run

##############################################
##############################################
##############################################
##############################################
# Configuration

TOOLS: Final[tuple[str, ...]] = ("onix", "duckdb", "polars")
RUNS_FULL: Final[int] = 5
RUNS_1M: Final[int] = 11
RAW_DIR: Final[Path] = Path(__file__).parent / "bench_raw"
_HASH_CHUNK_BYTES: Final[int] = 8 * 1024 * 1024


##############################################
##############################################
##############################################
##############################################
# Per-tool counts (shared by the correctness check and the timed workers)


def _onix_counts(fixture_dir: Path, key: list[str]) -> dict[str, int]:
    """
    Diff `fixture_dir`'s parquet pair with `diff_tables` and extract its
    row-level counts.

    :param fixture_dir: Directory holding `a.parquet`/`b.parquet`.
    :param key: The key column names.
    :return: `rows_added`, `rows_removed`, `cells_changed`, `duplicate_keys`.
    """
    left = pq.read_table(fixture_dir / "a.parquet")
    right = pq.read_table(fixture_dir / "b.parquet")
    summary = diff_tables(left, right, key=key).summary()

    return {k: summary[k] for k in ("rows_added", "rows_removed", "cells_changed", "duplicate_keys")}


def _duckdb_counts(fixture_dir: Path, key: list[str], out_dir: Path) -> dict[str, int]:
    """
    Diff `fixture_dir`'s parquet pair with the existing DuckDB oracle.

    :param fixture_dir: Directory holding `a.parquet`/`b.parquet`.
    :param key: The key column names.
    :param out_dir: Scratch directory the oracle writes its five result
        tables into.
    :return: `rows_added`, `rows_removed`, `cells_changed`, `duplicate_keys`.
    """
    summary = oracle_run(fixture_dir / "a.parquet", fixture_dir / "b.parquet", key, out_dir)

    return {k: summary[k] for k in ("rows_added", "rows_removed", "cells_changed", "duplicate_keys")}


def _polars_counts(fixture_dir: Path, key: list[str]) -> dict[str, int]:
    """
    Diff `fixture_dir`'s parquet pair with an anti-join/inner-join polars diff.

    :param fixture_dir: Directory holding `a.parquet`/`b.parquet`.
    :param key: The key column names.
    :return: `rows_added`, `rows_removed`, `cells_changed`, `duplicate_keys`
        (always 0 -- see the module docstring's "What the polars baseline
        does and does not see").
    """
    a = pl.read_parquet(fixture_dir / "a.parquet")
    b = pl.read_parquet(fixture_dir / "b.parquet")
    compare_columns = [c for c in a.columns if c in b.columns and c not in key]
    added = b.join(a, on=key, how="anti")
    removed = a.join(b, on=key, how="anti")
    matched = a.join(b, on=key, how="inner", suffix="_b")
    cells_changed = sum(int(matched[c].ne_missing(matched[f"{c}_b"]).sum()) for c in compare_columns)

    return {"rows_added": added.height, "rows_removed": removed.height, "cells_changed": cells_changed, "duplicate_keys": 0}


def _manifest_counts(fixture_dir: Path) -> dict[str, int]:
    """
    Read `generate_fixtures.py`'s sidecar manifest as the four comparison counts.

    :param fixture_dir: Directory holding `manifest.json`.
    :return: `rows_added`, `rows_removed`, `cells_changed`, `duplicate_keys`.
    """
    manifest = json.loads((fixture_dir / "manifest.json").read_text(encoding="utf-8"))

    return {
        "rows_added": manifest["rows_added"],
        "rows_removed": manifest["rows_deleted"],
        "cells_changed": manifest["rows_modified_amount"] + manifest["rows_modified_payload"],
        "duplicate_keys": manifest["duplicate_keys"],
    }


def _check_correctness(fixture_dir: Path, key: list[str]) -> None:
    """
    Verify all three tools agree with the fixture manifest before any timing.

    :param fixture_dir: Directory holding the parquet pair and manifest.
    :param key: The key column names.
    :raises SystemExit: If any tool's counts differ from the manifest;
        the message names every differing tool's full counts.
    """
    expected = _manifest_counts(fixture_dir)
    with tempfile.TemporaryDirectory(prefix="onix-bench-precheck-") as scratch:
        got = {
            "onix": _onix_counts(fixture_dir, key),
            "duckdb": _duckdb_counts(fixture_dir, key, Path(scratch)),
            "polars": _polars_counts(fixture_dir, key),
        }
    mismatches = {tool: counts for tool, counts in got.items() if counts != expected}
    if mismatches:
        raise SystemExit(f"correctness check failed for {fixture_dir}: expected {expected}, mismatches {mismatches}")


##############################################
##############################################
##############################################
##############################################
# One measured diff, run inside its own subprocess


def _normalize_maxrss(ru_maxrss: int) -> int:
    """
    Convert `resource.getrusage`'s `ru_maxrss` to bytes (bytes on macOS,
    kilobytes on Linux).

    :param ru_maxrss: The raw `ru_maxrss` value.
    :return: Peak resident set size, in bytes.
    """
    if sys.platform == "darwin":
        return ru_maxrss
    return ru_maxrss * 1024


def _run_worker(tool: str, fixture_dir: Path, key: list[str]) -> None:
    """
    Perform one diff and print its wall/CPU/RSS measurement as JSON on stdout.

    :param tool: One of :data:`TOOLS`.
    :param fixture_dir: Directory holding the parquet pair.
    :param key: The key column names.
    """
    scratch = Path(tempfile.mkdtemp(prefix="onix-bench-oracle-")) if tool == "duckdb" else None
    try:
        before = resource.getrusage(resource.RUSAGE_SELF)
        wall_start = time.perf_counter()
        if tool == "onix":
            _onix_counts(fixture_dir, key)
        elif tool == "duckdb":
            assert scratch is not None
            _duckdb_counts(fixture_dir, key, scratch)
        else:
            _polars_counts(fixture_dir, key)
        wall_s = time.perf_counter() - wall_start
        after = resource.getrusage(resource.RUSAGE_SELF)
    finally:
        if scratch is not None:
            shutil.rmtree(scratch, ignore_errors=True)

    cpu_s = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    print(json.dumps({"wall_s": wall_s, "cpu_s": cpu_s, "rss_bytes": _normalize_maxrss(after.ru_maxrss)}))


##############################################
##############################################
##############################################
##############################################
# Orchestration (parent process)


@dataclass(frozen=True)
class Measurement:
    """
    The median wall time, CPU seconds, and peak RSS over a tool's runs at one size.

    :param wall_s: Median wall-clock diff time, in seconds.
    :param cpu_s: Median CPU (user + system) diff time, in seconds.
    :param rss_bytes: Median process peak RSS, in bytes.
    """

    wall_s: float
    cpu_s: float
    rss_bytes: float


def _sha256_file(path: Path) -> str:
    """
    :param path: File to hash.
    :return: The file's SHA-256 hex digest.
    """
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(_HASH_CHUNK_BYTES), b""):
            digest.update(chunk)

    return digest.hexdigest()


def _fixture_checksums(fixture_dir: Path) -> dict[str, str]:
    """
    :param fixture_dir: Directory holding `a.parquet`/`b.parquet`.
    :return: `a_sha256`/`b_sha256`, each file's SHA-256 hex digest.
    """
    return {
        "a_sha256": _sha256_file(fixture_dir / "a.parquet"),
        "b_sha256": _sha256_file(fixture_dir / "b.parquet"),
    }


def _measure_tool(
    tool: str,
    size: str,
    fixture_dir: Path,
    key: list[str],
    runs: int,
    checksums: dict[str, str],
) -> Measurement:
    """
    Run `runs` independent subprocesses for one `(tool, size)` pair, write
    each run's raw JSON, and return the per-metric medians.

    :param tool: One of :data:`TOOLS`.
    :param size: `"1m"` or `"full"`, used only for the raw-JSON path.
    :param fixture_dir: Directory holding the parquet pair.
    :param key: The key column names.
    :param runs: How many independent subprocess runs to take the median of.
    :param checksums: This size's fixture checksums, recorded into every raw file.
    :return: The per-metric medians.
    """
    out_dir = RAW_DIR / size
    out_dir.mkdir(parents=True, exist_ok=True)
    walls: list[float] = []
    cpus: list[float] = []
    rsses: list[float] = []

    for run_index in range(runs):
        completed = subprocess.run(
            [sys.executable, __file__, "--worker", tool, str(fixture_dir), *key],
            capture_output=True,
            text=True,
            check=True,
        )
        payload = json.loads(completed.stdout.strip().splitlines()[-1])
        walls.append(payload["wall_s"])
        cpus.append(payload["cpu_s"])
        rsses.append(payload["rss_bytes"])
        (out_dir / f"{tool}_{run_index}.json").write_text(
            json.dumps({"tool": tool, "size": size, "run_index": run_index, "key": key, **checksums, **payload}, indent=2),
            encoding="utf-8",
        )

    return Measurement(statistics.median(walls), statistics.median(cpus), statistics.median(rsses))


def _fmt_ms(seconds: float) -> str:
    """:return: A duration formatted in milliseconds."""
    return f"{seconds * 1000:.2f} ms"


def _fmt_s(seconds: float) -> str:
    """:return: A duration formatted in seconds."""
    return f"{seconds:.3f} s"


def _fmt_mb(num_bytes: float) -> str:
    """:return: A byte count formatted in MB (1 MB = 1_000_000 bytes)."""
    return f"{num_bytes / 1_000_000:.1f} MB"


def _print_table(size: str, results: dict[str, Measurement]) -> None:
    """
    Print the size's results as a ready-to-paste Markdown table.

    :param size: `"1m"` or `"full"`.
    :param results: Tool name -> its median measurement.
    """
    print(f"\n## {size}\n")
    print("| Tool | Wall clock (median) | CPU seconds (median) | Peak RSS (median) |")
    print("| --- | --- | --- | --- |")
    for tool in TOOLS:
        m = results[tool]
        wall = _fmt_ms(m.wall_s) if m.wall_s < 1.0 else _fmt_s(m.wall_s)
        print(f"| {tool} | {wall} | {_fmt_s(m.cpu_s)} | {_fmt_mb(m.rss_bytes)} |")


def _run_size(size: str, fixture_dir: Path, key: list[str], runs: int) -> None:
    """
    Run the correctness check and the full tool sweep for one size.

    :param size: `"1m"` or `"full"`.
    :param fixture_dir: Directory holding the parquet pair and manifest.
    :param key: The key column names.
    :param runs: Run count for every tool at this size.
    """
    print(f"Checking {size} correctness against {fixture_dir / 'manifest.json'} ...")
    _check_correctness(fixture_dir, key)
    print(f"Hashing {size} fixture pair ...")
    checksums = _fixture_checksums(fixture_dir)
    print(json.dumps(checksums, indent=2))

    results = {tool: _measure_tool(tool, size, fixture_dir, key, runs, checksums) for tool in TOOLS}
    _print_table(size, results)


def main() -> None:
    """Parse CLI arguments and run every requested size's benchmark."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--fixtures-1m", type=Path, help="Directory holding the 1M-row fixture pair.")
    parser.add_argument("--fixtures-full", type=Path, help="Directory holding the full-size fixture pair.")
    parser.add_argument("--key", action="append", default=None, help="Key column(s); repeatable. Defaults to id.")
    args = parser.parse_args()
    key = args.key or ["id"]

    if not args.fixtures_1m and not args.fixtures_full:
        parser.error("pass at least one of --fixtures-1m / --fixtures-full")

    if args.fixtures_1m:
        _run_size("1m", args.fixtures_1m, key, RUNS_1M)
    if args.fixtures_full:
        _run_size("full", args.fixtures_full, key, RUNS_FULL)


if __name__ == "__main__":
    if len(sys.argv) >= 4 and sys.argv[1] == "--worker":
        _run_worker(sys.argv[2], Path(sys.argv[3]), sys.argv[4:])
    else:
        main()
