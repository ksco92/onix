# /// script
# requires-python = "==3.13.*"
# dependencies = ["deepdiff==9.1.0"]
# ///
"""Diff a fixture pair with real DeepDiff, self-instrumented for M6.

Usage::

    uv run perf/run_deepdiff.py <a.json> <b.json> [--ignore-order]

Prints `DeepDiff(t1, t2, verbose_level=2[, ignore_order=True]).to_json()` to
**stdout** (used by `run_bench.sh`'s correctness pre-check and, for the
timed runs, discarded/redirected — this harness's own fairness rule
requires both tools pay a comparable "write the full report" cost).

Prints exactly one line of JSON to **stderr**:

```
{"parse_ns": N, "diff_ns": N, "tracemalloc_peak_bytes": N,
 "tracemalloc_total_bytes": N, "ru_maxrss_before_bytes": N,
 "ru_maxrss_after_bytes": N, "ru_maxrss_delta_bytes": N}
```

- `parse_ns` times only the two `json.load` calls (perf_counter_ns).
- `diff_ns` times only the `DeepDiff(...)` construction call, after both
  files are already loaded — the headline "diff-only" metric.
- `tracemalloc_*_bytes` is Python allocator traffic during the diff call
  only (tracemalloc started immediately before, stopped immediately after).
- `ru_maxrss_*_bytes` is `resource.getrusage(RUSAGE_SELF).ru_maxrss` sampled
  before and after the diff call, isolating "data already loaded" memory
  from "diff overhead" memory (§5 item 3). **macOS reports `ru_maxrss` in
  bytes** (Linux reports kB) — this script assumes the macOS convention per
  the M6 brief's target platform; a Linux port would need to multiply by
  1024.
"""

import argparse
import json
import resource
import sys
import time
import tracemalloc
from pathlib import Path

from _common import JsonValue
from deepdiff import DeepDiff


def load_pair(a_path: Path, b_path: Path) -> tuple[tuple[JsonValue, JsonValue], int]:
    """
    Load both fixture files as JSON, timing only the two `json.load` calls.

    :param a_path: Path to the first fixture file.
    :param b_path: Path to the second fixture file.
    :return: A `((t1, t2), parse_ns)` tuple: the two parsed values and the
        nanoseconds spent in `json.load` for both.
    """
    start = time.perf_counter_ns()

    with a_path.open(encoding="utf-8") as f:
        t1 = json.load(f)

    with b_path.open(encoding="utf-8") as f:
        t2 = json.load(f)

    parse_ns = time.perf_counter_ns() - start

    return (t1, t2), parse_ns


def run_diff(t1: JsonValue, t2: JsonValue, ignore_order: bool) -> tuple[DeepDiff, dict[str, int]]:
    """
    Run `DeepDiff` once, self-instrumented for diff-only time, tracemalloc
    peak/total allocation, and the `ru_maxrss` before/after delta.

    :param t1: The first (already-parsed) value.
    :param t2: The second (already-parsed) value.
    :param ignore_order: Passed through to `DeepDiff(..., ignore_order=...)`
        — see the module doc's `--ignore-order` flag.
    :return: The `DeepDiff` result and its measurement dict.
    """
    ru_before = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    tracemalloc.start()

    diff_start = time.perf_counter_ns()
    result = DeepDiff(t1, t2, verbose_level=2, ignore_order=ignore_order)
    diff_ns = time.perf_counter_ns() - diff_start

    _current, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    ru_after = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss

    measurements = {
        "diff_ns": diff_ns,
        "tracemalloc_peak_bytes": peak,
        "ru_maxrss_before_bytes": ru_before,
        "ru_maxrss_after_bytes": ru_after,
        "ru_maxrss_delta_bytes": ru_after - ru_before,
    }

    return result, measurements


def main() -> None:
    """Parse arguments, run the timed diff, and print the two output streams."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("a_path", type=Path)
    parser.add_argument("b_path", type=Path)
    parser.add_argument(
        "--ignore-order",
        action="store_true",
        help="Pass ignore_order=True to DeepDiff (the ignore_order_10k baseline fixture).",
    )
    args = parser.parse_args()

    (t1, t2), parse_ns = load_pair(args.a_path, args.b_path)
    result, measurements = run_diff(t1, t2, args.ignore_order)

    print(result.to_json())
    print(json.dumps({"parse_ns": parse_ns, **measurements}), file=sys.stderr)


if __name__ == "__main__":
    main()
