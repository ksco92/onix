"""Regression + differential coverage for signed-zero hashing under ignore_order.

Python's DeepHash treats ``-0.0`` and ``+0.0`` as equal, so under
``ignore_order=True`` a list containing both signed zeros dedups to one item.
The Rust ignore-order item key normalizes signed zeros to match, while keeping
an integral float (``2.0``) distinct from the integer of the same value
(deepdiff reports that pairing as a ``type_changes``). These tests pin both
halves against real ``deepdiff``.
"""

import json
import random
from typing import Final

from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff as OnixDeepDiff

type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None


def _both(a: JsonValue, b: JsonValue, *, ignore_order: bool) -> tuple[dict, dict]:
    expected = json.loads(RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2).to_json())
    actual = json.loads(OnixDeepDiff(a, b, ignore_order=ignore_order).to_json())
    return actual, expected


def test_signed_zero_dedup_exact_repro() -> None:
    # The reported repro: two signed zeros collapse to a single removed item.
    actual, expected = _both([0.0, -0.0], [], ignore_order=True)
    assert actual == expected
    assert actual == {"iterable_item_removed": {"root[0]": 0.0}}


def test_signed_zeros_compare_equal_under_ignore_order() -> None:
    for a, b in ([[-0.0], [0.0]], [[0.0], [-0.0]], [[0.0, -0.0], [-0.0, 0.0]]):
        actual, expected = _both(a, b, ignore_order=True)
        assert actual == expected
        assert actual == {}


def test_integral_float_stays_distinct_from_integer_under_ignore_order() -> None:
    # Signed-zero normalization must not collapse an integral float onto the
    # integer key: deepdiff keeps these as type_changes / distinct items.
    for a, b in ([[2.0], [2]], [[0.0], [0]], [[0.0, -0.0, 0], []]):
        actual, expected = _both(a, b, ignore_order=True)
        assert actual == expected


# Signed-zero-biased differential batch, seeded and deterministic. Runs
# in-process through the bindings, so 1000 cases (x2 modes) stays fast.
_BIASED_SCALARS: Final[list[JsonValue]] = [
    0.0, -0.0, 0, 0.0, -0.0, 1, -1, 2, 2.0, -2.0, 1.5, -0.0, 0.0, "x", None, True,
]


def _gen(rng: random.Random, depth: int) -> JsonValue:
    if depth <= 0 or rng.random() < 0.6:
        return rng.choice(_BIASED_SCALARS)
    if rng.random() < 0.5:
        return [_gen(rng, depth - 1) for _ in range(rng.randint(0, 5))]
    return {k: _gen(rng, depth - 1) for k in rng.sample(["a", "b", "c"], rng.randint(0, 3))}


def test_signed_zero_biased_differential_matches_real_deepdiff() -> None:
    rng = random.Random(20260902)
    cases = 1000
    mismatches: list[str] = []
    for _ in range(cases):
        a = [_gen(rng, 3) for _ in range(rng.randint(0, 6))]
        b = [_gen(rng, 3) for _ in range(rng.randint(0, 6))]
        for ignore_order in (True, False):
            actual, expected = _both(a, b, ignore_order=ignore_order)
            if actual != expected:
                mismatches.append(
                    f"a={json.dumps(a)} b={json.dumps(b)} ignore_order={ignore_order}\n"
                    f"  onix={json.dumps(actual, sort_keys=True)}\n"
                    f"  dd  ={json.dumps(expected, sort_keys=True)}"
                )
    assert not mismatches, f"{len(mismatches)} mismatch(es):\n" + "\n".join(mismatches[:5])
