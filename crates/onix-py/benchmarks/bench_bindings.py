"""The bindings benchmark: real DeepDiff vs deepdiff_rs on live Python objects.

Times the product surface a caller actually uses -- live Python objects
through the Python-object-to-`Value` conversion -- reporting wall time,
peak RSS, and CPU seconds per side, each the median of `RUNS` independent,
no-warmup subprocess runs (`ru_maxrss` is a whole-process peak).

Usage: `uv run --group test python benchmarks/bench_bindings.py` (after
`maturin develop --release`; a debug build understates onix's numbers).
"""

import copy
import json
import random
import resource
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Final

from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff as OnixDeepDiff
from deepdiff_rs import diff_json

type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None
# `id`/`name` (int/str), `created_at` (datetime), `coordinate` (a tuple pair),
# `tags` (a string set) — the fields `_make_typed_record` builds.
type TypedRecord = dict[str, int | str | datetime | tuple[int, int] | tuple[float, float] | set[str]]

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
TYPED_RECORD_COUNT: Final[int] = 10_000
VALUE_CHANGE_RATE: Final[float] = 0.05
RUNS: Final[int] = 11

_ORIGINAL_INT_RANGE: Final[tuple[int, int]] = (0, 1_000_000)
_CHANGED_INT_RANGE: Final[tuple[int, int]] = (10_000_000, 20_000_000)

# A fixed offset, never the process's local zone, so the fixture's aware
# half is deterministic across machines.
_TYPED_RECORD_TZ: Final[timezone] = timezone(timedelta(hours=-5))
_TAG_POOL: Final[tuple[str, ...]] = ("alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta")


##############################################
##############################################
##############################################
##############################################
# Fixture generation (live Python objects)


def _mutation_indices(count: int, rng: random.Random) -> list[int]:
    """Sample the indices to mutate for a `VALUE_CHANGE_RATE` batch."""
    return rng.sample(range(count), int(count * VALUE_CHANGE_RATE))


def build_ignore_order_case() -> tuple[JsonValue, JsonValue]:
    """Build the `ignore_order_10k` shape: a shuffled, ~5%-mutated int list."""
    rng = random.Random(SEED)
    a: list[JsonValue] = [rng.randint(*_ORIGINAL_INT_RANGE) for _ in range(IGNORE_ORDER_SIZE)]
    b = list(a)
    rng.shuffle(b)

    for index in _mutation_indices(IGNORE_ORDER_SIZE, rng):
        b[index] = rng.randint(*_CHANGED_INT_RANGE)

    return a, b


def _make_record(index: int, rng: random.Random) -> dict[str, JsonValue]:
    """Build one heterogeneous "API payload" record; a narrower, standalone copy of `generate_fixtures.py`'s `_make_record`."""
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
    """Mutate one record for a "value changed" entry."""
    mutated = dict(record)
    mutated["score"] = round(rng.uniform(0, 100), 4)
    mutated["active"] = not record["active"]

    return mutated


def build_api_payloads_case() -> tuple[JsonValue, JsonValue]:
    """Build the `api_payloads` shape, ~5% record-changed; `b` is `copy.deepcopy(a)` so unchanged
    records stay non-identity-shared, avoiding DeepDiff's `t1 is t2` fast path."""
    rng = random.Random(SEED + 1)
    a: list[JsonValue] = [_make_record(i, rng) for i in range(RECORD_COUNT)]
    b = copy.deepcopy(a)

    for index in _mutation_indices(RECORD_COUNT, rng):
        record = b[index]
        assert isinstance(record, dict)
        b[index] = _mutate_record(record, rng)

    return a, b


def _make_typed_record(index: int, rng: random.Random) -> TypedRecord:
    """Build one record exercising the typed-conversion path: a datetime, a numeric-pair tuple, a string set."""
    coordinate: tuple[int, int] | tuple[float, float]

    if rng.random() < 0.5:
        coordinate = (round(rng.uniform(-90.0, 90.0), 4), round(rng.uniform(-180.0, 180.0), 4))
    else:
        coordinate = (rng.randint(-1000, 1000), rng.randint(-1000, 1000))

    # Drawn into a variable rather than inline in the dict literal below:
    # dict values evaluate in key order, and `tags` must draw before
    # `created_at` for this function's RNG consumption to stay positionally
    # fixed regardless of how the dict literal is ordered or edited.
    tags = set(rng.sample(_TAG_POOL, rng.randint(1, 4)))

    return {
        "id": index,
        "name": f"typed_{index:07d}",
        "created_at": datetime(
            2020 + rng.randint(0, 5),
            rng.randint(1, 12),
            rng.randint(1, 28),
            rng.randint(0, 23),
            rng.randint(0, 59),
            rng.randint(0, 59),
            tzinfo=_TYPED_RECORD_TZ if index % 2 == 0 else None,
        ),
        "coordinate": coordinate,
        "tags": tags,
    }


def _mutate_typed_record(record: TypedRecord, rng: random.Random) -> TypedRecord:
    """Mutate one typed record: shift its datetime and add a pool tag (a no-op ~30% of the time, since the tag may repeat)."""
    mutated = dict(record)
    created_at = record["created_at"]
    tags = record["tags"]
    assert isinstance(created_at, datetime)
    assert isinstance(tags, set)
    mutated["created_at"] = created_at + timedelta(days=rng.randint(1, 30))
    mutated["tags"] = tags | {rng.choice(_TAG_POOL)}

    return mutated


def build_typed_records_case(*, shuffle: bool = False) -> tuple[list[TypedRecord], list[TypedRecord]]:
    """Build the `typed_records` shape, ~5% mutated; with `shuffle=True`, `b` is reordered before mutating."""
    rng = random.Random(SEED + 2)
    a = [_make_typed_record(i, rng) for i in range(TYPED_RECORD_COUNT)]
    b = copy.deepcopy(a)

    if shuffle:
        rng.shuffle(b)

    for index in _mutation_indices(TYPED_RECORD_COUNT, rng):
        b[index] = _mutate_typed_record(b[index], rng)

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
# The `_file` case models the diff-two-files-on-disk workflow: the two JSON
# files are written from the seed as fixture setup, then both tools pay an
# identical `read_text()` inside the callable before parsing -- so the timed
# difference is purely materializing Python object trees (`json.loads`) versus
# parsing straight into onix's compact value (`diff_json`).
CASE_LABELS: Final[dict[str, str]] = {
    "ignore_order": "`ignore_order`, 10k shuffled ints, ~5% mutated (live objects)",
    "api_payloads": "Heterogeneous API-payload records, n=20,000 (live objects)",
    "typed_records": "Typed records (datetime/tuple/set fields), n=10,000 (live objects)",
    "typed_records_ignore_order": "Same typed-records shape, `ignore_order` (live objects)",
    "ignore_order_json": "Same `ignore_order` shape, via `diff_json` (JSON-string path)",
    "api_payloads_json": "Same API-payload shape, via `diff_json` (JSON-string path)",
    "api_payloads_file": "Same API-payload shape, both tools reading two JSON files from disk",
}
LIVE_CASES: Final[list[str]] = list(CASE_LABELS)
# The onix-only conversion-overhead proxy: a diff of two structurally equal
# but never identity-shared inputs (see `_conversion_proxy_line`).
PROXY_CASE: Final[str] = "api_payloads_equal"


def _text_diff_callable(
    tool: str,
    supply_a: Callable[[], str],
    supply_b: Callable[[], str],
    ignore_order: bool,
) -> Callable[[], object]:
    """Build the diff callable shared by the two JSON-text cases; `supply_a`/`supply_b` run inside it,
    so per-diff text-acquisition cost is timed too."""
    if tool == "deepdiff":
        return lambda: RealDeepDiff(
            json.loads(supply_a()),
            json.loads(supply_b()),
            ignore_order=ignore_order,
            verbose_level=2,
        ).to_json()
    return lambda: diff_json(supply_a(), supply_b(), ignore_order=ignore_order)


def _diff_callable(tool: str, case: str) -> Callable[[], object]:
    """Build the diff callable for one `(tool, case)` pair; the fixture is built outside it, so only the diff is timed."""
    if case in ("ignore_order", "ignore_order_json"):
        a, b = build_ignore_order_case()
        ignore_order = True
    elif case == "typed_records":
        a, b = build_typed_records_case()
        ignore_order = False
    elif case == "typed_records_ignore_order":
        a, b = build_typed_records_case(shuffle=True)
        ignore_order = True
    else:
        a, b = build_api_payloads_case()
        ignore_order = False

    if case == PROXY_CASE:
        equal_copy = copy.deepcopy(a)
        return lambda: OnixDeepDiff(a, equal_copy)

    if case.endswith("_file"):
        tmp = tempfile.TemporaryDirectory(prefix="onix-bench-")
        a_path = Path(tmp.name) / "a.json"
        b_path = Path(tmp.name) / "b.json"
        a_path.write_text(json.dumps(a), encoding="utf-8")
        b_path.write_text(json.dumps(b), encoding="utf-8")
        inner = _text_diff_callable(
            tool,
            lambda: a_path.read_text(encoding="utf-8"),
            lambda: b_path.read_text(encoding="utf-8"),
            ignore_order,
        )

        # Keep the TemporaryDirectory alive until after the diff has run by
        # binding it into the returned callable: the files must still exist
        # when the timed callable reads them, and cleanup must not fall inside
        # the timed window. Its finalizer removes the dir at process exit, so
        # nothing leaks across the (up to 22) subprocesses a run spawns.
        def run_file_diff(_keep_alive: tempfile.TemporaryDirectory = tmp) -> object:
            return inner()

        return run_file_diff

    if case.endswith("_json"):
        a_text = json.dumps(a)
        b_text = json.dumps(b)
        return _text_diff_callable(tool, lambda: a_text, lambda: b_text, ignore_order)

    if tool == "deepdiff":
        return lambda: RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2)
    return lambda: OnixDeepDiff(a, b, ignore_order=ignore_order)


def _normalize_maxrss(ru_maxrss: int) -> int:
    """Convert `resource.getrusage`'s `ru_maxrss` to bytes; it is already bytes on macOS but kilobytes on Linux."""
    if sys.platform == "darwin":
        return ru_maxrss
    return ru_maxrss * 1024


def _run_worker(tool: str, case: str) -> None:
    """Subprocess entry point: perform one diff and print its wall/CPU/RSS measurement as JSON on stdout."""
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
    """The median wall time, CPU seconds, and peak RSS for one tool on one case."""

    wall_s: float
    cpu_s: float
    rss_bytes: float


def measure(tool: str, case: str, runs: int = RUNS) -> Measurement:
    """Run `runs` independent subprocesses for one `(tool, case)` and take the median of each metric."""
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
    """Print the human-readable three-metric summary for one case."""
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
    """Format the conversion-overhead proxy line; `proxy_wall_s` pays full conversion but no per-node diff bookkeeping."""
    fraction = proxy_wall_s / onix_api_wall_s
    return (
        f"conversion proxy (deepdiff_rs, DeepDiff(a, deepcopy(a)), n={RECORD_COUNT}): "
        f"{_fmt_ms(proxy_wall_s)} = {fraction * 100:.1f}% of the mutated "
        f"api_payloads case's {_fmt_ms(onix_api_wall_s)}"
    )


def _markdown_table(results: dict[str, tuple[Measurement, Measurement]]) -> str:
    """Build the ready-to-paste README table: each shape row followed by peak-RSS and CPU-seconds sub-rows."""
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
