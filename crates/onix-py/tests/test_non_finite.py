"""Non-finite floats (`NaN`, `Infinity`, `-Infinity`): comparison, JSON
rendering, and the one documented divergence.

Every claim below was verified against live ``deepdiff==9.1.0`` before this
was written (see the observed outputs quoted in the comments): two distinct
``NaN`` objects report a change like any other differing scalar pair (`NaN !=
NaN`), ``Infinity == Infinity``, ``to_json()`` writes the same bare
``NaN``/``Infinity``/``-Infinity`` tokens Python's own ``json.dumps`` does by
default, and under ``ignore_order`` every ``NaN`` matches every other one
(``DeepHash`` digests a number from ``str(obj)``, which is the same three
characters for any ``NaN`` regardless of its bits). ``tests/golden/README.md``'s
"Non-finite floats" section has the full mechanism and the one place onix
deliberately differs: this crate's value model carries no Python object
identity, so it cannot reproduce the cases where real DeepDiff/CPython let a
``NaN`` match *itself* by identity (``t1 is t2``, or the same object appearing
twice in one input) rather than by content.

Because two distinct ``NaN`` objects are never Python-`==`, comparing parsed
JSON dicts (as the shared `_diverges` helper in `test_differential_fuzz` does)
would report every ``NaN``-containing case as "divergent" even when both
engines produced the correct ``NaN`` — the parsed dicts would never compare
equal to each other, `NaN`-for-`NaN`, regardless of which engine produced
them. `_diverges_non_finite` below instead compares the canonically re-dumped
JSON *text*, which is a plain string comparison and so is not affected by
`NaN != NaN`.

The biased fuzz batch draws `NaN`/`Infinity`/`-Infinity` through a generator
that calls `float("nan")`/`float("inf")` fresh each time rather than reusing a
value from a fixed alphabet list: a fixed list entry, drawn twice by
`random.choice`, would hand back the *same* Python object both times, and
real DeepDiff/CPython's identity-before-equality fast path (dict/set/list
internals, not plain `==`) then treats that shared reference specially in a
way this crate cannot and should not try to reproduce (see the module doc
above) — a batch that leaned on it would be exercising that unrelated,
already-documented identity divergence instead of the ordinary distinct-object
comparison and hashing rules this issue is about. Scoped to bare scalars in
containers, per this repo's biased-fuzz-generator convention (see
`differential-fuzz-alphabet.md`): a tuple/frozenset set *member* containing a
non-finite float would also exercise the pre-existing, already-pinned
order-/repetition-insensitive set-member-hashing divergence
(`tests/golden/README.md`'s "Set iteration order" section), not this one.
"""

import json
import math
import random
import time
from typing import Final

from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import MAX_DEPTH_CEILING
from deepdiff_rs import DeepDiff as OnixDeepDiff

from test_differential_fuzz import JsonValue, _gen_value

_FINITE_ALPHABET: Final[list[JsonValue]] = [0, 1, -1, 0.0, 1.5, "x", None, True]


def _gen_non_finite_scalar(rng: random.Random) -> JsonValue:
    """
    Pick a random scalar, biased toward a *freshly built* non-finite float.

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
    Re-dump parsed JSON text with sorted keys, for a comparison that is not
    itself broken by ``NaN != NaN``: two texts that each round-trip to the
    literal token ``NaN`` in the same position compare equal as strings even
    though the two *parsed* Python floats they produced would not.

    :param text: JSON text, `NaN`/`Infinity`/`-Infinity` tokens allowed.
    :return: The same value, canonically re-serialized.
    """
    return json.dumps(json.loads(text), sort_keys=True)


def _onix_json(a: JsonValue, b: JsonValue, *, ignore_order: bool) -> str:
    return _canonical_json(OnixDeepDiff(a, b, ignore_order=ignore_order).to_json())


def _dd_json(a: JsonValue, b: JsonValue, *, ignore_order: bool) -> str:
    return _canonical_json(
        RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2).to_json()
    )


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
    expected = _dd_json(a, b, ignore_order=ignore_order)
    actual = _onix_json(a, b, ignore_order=ignore_order)
    if expected != actual:
        return expected, actual
    return None


# --- directed cases: scalar, list, dict, set, under both diff modes --------


def test_two_distinct_nans_report_a_values_changed() -> None:
    # deepdiff==9.1.0: DeepDiff(float('nan'), float('nan')) with two distinct
    # objects -> {'values_changed': {'root': {'new_value': nan, 'old_value':
    # nan}}}; to_json() -> {"values_changed": {"root": {"new_value": NaN,
    # "old_value": NaN}}}
    assert _diverges_non_finite(float("nan"), float("nan"), ignore_order=False) is None
    assert _onix_json(float("nan"), float("nan"), ignore_order=False) == (
        '{"values_changed": {"root": {"new_value": NaN, "old_value": NaN}}}'
    )


def test_infinity_equals_infinity() -> None:
    # deepdiff==9.1.0: DeepDiff(float('inf'), float('inf')) -> {} (Infinity
    # compares equal to itself regardless of object identity, unlike NaN).
    assert _diverges_non_finite(float("inf"), float("inf"), ignore_order=False) is None
    assert OnixDeepDiff(float("inf"), float("inf")).to_json() == "{}"


def test_negative_infinity_differs_from_infinity() -> None:
    assert _diverges_non_finite(float("inf"), float("-inf"), ignore_order=False) is None


def test_nan_versus_finite_float() -> None:
    # deepdiff==9.1.0: DeepDiff(nan, 1.0) -> values_changed;
    # to_json() -> {"values_changed": {"root": {"new_value": 1.0,
    # "old_value": NaN}}}
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
    # deepdiff==9.1.0: under ignore_order, DeepHash's digest for any NaN is
    # content-based (str(obj) == "nan" regardless of identity or bits), so
    # every NaN matches every other one -- confirmed for lists, a value
    # nested in a dict inside a list, and sets.
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
    # deepdiff==9.1.0: stringify_param's repr()-then-ast.literal_eval round
    # trip fails on "nan"/"inf" and silently collapses the key to None
    # (confirmed live: DeepDiff({}, {float('nan'): 1}).to_json() ==
    # '{"dictionary_item_added": {"null": 1}}', with a warning printed).
    # onix instead renders the key deterministically rather than reproducing
    # that garble; pinned as onix's own behavior, not compared to DeepDiff.
    report = OnixDeepDiff({}, {float("nan"): 1})
    assert report.to_json() == '{"dictionary_item_added":{"root[nan]":1}}'

    report = OnixDeepDiff({}, {float("inf"): 1, float("-inf"): 2})
    parsed = json.loads(report.to_json())
    new_value = parsed["values_changed"]["root"]["new_value"]
    assert new_value == {"Infinity": 1, "-Infinity": 2}


# --- the one documented divergence: no Python object identity -------------


def test_same_nan_object_compared_to_itself_is_onixs_one_divergence() -> None:
    # deepdiff==9.1.0: DeepDiff(t1, t2) short-circuits on `t1 is t2` before
    # ever comparing values, so the SAME NaN object compared to itself is {}
    # -- but onix's value model carries no object identity, so it always
    # takes the (far more common) distinct-objects answer. Pinned as onix's
    # own behavior, per tests/golden/README.md's "Non-finite floats" section,
    # not asserted equal to DeepDiff's.
    nan = float("nan")
    real = RealDeepDiff(nan, nan, verbose_level=2)
    assert not real  # DeepDiff: no difference (t1 is t2 shortcut)

    onix = OnixDeepDiff(nan, nan)
    assert onix  # onix: always reports a change for a NaN pair
    assert onix.to_json() == (
        '{"values_changed":{"root":{"new_value":NaN,"old_value":NaN}}}'
    )


def test_two_distinct_bit_identical_nans_in_a_carried_set_dedup_in_onix_not_deepdiff() -> None:
    # A real Python set never merges the pair (nan != nan, and the two
    # objects differ, so neither the identity nor the equality probe fires) --
    # confirmed: len({float('nan'), float('nan')}) == 2. DeepDiff's own
    # *set-vs-set* diffing (`_diff_set`) always collapses to one entry per
    # content digest regardless -- onix already matches that (see the
    # `ignore_order` tests above) -- so the divergence shows only when the
    # set is carried whole, as an added/removed *value* rather than diffed
    # member by member: DeepDiff({'a': 1}, {'a': 1, 'b': {nan_a, nan_b}})
    # -> dictionary_item_added: {"root['b']": {nan, nan}} (the real,
    # 2-member set, untouched). onix's SetItems canonicalizes by structural
    # value at conversion time (see value.rs's SetItems::new doc), which
    # folds the two bit-identical NaNs into one canonical member before the
    # diff ever runs -- deterministic (unlike DeepDiff's own hash-order-
    # dependent answers for other set cases), but a real divergence from this
    # specific real-Python set, pinned here rather than compared to DeepDiff.
    nans = {float("nan"), float("nan")}
    assert len(nans) == 2

    real = RealDeepDiff({"a": 1}, {"a": 1, "b": nans}, verbose_level=2)
    assert len(real["dictionary_item_added"]["root['b']"]) == 2

    onix = OnixDeepDiff({"a": 1}, {"a": 1, "b": nans})
    added = onix.to_dict()["dictionary_item_added"]["root['b']"]
    assert len(added) == 1


# --- regression: JSON rendering of a buried non-finite leaf stays linear ---


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
    assert elapsed < 2.0, elapsed


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
