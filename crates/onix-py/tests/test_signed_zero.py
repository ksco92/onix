"""Regression + differential coverage for signed-zero hashing under ignore_order.

Python's DeepHash treats ``-0.0`` and ``+0.0`` as equal, so under
``ignore_order=True`` a list containing both signed zeros dedups to one item.
The Rust ignore-order item key normalizes signed zeros to match, while keeping
an integral float (``2.0``) distinct from the integer of the same value
(deepdiff reports that pairing as a ``type_changes``). These tests pin both
halves against real ``deepdiff``, reusing the differential harness in
``test_differential_fuzz`` (its generator and both-engines comparator) with a
signed-zero-biased scalar alphabet.
"""

import json
import random
from typing import Final

from deepdiff_rs import DeepDiff as OnixDeepDiff

from test_differential_fuzz import JsonValue, _diverges, _gen_value


def _onix(a: JsonValue, b: JsonValue, *, ignore_order: bool) -> dict:
    return json.loads(OnixDeepDiff(a, b, ignore_order=ignore_order).to_json())


def test_signed_zero_dedup_exact_repro() -> None:
    # The reported repro: two signed zeros collapse to a single removed item.
    assert _diverges([0.0, -0.0], [], ignore_order=True) is None
    assert _onix([0.0, -0.0], [], ignore_order=True) == {
        "iterable_item_removed": {"root[0]": 0.0}
    }


def test_signed_zeros_compare_equal_under_ignore_order() -> None:
    for a, b in ([[-0.0], [0.0]], [[0.0], [-0.0]], [[0.0, -0.0], [-0.0, 0.0]]):
        assert _diverges(a, b, ignore_order=True) is None
        assert _onix(a, b, ignore_order=True) == {}


def test_integral_float_stays_distinct_from_integer_under_ignore_order() -> None:
    # Signed-zero normalization must not collapse an integral float onto the
    # integer key: deepdiff keeps these as type_changes / distinct items.
    for a, b in ([[2.0], [2]], [[0.0], [0]], [[0.0, -0.0, 0], []]):
        assert _diverges(a, b, ignore_order=True) is None


# Signed-zero-biased alphabet fed to the shared harness generator.
_BIASED_SCALARS: Final[list[JsonValue]] = [
    0.0, -0.0, 0, 0.0, -0.0, 1, -1, 2, 2.0, -2.0, 1.5, -0.0, 0.0, "x", None, True,
]


def test_signed_zero_biased_differential_matches_real_deepdiff() -> None:
    # Reuses the differential harness (`_gen_value` + `_diverges`) with a
    # signed-zero-heavy alphabet. In-process through the bindings, so 1000
    # cases (x2 modes) stays fast.
    rng = random.Random(20260902)
    cases = 1000
    mismatches: list[str] = []
    for _ in range(cases):
        a = [_gen_value(rng, 3, _BIASED_SCALARS) for _ in range(rng.randint(0, 6))]
        b = [_gen_value(rng, 3, _BIASED_SCALARS) for _ in range(rng.randint(0, 6))]
        for ignore_order in (True, False):
            divergence = _diverges(a, b, ignore_order)
            if divergence is not None:
                expected, actual = divergence
                mismatches.append(
                    f"a={json.dumps(a)} b={json.dumps(b)} ignore_order={ignore_order}\n"
                    f"  onix={json.dumps(actual, sort_keys=True)}\n"
                    f"  dd  ={json.dumps(expected, sort_keys=True)}"
                )
    assert not mismatches, f"{len(mismatches)} mismatch(es):\n" + "\n".join(mismatches[:5])
