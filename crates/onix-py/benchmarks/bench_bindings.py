"""The M8 bindings benchmark: real DeepDiff vs deepdiff_rs on live Python objects.

This is the headline number M8 exists to produce: `perf/RESULTS.md` (M6)
compares onix-core against pure Python on already-parsed JSON, a
structurally flattering upper bound. This script instead times the actual
product surface a Python caller uses — live Python objects in, going
through this crate's own Python-object-to-`Value` conversion — and reports
that conversion cost as part of the number rather than hiding it.

Three shapes, each timed as median-of-N (`RUNS`, with one discarded warmup
call first):

1. `ignore_order`, 10k shuffled ints with ~5% mutated (the exact shape
   `perf/generate_fixtures.py`'s `ignore_order_10k` fixture uses) — real
   `DeepDiff(..., ignore_order=True)` vs `deepdiff_rs.DeepDiff(...,
   ignore_order=True)`, both on the same live Python list objects.
2. A realistic heterogeneous "API payload" record list (`RECORD_COUNT`
   records — scalars, nested dicts, nested lists; the same shape as
   `perf/generate_fixtures.py`'s `api_payloads` fixture) — plain (ordered)
   `DeepDiff(...)` vs `deepdiff_rs.DeepDiff(...)`.
3. The same two shapes again, but through the fast, JSON-string-only path:
   `deepdiff_rs.diff_json(a_text, b_text)` (parse + diff + serialize
   entirely in Rust, no Python-object traversal at all) vs the equivalent
   Python workflow a caller holding JSON text would actually run —
   `json.loads` both sides, diff, `.to_json()`.

Usage (from `crates/onix-py/`, after building the extension in release
mode — a debug build understates onix's numbers by an order of magnitude
or more):

    uv sync --group test
    uv run --group test maturin develop --release
    uv run --group test python benchmarks/bench_bindings.py
"""

import json
import random
import statistics
import time
from collections.abc import Callable
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

# Matches perf/generate_fixtures.py's BASE_SEED + IGNORE_ORDER_LIST_SIZE/
# API_PAYLOAD_RECORD_COUNT conventions (a fixed, recorded seed; disjoint
# value ranges for genuine, guaranteed mutations) without importing that
# module directly — this script is a standalone, project-venv script (it
# needs the locally-built deepdiff_rs, which perf/'s PEP-723 scripts never
# do), not part of that separate harness.
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
    Build one heterogeneous "API payload" record — the same realistic mix
    of scalars, a nested object, and nested lists as
    `perf/generate_fixtures.py`'s `_make_record`.

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

    :return: The `(a, b)` pair, as live Python lists of dicts.
    """
    rng = random.Random(SEED + 1)
    a: list[JsonValue] = [_make_record(i, rng) for i in range(RECORD_COUNT)]
    b = list(a)
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
# Timing


def time_median(fn: Callable[[], object], runs: int = RUNS) -> float:
    """
    Time `fn` `runs` times (plus one discarded warmup call) and return the median.

    :param fn: The zero-argument callable to time.
    :param runs: How many timed calls to take the median of.
    :return: The median wall-clock time, in seconds.
    """
    fn()  # warmup: excludes one-time import/allocator warmup noise
    samples = []

    for _ in range(runs):
        start = time.perf_counter()
        fn()
        samples.append(time.perf_counter() - start)

    return statistics.median(samples)


def report(label: str, deepdiff_seconds: float, onix_seconds: float) -> None:
    """
    Print one benchmark row: both medians and the speedup multiple.

    :param label: The shape/path being reported.
    :param deepdiff_seconds: Real DeepDiff's median time, in seconds.
    :param onix_seconds: deepdiff_rs's median time, in seconds.
    """
    multiple = deepdiff_seconds / onix_seconds
    print(
        f"{label}: deepdiff={deepdiff_seconds * 1000:.2f}ms  "
        f"deepdiff_rs={onix_seconds * 1000:.2f}ms  ({multiple:.2f}x)",
    )


##############################################
##############################################
##############################################
##############################################
# Benchmarks


def bench_ignore_order(a: JsonValue, b: JsonValue) -> None:
    """
    Benchmark shape (1): `ignore_order=True` on live Python objects.

    :param a: The first list.
    :param b: The second list.
    """
    deepdiff_seconds = time_median(lambda: RealDeepDiff(a, b, ignore_order=True, verbose_level=2))
    onix_seconds = time_median(lambda: OnixDeepDiff(a, b, ignore_order=True))
    report(f"ignore_order_10k (live objects, n={IGNORE_ORDER_SIZE})", deepdiff_seconds, onix_seconds)


def bench_api_payloads(a: JsonValue, b: JsonValue) -> None:
    """
    Benchmark shape (2): plain ordered diff on live heterogeneous records.

    :param a: The first record list.
    :param b: The second record list.
    """
    deepdiff_seconds = time_median(lambda: RealDeepDiff(a, b, verbose_level=2))
    onix_seconds = time_median(lambda: OnixDeepDiff(a, b))
    report(f"api_payloads (live objects, n={RECORD_COUNT})", deepdiff_seconds, onix_seconds)


def bench_json_path(label: str, a: JsonValue, b: JsonValue, *, ignore_order: bool) -> None:
    """
    Benchmark shape (3): the JSON-string-only fast path vs the equivalent
    Python "parse, diff, serialize" workflow.

    :param label: The shape being reported.
    :param a: The first value (will be serialized to JSON text).
    :param b: The second value (will be serialized to JSON text).
    :param ignore_order: Whether to diff with `ignore_order=True`.
    """
    a_text = json.dumps(a)
    b_text = json.dumps(b)

    def deepdiff_json_workflow() -> str:
        """The equivalent Python workflow a caller holding JSON text would run."""
        parsed_a = json.loads(a_text)
        parsed_b = json.loads(b_text)
        return RealDeepDiff(parsed_a, parsed_b, ignore_order=ignore_order, verbose_level=2).to_json()

    deepdiff_seconds = time_median(deepdiff_json_workflow)
    onix_seconds = time_median(lambda: diff_json(a_text, b_text, ignore_order=ignore_order))
    report(f"{label} (JSON-string path)", deepdiff_seconds, onix_seconds)


def main() -> None:
    """Run every benchmark shape and print the results table."""
    ignore_order_a, ignore_order_b = build_ignore_order_case()
    api_payloads_a, api_payloads_b = build_api_payloads_case()

    print(f"onix bindings benchmark (median of {RUNS} runs, 1 discarded warmup call)\n")

    bench_ignore_order(ignore_order_a, ignore_order_b)
    bench_api_payloads(api_payloads_a, api_payloads_b)
    bench_json_path("ignore_order_10k", ignore_order_a, ignore_order_b, ignore_order=True)
    bench_json_path("api_payloads", api_payloads_a, api_payloads_b, ignore_order=False)


if __name__ == "__main__":
    main()
