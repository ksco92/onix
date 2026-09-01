# /// script
# requires-python = "==3.13.*"
# dependencies = ["deepdiff==9.1.0"]
# ///
"""Generate onix's M5b golden corpus from real DeepDiff output.

For each hand-designed case in ``CASES`` this writes three files under
``tests/golden/<case_name>/``:

- ``a.json`` / ``b.json``: the two inputs, exactly as fed to both DeepDiff and
  (via the Rust golden test) onix.
- ``expected.json``: ``json.loads(DeepDiff(a, b, verbose_level=2).to_json())``
  re-dumped with ``sort_keys=True`` — the canonical spec onix's own report
  must match.

This is the ONLY source of the golden corpus: every file it writes is
committed, and regenerating must reproduce them byte-for-byte (no timestamps,
no random ordering — ``CASES`` and its inputs are the sole source of
variation). Run with the pinned interpreter/dependency declared in this
file's inline script metadata:

    uv run scripts/gen_goldens.py

See ``tests/golden/README.md`` for the pinned versions and any documented,
out-of-scope DeepDiff quirk excluded from this corpus.
"""

import json
import random
from pathlib import Path
from typing import Final

from deepdiff import DeepDiff

# A JSON-shaped value: exactly the recursive shape onix and DeepDiff both
# diff. Named instead of `typing.Any` per the python-coding-guide's ban on
# `Any` (JSON payloads are its named exception case).
type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None

GOLDEN_ROOT = Path(__file__).resolve().parent.parent / "tests" / "golden"

# Each case is a small, hand-designed (t1, t2) pair. Keep cases SMALL and
# focused on one behavior each — this is the correctness corpus, not a
# performance fixture (see perf/ for those, arriving at M6).
CASES: dict[str, tuple[JsonValue, JsonValue]] = {
    # Scalars: values_changed vs type_changes, and DeepDiff's numeric
    # semantics (int/float/bool are distinct Python types; ints compare by
    # value regardless of magnitude).
    "values_changed_scalar": ({"a": 1}, {"a": 2}),
    "type_change_int_vs_str": ({"a": 1}, {"a": "1"}),
    "type_change_int_vs_float": ({"a": 1}, {"a": 1.0}),
    "type_change_bool_vs_int": ({"a": True}, {"a": 1}),
    "null_vs_value": ({"a": None}, {"a": 1}),
    "float_change": ({"a": 1.5}, {"a": 2.5}),
    "large_integer_equal": ({"a": 18446744073709551615}, {"a": 18446744073709551615}),
    # Type changes between container kinds, at the root and at depth.
    "type_change_dict_vs_scalar": ({"a": {"x": 1}}, {"a": 5}),
    "type_change_list_vs_dict": ({"a": [1, 2]}, {"a": {"x": 1}}),
    "type_change_at_depth": ({"a": {"b": {"c": 1}}}, {"a": {"b": {"c": "1"}}}),
    # Dict item added/removed, including from/to an empty dict and both
    # categories firing together at depth.
    "dictionary_item_added_from_empty": ({}, {"a": 1}),
    "dictionary_item_removed_to_empty": ({"a": 1}, {}),
    "dictionary_added_and_removed_at_depth": (
        {"a": {"x": 1, "y": 2}},
        {"a": {"x": 1, "z": 3}},
    ),
    # List item added/removed: from/to empty, tail growth/shrink (surplus
    # keyed by absolute original index, not renumbered), and a same-length
    # element change.
    "iterable_item_added_from_empty": ([], [1]),
    "iterable_item_removed_to_empty": ([1], []),
    "iterable_item_added_tail": ([1, 2], [1, 2, 3, 4]),
    "iterable_item_removed_tail": ([1, 2, 3, 4], [1, 2]),
    "same_length_list_element_changed": ([1, 2, 3], [1, 9, 3]),
    # Nesting in every combination: dict-in-dict, list-in-list, dict-in-list,
    # list-in-dict.
    "nested_dict_in_dict": ({"a": {"b": 1}}, {"a": {"b": 1, "c": 2}}),
    "nested_list_in_list": ([[1, 2], [3]], [[1, 2, 9], [3]]),
    "nested_dict_in_list": ([{"a": 1}], [{"a": 2}]),
    "nested_list_in_dict": ({"a": [1, 2]}, {"a": [1, 3]}),
    # Unicode + key quoting/escaping. DeepDiff's path quoting is observed
    # behavior, not documented: see tests/golden/README.md.
    "unicode_key": ({"héllo世界": 1}, {"héllo世界": 2}),
    "key_single_quote": ({"it's": 1}, {"it's": 2}),
    "key_double_quote": ({'he said "hi"': 1}, {'he said "hi"': 2}),
    "key_both_quotes": ({"it's \"cool\"": 1}, {"it's \"cool\"": 2}),
    "key_backslash": ({"a\\b": 1}, {"a\\b": 2}),
    "key_control_chars": ({"a\n\t\x00\x7fb": 1}, {"a\n\t\x00\x7fb": 2}),
    # Known DeepDiff quirk (documented in tests/golden/README.md): a dict key
    # whose own text contains `']['`-shaped syntax renders identically to an
    # unrelated, differently-nested path. DeepDiff's own to_json() collapses
    # the collision (keeping one finding); onix's collapse survivor need not
    # match DeepDiff's insertion-order-dependent choice — see the Rust
    # regression test in crates/onix-core/tests/golden.rs for what IS
    # required (no panic, valid DeepDiff-shaped output).
    "path_rendering_collision": (
        {"p'\"][\"q'": 1, "p'": {"q'": 10}},
        {"p'\"][\"q'": 2, "p'": {"q'": 20}},
    ),
    # M6 list-compat fix: DeepDiff's default (non-ignore_order) list
    # comparison runs an LCS/difflib-style match instead of plain
    # index-aligned comparison whenever every element of *both* lists is a
    # JSON scalar (its own "basic hashable" check) — see
    # crates/onix-core/src/diff/mod.rs's "List diffing" module doc for the full
    # spec these cases pin down.
    #
    # The exact repro that surfaced the M6 finding: real DeepDiff matches
    # this as an insert of True at the front plus a delete of the trailing
    # False, not the three-way values_changed a naive index-aligned scan
    # would report.
    "list_lcs_repro_bool_reorder": ([False, True, False], [True, False, True]),
    # A same-length list where the LCS match and the plain index-aligned
    # scan agree (no reordering) — pins down the common case still working.
    "list_lcs_equal_length_replace": ([1, 2, 3], [1, 5, 3]),
    "list_lcs_mid_insert": ([1, 2, 3], [1, 9, 2, 3]),
    "list_lcs_mid_delete": ([1, 9, 2, 3], [1, 2, 3]),
    "list_lcs_shifted": ([1, 2, 3, 4, 5], [2, 3, 4, 5, 6]),
    "list_lcs_repeated_elements": (["a", "a", "b"], ["b", "a", "a"]),
    # A dict element anywhere in either list disqualifies the *whole* list
    # from LCS matching, falling back to plain index-aligned comparison —
    # even though 1 and 2 are hashable scalars themselves.
    "list_lcs_disqualified_by_unhashable_element": (
        [1, {"x": 1}, 2],
        [2, {"x": 1}, 1],
    ),
    # A nested list is unhashable too (not just a dict) — same
    # disqualification, exercised separately from the dict case above.
    "list_lcs_nested_lists_disqualify_matching": ([[1, 2], [3, 4]], [[3, 4], [1, 2]]),
    "list_lcs_mixed_scalar_kinds": (
        [1, 1.5, "a", None, True],
        [True, "a", None, 1.5, 1],
    ),
    # The hashability finding: DeepDiff's LCS match compares elements with
    # Python's cross-type `==` (1 == 1.0 == True), and a matched 'equal'
    # opcode is never diffed further — so this reports as completely empty,
    # unlike every other int/float comparison in DeepDiff (including this
    # same pair inside a dict, see "type_change_int_vs_float" above).
    "list_lcs_int_vs_float_single_matches_via_python_equality": ([1], [1.0]),
    # The new_path finding: an earlier delete shifts this pair's old (a)
    # and new (b) indices apart, so the values_changed/type_changes entry
    # carries a "new_path" alongside its old-index-keyed path.
    "list_lcs_new_path_after_index_drift": (
        [0, 0, 3, 3, 0, 1, 0],
        [4, 3, 0, 4],
    ),
    # Same index-drift shape as above, but the drifted pair also differs in
    # type — exercises new_path on a type_changes entry, not just
    # values_changed.
    "list_lcs_new_path_on_type_change": (
        [0, 0, 3, 3, 0, "x", 0],
        [4, 3, 0, 4],
    ),
    "list_lcs_new_path_unicode_reorder": (["héllo", "世界"], ["世界", "héllo", "new"]),
    # The "keep the smaller, ties favor index-aligned" rule: the LCS match
    # and the plain index-aligned scan both find exactly 2 findings here, so
    # DeepDiff keeps the index-aligned (type_changes + values_changed)
    # result rather than the LCS one (which would report the same 2 findings
    # but differently shaped, keyed off int/float cross-equality matching).
    "list_lcs_tie_break_favors_index_aligned": ([1.0, 2], [2, 1]),
    # >=200 items with one popular value: DeepDiff always constructs its
    # matcher with autojunk=False, so the popular value matches like any
    # other — no special-cased behavior at the stdlib difflib autojunk
    # threshold. 210 items keeps the golden file small while still crossing
    # the threshold difflib's own (disabled) heuristic would apply at.
    "list_lcs_autojunk_disabled_at_scale": (
        ["x"] * 5 + ["distinct_a"] + ["x"] * 204,
        ["x"] * 204 + ["distinct_b"] + ["x"] * 5,
    ),
    # M6c reviewer finding: DeepDiff's global, whole-tree
    # mutual_add_removes_to_become_value_changes() post-pass (model.py) —
    # runs once, after the entire diff, and merges any iterable_item_added
    # and iterable_item_removed pair that renders to the exact same path
    # string into one values_changed, purely because the path strings
    # coincide (no relation between the two values otherwise). Minimal
    # repro from the reviewer.
    "list_lcs_mutual_add_remove_merge": (
        [False, False, None, 2, 3.8, None],
        [None, False, -3, None, None, 3, 2, None],
    ),
}


# Seeded-random scalar-list cases (M6c): bakes the reviewer's differential-fuzz
# insight into the permanent golden suite. Fixed seed, deterministic, small
# alphabet (biased toward collisions/repeats so the LCS match, the
# index-aligned tie-break, AND the mutual_add_removes_to_become_value_changes
# merge all get exercised across the batch) — regenerating must reproduce
# these byte-for-byte, same as every other case here.
_FUZZ_SEED = 0xF0F0_C0DE
_FUZZ_CASE_COUNT = 20
_FUZZ_ALPHABET: Final[list[JsonValue]] = [
    None, True, False, 0, 1, 2, 3, -3, 0.0, 1.0, 2.0, 3.8, "a", "b",
]


def _generate_fuzz_cases() -> dict[str, tuple[JsonValue, JsonValue]]:
    """
    Generate the seeded-random scalar-list golden cases.

    :return: A mapping of case name to `(a, b)`, deterministic across runs.
    """
    rng = random.Random(_FUZZ_SEED)
    cases: dict[str, tuple[JsonValue, JsonValue]] = {}

    for i in range(_FUZZ_CASE_COUNT):
        len_a = rng.randint(0, 9)
        len_b = rng.randint(0, 9)
        a = [rng.choice(_FUZZ_ALPHABET) for _ in range(len_a)]
        b = [rng.choice(_FUZZ_ALPHABET) for _ in range(len_b)]
        cases[f"list_lcs_fuzz_seed_{i:02d}"] = (a, b)

    return cases


# M7 (ignore_order=True) hand-designed cases — see
# crates/onix-core/src/ignore_order/mod.rs's module doc for the full,
# source-cited spec these pin down. Each entry carries an
# explicit {"ignore_order": True} kwargs dict (the third tuple element),
# distinguishing it from the ordered-path CASES above.
IGNORE_ORDER_CASES: dict[str, tuple[JsonValue, JsonValue, dict[str, bool]]] = {
    "ignore_order_pure_shuffle_is_empty": ([1, 2, 3], [3, 2, 1], {"ignore_order": True}),
    "ignore_order_shuffle_plus_one_changed": (
        [10, 20, 30, 40, 50],
        [50, 999, 30, 10, 20],
        {"ignore_order": True},
    ),
    "ignore_order_shuffle_plus_add_remove": (
        [1, 2, 3, 4],
        [4, 2, 3, 5],
        {"ignore_order": True},
    ),
    "ignore_order_duplicates_multiplicity_invisible": (
        [1, 1, 2],
        [1, 2, 2],
        {"ignore_order": True},
    ),
    "ignore_order_nested_dict_pairing": (
        [{"id": 1, "name": "a"}, {"id": 2, "name": "b"}],
        [{"id": 2, "name": "b"}, {"id": 1, "name": "changed"}],
        {"ignore_order": True},
    ),
    "ignore_order_list_in_dict_in_list": (
        [{"tags": ["x", "y", "z"]}, "anchor"],
        ["anchor", {"tags": ["z", "y", "x"]}],
        {"ignore_order": True},
    ),
    "ignore_order_nested_list_order_insensitive_hashing": (
        [[1, 2, 3]],
        [[3, 2, 1]],
        {"ignore_order": True},
    ),
    "ignore_order_type_changes_mixed": (
        [1, "2", 3.0],
        [3.0, 2, "1"],
        {"ignore_order": True},
    ),
    "ignore_order_int_vs_float_single_element": ([1], [1.0], {"ignore_order": True}),
    "ignore_order_bool_vs_int_never_hash_equal": ([True, 2], [1, 2], {"ignore_order": True}),
    "ignore_order_one_sided_all_added": ([], [1, 2, 3], {"ignore_order": True}),
    "ignore_order_one_sided_all_removed": ([1, 2, 3], [], {"ignore_order": True}),
    "ignore_order_gate_boundary_below_threshold_pairs": (
        list(range(20)),
        [*reversed(range(1, 20)), 999],
        {"ignore_order": True},
    ),
    "ignore_order_gate_boundary_above_threshold_raw_add_remove": (
        [1, 1, 2],
        [3, 4],
        {"ignore_order": True},
    ),
    "ignore_order_tiebreak_earliest_t1_index_wins": (
        ["anchor0", "anchor1", "anchor2", {"a": 1, "b": 1}, {"a": 1, "b": 2}],
        ["anchor0", "anchor1", "anchor2", {"a": 1, "b": 3}],
        {"ignore_order": True},
    ),
    "ignore_order_index_drift_new_path_on_nested_finding": (
        [{"id": 1, "meta": {"x": 1}}, "anchorA", "anchorB", "anchorC"],
        ["anchorA", "anchorB", "anchorC", {"id": 1, "meta": {"x": 2}}],
        {"ignore_order": True},
    ),
    "ignore_order_added_removed_no_new_path": (
        [{"id": 1, "meta": {"x": 1}}, "anchorA", "anchorB", "anchorC"],
        ["anchorA", "anchorB", "anchorC", {"id": 1, "meta": {"x": 1}, "extra": 9}],
        {"ignore_order": True},
    ),
    # High key-overlap by design (avoids DeepDiff's own default
    # threshold_to_diff_deeper=0.33 "give up and report a wholesale
    # values_changed instead of granular add/remove" behavior — a genuine,
    # PRE-EXISTING gap in onix-core's object_diff, unrelated to ignore_order
    # (confirmed to fire identically on the plain ordered path too) and
    # explicitly out of scope for this slice; tracked separately as its
    # own follow-up, not fixed here.
    "ignore_order_dict_comparison_unaffected": (
        {"a": 1, "s1": 1, "s2": 2, "s3": 3},
        {"b": 1, "s1": 1, "s2": 2, "s3": 3},
        {"ignore_order": True},
    ),
    # Type-change distance formula, general coercion rule (found during
    # review of the M7 change): DeepDiff's DELTA_VIEW omits a type_changes
    # entry's new_value whenever applying the new side's own type to the
    # old value reproduces it exactly (new_type(old_value) == new_value) —
    # a general Python-coercion rule, not a `new_value == True`-only
    # special case. This pair's outer structural pairing distance depends
    # on that rule (float(0) == 0.0 pairs a nested type_changes with
    # new_value omitted): without it, the pairing distance crosses the 0.3
    # cutoff and the whole comparison falls back to raw add/remove instead.
    "ignore_order_type_change_coercion_general_rule": (
        [[["", ""], []], {}],
        [[1, [], True, {"c": True}], {}],
        {"ignore_order": True},
    ),
    # Sibling case: the coercion rule generalizes past any single literal
    # (new_value here is 0.0, never `true`) — int(0) cast to float equals
    # 0.0, so this recurses to a nested type_changes instead of reporting
    # nothing/something else.
    "ignore_order_type_change_coercion_int_float": (
        [[0]],
        [[0.0]],
        {"ignore_order": True},
    ),
    # Re-verify-round finding: `count_array_diff_leaves`'s trial
    # sub-diff recursed into a nested dict-vs-dict pair (`[{aa,bb,cc}]` vs
    # `[{}]`) through the real, non-`threshold_to_diff_deeper`-aware
    # `crate::diff::object_diff`, inflating that candidate's measured
    # distance past `CUTOFF_DISTANCE_FOR_PAIRS` and corrupting the *pairing
    # decision itself* (not just the reported shape) — the disclosed M2
    # `threshold_to_diff_deeper` gap reaching into `ignore_order` through a
    # route the M7 review missed the first time. Fixed at the distance
    # layer (`crate::DiffOptions::collapse_low_overlap_dicts`); the pairing
    # below now matches real `DeepDiff` exactly. This case is still listed
    # in `KNOWN_DIVERGENT_CASES` (`crates/onix-core/tests/golden.rs`)
    # because the nested `root[2][0]` subtree's own *shape* still shows the
    # separate, disclosed, pre-existing `threshold_to_diff_deeper` gap in
    # `object_diff`'s real reported output (unfixed, next-up) — see
    # `tests/golden/README.md`'s "Known DeepDiff quirks".
    "ignore_order_nested_low_overlap_dict_pairing": (
        ["y", 1, [{"aa": 1, "bb": 2, "cc": 3}]],
        ["y", 0.0, 2, [{}]],
        {"ignore_order": True},
    ),
}

# Seeded-random ignore_order fuzz cases: the M6 ignore_order_10k fixture
# shape (perf/generate_fixtures.py::build_ignore_order_list) at small n — a
# shuffled copy of `a` with a slice of values overwritten from a disjoint
# range, so mutated values can never accidentally collide with untouched
# originals. Small alphabet ones bias toward hash-collisions/duplicates and
# genuine tie-break scenarios, mirroring the ordered-path fuzz batch above.
_IGNORE_ORDER_FUZZ_SEED = 0xC0FF_EE01
_IGNORE_ORDER_FUZZ_CASE_COUNT = 20


def _generate_ignore_order_fuzz_cases() -> dict[str, tuple[JsonValue, JsonValue, dict[str, bool]]]:
    """
    Generate the seeded-random ignore_order golden cases.

    :return: A mapping of case name to `(a, b, {"ignore_order": True})`, deterministic across runs.
    """
    rng = random.Random(_IGNORE_ORDER_FUZZ_SEED)
    cases: dict[str, tuple[JsonValue, JsonValue, dict[str, bool]]] = {}

    for i in range(_IGNORE_ORDER_FUZZ_CASE_COUNT):
        size = rng.randint(0, 12)
        a = [rng.choice(_FUZZ_ALPHABET) for _ in range(size)]
        b = list(a)
        rng.shuffle(b)
        change_n = rng.randint(0, size)
        for index in rng.sample(range(size), change_n) if size else []:
            b[index] = rng.choice(_FUZZ_ALPHABET)
        cases[f"ignore_order_fuzz_seed_{i:02d}"] = (a, b, {"ignore_order": True})

    return cases


def write_json(path: Path, value: JsonValue) -> None:
    """
    Write `value` as pretty-printed, sorted-key, deterministic JSON.

    :param path: File to write.
    :param value: The JSON-serializable value to write.
    """
    with path.open("w", encoding="utf-8") as f:
        json.dump(value, f, indent=2, sort_keys=True, ensure_ascii=False)
        f.write("\n")


def main() -> None:
    """Regenerate every case directory under tests/golden/ from every case dict above."""
    ordered_cases: dict[str, tuple[JsonValue, JsonValue, dict[str, bool]]] = {
        name: (a, b, {}) for name, (a, b) in {**CASES, **_generate_fuzz_cases()}.items()
    }
    all_cases: dict[str, tuple[JsonValue, JsonValue, dict[str, bool]]] = {
        **ordered_cases,
        **IGNORE_ORDER_CASES,
        **_generate_ignore_order_fuzz_cases(),
    }

    for name, (a, b, kwargs) in all_cases.items():
        case_dir = GOLDEN_ROOT / name
        case_dir.mkdir(parents=True, exist_ok=True)

        write_json(case_dir / "a.json", a)
        write_json(case_dir / "b.json", b)
        write_json(case_dir / "options.json", {"ignore_order": bool(kwargs.get("ignore_order", False))})

        expected = json.loads(DeepDiff(a, b, verbose_level=2, **kwargs).to_json())
        write_json(case_dir / "expected.json", expected)

    print(f"Wrote {len(all_cases)} golden cases to {GOLDEN_ROOT}")


if __name__ == "__main__":
    main()
