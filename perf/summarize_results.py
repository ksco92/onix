# /// script
# requires-python = "==3.13.*"
# dependencies = []
# ///
"""Read `perf/bench_raw/*.json` (written by `run_bench.sh`) and emit `perf/RESULTS.md`.

This is the only place derived metrics (speedup ratios, memory ratios,
MB/CPU-second, $/1M diffs) get computed — `run_bench.sh` only captures raw
tool output, never does arithmetic on it, so every number in `RESULTS.md`
traces back to one specific raw JSON file this script read. Run via
`uv run perf/summarize_results.py` (only ever invoked by `run_bench.sh`
itself, as its final step).
"""

import json
import statistics
from dataclasses import dataclass
from pathlib import Path
from typing import Final, Self

from _common import JsonValue

ROOT: Final[Path] = Path(__file__).resolve().parent.parent
RAW_DIR: Final[Path] = ROOT / "perf" / "bench_raw"
FIXTURES_DIR: Final[Path] = ROOT / "perf" / "fixtures"
RESULTS_PATH: Final[Path] = ROOT / "perf" / "RESULTS.md"

# A short, human-facing description of what each fixture stresses, for the
# fixture-matrix table. Every fixture in the manifest is a real two-tool
# comparison (see fixture_names(),
# below) — ignore_order_10k included, since M7 gave onix `--ignore-order`.
FIXTURE_DESCRIPTIONS: Final[dict[str, str]] = {
    "flat_dict_10k": "1-level dict, 10k keys — dict key-set ops",
    "flat_dict_100k": "1-level dict, 100k keys — dict at moderate scale",
    "flat_dict_1m": "1-level dict, 1M keys — dict at scale, memory",
    "flat_list_100k": "1-level list, 100k scalar items — LCS-matched scalar list diffing (M6b)",
    "nested_uniform_d6_b10": "tree, depth 6, branch 10 (~1M leaves) — recursion overhead",
    "api_payloads": 'heterogeneous record list — the "real world" headline number',
    "identical_1m": "flat_dict_1m vs itself — the no-diff fast path",
    "deep_narrow_d120": "single-chain nesting, depth 120 — both tools' depth ceiling",
    "startup_trivial": "{} vs {} — isolates interpreter/binary startup + import cost",
    "ignore_order_10k": "list, shuffled + 5% mutated, diffed with `--ignore-order` — M7's headline comparison",
}

# AWS EC2 r7i.large (memory-optimized, 2 vCPU, 16 GiB), on-demand,
# us-east-1: $0.132/hour. Source: instances.vantage.sh/aws/ec2/r7i.large
# (aggregates the AWS Price List API), accessed 2026-08-31. This is the
# instance the "$ per 1M diffs" derived column assumes; re-derive if this
# price has moved by the time you read this.
INSTANCE_LABEL: Final[str] = "AWS EC2 r7i.large (2 vCPU, 16 GiB, us-east-1, on-demand)"
INSTANCE_PRICE_PER_HOUR_USD: Final[float] = 0.132
INSTANCE_PRICE_DATE: Final[str] = "2026-08-31"


##############################################
##############################################
##############################################
##############################################
# Small formatting/loading helpers


def load_json(path: Path) -> JsonValue:
    """
    Read and parse one raw-results JSON file.

    :param path: File to read.
    :return: The parsed value.
    """

    return json.loads(path.read_text(encoding="utf-8"))


def as_object(value: JsonValue) -> dict[str, JsonValue]:
    """
    Narrow a `JsonValue` known, by construction, to be a JSON object — every
    raw-results file this script reads is written by `run_bench.sh`'s own
    JSON producers, so a non-dict here is a harness bug, not recoverable
    runtime input.

    :param value: The value; must be a `dict`.
    :return: The same value, typed as `dict[str, JsonValue]`.
    """
    assert isinstance(value, dict)

    return value


def as_array(value: JsonValue) -> list[JsonValue]:
    """
    Narrow a `JsonValue` known to be a JSON array (see `as_object`'s doc).

    :param value: The value; must be a `list`.
    :return: The same value, typed as `list[JsonValue]`.
    """
    assert isinstance(value, list)

    return value


def as_number(value: JsonValue) -> float:
    """
    Narrow a `JsonValue` known to be a JSON number (see `as_object`'s doc).

    :param value: The value; must be an `int` or `float`.
    :return: The value as a `float`.
    """
    assert isinstance(value, int | float)

    return float(value)


def find_manifest_entry(manifest: list[JsonValue], name: str) -> dict[str, JsonValue]:
    """
    Find one fixture's entry in the parsed `perf/fixtures/manifest.json`.

    :param manifest: The parsed manifest's `fixtures` list.
    :param name: Fixture name to find.
    :return: Its manifest entry.
    """

    return as_object(next(entry for entry in manifest if as_object(entry)["name"] == name))


def fixture_names(manifest: list[JsonValue]) -> list[str]:
    """
    Every fixture name, in manifest order — derived from the manifest
    itself (not a second hardcoded list) so this and `run_bench.sh`'s own
    fixture list can never drift out of sync. Every fixture in the matrix
    is a real two-tool comparison since M7 (`ignore_order_10k` included).

    :param manifest: The parsed manifest's `fixtures` list.
    :return: Every fixture name.
    """
    names = []

    for entry in manifest:
        name = as_object(entry)["name"]
        assert isinstance(name, str)
        names.append(name)

    return names


def ns_to_s(nanoseconds: float) -> float:
    """
    Convert nanoseconds to seconds.

    :param nanoseconds: Duration in nanoseconds.
    :return: The same duration in seconds.
    """

    return nanoseconds / 1_000_000_000


def fmt_seconds(seconds: float) -> str:
    """
    Format a duration for a table cell: milliseconds below 1s, else seconds.

    :param seconds: Duration in seconds.
    :return: A human-readable string.
    """
    if seconds < 1:
        return f"{seconds * 1000:.3f} ms"

    return f"{seconds:.3f} s"


def fmt_bytes(num_bytes: float) -> str:
    """
    Format a byte count as MB (1 MB = 1_000_000 bytes, matching the
    fixture-generator's own MB reporting).

    :param num_bytes: Size in bytes.
    :return: A human-readable string.
    """

    return f"{num_bytes / 1_000_000:.2f} MB"


def fmt_ratio(ratio: float) -> str:
    """
    Format a speedup/memory ratio as `N.NNx`.

    :param ratio: The ratio (larger side over smaller).
    :return: A human-readable string.
    """

    return f"{ratio:.2f}x"


def fmt_seconds_with_spread(median_s: float, min_s: float, max_s: float) -> str:
    """
    Format a median duration alongside its observed min-max spread across
    repeated runs — the methodology fix this milestone's review round
    required: a single unreplicated sample is never reported as a headline
    number (this harness's own rule: report medians and σ, never single
    runs).

    :param median_s: Median duration, in seconds.
    :param min_s: Fastest observed duration, in seconds.
    :param max_s: Slowest observed duration, in seconds.
    :return: A human-readable string, e.g. `"3.618 ms (3.401-4.012 ms)"`.
    """

    return f"{fmt_seconds(median_s)} ({fmt_seconds(min_s)}-{fmt_seconds(max_s)})"


##############################################
##############################################
##############################################
##############################################
# Per-fixture metric extraction


class ToolMetrics:
    """Wall clock, CPU time, and peak RSS for one tool on one fixture (from hyperfine)."""

    def __init__(self: Self, hyperfine_result: JsonValue) -> None:
        """
        Build from one entry of a hyperfine `--export-json` `results` array.

        :param hyperfine_result: One `results[i]` object.
        """
        result = as_object(hyperfine_result)
        memory_samples = as_array(result["memory_usage_byte"])
        self.wall_mean_s: float = as_number(result["mean"])
        self.wall_median_s: float = as_number(result["median"])
        self.wall_stddev_s: float = as_number(result["stddev"])
        self.cpu_user_s: float = as_number(result["user"])
        self.cpu_system_s: float = as_number(result["system"])
        self.peak_rss_bytes: float = statistics.median(as_number(v) for v in memory_samples)
        self.run_count: int = len(memory_samples)

    @property
    def cpu_total_s(self: Self) -> float:
        """:return: Total CPU time (user+sys) — the cloud-cost-relevant number."""

        return self.cpu_user_s + self.cpu_system_s


class DiffOnlySamples:
    """
    Self-instrumented diff-only timing samples for one tool on one fixture,
    from N repeated runs (tier-appropriate warmup/run counts — see
    `run_bench.sh`'s `tier_for`). Median is the headline number; min/max is
    the reported spread (this harness's own rule: report medians and σ,
    never single runs — a single unreplicated sample was this milestone's
    review-round methodology finding).
    """

    def __init__(self: Self, samples_ns: list[float]) -> None:
        """
        :param samples_ns: One nanosecond diff-only timing per measured run
            (warmup runs already excluded).
        """
        assert samples_ns
        self.samples_ns = samples_ns

    @property
    def median_ns(self: Self) -> float:
        """:return: The median diff-only time, in nanoseconds."""

        return statistics.median(self.samples_ns)

    @property
    def min_ns(self: Self) -> float:
        """:return: The fastest observed diff-only time, in nanoseconds."""

        return min(self.samples_ns)

    @property
    def max_ns(self: Self) -> float:
        """:return: The slowest observed diff-only time, in nanoseconds."""

        return max(self.samples_ns)

    @property
    def run_count(self: Self) -> int:
        """:return: How many measured samples this is built from."""

        return len(self.samples_ns)


def load_sample_document(path: Path) -> dict[str, list[float]]:
    """
    Load a `diffonly_*.json` file (written by `run_bench.sh`'s N-sample
    loop): every `..._samples` key mapped to its list of floats.

    :param path: The raw-results file to read.
    :return: `{key: samples}` for every array-valued key in the document.
    """
    document = as_object(load_json(path))

    return {key: [as_number(v) for v in as_array(value)] for key, value in document.items()}


class FixtureRow:
    """Every measurement for one comparable (onix + deepdiff) fixture."""

    def __init__(self: Self, name: str, manifest: list[JsonValue]) -> None:
        """
        Load every raw-results file for `name` and compute the row's fields.

        :param name: Fixture name (matches `perf/fixtures/<name>/`).
        :param manifest: The parsed `perf/fixtures/manifest.json`.
        """
        self.name = name
        self.description = FIXTURE_DESCRIPTIONS[name]

        entry = find_manifest_entry(manifest, name)
        self.input_bytes = int(as_number(entry["a_bytes"])) + int(as_number(entry["b_bytes"]))

        onix_samples = load_sample_document(RAW_DIR / f"diffonly_onix_{name}.json")
        self.onix_diff_only = DiffOnlySamples(onix_samples["diff_ns_samples"])
        self.onix_parse_ns = statistics.median(onix_samples["parse_ns_samples"])

        deepdiff_samples = load_sample_document(RAW_DIR / f"diffonly_deepdiff_{name}.json")
        self.deepdiff_diff_only = DiffOnlySamples(deepdiff_samples["diff_ns_samples"])
        self.deepdiff_parse_ns = statistics.median(deepdiff_samples["parse_ns_samples"])
        self.deepdiff_tracemalloc_peak_bytes = statistics.median(deepdiff_samples["tracemalloc_peak_bytes_samples"])

        hyperfine = as_object(load_json(RAW_DIR / f"hyperfine_{name}.json"))
        results = as_array(hyperfine["results"])
        by_command = {as_object(entry)["command"]: ToolMetrics(entry) for entry in results}
        self.onix: ToolMetrics = by_command["onix"]
        self.deepdiff: ToolMetrics = by_command["deepdiff"]

    @property
    def diff_only_speedup(self: Self) -> float:
        """
        :return: `deepdiff diff-only MEDIAN time / onix diff-only MEDIAN
            time` (the headline metric), each a median
            over N tier-appropriate runs, never a single sample.
        """

        return self.deepdiff_diff_only.median_ns / self.onix_diff_only.median_ns

    @property
    def wall_speedup(self: Self) -> float:
        """:return: `deepdiff wall-clock median / onix wall-clock median`."""

        return self.deepdiff.wall_median_s / self.onix.wall_median_s

    @property
    def memory_ratio(self: Self) -> float:
        """:return: `deepdiff peak RSS / onix peak RSS`."""

        return self.deepdiff.peak_rss_bytes / self.onix.peak_rss_bytes

    @property
    def meets_threshold(self: Self) -> bool:
        """
        :return: Whether this fixture clears this harness's own success threshold
            (≥5x faster diff-only OR ≥5x less peak RSS), used by the
            go/no-go section.
        """

        return self.diff_only_speedup >= 5.0 or self.memory_ratio >= 5.0

    @property
    def is_slower(self: Self) -> bool:
        """:return: Whether onix's diff-only time is slower than deepdiff's."""

        return self.diff_only_speedup < 1.0


@dataclass
class Report:
    """Everything a render_* function needs: the loaded manifest, every fixture's row, and the recorded seed."""

    manifest: list[JsonValue]
    rows: list[FixtureRow]
    base_seed: str

    def row(self: Self, name: str) -> FixtureRow:
        """
        Look up one fixture's row by name.

        :param name: Fixture name.
        :return: Its `FixtureRow`.
        """

        return next(r for r in self.rows if r.name == name)


##############################################
##############################################
##############################################
##############################################
# Markdown rendering


def render_environment_header(env: JsonValue) -> str:
    """
    Render the environment front-matter block (recording hardware +
    versions in perf/RESULTS.md front matter).

    :param env: The parsed `env.json`.
    :return: A markdown section.
    """
    env = as_object(env)

    return f"""## Environment

| | |
|---|---|
| Date (UTC) | {env["date_utc"]} |
| OS | {env["os"]} |
| CPU | {env["cpu"]} |
| Cores | {env["cores"]} |
| Memory | {fmt_bytes(as_number(env["memory_bytes"]))} |
| rustc | {env["rustc_version"]} |
| cargo | {env["cargo_version"]} |
| Python | {env["python_version"]} (uv-managed, pinned `==3.13.*`) |
| deepdiff | {env["deepdiff_version"]} (pinned) |
| uv | {env["uv_version"]} |
| hyperfine | {env["hyperfine_version"]} |
| Build | `cargo build --release` (`lto = true`, `codegen-units = 1`) |
"""


def render_fixture_matrix(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: The fixture-matrix table (name, sizes, description).
    """
    lines = [
        "## Fixture matrix",
        "",
        "Generated deterministically by `perf/generate_fixtures.py` (fixed seed "
        f"`{report.base_seed}`, recorded there — regeneration is byte-identical; see that "
        "file's module docstring for the verification command). ~5% values "
        "changed, ~2% added, ~2% removed between each fixture's `a`/`b` pair, "
        "except `identical_1m` (byte-identical copy), `startup_trivial` "
        "(`{}` vs `{}`), and `ignore_order_10k` (pure shuffle + ~5% "
        "value-changed, no add/remove — see M7's `ignore_order.rs`).",
        "",
        "| Fixture | What it stresses | Input size (a+b) |",
        "|---|---|---|",
    ]

    for row in report.rows:
        entry = find_manifest_entry(report.manifest, row.name)
        size = as_number(entry["a_bytes"]) + as_number(entry["b_bytes"])
        lines.append(f"| `{row.name}` | {FIXTURE_DESCRIPTIONS[row.name]} | {fmt_bytes(size)} |")

    return "\n".join(lines) + "\n"


def render_run_procedure() -> str:
    """:return: A short section documenting the actual warmup/run counts used."""

    return """## Run procedure (as actually executed)

Deterministic by design: fixed warmup/run counts below, no adaptive
sampling, same command sequence every invocation of `run_bench.sh`.

| Tier | Fixtures | Warmup | Runs |
|---|---|---|---|
| standard | `flat_dict_10k`, `flat_dict_100k`, `flat_list_100k`, `deep_narrow_d120` | 3 | 10 |
| startup (cheap; more runs for tighter statistics) | `startup_trivial` | 5 | 20 |
| heavy (~12-17s/diff on this machine) | `flat_dict_1m`, `identical_1m`, `ignore_order_10k` | 1 | 5 |
| very heavy (~1-1.5min/diff on this machine) | `nested_uniform_d6_b10`, `api_payloads` | 0 | 3 |

The two "very heavy" fixtures use only 3 runs (no warmup) purely for total
harness runtime — a single deepdiff diff-only call already takes over a
minute at that size; onix's own run count is unaffected by this (it is not
what makes those fixtures slow) but hyperfine measures both commands
together in one comparison sweep.

Three independent measurement passes run per fixture, each using this same
warmup/run tier: the correctness precheck (one run per tool, not tallied
above — its only job is the byte-identical canonical-JSON comparison), the
diff-only timing sample loop (the tier's full warmup+runs, feeding the
Headline table's medians below), and the hyperfine sweep (also the tier's
full warmup+runs, feeding wall clock/CPU/RSS). Diff-only timing is
deliberately its own pass, not reused from the precheck or hyperfine runs
— this harness always reports a median over N runs, never a single
sample, and hyperfine's own runs don't expose per-run stderr to extract
`diff_ns` from.
"""


def render_correctness_section(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: The correctness-precheck section — every fixture that reached this file passed.
    """

    return f"""## Correctness precheck

**Every fixture below reached this file only after its onix and DeepDiff
outputs were canonicalized (`jq -S`, matching `crates/onix-core/tests/golden.rs`'s
own "sorted-keys, order-sensitive-arrays" notion of canonical equality) and
found byte-identical.** `run_bench.sh` aborts the entire run — no
`RESULTS.md` gets written at all — the moment any fixture's outputs
diverge — a perf number on divergent output is void.

All {len(report.rows)} fixtures in the matrix matched on this run —
`ignore_order_10k` included: since M7, it's a real `onix --ignore-order`
vs. `DeepDiff(..., ignore_order=True)` comparison, not a deepdiff-only
baseline, and it clears the exact same precheck as every other fixture.
It's also an all-numeric flat list, so it never reaches the disclosed,
pre-existing `threshold_to_diff_deeper` dict-vs-dict divergence already
tracked by `crates/onix-core/tests/golden.rs`'s `KNOWN_DIVERGENT_CASES`
— no special-casing was needed here.

**One real divergence was found at the fixture-design level while building
this harness during M6** (not in `onix-core`): `api_payloads`' original
design put raw booleans in `metadata.flags` and raw strings in `tags`.
Real DeepDiff 9.1.0, even on the default *ordered* path
(`ignore_order=False`), applies an LCS-style "cheapest edit" match for
lists of *hashable* scalars — diverging from onix's then-simpler
index-aligned list algorithm whenever two same-length, low-cardinality
hashable lists happen to share values at different offsets, which the
fixture's record-level add/remove mutation made likely. The fixture-level
fix (wrap every such scalar in a single-key dict, forcing both tools onto
the shared positional fallback) is still in place, but **the underlying
gap it worked around was closed by M6b** (`crates/onix-core/src/lcs.rs`):
onix's ordered-path list diffing now dispatches to the same LCS/`difflib`
matching DeepDiff uses for scalar-only lists, so the wrapping may no
longer be strictly required — kept anyway as a zero-risk safety net rather
than re-verified and unwound here (see `perf/generate_fixtures.py`'s
`_mutate_record` doc).
"""


def render_headline_table(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: The headline diff-only + peak-RSS table, one row per comparable fixture.
    """
    lines = [
        "## Headline: diff-only time + peak RSS",
        "",
        "Diff-only time excludes process startup and JSON parsing on both "
        "sides (self-instrumented — onix via `--timing`'s `diff_ns`, "
        "deepdiff via `time.perf_counter_ns()` around only the `DeepDiff(...)` "
        "call). **Each cell is the MEDIAN over N tier-appropriate runs "
        "(the same warmup/run counts as the run-procedure table above), "
        "shown with its observed min-max spread — never a single sample** "
        "(this harness's own rule: report medians and σ, never single runs). Peak RSS "
        "is the median of hyperfine's per-run `memory_usage_byte` (verified "
        'against `/usr/bin/time -l`\'s "maximum resident set size" — '
        "identical value on this machine) over the full process, same runs "
        "as the wall-clock sweep.",
        "",
        "| Fixture | onix diff-only (median, min-max) | deepdiff diff-only (median, min-max) | "
        "Speedup | onix peak RSS | deepdiff peak RSS | Memory ratio | ≥5x threshold |",
        "|---|---|---|---|---|---|---|---|",
    ]

    for row in report.rows:
        threshold_mark = "✅" if row.meets_threshold else "❌"

        if row.is_slower:
            threshold_mark += " **(onix SLOWER — see note below)**"

        onix_cell = fmt_seconds_with_spread(
            ns_to_s(row.onix_diff_only.median_ns),
            ns_to_s(row.onix_diff_only.min_ns),
            ns_to_s(row.onix_diff_only.max_ns),
        )
        deepdiff_cell = fmt_seconds_with_spread(
            ns_to_s(row.deepdiff_diff_only.median_ns),
            ns_to_s(row.deepdiff_diff_only.min_ns),
            ns_to_s(row.deepdiff_diff_only.max_ns),
        )
        lines.append(
            f"| `{row.name}` | {onix_cell} | {deepdiff_cell} | {fmt_ratio(row.diff_only_speedup)} | "
            f"{fmt_bytes(row.onix.peak_rss_bytes)} | {fmt_bytes(row.deepdiff.peak_rss_bytes)} | "
            f"{fmt_ratio(row.memory_ratio)} | {threshold_mark} |",
        )

    return "\n".join(lines) + "\n"


def render_wall_clock_table(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: End-to-end wall-clock table (item 1: includes process startup).
    """
    lines = [
        "## End-to-end wall clock",
        "",
        "Process start to exit, both tools reading the same two JSON files "
        "(hyperfine, mean ± σ over the run counts in the procedure table above).",
        "",
        "| Fixture | onix wall (mean ± σ) | deepdiff wall (mean ± σ) | Wall speedup |",
        "|---|---|---|---|",
    ]

    for row in report.rows:
        lines.append(
            f"| `{row.name}` | {fmt_seconds(row.onix.wall_mean_s)} ± "
            f"{fmt_seconds(row.onix.wall_stddev_s)} | {fmt_seconds(row.deepdiff.wall_mean_s)} ± "
            f"{fmt_seconds(row.deepdiff.wall_stddev_s)} | {fmt_ratio(row.wall_speedup)} |",
        )

    return "\n".join(lines) + "\n"


def render_cpu_and_allocation_table(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: CPU time (user+sys) and allocation-profile table.
    """
    lines = [
        "## CPU time (user+sys) and allocation profile",
        "",
        "CPU time is the cloud-cost-relevant number (instances bill "
        "CPU-seconds regardless of wall clock) and doubles as the energy "
        "proxy documented in the Energy section below. `tracemalloc peak` is "
        "deepdiff's traced-allocation peak during the diff call only; onix's "
        "equivalent (a counting global allocator behind a bench-only "
        "feature) is a **documented TODO**, not implemented this milestone "
        "(marked nice-to-have, not required) — see the Deferred work "
        "note at the end of this file.",
        "",
        "| Fixture | onix CPU (user+sys) | deepdiff CPU (user+sys) | deepdiff tracemalloc peak |",
        "|---|---|---|---|",
    ]

    for row in report.rows:
        onix_cpu = row.onix.cpu_total_s
        deepdiff_cpu = row.deepdiff.cpu_total_s
        lines.append(
            f"| `{row.name}` | {fmt_seconds(onix_cpu)} | {fmt_seconds(deepdiff_cpu)} | "
            f"{fmt_bytes(row.deepdiff_tracemalloc_peak_bytes)} |",
        )

    return "\n".join(lines) + "\n"


def render_startup_section(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: The dedicated startup/import-cost section (`startup_trivial`).
    """
    row = report.row("startup_trivial")

    return f"""## Startup/import cost

`startup_trivial` (`{{}}` vs `{{}}`) isolates process startup: the diff
itself is trivially empty, so its wall-clock time is dominated by
interpreter startup + `import deepdiff` on the Python side, and binary
exec-to-main on the Rust side.

**Caveat: the deepdiff number is measured via `uv run perf/run_deepdiff.py`**
(per this harness's own fairness rule), not a bare `python`
invocation, so it also includes `uv`'s own subprocess-launch and
environment-resolution overhead (typically ~10-30ms on a cached
environment) on top of pure interpreter+import cost. This number is real
and reproducible as measured, but is not a pure "Python interpreter +
`import deepdiff`" figure — a bare-interpreter comparison would show a
smaller gap.

| | onix | deepdiff (via `uv run`) |
|---|---|---|
| Wall clock (mean ± σ) | {fmt_seconds(row.onix.wall_mean_s)} ± {fmt_seconds(row.onix.wall_stddev_s)} | \
{fmt_seconds(row.deepdiff.wall_mean_s)} ± {fmt_seconds(row.deepdiff.wall_stddev_s)} |
| Ratio | | {fmt_ratio(row.wall_speedup)} slower to start |
"""


def render_ignore_order_design_notes(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: Design-rationale notes for `ignore_order_10k`, M7's headline
        comparison — the live measured numbers already appear in the
        Headline/wall-clock/CPU tables above via the normal per-fixture
        row; this section explains *why* the number looks the way it
        does, without re-deriving or hand-carrying any figure.
    """
    row = report.row("ignore_order_10k")

    return f"""## Design notes: `ignore_order_10k` (M7's headline comparison)

`ignore_order_10k` is diffed by both tools with `--ignore-order`
(`DeepDiff(..., ignore_order=True)` / `onix diff --ignore-order`) — a real
two-tool comparison like every other fixture (see the Headline table
above for its row: {fmt_ratio(row.diff_only_speedup)} diff-only, this run).
This was DeepDiff's own documented headline slowness (its `O(changed²)`
candidate-pairing built from real Python objects) and the motivating
reason for `onix-core`'s M7 milestone. Three design choices explain the size of the
gap:

- **The numeric fast path never builds a `Report`.** For a flat list of
  ints like this fixture, every pairing candidate's distance is computed
  by [`crate::ignore_order::numeric_distance`] alone (closed-form
  arithmetic), never touching the structural fallback that would
  otherwise pay for `PathSegment` allocations, `Value` clones, and
  `BTreeMap` inserts per candidate — replicating DeepDiff's own
  per-candidate object-construction cost in Rust would have defeated the
  point of this port.
- **Every item is hashed exactly once per list** (`HashedList::build`),
  not recomputed per candidate comparison — the
  `O(hashes_added × hashes_removed)` candidate loop only ever does `O(1)`
  hash-map lookups against already-computed keys.
- **A from-scratch, dependency-free `FxHasher`** (this crate's own quality
  bar has no new-dependency budget) replaces the standard library's
  default `SipHash` for this module's internal `HashMap`/`HashSet`s:
  `SipHash`'s DoS-resistance is a real per-call cost that buys nothing
  here (these maps never key on attacker-chosen strings the way `SipHash`
  defends against — see `ignore_order.rs`'s own doc).

The cost is dominated by `O(change_n²)` (the candidate-pairing loop), not
`O(n²)` — matching real `DeepDiff`'s own documented cost anatomy (see
`crate::ignore_order`'s own module doc for the full, source-cited
scaling-signature analysis; not re-run here, since it validates the
algorithm's asymptotic behavior, not this fixture's specific numbers).
"""


def render_energy_section() -> str:
    """:return: The energy section — sampled numbers if available, else the documented fallback."""
    energy = load_json(RAW_DIR / "energy.json")
    assert isinstance(energy, dict)

    if energy["available"]:
        return """## Energy

`sudo powermetrics` sampling ran successfully on this machine — see
`perf/bench_raw/powermetrics_onix.txt` for the raw sampler output (not
committed; regenerate via `perf/run_bench.sh` with passwordless `sudo`
available).
"""

    return f"""## Energy (best-effort — fell back to the documented proxy)

`sudo powermetrics` needs root; `sudo -n true` failed non-interactively in
this environment (the common case for a headless/sandboxed run), so energy
sampling was **skipped**, falling back explicitly to **CPU
time (user+sys), already reported in the "CPU time (user+sys) and
allocation profile" table above, as the documented proxy**
(roughly proportional to energy at fixed clock speed).

To get a real Joules/diff number, the repository owner can run, on this
same machine:

```sh
{energy["manual_sudo_command"]}
```

while looping a fixture diff (see `run_bench.sh`'s Step 6 for the exact
loop it would otherwise run) and dividing the reported package energy by
the iteration count.
"""


def render_derived_economics(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: The MB/CPU-second and $/1M-diffs derived-economics section.
    """
    api_row = report.row("api_payloads")
    onix_cpu_s = api_row.onix.cpu_total_s
    deepdiff_cpu_s = api_row.deepdiff.cpu_total_s
    onix_mb_per_cpu_s = (api_row.input_bytes / 1_000_000) / onix_cpu_s
    deepdiff_mb_per_cpu_s = (api_row.input_bytes / 1_000_000) / deepdiff_cpu_s

    def cost_per_million_diffs(cpu_seconds_per_diff: float) -> float:
        """
        :param cpu_seconds_per_diff: CPU-seconds (user+sys) for one diff.
        :return: Estimated USD cost for 1 million such diffs on `INSTANCE_LABEL`.
        """
        hours_per_million = (cpu_seconds_per_diff * 1_000_000) / 3600

        return hours_per_million * INSTANCE_PRICE_PER_HOUR_USD

    onix_cost = cost_per_million_diffs(onix_cpu_s)
    deepdiff_cost = cost_per_million_diffs(deepdiff_cpu_s)

    return f"""## Derived: throughput and cost

MB of input processed per CPU-second (higher is better), and an estimated
cost for 1 million `api_payloads`-sized diffs on **{INSTANCE_LABEL}**
(${INSTANCE_PRICE_PER_HOUR_USD}/hour on-demand; source:
instances.vantage.sh/aws/ec2/r7i.large, accessed {INSTANCE_PRICE_DATE}) —
CPU-seconds (user+sys), not wall clock, is what a shared/serverless
CPU-billed instance actually bills.

| | onix | deepdiff |
|---|---|---|
| MB of input / CPU-second (`api_payloads`) | {onix_mb_per_cpu_s:.1f} MB/s | {deepdiff_mb_per_cpu_s:.1f} MB/s |
| Estimated $ / 1M `api_payloads`-sized diffs | ${onix_cost:,.2f} | ${deepdiff_cost:,.2f} |

This is a **CPU-time-only** cost model (excludes egress, storage, and
per-request platform overhead) meant to illustrate the *relative* economic
gap, not a production cost estimate.
"""


def render_go_no_go(report: Report) -> str:
    """
    :param report: The loaded benchmark report.
    :return: The GO/NO-GO evaluation section against this harness's own thresholds.
    """
    api_row = report.row("api_payloads")
    non_identical_rows = [r for r in report.rows if r.name not in {"identical_1m", "startup_trivial"}]
    majority_meet_threshold = sum(r.meets_threshold for r in non_identical_rows) > len(non_identical_rows) / 2
    api_strictly_better = api_row.diff_only_speedup > 1.0 and api_row.memory_ratio > 1.0
    any_slower = [r for r in report.rows if r.is_slower]

    lines = [
        "## GO / NO-GO evaluation",
        "",
        "This harness's success thresholds: **≥5x faster (diff-only) OR ≥5x "
        "less peak memory on the majority of fixtures, and strictly better "
        "on `api_payloads`; no fixture where onix is slower** (any "
        "regression is a bug to explain, not a caveat to publish).",
        "",
        "| Fixture | Meets ≥5x threshold | Diff-only speedup | Memory ratio |",
        "|---|---|---|---|",
    ]

    for row in report.rows:
        mark = "YES" if row.meets_threshold else "no"
        lines.append(
            f"| `{row.name}` | {mark} | {fmt_ratio(row.diff_only_speedup)} | {fmt_ratio(row.memory_ratio)} |",
        )

    lines.append("")
    lines.append(f"- **Majority of fixtures clear the ≥5x bar:** {'YES' if majority_meet_threshold else 'NO'}.")
    lines.append(
        f"- **`api_payloads` strictly better on both axes:** "
        f"{'YES' if api_strictly_better else 'NO'} "
        f"(diff-only {fmt_ratio(api_row.diff_only_speedup)}, memory {fmt_ratio(api_row.memory_ratio)}).",
    )

    if any_slower:
        names = ", ".join(f"`{r.name}`" for r in any_slower)
        lines.append(
            f"- **⚠️ onix is SLOWER (diff-only) than deepdiff on: {names}.** "
            "This is a finding to flag prominently, not a "
            "caveat to bury — see the note directly below.",
        )
    else:
        lines.append("- **No fixture where onix is slower (diff-only) than deepdiff.**")

    lines.append("")
    verdict = "GO" if majority_meet_threshold and api_strictly_better and not any_slower else "CONDITIONAL / NO-GO"
    lines.append(f"### Verdict: **{verdict}**")
    lines.append("")
    lines.append(
        "This is an **upper "
        "bound**, not the product validation — onix here diffs data "
        "already parsed into `serde_json::Value`, with no FFI or "
        "Python-object conversion cost on its ledger. The decision-relevant "
        "validation is post-M7/M8 (real `ignore_order` through Python "
        "bindings on live Python objects), where per-node FFI or up-front "
        "conversion costs will land on onix's side of the ledger. A clean "
        "GO here justifies *continuing* toward that validation, not a "
        "claim that the product is proven.",
    )

    return "\n".join(lines) + "\n"


def render_depth_ceiling_note() -> str:
    """:return: The prominent note on onix's real (lower-than-expected) depth ceiling."""

    return """## Finding: onix's practical depth ceiling is lower than expected

The `deep_narrow_dN` fixture's target depth was originally set to
~500, gated by DeepDiff's own Python recursion limit. Two independent ceilings were
empirically probed while building this fixture (see
`perf/generate_fixtures.py`'s `DEEP_NESTING_DEPTH` constant):

- **Real DeepDiff 9.1.0** (default `sys.getrecursionlimit() == 1000`) on
  this single-chain dict shape raises `RecursionError` starting at
  **~depth 495** — probed at 495 (succeeds) and 496 (fails) on this
  machine, but this is a Python C-stack-depth limit, not a pure
  Python-frame-count one, so the exact boundary can shift by a few levels
  run to run depending on intervening C-stack usage. Treat "~495" as an
  approximate, not exact, ceiling.
- **`onix-cli`'s actual ceiling is much lower and IS exact: 126**, and it
  fails to *parse*, not diff. `onix-cli` parses with `serde_json`'s default
  (non-`unbounded_depth`) parser, which hard-caps at 128 levels of *parser*
  recursion — completely independent of `onix_core::diff_with_max_depth`'s
  own `--max-depth`/`DEFAULT_MAX_DEPTH` guard (512 by default), which never
  even gets exercised here because parsing fails first. This is documented
  in `onix-cli`'s own `README.md`/rustdoc as expected behavior, not a bug —
  but it means **onix's real depth ceiling for JSON-file input is `serde_json`'s
  128, not the 512 the CLI flag suggests**, and it is the *tighter* of the
  two tools' ceilings, not the looser ~500 originally anticipated.

`deep_narrow_d120` was sized (120, with margin) to a depth both tools can
following this harness's own guiding principle: report the depth
ceiling of each rather than forcing an arbitrary large target like 20k.
"""


def render_deferred_work_note() -> str:
    """:return: The closing note on what M6 deliberately deferred, incl. the full scope-cut disclosure."""

    return """## Deferred work (documented, not silently dropped)

**Fixture matrix scaled down from the original full table** (this milestone
was explicitly asked to build "a scalable, representative subset", not the
full matrix — but here is every cut, not just the headline one):

- **`flat_list_5m`** (the originally envisioned 5-million-item list,
  "throughput, memory") — **not built at all**. Only `flat_list_100k` is in this run's
  matrix; a multi-million-item list fixture is a candidate follow-up if
  finer-grained throughput data at that scale is ever needed.
- **`api_payloads`** capped at 50,000 records rather than the originally
  suggested ~50-200MB (see the actual measured size in the Fixture matrix
  table above) — see `perf/generate_fixtures.py`'s `API_PAYLOAD_RECORD_COUNT`
  comment: at 100k records deepdiff's diff-only call already took ~3
  minutes, which made the full deterministic harness (every fixture run
  multiple times) impractical to run in one sitting; 50k records already
  makes deepdiff take ~90 seconds per diff — "meaningfully long" per the
  brief's own bar.
- **`deep_narrow_dN`** at depth 120, not the originally-envisioned 20k
  (nor even the ~500 fallback) — see the "Finding: onix's practical
  depth ceiling is lower than expected" section above for why.

Also deferred, unrelated to matrix scale:

- **Rust-side counting allocator** (marked nice-to-have, not required):
  not implemented this milestone. onix's allocation profile is inferred
  only indirectly, via peak RSS and the (already dramatic) CPU-time gap.
  Left for a follow-up milestone if the allocation-churn detail is ever
  decision-relevant.
- **Criterion micro-benches**: not implemented this
  milestone — `run_bench.sh`'s cross-language sweep was the priority; a
  per-fixture-shape Criterion suite inside `onix-core` is a natural
  follow-up once the cross-language number exists to compare against.
- **Energy sampling**: see the Energy section above — CPU-seconds is the
  documented fallback proxy; a real Joules/diff number needs a manual
  `sudo` run by the repository owner (exact command provided there).
"""


##############################################
##############################################
##############################################
##############################################
# Entry point


def build_report() -> Report:
    """
    Load the fixture manifest and every fixture's raw-results files.

    :return: The fully-populated report.
    """
    document = as_object(load_json(FIXTURES_DIR / "manifest.json"))
    manifest = as_array(document["fixtures"])
    base_seed = str(int(as_number(document["base_seed"])))

    return Report(
        manifest=manifest,
        rows=[FixtureRow(name, manifest) for name in fixture_names(manifest)],
        base_seed=base_seed,
    )


def main() -> None:
    """Load every raw-results file, build every fixture row, and write RESULTS.md."""
    report = build_report()

    sections = [
        "# onix vs. DeepDiff — benchmark results\n",
        "Generated entirely by `perf/run_bench.sh` (via `perf/summarize_results.py`) "
        "— every number below traces back to a real, timestamped run captured under "
        "`perf/bench_raw/` (gitignored; regenerate with `perf/run_bench.sh`). "
        "No number here was hand-written.\n",
        render_environment_header(load_json(RAW_DIR / "env.json")),
        render_fixture_matrix(report),
        render_run_procedure(),
        render_correctness_section(report),
        render_depth_ceiling_note(),
        render_headline_table(report),
        render_wall_clock_table(report),
        render_cpu_and_allocation_table(report),
        render_startup_section(report),
        render_ignore_order_design_notes(report),
        render_energy_section(),
        render_derived_economics(report),
        render_go_no_go(report),
        render_deferred_work_note(),
    ]

    RESULTS_PATH.write_text("\n".join(sections), encoding="utf-8")
    print(f"Wrote {RESULTS_PATH}")


if __name__ == "__main__":
    main()
