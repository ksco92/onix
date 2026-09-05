"""Regression + differential coverage for signed-zero hashing under ignore_order.

Directed regression cases plus two signed-zero-biased differential batches,
both reusing ``test_differential_fuzz``'s generators and both-engines
comparators with a biased scalar alphabet: one over plain lists (the
pre-existing ``ignore_order`` hashing fix), one building sets and frozensets
directly (issue #46), through ``_diverges_with_sets`` (which tolerates
DeepDiff's own hash-order instability and the documented
`list(a_set) == some_list` coercion class -- see `test_differential_fuzz`'s
own module doc for the mechanism). A real Python `set`/`frozenset` can never
hold both `-0.0` and `0.0` -- `set(...)`/`frozenset(...)` collapse the pair to
one member before the value ever reaches onix -- so this batch, run through
the bindings, pins that a genuine single-representative signed-zero set
matches real DeepDiff at fuzz scale; it cannot exercise `SetItems::new`'s own
dedup of a *two-zero* set, which only a directly-built value can produce and
which is guarded instead by
`value_tests::set_items_dedup_signed_zero_like_a_real_python_set` and
`ignore_order::tests::dist_key_hash_agrees_with_equality_on_tricky_equal_values`
(crates/onix-core). See ``ignore_order::hash::item_key``'s float branch and
``onix_core::value::number_cmp`` (crates/onix-core) for why signed zeros are
normalized.
"""

import json
import random
from typing import Final

from deepdiff_rs import DeepDiff as OnixDeepDiff

from test_differential_fuzz import (
    JsonValue,
    _deterministic_members,
    _diverges,
    _diverges_with_sets,
    _gen_scalar,
    _gen_value,
)


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


def _gen_biased_set(rng: random.Random) -> set[object] | frozenset[object]:
    """
    Generate a random set or frozenset of bare members drawn from `_BIASED_SCALARS`.

    Bare scalars only, deliberately: a tuple or frozenset *member* of a set
    hits a different, already-documented divergence (DeepHash's
    order-/repetition-insensitive member hashing, `tests/golden/README.md`'s
    "Set iteration order" section) far more readily once the alphabet itself
    is full of repeated-by-value entries, which would swamp this batch with
    unrelated known noise instead of exercising the signed-zero dedup this
    issue is about.

    :param rng: Seeded RNG.
    :return: A `set` most of the time, a `frozenset` otherwise.
    """
    members = [_gen_scalar(rng, _BIASED_SCALARS) for _ in range(rng.randint(0, 6))]
    return frozenset(members) if rng.random() < 0.3 else set(members)


def _mutate_biased_set(
    rng: random.Random, value: set[object] | frozenset[object]
) -> set[object] | frozenset[object]:
    """
    Build a related-but-different copy of a biased set, keeping its kind.

    Mirrors `_mutate_set_value`'s set/frozenset branch, drawing replacement
    members from `_BIASED_SCALARS` instead of the general-purpose alphabet.

    :param rng: Seeded RNG.
    :param value: The set or frozenset to derive a mutated copy from.
    :return: A structurally related, partially mutated copy of the same kind.
    """
    members = [
        _gen_scalar(rng, _BIASED_SCALARS) if rng.random() < 0.4 else member
        for member in _deterministic_members(value)
    ]
    if rng.random() < 0.3:
        members.append(_gen_scalar(rng, _BIASED_SCALARS))
    return frozenset(members) if isinstance(value, frozenset) else set(members)


def test_signed_zero_biased_set_differential_matches_real_deepdiff() -> None:
    # Builds sets and frozensets directly; see the module docstring for what
    # this batch can and cannot exercise.
    rng = random.Random(20260904)
    cases = 1000
    mismatches: list[str] = []
    for _ in range(cases):
        a = _gen_biased_set(rng)
        b = _mutate_biased_set(rng, a)
        for ignore_order in (True, False):
            divergence = _diverges_with_sets(a, b, ignore_order)
            if divergence is not None:
                expected, actual = divergence
                mismatches.append(
                    f"a={a!r} b={b!r} ignore_order={ignore_order}\n"
                    f"  onix={actual!r}\n"
                    f"  dd  ={expected!r}"
                )
    assert not mismatches, f"{len(mismatches)} mismatch(es):\n" + "\n".join(mismatches[:5])
