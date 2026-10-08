"""Non-finite floats: see tests/golden/README.md, "Non-finite floats"."""

import json
import math
import random
import time
from typing import Final

import pytest
from conftest import require_deepdiff

require_deepdiff()

from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import MAX_DEPTH_CEILING
from deepdiff_rs import DeepDiff as OnixDeepDiff

from test_differential_fuzz import JsonValue, _gen_value

_FINITE_ALPHABET: Final[list[JsonValue]] = [0, 1, -1, 0.0, 1.5, "x", None, True]


def _gen_non_finite_scalar(rng: random.Random) -> JsonValue:
    """
    Pick a random scalar, biased toward a *freshly built* non-finite float.

    Draws a fresh `NaN`/`Infinity` object each time and only as a bare scalar, so a batch
    never exercises the identity or set-member-hashing divergences.

    :param rng: Seeded RNG.
    :return: A random scalar; roughly half the time, a new `NaN`, `Infinity`,
        or `-Infinity` object (never one shared with an earlier draw).
    """
    choice = rng.random()
    if choice < 0.2:
        return float("nan")
    if choice < 0.35:
        return float("inf")
    if choice < 0.5:
        return float("-inf")
    return rng.choice(_FINITE_ALPHABET)


def _canonical_json(text: str) -> str:
    """
    Re-dump JSON text with sorted keys, so two `NaN` tokens compare equal as text.

    :param text: JSON text, `NaN`/`Infinity`/`-Infinity` tokens allowed.
    :return: The same value, canonically re-serialized.
    """
    return json.dumps(json.loads(text), sort_keys=True)


def _onix_json(a: JsonValue, b: JsonValue, *, ignore_order: bool) -> str:
    return _canonical_json(OnixDeepDiff(a, b, ignore_order=ignore_order).to_json())


def _diverges_non_finite(
    a: JsonValue, b: JsonValue, *, ignore_order: bool
) -> tuple[str, str] | None:
    """
    Diff `a`/`b` with both engines and return both canonical JSON texts if
    they disagree.

    :param a: The first value.
    :param b: The second value.
    :param ignore_order: Whether to diff with `ignore_order=True`.
    :return: `(expected, actual)` if they diverge, else `None`.
    """
    expected = _canonical_json(RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2).to_json())
    actual = _onix_json(a, b, ignore_order=ignore_order)
    if expected != actual:
        return expected, actual
    return None


# --- directed cases: scalar, list, dict, set, under both diff modes --------


def test_two_distinct_nans_report_a_values_changed() -> None:
    assert _diverges_non_finite(float("nan"), float("nan"), ignore_order=False) is None
    assert _onix_json(float("nan"), float("nan"), ignore_order=False) == (
        '{"values_changed": {"root": {"new_value": NaN, "old_value": NaN}}}'
    )


def test_infinity_equals_infinity() -> None:
    assert _diverges_non_finite(float("inf"), float("inf"), ignore_order=False) is None
    assert OnixDeepDiff(float("inf"), float("inf")).to_json() == "{}"


def test_negative_infinity_differs_from_infinity() -> None:
    assert _diverges_non_finite(float("inf"), float("-inf"), ignore_order=False) is None


def test_nan_versus_finite_float() -> None:
    assert _diverges_non_finite(float("nan"), 1.0, ignore_order=False) is None
    assert _onix_json(float("nan"), 1.0, ignore_order=False) == (
        '{"values_changed": {"root": {"new_value": 1.0, "old_value": NaN}}}'
    )


def test_non_finite_in_a_list() -> None:
    for a, b in (
        ([float("nan")], [float("nan")]),
        ([float("inf")], [float("inf")]),
        ([float("nan"), 1], [1, float("-inf")]),
    ):
        assert _diverges_non_finite(a, b, ignore_order=False) is None


def test_non_finite_in_a_dict() -> None:
    for a, b in (
        ({"a": float("nan")}, {"a": float("nan")}),
        ({"a": float("inf")}, {"a": 1.0}),
    ):
        assert _diverges_non_finite(a, b, ignore_order=False) is None


def test_non_finite_in_a_set() -> None:
    # A bare non-finite float is an ordinary hashable set member.
    for a, b in (
        ({float("nan")}, {float("nan")}),
        ({float("inf"), 1}, {float("inf"), 2}),
        (frozenset({float("-inf")}), frozenset({float("-inf")})),
    ):
        assert _diverges_non_finite(a, b, ignore_order=False) is None


def test_non_finite_under_ignore_order() -> None:
    for a, b in (
        ([float("nan")], [float("nan")]),
        ([1, float("nan")], [float("nan"), 1]),
        ([{"a": float("nan")}], [{"a": float("nan")}]),
        ({float("nan")}, {float("nan")}),
    ):
        assert _diverges_non_finite(a, b, ignore_order=True) is None


def test_to_dict_returns_real_floats() -> None:
    report = OnixDeepDiff(float("nan"), 1.0).to_dict()
    old_value = report["values_changed"]["root"]["old_value"]
    new_value = report["values_changed"]["root"]["new_value"]
    assert isinstance(old_value, float) and math.isnan(old_value)
    assert new_value == 1.0

    report = OnixDeepDiff([float("inf")], [float("-inf")]).to_dict()
    assert report["values_changed"]["root[0]"]["old_value"] == float("inf")
    assert report["values_changed"]["root[0]"]["new_value"] == float("-inf")


def test_non_finite_dict_key_renders_without_crashing() -> None:
    # See "Non-finite floats".
    report = OnixDeepDiff({}, {float("nan"): 1})
    assert report.to_json() == '{"dictionary_item_added":{"root[nan]":1}}'

    report = OnixDeepDiff({}, {float("inf"): 1, float("-inf"): 2})
    parsed = json.loads(report.to_json())
    new_value = parsed["values_changed"]["root"]["new_value"]
    assert new_value == {"Infinity": 1, "-Infinity": 2}


# --- the one documented divergence: no Python object identity -------------


def test_same_nan_object_compared_to_itself_is_onixs_one_divergence() -> None:
    # No object identity in onix's value model ("Non-finite floats").
    nan = float("nan")
    real = RealDeepDiff(nan, nan, verbose_level=2)
    assert not real  # DeepDiff: no difference (t1 is t2 shortcut)

    onix = OnixDeepDiff(nan, nan)
    assert onix  # onix: always reports a change for a NaN pair
    assert onix.to_json() == (
        '{"values_changed":{"root":{"new_value":NaN,"old_value":NaN}}}'
    )


def test_two_distinct_bit_identical_nans_in_a_carried_set_dedup_in_onix_not_deepdiff() -> None:
    # See "Non-finite floats".
    nans = {float("nan"), float("nan")}
    assert len(nans) == 2

    real = RealDeepDiff({"a": 1}, {"a": 1, "b": nans}, verbose_level=2)
    assert len(real["dictionary_item_added"]["root['b']"]) == 2

    onix = OnixDeepDiff({"a": 1}, {"a": 1, "b": nans})
    added = onix.to_dict()["dictionary_item_added"]["root['b']"]
    assert len(added) == 1


# --- JSON rendering of a buried non-finite leaf stays linear ---


def test_deep_report_with_a_buried_non_finite_leaf_renders_to_json_quickly() -> None:
    """A report carrying a deeply nested `NaN` renders to JSON well under the ceiling's timeout."""
    depth = MAX_DEPTH_CEILING - 5_000
    deep = float("nan")
    for _ in range(depth):
        deep = {"k": deep}

    diff = OnixDeepDiff({}, {"x": deep}, max_depth=MAX_DEPTH_CEILING)
    start = time.perf_counter()
    text = diff.to_json()
    elapsed = time.perf_counter() - start

    assert "NaN" in text
    assert elapsed < 0.1, elapsed


# --- biased differential fuzz -----------------------------------------------


def test_non_finite_biased_differential_matches_real_deepdiff() -> None:
    # See the module docstring for the generator's shape and why it always
    # builds a fresh NaN/Infinity/-Infinity rather than drawing one from a
    # fixed alphabet.
    rng = random.Random(20260905)
    cases = 1000
    mismatches: list[str] = []
    for _ in range(cases):
        a = [_gen_value(rng, 2, None) for _ in range(rng.randint(0, 5))]
        b = [_gen_value(rng, 2, None) for _ in range(rng.randint(0, 5))]
        # Splice in the non-finite-biased scalars at the leaves directly,
        # rather than through _gen_value's own alphabet (which never
        # produces a non-finite float).
        a = [_gen_non_finite_scalar(rng) if rng.random() < 0.5 else v for v in a]
        b = [_gen_non_finite_scalar(rng) if rng.random() < 0.5 else v for v in b]
        for ignore_order in (True, False):
            divergence = _diverges_non_finite(a, b, ignore_order=ignore_order)
            if divergence is not None:
                expected, actual = divergence
                mismatches.append(
                    f"a={a!r} b={b!r} ignore_order={ignore_order}\n"
                    f"  onix={actual}\n"
                    f"  dd  ={expected}"
                )
    assert not mismatches, f"{len(mismatches)} mismatch(es):\n" + "\n".join(mismatches[:5])
