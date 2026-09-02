"""The bindings benchmark: real DeepDiff vs deepdiff_rs on live Python objects.

This is the headline number this benchmark exists to produce: `perf/RESULTS.md`
compares onix-core against pure Python on already-parsed JSON, a
structurally flattering upper bound. This script instead times the actual
product surface a Python caller uses — live Python objects in, going
through this crate's own Python-object-to-`Value` conversion — and reports
that conversion cost as part of the number rather than hiding it.

Each case reports three metrics per side (deepdiff and deepdiff_rs): wall
time, peak resident memory (RSS), and CPU seconds (user + system).

# How the three metrics are measured

Every measurement runs in its own short-lived subprocess — one per tool, per
case, per run — with all three metrics taken from that same run:

1. Why a subprocess: `resource.getrusage`'s `ru_maxrss` is a whole-process
   high-water mark, so a single interpreter running both tools back to back
   could never attribute peak memory to a side. One diff per process fixes it.
2. Matched shape: each subprocess imports both libraries and builds the same
   fixture deterministically from a fixed seed, then times only the diff. That
   shared interpreter + libraries + fixture baseline cancels in the deepdiff /
   deepdiff_rs ratio, so peak RSS reflects the diff's *incremental* footprint,
   not a from-zero measurement.
3. No warmup: each subprocess is a cold start, so the reported figure is the
   median of `RUNS` independent subprocesses (an in-process warmup is
   meaningless when every run is a fresh process).

Wall time and CPU seconds are the delta across the diff call alone; peak RSS
is the process high-water mark. See `_normalize_maxrss` for the byte/kilobyte
platform normalization.

Usage (from `crates/onix-py/`, after building the extension in release
mode — a debug build understates onix's numbers by an order of magnitude
or more):

    uv sync --group test
    uv run --group test maturin develop --release
    uv run --group test python benchmarks/bench_bindings.py

The run prints a human-readable summary, the conversion-overhead proxy, and a
ready-to-paste Markdown table (the exact README table, every shape with its
peak-RSS and CPU-seconds sub-rows) so the published numbers are regenerated
by re-running this script, with no hand-transcription.
"""

import copy
import json
import random
import resource
import statistics
import subprocess
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Final

from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff as OnixDeepDiff
from deepdiff_rs import diff_json

type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None

##############################################
##############################################
##############################################
##############################################
# Configuration

# A fixed, recorded seed with disjoint value ranges for genuine, guaranteed
# mutations, matching perf/generate_fixtures.py's ignore_order/api_payloads
# conventions so the two harnesses' fixture shapes stay comparable.
SEED: Final[int] = 20260901
IGNORE_ORDER_SIZE: Final[int] = 10_000
RECORD_COUNT: Final[int] = 20_000
VALUE_CHANGE_RATE: Final[float] = 0.05
RUNS: Final[int] = 11

_ORIGINAL_INT_RANGE: Final[tuple[int, int]] = (0, 1_000_000)
_CHANGED_INT_RANGE: Final[tuple[int, int]] = (10_000_000, 20_000_000)


##############################################
##############################################
##############################################
##############################################
# Fixture generation (live Python objects)


def build_ignore_order_case() -> tuple[JsonValue, JsonValue]:
    """
    Build the `ignore_order_10k` shape: a shuffled, ~5%-mutated int list.

    :return: The `(a, b)` pair, as live Python lists.
    """
    rng = random.Random(SEED)
    a: list[JsonValue] = [rng.randint(*_ORIGINAL_INT_RANGE) for _ in range(IGNORE_ORDER_SIZE)]
    b = list(a)
    rng.shuffle(b)
    change_n = int(IGNORE_ORDER_SIZE * VALUE_CHANGE_RATE)

    for index in rng.sample(range(IGNORE_ORDER_SIZE), change_n):
        b[index] = rng.randint(*_CHANGED_INT_RANGE)

    return a, b


def _make_record(index: int, rng: random.Random) -> dict[str, JsonValue]:
    """
    Build one heterogeneous "API payload" record: a realistic mix of
    scalars, a nested object, and nested lists.

    This is an intentionally self-contained, narrower copy of
    `perf/generate_fixtures.py`'s `_make_record` — it drops that fixture's
    `uuid`/`created_at`/`history` fields and its `metadata.flags` list. The
    two are kept as independent copies rather than shared through a common
    module so this benchmark stays a single standalone script with no
    cross-tree import; the shapes only need to be representative, not
    byte-identical, and this narrower field set is deliberate.

    :param index: The record's position (used for its `id`).
    :param rng: Seeded random source.
    :return: The built record.
    """
    tag_count = rng.randint(0, 5)

    return {
        "id": index,
        "name": f"user_{index:07d}",
        "email": f"user_{index:07d}@example.test",
        "active": rng.random() < 0.8,
        "score": round(rng.uniform(0, 100), 4),
        "tags": [{"tag": f"tag_{rng.randint(0, 999)}"} for _ in range(tag_count)],
        "address": {
            "street": f"{rng.randint(1, 9999)} Main St",
            "city": rng.choice(["Springfield", "Shelbyville", "Ogdenville", "Capital City"]),
            "state": rng.choice(["CA", "NY", "TX", "WA", "CO"]),
            "zip": f"{rng.randint(10000, 99999)}",
        },
        "metadata": {
            "source": rng.choice(["web", "mobile", "api", "batch"]),
            "priority": rng.randint(0, 5),
        },
    }


def _mutate_record(record: dict[str, JsonValue], rng: random.Random) -> dict[str, JsonValue]:
    """
    Mutate one record for a "value changed" entry.

    :param record: The original record; not mutated in place.
    :param rng: Seeded random source.
    :return: The mutated copy.
    """
    mutated = dict(record)
    mutated["score"] = round(rng.uniform(0, 100), 4)
    mutated["active"] = not record["active"]

    return mutated


def build_api_payloads_case() -> tuple[JsonValue, JsonValue]:
    """
    Build the `api_payloads` shape: `RECORD_COUNT` heterogeneous records,
    ~5% value-changed at record granularity.

    `b` is a `copy.deepcopy` of `a`, not a shallow `list(a)`: a shallow copy
    leaves every *unchanged* record (~95% of them) identity-shared between
    `a` and `b` (`b[i] is a[i]`), which real `DeepDiff` fast-paths via its
    own `t1 is t2` identity check (`diff.py`) -- a shortcut no realistic
    caller benefits from, since two independently fetched/deserialized API
    responses are never identity-shared at any level. `copy.deepcopy`
    guarantees every record (and everything nested inside it) is a fresh
    object, structurally equal but never identity-shared, in both the
    unchanged 95% and the freshly-rebuilt mutated 5% alike.

    :return: The `(a, b)` pair, as live Python lists of dicts.
    """
    rng = random.Random(SEED + 1)
    a: list[JsonValue] = [_make_record(i, rng) for i in range(RECORD_COUNT)]
    b = copy.deepcopy(a)
    change_n = int(RECORD_COUNT * VALUE_CHANGE_RATE)

    for index in rng.sample(range(RECORD_COUNT), change_n):
        record = b[index]
        assert isinstance(record, dict)
        b[index] = _mutate_record(record, rng)

    return a, b


##############################################
##############################################
##############################################
##############################################
# One measured diff, run inside its own subprocess

# The measurable cases: the diff each (tool, case) pair performs. The fixture
# is built once inside the subprocess; the returned callable times only the
# diff itself. For the JSON-string cases, serialization to text is fixture
# setup (done before the callable), matching how a caller holding JSON text
# starts; parsing is inside the callable because a real caller pays it (onix
# does it inside `diff_json`; the Python side does it with `json.loads`).
CASE_LABELS: Final[dict[str, str]] = {
    "ignore_order": "`ignore_order`, 10k shuffled ints, ~5% mutated (live objects)",
    "api_payloads": "Heterogeneous API-payload records, n=20,000 (live objects)",
    "ignore_order_json": "Same `ignore_order` shape, via `diff_json` (JSON-string path)",
    "api_payloads_json": "Same API-payload shape, via `diff_json` (JSON-string path)",
}
LIVE_CASES: Final[list[str]] = list(CASE_LABELS)
# The onix-only conversion-overhead proxy: a diff of two structurally equal
# but never identity-shared inputs (see `_conversion_proxy_line`).
PROXY_CASE: Final[str] = "api_payloads_equal"


def _diff_callable(tool: str, case: str) -> Callable[[], object]:
    """
    Build the zero-argument diff callable for one `(tool, case)` pair.

    The fixture is constructed here (outside the returned callable) so only
    the diff itself is timed.

    :param tool: Either `"deepdiff"` or `"deepdiff_rs"`.
    :param case: One of the keys in :data:`CASE_LABELS`, or :data:`PROXY_CASE`.
    :return: A callable that performs exactly one diff and returns its result.
    """
    if case in ("ignore_order", "ignore_order_json"):
        a, b = build_ignore_order_case()
        ignore_order = True
    else:
        a, b = build_api_payloads_case()
        ignore_order = False

    if case == PROXY_CASE:
        equal_copy = copy.deepcopy(a)
        return lambda: OnixDeepDiff(a, equal_copy)

    if case.endswith("_json"):
        a_text = json.dumps(a)
        b_text = json.dumps(b)
        if tool == "deepdiff":
            return lambda: RealDeepDiff(
                json.loads(a_text),
                json.loads(b_text),
                ignore_order=ignore_order,
                verbose_level=2,
            ).to_json()
        return lambda: diff_json(a_text, b_text, ignore_order=ignore_order)

    if tool == "deepdiff":
        return lambda: RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2)
    return lambda: OnixDeepDiff(a, b, ignore_order=ignore_order)


def _normalize_maxrss(ru_maxrss: int) -> int:
    """
    Convert `resource.getrusage`'s `ru_maxrss` to bytes.

    `ru_maxrss` is bytes on macOS but kilobytes on Linux.

    :param ru_maxrss: The raw `ru_maxrss` value.
    :return: Peak resident set size, in bytes.
    """
    if sys.platform == "darwin":
        return ru_maxrss
    return ru_maxrss * 1024


def _run_worker(tool: str, case: str) -> None:
    """
    Perform one diff and print its wall/CPU/RSS measurement as JSON on stdout.

    This is the subprocess entry point: it performs exactly one diff (see the
    module docstring for why one per process).

    :param tool: Either `"deepdiff"` or `"deepdiff_rs"`.
    :param case: The case name to measure.
    """
    run_diff = _diff_callable(tool, case)

    before = resource.getrusage(resource.RUSAGE_SELF)
    wall_start = time.perf_counter()
    result = run_diff()
    wall_s = time.perf_counter() - wall_start
    after = resource.getrusage(resource.RUSAGE_SELF)

    cpu_s = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    rss_bytes = _normalize_maxrss(after.ru_maxrss)
    del result  # kept alive through the RSS sample so its memory counts toward the peak

    print(json.dumps({"wall_s": wall_s, "cpu_s": cpu_s, "rss_bytes": rss_bytes}))


##############################################
##############################################
##############################################
##############################################
# Orchestration (parent process)


@dataclass(frozen=True)
class Measurement:
    """
    The median wall time, CPU seconds, and peak RSS for one tool on one case.

    :param wall_s: Median wall-clock diff time, in seconds.
    :param cpu_s: Median CPU (user + system) diff time, in seconds.
    :param rss_bytes: Median process peak RSS, in bytes.
    """

    wall_s: float
    cpu_s: float
    rss_bytes: float


def measure(tool: str, case: str, runs: int = RUNS) -> Measurement:
    """
    Run `runs` independent subprocesses for one `(tool, case)` and take the
    median of each metric.

    :param tool: Either `"deepdiff"` or `"deepdiff_rs"`.
    :param case: The case name to measure.
    :param runs: How many independent subprocess runs to take the median of.
    :return: The per-metric medians.
    """
    walls: list[float] = []
    cpus: list[float] = []
    rsses: list[float] = []

    for _ in range(runs):
        completed = subprocess.run(
            [sys.executable, __file__, "--worker", tool, case],
            capture_output=True,
            text=True,
            check=True,
        )
        payload = json.loads(completed.stdout.strip().splitlines()[-1])
        walls.append(payload["wall_s"])
        cpus.append(payload["cpu_s"])
        rsses.append(payload["rss_bytes"])

    return Measurement(statistics.median(walls), statistics.median(cpus), statistics.median(rsses))


def _fmt_ms(seconds: float) -> str:
    """:return: A duration formatted in milliseconds."""
    return f"{seconds * 1000:.2f}ms"


def _fmt_mb(num_bytes: float) -> str:
    """:return: A byte count formatted in MB (1 MB = 1_000_000 bytes)."""
    return f"{num_bytes / 1_000_000:.1f} MB"


def _fmt_cpu(seconds: float) -> str:
    """:return: A CPU duration formatted in seconds."""
    return f"{seconds:.3f} s"


def _fmt_ratio(ratio: float) -> str:
    """:return: A deepdiff / deepdiff_rs speedup multiple in bold."""
    return f"**{ratio:.2f}x**"


def _print_case_summary(label: str, deepdiff: Measurement, onix: Measurement) -> None:
    """
    Print the human-readable three-metric summary for one case.

    :param label: The shape being reported.
    :param deepdiff: Real DeepDiff's medians.
    :param onix: deepdiff_rs's medians.
    """
    print(label)
    print(
        f"  wall: deepdiff={_fmt_ms(deepdiff.wall_s)}  deepdiff_rs={_fmt_ms(onix.wall_s)}  "
        f"({_fmt_ratio(deepdiff.wall_s / onix.wall_s)})",
    )
    print(
        f"  peak RSS: deepdiff={_fmt_mb(deepdiff.rss_bytes)}  deepdiff_rs={_fmt_mb(onix.rss_bytes)}  "
        f"({_fmt_ratio(deepdiff.rss_bytes / onix.rss_bytes)})",
    )
    print(
        f"  CPU seconds: deepdiff={_fmt_cpu(deepdiff.cpu_s)}  deepdiff_rs={_fmt_cpu(onix.cpu_s)}  "
        f"({_fmt_ratio(deepdiff.cpu_s / onix.cpu_s)})",
    )


def _conversion_proxy_line(onix_api_wall_s: float, proxy_wall_s: float) -> str:
    """
    Format the conversion-overhead proxy line.

    :param onix_api_wall_s: deepdiff_rs's median wall time on the mutated
        api_payloads case.
    :param proxy_wall_s: deepdiff_rs's median wall time on the equal-inputs
        proxy (`DeepDiff(a, deepcopy(a))`), which pays the full conversion of
        both sides plus onix's cheap whole-input equality short-circuit but
        none of the per-node diff bookkeeping.
    :return: The formatted line.
    """
    fraction = proxy_wall_s / onix_api_wall_s
    return (
        f"conversion proxy (deepdiff_rs, DeepDiff(a, deepcopy(a)), n={RECORD_COUNT}): "
        f"{_fmt_ms(proxy_wall_s)} = {fraction * 100:.1f}% of the mutated "
        f"api_payloads case's {_fmt_ms(onix_api_wall_s)}"
    )


def _markdown_table(results: dict[str, tuple[Measurement, Measurement]]) -> str:
    """
    Build the ready-to-paste README table: four columns, each shape row
    followed by peak-RSS and CPU-seconds sub-rows.

    :param results: Case name -> `(deepdiff, deepdiff_rs)` measurements.
    :return: The Markdown table.
    """
    lines = [
        "| Shape | deepdiff | deepdiff_rs | Speedup |",
        "| --- | --- | --- | --- |",
    ]

    for case in LIVE_CASES:
        deepdiff, onix = results[case]
        lines.append(
            f"| {CASE_LABELS[case]} | {_fmt_ms(deepdiff.wall_s)} | {_fmt_ms(onix.wall_s)} | "
            f"{_fmt_ratio(deepdiff.wall_s / onix.wall_s)} |",
        )
        lines.append(
            f"| &nbsp;&nbsp;peak RSS | {_fmt_mb(deepdiff.rss_bytes)} | {_fmt_mb(onix.rss_bytes)} | "
            f"{_fmt_ratio(deepdiff.rss_bytes / onix.rss_bytes)} |",
        )
        lines.append(
            f"| &nbsp;&nbsp;CPU seconds | {_fmt_cpu(deepdiff.cpu_s)} | {_fmt_cpu(onix.cpu_s)} | "
            f"{_fmt_ratio(deepdiff.cpu_s / onix.cpu_s)} |",
        )

    return "\n".join(lines)


def main() -> None:
    """Run every benchmark shape in isolated subprocesses and print the results."""
    print(
        f"onix bindings benchmark (median of {RUNS} isolated subprocess runs per side)\n",
    )

    results: dict[str, tuple[Measurement, Measurement]] = {}

    for case in LIVE_CASES:
        deepdiff = measure("deepdiff", case)
        onix = measure("deepdiff_rs", case)
        results[case] = (deepdiff, onix)
        _print_case_summary(CASE_LABELS[case], deepdiff, onix)

    proxy = measure("deepdiff_rs", PROXY_CASE)
    onix_api_wall_s = results["api_payloads"][1].wall_s
    print()
    print(_conversion_proxy_line(onix_api_wall_s, proxy.wall_s))

    print("\n--- README table (ready to paste) ---\n")
    print(_markdown_table(results))


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "--worker":
        _run_worker(sys.argv[2], sys.argv[3])
    else:
        main()
