# /// script
# requires-python = "==3.14.*"
# dependencies = ["deepdiff==9.1.0"]
# ///
"""Generate onix's golden corpus from real DeepDiff output.

For each hand-designed case in ``CASES`` this writes three files under
``tests/golden/<case_name>/``:

- ``a.json`` / ``b.json``: the two inputs, exactly as fed to both DeepDiff and
  (via the Rust golden test) onix. A value JSON cannot express is written in
  the tagged encoding ``golden_tags`` defines, which marks each tag supported
  or reserved, and every written file is read back and checked against the
  case it came from before ``expected.json`` is generated.
- ``expected.json``: ``json.loads(DeepDiff(a, b, verbose_level=2).to_json(
  default_mapping=golden_tags.JSON_DEFAULT_MAPPING))`` re-dumped with
  ``sort_keys=True`` — the canonical spec onix's own report must match. The
  mapping is what lets a case hold a ``date``, which DeepDiff's stock
  ``to_json()`` refuses to serialize; see ``golden_tags`` for why it renders
  exactly what onix does.

This is the ONLY source of the golden corpus: every file it writes is
committed, and regenerating must reproduce them byte-for-byte (no timestamps,
no random ordering — ``CASES`` and its inputs are the sole source of
variation). A Python set's iteration order is the one thing that does vary
per process — it follows hash order, and for ``str`` members
``PYTHONHASHSEED`` — so nothing here records or depends on it: fixtures write
a set's members in onix's canonical order, and ``golden_tags.canonical_report``
puts DeepDiff's own set-derived output into that same order before it is
written. See ``tests/golden/README.md``'s "Set iteration order" section. Run
with the pinned interpreter/dependency declared in this file's inline script
metadata:

    uv run scripts/gen_goldens.py

See ``tests/golden/README.md`` for the pinned versions and any documented,
out-of-scope DeepDiff quirk excluded from this corpus.
"""

import json
import random
import unicodedata
from datetime import date, datetime, time, timedelta, timezone
from pathlib import Path
from typing import Final

from deepdiff import DeepDiff
from golden_tags import TaggedValue, canonical_report, decode_tags, encode_tags

UTC: Final[timezone] = timezone.utc
PLUS_TWO: Final[timezone] = timezone(timedelta(hours=2))
MINUS_FIVE: Final[timezone] = timezone(timedelta(hours=-5))
# An offset that is not a whole number of minutes, which widens `isoformat()`'s
# suffix from `+HH:MM` to `+HH:MM:SS`.
PLUS_THIRTY_THIRTY: Final[timezone] = timezone(timedelta(seconds=1830))

GOLDEN_ROOT = Path(__file__).resolve().parent.parent / "tests" / "golden"

# Each case is a small, hand-designed (t1, t2) pair. Keep cases SMALL and
# focused on one behavior each — this is the correctness corpus, not a
# performance fixture (see perf/ for those).
CASES: dict[str, tuple[TaggedValue, TaggedValue]] = {
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
    # threshold_to_diff_deeper=0.33: below this key-overlap ratio
    # (intersection / union), a dict-vs-dict comparison collapses into one
    # wholesale values_changed (old/new value the whole dict) instead of
    # recursing key by key. Zero overlap at the root.
    "threshold_collapse_root_zero_overlap": (
        {"a": 1, "b": 2, "c": 3},
        {"d": 4, "e": 5, "f": 6},
    ),
    # Same collapse one level down, nested inside an unrelated key.
    "threshold_collapse_nested_low_overlap": (
        {"x": {"a": 1, "b": 2, "c": 3}},
        {"x": {"d": 4, "e": 5, "f": 6}},
    ),
    # A shared key with an unchanged value doesn't save the pair from
    # collapsing: overlap is 1/5 = 0.2, still below the cutoff, so the
    # whole dict collapses even though "a" is identical on both sides.
    "threshold_collapse_shared_key_same_value_still_collapses": (
        {"a": 1, "b": 2, "c": 3},
        {"a": 1, "d": 4, "e": 5},
    ),
    # Boundary: 33 shared keys out of 100 total is a ratio of exactly
    # 0.33, which does NOT collapse (the check is strict "<", not "<=").
    "threshold_collapse_boundary_exactly_0_33_not_collapsed": (
        {f"k{i}": i for i in range(100)},
        {f"k{i}": i + 1000 for i in range(33)},
    ),
    # One key overlap fewer (32/100 = 0.32) crosses the boundary and
    # collapses.
    "threshold_collapse_boundary_just_below_0_32_collapsed": (
        {f"k{i}": i for i in range(100)},
        {f"k{i}": i + 1000 for i in range(32)},
    ),
    # The same collapse fires for a dict nested inside a list, on the
    # ordinary (non-ignore_order) index-aligned comparison path.
    "threshold_collapse_dict_in_list_ordered": (
        [{"a": 1, "b": 2, "c": 3}],
        [{"d": 4, "e": 5, "f": 6}],
    ),
    # The collapsed old/new value can itself carry arbitrarily nested
    # structure — the whole dict is cloned in as-is.
    "threshold_collapse_deep_nested_values": (
        {"p": {"q": {"r": 1}}, "s": 2, "t": 3},
        {"u": 4, "v": 5, "w": 6},
    ),
    # A collapsed dict-vs-dict pair coexists with an unrelated type_changes
    # finding elsewhere in the same tree.
    "threshold_collapse_alongside_type_changes": (
        {"x": {"a": 1, "b": 2, "c": 3}, "y": 5},
        {"x": {"d": 4, "e": 5, "f": 6}, "y": "5"},
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
    # DeepDiff's default (non-ignore_order) list comparison runs an
    # LCS/difflib-style match instead of plain index-aligned comparison
    # whenever every element of *both* lists is a JSON scalar (its own
    # "basic hashable" check) — see crates/onix-core/src/diff/mod.rs's
    # "List diffing" module doc for the full spec these cases pin down.
    #
    # Real DeepDiff matches this pair as an insert of True at the front plus
    # a delete of the trailing False, not the three-way values_changed a
    # naive index-aligned scan would report.
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
    # Tuples. DeepDiff diffs a tuple positionally exactly like a list — the
    # same difflib match for all-scalar contents, the same surplus-tail
    # add/remove, the same recursion into elements — but a tuple and a list
    # are different Python *types*, so pairing the two is a type_changes at
    # the container itself, never an element-wise diff. Tuples reach these
    # fixtures through the tagged encoding (see tests/golden/README.md).
    "tuple_values_changed": ((1, 2, 3), (1, 2, 4)),
    "tuple_length_change": ((1, 2), (1, 2, 3)),
    "tuple_of_dicts": (({"a": 1},), ({"a": 2},)),
    "tuple_nested_in_tuple": (((1, 2),), ((1, 3),)),
    "tuple_added_as_dict_value": ({}, {"s": (1, 2)}),
    "tuple_vs_list_at_root": ((1, 2), [1, 2]),
    "tuple_vs_list_nested_in_dict": ({"a": [1, 2]}, {"a": (1, 2)}),
    "tuple_empty_vs_empty_list": ((), []),
    # The same pair as list_lcs_new_path_after_index_drift, as tuples: the
    # difflib match (and its new_path) applies inside a tuple too.
    "tuple_of_scalars_uses_lcs_match": ((0, 0, 3, 3, 0, 1, 0), (4, 3, 0, 4)),
    # A tuple element is not basic-hashable, so it disqualifies its list from
    # the difflib match exactly as a nested list or dict element does.
    "tuple_element_disqualifies_list_lcs": ([(1, 2), 3], [3]),
    # Sets and frozensets: two report categories of their own
    # (set_item_added/set_item_removed), whose entries are paths ending in
    # the item itself. See crates/onix-core/src/diff/set.rs.
    "set_added_and_removed_at_root": ({1, 2, 3}, {2, 3, 4}),
    "set_nested_in_dict": ({"s": {1, "a"}}, {"s": {1, "b"}}),
    "set_item_removed_at_depth": ({"a": {1, 2}}, {"a": {1}}),
    "set_added_as_dict_value": ({}, {"s": {1, 2}}),
    "set_same_items_is_empty": ({1, 2}, {2, 1}),
    "set_empty_vs_one_item": (set(), {1}),
    "set_multiple_items_each_side": (set(range(5)), set(range(3, 8))),
    "set_vs_list_at_root": ({1, 2}, [1, 2]),
    "set_vs_dict_at_root": ({"a": 1}, {1, 2}),
    "frozenset_item_added": (frozenset({1}), frozenset({1, 2})),
    # Item rendering, one case per kind: DeepDiff formats a set item with
    # `str()`, except a top-level `str` item which it wraps in single quotes
    # unconditionally and unescaped (NOT the dict-key rule).
    "set_none_item_removed": ({None, 1}, {1}),
    "set_bool_vs_int_are_distinct_items": ({True}, {1}),
    "set_int_vs_float_are_distinct_items": ({1}, {1.0}),
    "set_float_item_reprs": ({1.0, 0.1, 1e16, 1e-05, -0.0}, {2.0}),
    # Two tuple members whose leading elements are `-0.0` and `0.0`: those
    # fold together in canonical order (matching Python's own `set`, which
    # treats the two zeros as equal too), so the pair ranks by the one thing
    # left to compare once that element ties: length.
    "set_signed_zero_tuple_members_ranked_by_length": ({}, {"s": {(0.0,), (-0.0, -1)}}),
    # A bare `-0.0` and `0.0` are one Python-equal, one-element set apiece
    # (`{0.0}` and `{-0.0}` are each single-member sets, never a two-zero
    # set — a real `set` collapses that pair before it ever reaches onix),
    # so comparing the two sets reports nothing.
    "set_bare_signed_zero_members_compare_equal": ({0.0}, {-0.0}),
    # The same equality one level down, alongside an unrelated shared member
    # that keeps the outer list genuinely worth comparing.
    "set_signed_zero_members_compare_equal_nested": ([{0.0, 1}], [{-0.0, 1}]),
    # A set holding a bare zero next to another member, diffed against a set
    # missing it: the removed item renders as the sign it was given, `0.0`.
    "set_bare_zero_item_removed": ({0.0, 1}, {1}),
    "set_str_items": ({"a"}, {"b"}),
    "set_str_item_with_single_quote": ({"it's"}, {"x"}),
    "set_str_item_with_double_quote": ({'he said "hi"'}, {"x"}),
    "set_tuple_items": ({(1, 2)}, {(1, 3)}),
    "set_nested_tuple_item": ({(1, (2, 3))}, {(1, (2, 4))}),
    # A `str` nested inside a tuple item IS escaped (Python `repr`), unlike a
    # top-level one — the two halves of the rule in one case.
    "set_str_inside_tuple_item": ({("it's",)}, {"x"}),
    # Non-printable code points above U+0100, one per category: U+200B (Cf,
    # zero width space), U+2028 (Zl, line separator), U+E000 (Co, private
    # use) and U+0378 (Cn, unassigned in Unicode 16.0.0).
    "set_str_inside_tuple_item_non_printable_above_u0100": (
        {("\u200b\u2028\ue000\u0378",)},
        {"x"},
    ),
    "set_frozenset_items": ({frozenset({1, 2})}, {frozenset({1, 3})}),
    "set_empty_frozenset_item": ({frozenset()}, {1}),
    # Only a *bare* number is type-wrapped, so a container Python's `==`
    # calls equal is one member: these two pairs are empty where the
    # bare-number pair above is a change.
    "set_tuple_item_python_equality": ({(1,)}, {(1.0,)}),
    "set_frozenset_item_python_equality": ({frozenset({1})}, {frozenset({1.0})}),
    # A set element disqualifies its list from the difflib match, the way a
    # nested list or a tuple element does.
    "set_element_disqualifies_list_lcs": ([{1}, 3], [3]),
    "set_inside_list_diffs_positionally": ([{1, 2}], [{2, 3}]),
    # A tuple and a frozenset holding the same members are not Python-equal,
    # so DeepHash's shared cache must keep them in separate namespaces: if it
    # did not, these two items would share a digest and the report would be
    # empty. Both are reported by path only, which is what lets this be a
    # golden at all (a frozenset *value* cannot be serialized by DeepDiff).
    "set_tuple_and_frozenset_items_never_share_a_digest": (
        {(1,)},
        {frozenset({1})},
    ),
    # DeepDiff's global, whole-tree
    # mutual_add_removes_to_become_value_changes() post-pass (model.py) —
    # runs once, after the entire diff, and merges any iterable_item_added
    # and iterable_item_removed pair that renders to the exact same path
    # string into one values_changed, purely because the path strings
    # coincide (no relation between the two values otherwise).
    "list_lcs_mutual_add_remove_merge": (
        [False, False, None, 2, 3.8, None],
        [None, False, -3, None, None, 3, 2, None],
    ),
    # --- datetime and date -------------------------------------------
    # DeepDiff compares two datetimes by instant and reports the pair
    # NORMALIZED to UTC in values_changed (_diff_datetime assigns
    # datetime_normalize's result back onto the level it reports).
    "datetime_values_changed_normalized_to_utc": (
        datetime(2024, 1, 1, 10),
        datetime(2024, 1, 2, 10),
    ),
    # A naive value is stamped as UTC, not read in local time, so these two
    # are one instant and the diff is empty.
    "datetime_naive_and_aware_same_instant_are_equal": (
        datetime(2024, 1, 1, 10),
        datetime(2024, 1, 1, 10, tzinfo=UTC),
    ),
    "datetime_different_offsets_same_instant_are_equal": (
        datetime(2024, 1, 1, 10, tzinfo=UTC),
        datetime(2024, 1, 1, 12, tzinfo=PLUS_TWO),
    ),
    "datetime_microsecond_change": (
        datetime(2024, 1, 1, 10, 0, 0, 123456),
        datetime(2024, 1, 1, 10, 0, 0, 123457),
    ),
    # isoformat() prints microseconds only when they are non-zero, so this
    # pins all three boundaries (0, 1, 999999) in one report.
    "datetime_microsecond_rendering_boundaries": (
        {
            "zero": datetime(2024, 1, 1, 10),
            "one": datetime(2024, 1, 1, 10, 0, 0, 1),
            "max": datetime(2024, 1, 1, 10, 0, 0, 999999),
        },
        {
            "zero": datetime(2024, 1, 2, 10, 0, 0, 1),
            "one": datetime(2024, 1, 2, 10),
            "max": datetime(2024, 1, 2, 10, 0, 0, 999999),
        },
    ),
    "datetime_in_dict_values_changed": (
        {"t": datetime(2024, 1, 1)},
        {"t": datetime(2024, 1, 2)},
    ),
    # Every category other than a datetime-pair values_changed carries the
    # RAW value: no offset suffix for a naive one, the original offset for an
    # aware one.
    "datetime_dictionary_item_added_reports_raw_value": (
        {},
        {"t": datetime(2024, 1, 1)},
    ),
    "datetime_dictionary_item_removed_reports_raw_value": (
        {"t": datetime(2024, 1, 1, 10, tzinfo=MINUS_FIVE)},
        {},
    ),
    "datetime_iterable_item_added_reports_raw_value": (
        [datetime(2024, 1, 1)],
        [datetime(2024, 1, 1), datetime(2024, 1, 2, 10, tzinfo=PLUS_THIRTY_THIRTY)],
    ),
    "datetime_iterable_item_removed_reports_raw_value": (
        [datetime(2024, 1, 1), datetime(2024, 1, 2)],
        [datetime(2024, 1, 1)],
    ),
    "datetime_vs_str_type_change_reports_raw_value": (
        datetime(2024, 1, 1),
        "2024-01-01",
    ),
    "datetime_vs_int_type_change": ({"t": datetime(2024, 1, 1)}, {"t": 5}),
    # An offset with seconds in it: raw on the left of the pair (via the
    # type_changes value), normalized to +00:00 in values_changed.
    "datetime_offset_with_seconds_normalizes": (
        datetime(2024, 1, 1, 10, tzinfo=PLUS_THIRTY_THIRTY),
        datetime(2024, 1, 2, 10, tzinfo=PLUS_THIRTY_THIRTY),
    ),
    "datetime_negative_offset_normalizes": (
        datetime(2024, 1, 1, 10, tzinfo=MINUS_FIVE),
        datetime(2024, 1, 2, 10, tzinfo=MINUS_FIVE),
    ),
    "datetime_leap_day_and_year_boundary": (
        {"leap": datetime(2024, 2, 29, 23, 59, 59), "eve": datetime(2023, 12, 31, 23, 59, 59)},
        {"leap": datetime(2024, 3, 1), "eve": datetime(2024, 1, 1)},
    ),
    # datetime is in DeepDiff's `basic_types`, so a list of them takes the
    # difflib/LCS path: a shifted list aligns and reports one insert plus one
    # delete rather than three values_changed.
    "list_lcs_datetime_shift": (
        [datetime(2024, 1, 1), datetime(2024, 1, 2), datetime(2024, 1, 3)],
        [datetime(2024, 1, 2), datetime(2024, 1, 3), datetime(2024, 1, 4)],
    ),
    # difflib matches with Python's own `==`, which never equates a naive
    # datetime with an aware one — so this pair reaches the 'replace' opcode
    # and is then compared by instant, reporting nothing at all.
    "list_lcs_datetime_naive_vs_aware_same_instant_reports_nothing": (
        [datetime(2024, 1, 1, 10), "anchor"],
        [datetime(2024, 1, 1, 10, tzinfo=UTC), "anchor"],
    ),
    # ...while two aware values at one instant are Python-equal, so difflib
    # matches them as 'equal' and never compares them at all.
    "list_lcs_datetime_aware_same_instant_matches_as_equal": (
        [datetime(2024, 1, 1, 10, tzinfo=UTC), 1],
        [datetime(2024, 1, 1, 12, tzinfo=PLUS_TWO), 2],
    ),
    # An earlier delete drifts the datetime's index, so its 'replace'-opcode
    # values_changed carries `new_path` alongside the normalized pair.
    "list_lcs_datetime_new_path_on_index_drift": (
        ["x", "y", datetime(2024, 1, 1, 10)],
        ["y", datetime(2024, 1, 2, 12, tzinfo=PLUS_TWO)],
    ),
    # A `date` is its own type: never equal to a `datetime`, in either
    # direction, and reported under the name `date`.
    "date_values_changed": (date(2024, 1, 1), date(2024, 1, 2)),
    "date_vs_datetime_type_change": (date(2024, 1, 1), datetime(2024, 1, 1)),
    "date_in_dict_values_changed": ({"d": date(2024, 1, 1)}, {"d": date(2024, 3, 5)}),
    "date_in_dict_added": ({}, {"d": date(2024, 1, 1)}),
    "date_and_datetime_in_one_report": (
        {"d": date(2024, 1, 1), "t": datetime(2024, 1, 1, 10)},
        {"d": date(2024, 1, 2), "t": datetime(2024, 1, 2, 10)},
    ),
    "date_leap_day": (date(2024, 2, 29), date(2024, 3, 1)),
    # --- time and timedelta (issue #61) ----------------------------------
    # `_diff_time` (real DeepDiff's function for time, date AND timedelta
    # alike) is a plain `!=` with no normalization step, unlike
    # `_diff_datetime` -- so a values_changed entry here carries the RAW
    # pair, never a normalized one.
    "time_values_changed": (time(10, 30), time(12, 0)),
    "time_vs_datetime_type_change": (time(10, 30), datetime(2024, 1, 1, 10, 30)),
    "time_vs_date_type_change": (time(10, 30), date(2024, 1, 1)),
    "time_in_dict_values_changed": ({"t": time(10, 30)}, {"t": time(12, 0)}),
    "time_in_dict_added": ({}, {"t": time(10, 30)}),
    "time_in_dict_removed": ({"t": time(10, 30, tzinfo=MINUS_FIVE)}, {}),
    # Unlike a datetime, a naive time is NEVER equal to an aware one --
    # real `time.__eq__` never reads a naive value as if it were UTC.
    "time_naive_vs_aware_same_hms_are_different": (time(10, 0), time(10, 0, tzinfo=UTC)),
    # Two aware times at the same offset-adjusted instant ARE equal, at
    # full microsecond precision (only the ignore_order hash truncates).
    "time_aware_different_offsets_same_instant_are_equal": (
        time(10, 0, tzinfo=UTC),
        time(12, 0, tzinfo=PLUS_TWO),
    ),
    "time_microsecond_change": (time(10, 0, 0, 123_456), time(10, 0, 0, 123_457)),
    # isoformat() always shows seconds (unlike datetime.isoformat()) and
    # microseconds only when non-zero.
    "time_microsecond_rendering_boundaries": (
        {"zero": time(10, 0), "one": time(10, 0, 0, 1), "max": time(10, 0, 0, 999_999)},
        {"zero": time(11, 0, 0, 1), "one": time(11, 0), "max": time(11, 0, 0, 999_999)},
    ),
    "time_offset_with_seconds_renders_widened_suffix": (
        time(10, 0, tzinfo=PLUS_THIRTY_THIRTY),
        time(11, 0, tzinfo=PLUS_THIRTY_THIRTY),
    ),
    # `time` is in DeepDiff's `basic_types`, so a list of them takes the
    # difflib/LCS path.
    "list_lcs_time_shift": (
        [time(1, 0), time(2, 0), time(3, 0)],
        [time(2, 0), time(3, 0), time(4, 0)],
    ),
    # difflib matches with Python's own `==`, which -- unlike datetime --
    # never equates a naive time with an aware one, so this pair reaches
    # the 'replace' opcode and IS reported (the opposite of the analogous
    # datetime golden case, since `_diff_time` never normalizes).
    "list_lcs_time_naive_vs_aware_reports_a_change": (
        [time(10, 0), "anchor"],
        [time(10, 0, tzinfo=UTC), "anchor"],
    ),
    # ...while two aware values at one instant are Python-equal, so difflib
    # matches them as 'equal' and never compares them at all.
    "list_lcs_time_aware_same_instant_matches_as_equal": (
        [time(10, 0, tzinfo=UTC), 1],
        [time(12, 0, tzinfo=PLUS_TWO), 2],
    ),
    "timedelta_values_changed": (
        timedelta(days=1, seconds=3600),
        timedelta(days=2),
    ),
    "timedelta_vs_int_type_change": ({"t": timedelta(seconds=1)}, {"t": 1}),
    "timedelta_vs_str_type_change_reports_raw_value": (timedelta(seconds=1), "0:00:01"),
    "timedelta_in_dict_values_changed": (
        {"t": timedelta(0)},
        {"t": timedelta(days=1)},
    ),
    "timedelta_in_dict_added": ({}, {"t": timedelta(seconds=1)}),
    "timedelta_negative_and_zero": (timedelta(0), timedelta(seconds=-1)),
    "timedelta_rendering_boundaries": (
        {
            "zero": timedelta(0),
            "one_day": timedelta(days=1),
            "micro": timedelta(microseconds=1),
        },
        {
            "zero": timedelta(seconds=1),
            "one_day": timedelta(days=-1),
            "micro": timedelta(0),
        },
    ),
    "list_lcs_timedelta_shift": (
        [timedelta(seconds=1), timedelta(seconds=2), timedelta(seconds=3)],
        [timedelta(seconds=2), timedelta(seconds=3), timedelta(seconds=4)],
    ),
    "time_and_timedelta_in_one_report": (
        {"t": time(10, 30), "d": timedelta(seconds=1)},
        {"t": time(12, 0), "d": timedelta(seconds=2)},
    ),
    # A `time`/`timedelta` set item renders with str() (space/no quotes),
    # like datetime/date -- and str() genuinely differs from repr() for both.
    "set_time_naive_item": ({time(10, 30)}, {"sentinel"}),
    "set_time_aware_item": ({time(10, 30, tzinfo=UTC)}, {"sentinel"}),
    "set_timedelta_item": ({timedelta(days=1, seconds=3600)}, {"sentinel"}),
    "set_time_nested_in_tuple_item": ({(time(10, 30),)}, {"sentinel"}),
    "set_timedelta_nested_in_frozenset_item": (
        {frozenset({timedelta(seconds=1)})},
        {"sentinel"},
    ),
    # --- datetime and date as set members (issue #21) -------------------
    # A set item is rendered with str() (space separator), unlike every
    # other item kind (rendered with repr()) -- and a calendar value is the
    # one item kind whose str() and repr() genuinely differ.
    "set_datetime_naive_item": ({datetime(2024, 1, 1)}, {"sentinel"}),
    "set_datetime_aware_item": ({datetime(2024, 1, 1, tzinfo=UTC)}, {"sentinel"}),
    "set_datetime_microsecond_item": (
        {datetime(2024, 1, 1, 10, 0, 0, 123456)},
        {"sentinel"},
    ),
    "set_date_item": ({date(2024, 1, 1)}, {"sentinel"}),
    # _diff_set hashes through DeepHash, whose _prep_datetime normalizes to
    # UTC before hashing -- so a naive and an aware value at one instant are
    # a single set member, unlike plain Python `==` (which never equates
    # the two). See tests/golden/README.md's "Set iteration order" section.
    "set_datetime_naive_and_aware_same_instant_are_one_member": (
        {datetime(2024, 1, 1, 10)},
        {datetime(2024, 1, 1, 10, tzinfo=UTC)},
    ),
    # The same pairing, but with an unrelated second member on each side so
    # the two sets are not equal as wholes: a single-member pair that
    # hash-matches makes the two SETS themselves structurally equal, which
    # short-circuits before _diff_set's per-member identity comparison ever
    # runs at all -- this case forces that comparison to genuinely execute.
    "set_datetime_naive_and_aware_same_instant_alongside_a_change": (
        {datetime(2024, 1, 1, 10), 1},
        {datetime(2024, 1, 1, 10, tzinfo=UTC), 2},
    ),
    # ...while _prep_date deliberately skips normalization, so a date and a
    # datetime at the same midnight never share a digest.
    "set_date_and_datetime_never_share_a_digest": (
        {date(2024, 1, 1)},
        {datetime(2024, 1, 1)},
    ),
    # A tuple pairing a naive/aware datetime with a bool: the naive/aware
    # pair blocks DeepHash's whole-tuple equality cache, so the tuple's
    # content digest is what has to agree instead, and that digest's own
    # bool element has its own ItemKey variant, not one that folds into a
    # plain int/float. The bool DIFFERS across sides (True vs False), so the
    # two tuples must NOT match: pinning the bool's own value here is what a
    # `Bool(b) -> Bool(true)` mutation of the content path fails (it would make
    # False read as True and the tuples spuriously match). An unrelated second
    # member on each side keeps the two sets unequal as wholes regardless,
    # forcing the per-member comparison to run rather than short-circuiting on
    # whole-set equality.
    "set_tuple_datetime_and_bool_sibling_differs_via_content_path": (
        {(datetime(2024, 1, 1, 10), True), 1},
        {(datetime(2024, 1, 1, 10, tzinfo=UTC), False), 2},
    ),
    # DeepHash decides cache-versus-content at EVERY node, not once per whole
    # member: hashing a node first tries a run-scoped cache keyed by the Python
    # object (so a Python-equal nested tuple/frozenset shares one digest, but a
    # naive datetime never shares with an aware one), and only on a miss builds
    # a content digest from the children's (already-cached) digests. So a member
    # can miss the cache at its outer tuple (a naive/aware sibling blocks it)
    # yet its inner container still hits the cache, and the two outer content
    # digests coincide once the datetimes normalize to one instant. Each of
    # these five is `{}` in real deepdiff==9.1.0 (TZ=UTC, verbose_level=2) and
    # was a spurious removal+addition before the per-node model; the bare-number
    # sibling counterpart (set_tuple_datetime_and_bool_sibling_differs...) stays
    # a genuine change, because a bare number is type-distinct with no shared
    # cache entry.
    "set_tuple_datetime_and_nested_tuple_share_inner_cache": (
        {(datetime(2024, 1, 1), (1,))},
        {(datetime(2024, 1, 1, tzinfo=UTC), (1.0,))},
    ),
    "set_tuple_datetime_and_nested_tuple_bool_share_inner_cache": (
        {(datetime(2024, 1, 1), (True,))},
        {(datetime(2024, 1, 1, tzinfo=UTC), (1,))},
    ),
    "set_tuple_datetime_and_nested_frozenset_bool_share_inner_cache": (
        {(datetime(2024, 1, 1), frozenset({True}))},
        {(datetime(2024, 1, 1, tzinfo=UTC), frozenset({1}))},
    ),
    "set_tuple_datetime_and_nested_frozenset_float_share_inner_cache": (
        {(datetime(2024, 1, 1), frozenset({1}))},
        {(datetime(2024, 1, 1, tzinfo=UTC), frozenset({1.0}))},
    ),
    "set_tuple_datetime_and_doubly_nested_tuple_share_inner_cache": (
        {(datetime(2024, 1, 1), ((1,),))},
        {(datetime(2024, 1, 1, tzinfo=UTC), ((1.0,),))},
    ),
    # The naive/aware difference is BELOW the member's own root -- the member is
    # `((datetime,),)`, and the datetimes sit one tuple deeper. The content
    # digest is built through the shared cache at every node, so the inner
    # `(naive,)` and `(aware,)` normalize to one instant and collapse, and the
    # two outer members match; only the `x`/`y` distractors (added so the two
    # sets are not equal as wholes) are reported. `{}` for the members in real
    # deepdiff==9.1.0; a regression that named a nested container by a
    # Python-identity id rather than its content digest reported these members
    # as a spurious removal+addition.
    "set_nested_tuple_naive_aware_collapses_below_the_member_root": (
        {((datetime(2024, 1, 1),),), "x"},
        {((datetime(2024, 1, 1, tzinfo=UTC),),), "y"},
    ),
    # Nested one level inside a tuple or a frozenset item, a calendar value
    # renders with repr() instead -- the same rule a nested str() follows.
    "set_datetime_nested_in_tuple_item": ({(datetime(2024, 1, 1),)}, {"sentinel"}),
    "set_date_nested_in_frozenset_item": (
        {frozenset({date(2024, 1, 1)})},
        {"sentinel"},
    ),
    # --- combined goldens: tuple + set/frozenset + datetime/date in one input
    # (issue #21) --------------------------------------------------------
    "combined_tuple_of_sets_of_tuples": (
        ({(1, 2)}, {(3, 4)}),
        ({(1, 2)}, {(3, 5)}),
    ),
    "combined_set_of_frozensets_of_tuples": (
        {frozenset({(1, 2)}), frozenset({(3, 4)})},
        {frozenset({(1, 2)}), frozenset({(3, 5)})},
    ),
    "combined_type_change_between_tuple_and_set": (
        {"x": (1, datetime(2024, 1, 1))},
        {"x": {1, 2}},
    ),
    # Multi-line string values: at verbose_level=2 DeepDiff adds a `diff`
    # field (a difflib.unified_diff of the two strings) to a values_changed
    # entry whenever both values are strings and one contains a newline
    # (_diff_str in deepdiff/diff.py). See crates/onix-core/src/unified_diff.rs.
    "multiline_string_values_changed_at_root": ("a\nb", "c\nd"),
    "multiline_string_in_dict_value": (
        {"k": "line1\nline2"},
        {"k": "line1\nline3"},
    ),
    # A newline on only one side still triggers the field; both sides are
    # split with splitlines().
    "multiline_string_newline_on_old_side_only": ("a\nb", "cd"),
    "multiline_string_newline_on_new_side_only": ("ab", "c\nd"),
    # A plain single-line string change gets no diff field at all.
    "singleline_string_change_has_no_diff_field": ({"a": "ab"}, {"a": "cd"}),
    # splitlines() treats \r\n as one boundary and \r as its own.
    "multiline_string_crlf_boundary": ("a\r\nb", "a\r\nc"),
    "multiline_string_bare_cr_splits_once_a_newline_triggers": (
        "a\rb\nc",
        "x\ry\nz",
    ),
    # Leading and trailing newlines: a leading newline is a blank first
    # line; a trailing newline is dropped by splitlines().
    "multiline_string_leading_and_trailing_newlines": ("\na\nb\n", "\nc\nd\n"),
    # The strings differ only by a trailing newline, which splitlines()
    # drops, so unified_diff is empty and no diff field is added — but a
    # values_changed entry is still reported.
    "multiline_string_trailing_newline_only_has_no_diff_field": (
        {"a": "a\nb"},
        {"a": "a\nb\n"},
    ),
    # Identical multi-line strings produce no entry at all.
    "multiline_string_identical_no_entry": ({"k": "a\nb"}, {"k": "a\nb"}),
    # Two far-apart changes become two hunks, each with up to 3 context
    # lines (difflib's default n=3).
    "multiline_string_two_hunks": (
        "\n".join("L%d" % i for i in range(20)),
        "\n".join(
            "CHANGED1" if i == 1 else "CHANGED15" if i == 15 else "L%d" % i
            for i in range(20)
        ),
    ),
    # A multi-line string of 250+ lines with a popular repeated line: unlike
    # ordered LIST comparison (autojunk=False), difflib's unified_diff uses
    # the stdlib default (autojunk=True), which purges a line occurring more
    # than len/100 + 1 times, changing the alignment. This pins that the
    # string-diff path reproduces it.
    "multiline_string_autojunk_scale": (
        "\n".join("dup" if i % 3 == 0 else "a%d" % i for i in range(250)),
        "\n".join("dup" if i % 3 == 0 else "b%d" % i for i in range(250)),
    ),
    # A 200-line popular run (purged by autojunk) sits before a shared block
    # that is shifted by one line, so difflib re-bridges the match backward
    # over the purged lines — the extension step's backward direction.
    "multiline_string_autojunk_backward_extension": (
        "\n".join(["P"] * 200 + ["u%d" % i for i in range(50)] + ["tail_a"]),
        "\n".join(["head_b"] + ["P"] * 200 + ["u%d" % i for i in range(50)]),
    ),
}


# Seeded-random scalar-list cases: a permanent, deterministic batch. Fixed
# seed, small alphabet (biased toward collisions/repeats so the LCS match,
# the index-aligned tie-break, AND the
# mutual_add_removes_to_become_value_changes merge all get exercised across
# the batch) — regenerating must reproduce these byte-for-byte, same as
# every other case here.
_FUZZ_SEED = 0xF0F0_C0DE
_FUZZ_CASE_COUNT = 20
_FUZZ_ALPHABET: Final[list[TaggedValue]] = [
    None, True, False, 0, 1, 2, 3, -3, 0.0, 1.0, 2.0, 3.8, "a", "b",
]


def _generate_fuzz_cases() -> dict[str, tuple[TaggedValue, TaggedValue]]:
    """
    Generate the seeded-random scalar-list golden cases.

    :return: A mapping of case name to `(a, b)`, deterministic across runs.
    """
    rng = random.Random(_FUZZ_SEED)
    cases: dict[str, tuple[TaggedValue, TaggedValue]] = {}

    for i in range(_FUZZ_CASE_COUNT):
        len_a = rng.randint(0, 9)
        len_b = rng.randint(0, 9)
        a = [rng.choice(_FUZZ_ALPHABET) for _ in range(len_a)]
        b = [rng.choice(_FUZZ_ALPHABET) for _ in range(len_b)]
        cases[f"list_lcs_fuzz_seed_{i:02d}"] = (a, b)

    return cases


# Seeded-random time/timedelta cases (issue #61's differential-fuzz
# requirement): a small, deterministic alphabet biased toward the shapes
# whose comparison/hashing rules are the trickiest (naive vs aware at the
# same wall-clock and at the same offset-adjusted instant, microsecond-only
# and offset-only differences). Bare scalars in a plain list, never nested
# in a hashable container, per the differential-fuzz-alphabet convention.
_TIME_FUZZ_SEED = 0xDA7E_71ED
_TIME_FUZZ_CASE_COUNT = 15
_TIME_FUZZ_ALPHABET: Final[list[TaggedValue]] = [
    time(0, 0),
    time(10, 30),
    time(10, 30, 0, 123_456),
    time(10, 30, tzinfo=UTC),
    time(10, 30, tzinfo=PLUS_TWO),
    time(12, 0, tzinfo=PLUS_TWO),
    time(23, 59, 59, 999_999),
    timedelta(0),
    timedelta(seconds=1),
    timedelta(days=1),
    timedelta(days=-1, seconds=3600),
    timedelta(microseconds=1),
    "anchor",
]


def _generate_time_timedelta_fuzz_cases() -> dict[str, tuple[TaggedValue, TaggedValue]]:
    """
    Generate the seeded-random time/timedelta ordered-list golden cases.

    :return: A mapping of case name to `(a, b)`, deterministic across runs.
    """
    rng = random.Random(_TIME_FUZZ_SEED)
    cases: dict[str, tuple[TaggedValue, TaggedValue]] = {}

    for i in range(_TIME_FUZZ_CASE_COUNT):
        len_a = rng.randint(0, 8)
        len_b = rng.randint(0, 8)
        a = [rng.choice(_TIME_FUZZ_ALPHABET) for _ in range(len_a)]
        b = [rng.choice(_TIME_FUZZ_ALPHABET) for _ in range(len_b)]
        cases[f"list_lcs_time_timedelta_fuzz_seed_{i:02d}"] = (a, b)

    return cases


def _generate_time_timedelta_ignore_order_fuzz_cases() -> (
    dict[str, tuple[TaggedValue, TaggedValue, dict[str, bool]]]
):
    """
    Generate the seeded-random time/timedelta ignore_order golden cases.

    :return: A mapping of case name to `(a, b, {"ignore_order": True})`, deterministic across
        runs.
    """
    rng = random.Random(_TIME_FUZZ_SEED + 1)
    cases: dict[str, tuple[TaggedValue, TaggedValue, dict[str, bool]]] = {}

    for i in range(_TIME_FUZZ_CASE_COUNT):
        size = rng.randint(0, 8)
        a = [rng.choice(_TIME_FUZZ_ALPHABET) for _ in range(size)]
        b = list(a)
        rng.shuffle(b)
        change_n = rng.randint(0, size)
        for index in rng.sample(range(size), change_n) if size else []:
            b[index] = rng.choice(_TIME_FUZZ_ALPHABET)
        cases[f"ignore_order_time_timedelta_fuzz_seed_{i:02d}"] = (a, b, {"ignore_order": True})

    return cases


# Hand-designed ignore_order=True cases — see
# crates/onix-core/src/ignore_order/mod.rs's module doc for the full,
# source-cited spec these pin down. Each entry carries an
# explicit {"ignore_order": True} kwargs dict (the third tuple element),
# distinguishing it from the ordered-path CASES above.
IGNORE_ORDER_CASES: dict[str, tuple[TaggedValue, TaggedValue, dict[str, bool]]] = {
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
    # threshold_to_diff_deeper=0.33 wholesale-collapse behavior, see the
    # threshold_collapse_* cases above), so this exercises the normal
    # per-key recursion path and pins down that dict comparison itself is
    # unaffected by ignore_order.
    "ignore_order_dict_comparison_unaffected": (
        {"a": 1, "s1": 1, "s2": 2, "s3": 3},
        {"b": 1, "s1": 1, "s2": 2, "s3": 3},
        {"ignore_order": True},
    ),
    # Type-change distance formula, general coercion rule: DeepDiff's
    # DELTA_VIEW omits a type_changes entry's new_value whenever applying
    # the new side's own type to the old value reproduces it exactly
    # (new_type(old_value) == new_value) —
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
    # A nested dict-vs-dict pair with low key overlap (`[{aa,bb,cc}]` vs
    # `[{}]`) inside an ignore_order pairing candidate: both the pairing
    # decision itself and the nested pair's own reported shape (a
    # collapsed values_changed, not granular add/remove) now match real
    # DeepDiff exactly.
    "ignore_order_nested_low_overlap_dict_pairing": (
        ["y", 1, [{"aa": 1, "bb": 2, "cc": 3}]],
        ["y", 0.0, 2, [{}]],
        {"ignore_order": True},
    ),
    # Two sibling subtrees whose inner lists share a DeepHash item key (the
    # order- and repetition-insensitive set {3, 4}) but differ in element
    # repetition: paired against [9, 8], the short list is close enough to pair
    # (a whole-element values_changed) while the long list recurses. onix's
    # distance memo must not hand the short list's cached distance to the long
    # one -- it keys the cache by each value's exact structural identity, not by
    # the item key (issue #31). DeepDiff is stable across PYTHONHASHSEED here.
    "ignore_order_memo_repetition_collision_distinct_distances": (
        {"p": [[3, 4]], "q": [[3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4]]},
        {"p": [[9, 8]], "q": [[9, 8]]},
        {"ignore_order": True},
    ),
    # Tuples under ignore_order: the tuple itself is hash-paired like a list
    # (including new_path on a drifted pairing), a tuple nested as an element
    # hashes order-insensitively like a nested list, and a tuple never
    # hash-matches a list with the same items (DeepHash carries the type), so
    # that pairing reports a type_changes instead of nothing.
    "ignore_order_tuple_pairs_with_new_path": ((1, 2, 3), (3, 2, 5), {"ignore_order": True}),
    "ignore_order_tuple_inside_list": (
        ["anchor", (1, 2)],
        [(1, 3), "anchor"],
        {"ignore_order": True},
    ),
    "ignore_order_tuple_vs_list_never_hash_match": ([(1, 2)], [[1, 2]], {"ignore_order": True}),
    "ignore_order_tuple_items_hash_order_insensitively": (
        [(1, 2)],
        [(2, 1)],
        {"ignore_order": True},
    ),
    # DeepHash keys its shared cache by the object itself, so a *hashable*
    # tuple inherits the digest of an earlier Python-equal one in the same
    # run: `(1,)`, `(1.0,)` and `(True,)` are one key to a Python dict. These
    # pin that collision (and its absence for an unhashable tuple) — see
    # crates/onix-core/src/ignore_order/memo.rs's "Tuple digests" section.
    "ignore_order_tuple_digest_collides_int_float": (
        [(1,)],
        [(1.0,)],
        {"ignore_order": True},
    ),
    "ignore_order_tuple_digest_collides_bool_int": (
        [(True,)],
        [(1,)],
        {"ignore_order": True},
    ),
    "ignore_order_tuple_digest_collides_nested_in_tuple": (
        [((1,),)],
        [((1.0,),)],
        {"ignore_order": True},
    ),
    "ignore_order_tuple_digest_collides_as_dict_value": (
        [{"k": (1,)}],
        [{"k": (1.0,)}],
        {"ignore_order": True},
    ),
    # Two items with one digest between them: removing the list reports a
    # single removal, at the first index.
    "ignore_order_tuple_digest_collision_dedupes_removal": (
        [(1,), (1.0,)],
        [],
        {"ignore_order": True},
    ),
    # Control: a tuple holding a list is unhashable, misses the cache, and
    # keeps its own type-strict digest.
    "ignore_order_unhashable_tuple_never_collides": (
        [(1, [1])],
        [(1.0, [1])],
        {"ignore_order": True},
    ),
    # Python's tuple equality is positional, so a reordered pair of the other
    # numeric type inherits nothing.
    "ignore_order_tuple_digest_collision_is_positional": (
        [(1, 2)],
        [(2.0, 1.0)],
        {"ignore_order": True},
    ),
    # Which class member is hashed first is observable: `(1,)` matches the
    # deduplicated `(1, 1)` content digest, `(1.0,)` does not.
    "ignore_order_tuple_digest_first_hashed_member_wins": (
        [(1.0,)],
        [(1, 1)],
        {"ignore_order": True},
    ),
    # `list((1,)) == [1.0]` in Python, so the delta view omits new_value and
    # the pair stays within the pairing cutoff.
    "ignore_order_tuple_vs_list_python_equal_items_pair": (
        [(1,)],
        [[1.0]],
        {"ignore_order": True},
    ),
    # ...while a nested tuple never equals a nested list, so this one does not.
    # Sets under ignore_order: a set has no order to ignore, so DeepDiff
    # dispatches to the same set diff either way; and a set/frozenset is
    # hashed in its own bucket, so it never hash-matches a list or a tuple.
    "ignore_order_set_diff_is_unaffected": ({1, 2}, {2, 3}, {"ignore_order": True}),
    # Signed zero under ignore_order: a set has no order to ignore, so these
    # three answer identically with the flag on or off (see the plain
    # `set_bare_signed_zero_members_compare_equal`,
    # `set_signed_zero_members_compare_equal_nested` and
    # `set_bare_zero_item_removed` cases above).
    "ignore_order_set_bare_signed_zero_members_compare_equal": (
        {0.0},
        {-0.0},
        {"ignore_order": True},
    ),
    "ignore_order_set_signed_zero_members_compare_equal_nested": (
        [{0.0, 1}],
        [{-0.0, 1}],
        {"ignore_order": True},
    ),
    "ignore_order_set_bare_zero_item_removed": (
        {0.0, 1},
        {1},
        {"ignore_order": True},
    ),
    "ignore_order_list_of_sets_pairs": (
        [{1, 2}, {3}],
        [{3}, {1, 2}],
        {"ignore_order": True},
    ),
    "ignore_order_set_vs_list_never_hash_match": ([{1, 2}], [[1, 2]], {"ignore_order": True}),
    "ignore_order_sets_pair_then_diff": (
        [{1, 2, 3, 4}],
        [{1, 2, 3, 5}],
        {"ignore_order": True},
    ),
    # Above the pairing cutoff, so the two sets stay a raw add plus remove,
    # which the whole-tree mutual-add-remove merge then folds into one
    # values_changed.
    "ignore_order_set_pair_above_cutoff_merges": ([{1}], [{1.0}], {"ignore_order": True}),
    # A set is unhashable in Python, so it never inherits another set's
    # digest — the boundary of the shared-cache rule the frozenset case above
    # sits on the other side of.
    "ignore_order_unhashable_set_never_collides": ([{1}, {1.0}], [], {"ignore_order": True}),
    "ignore_order_tuple_and_frozenset_items_never_share_a_digest": (
        {(1,)},
        {frozenset({1})},
        {"ignore_order": True},
    ),
    "ignore_order_set_in_dict_in_list": (
        [{"s": {1, 2}}, "anchor"],
        ["anchor", {"s": {1, 3}}],
        {"ignore_order": True},
    ),
    "ignore_order_tuple_vs_list_nested_kinds_differ": (
        [(1, (2,))],
        [[1, [2]]],
        {"ignore_order": True},
    ),
    # threshold_to_diff_deeper collapse surfacing through a matched pair
    # under ignore_order: "anchor" keeps this list's overlap high enough
    # that get_pairs engages and the low-overlap dicts land in the same
    # slot, so the collapse is visible in the paired recursion's own
    # reported output, not just a top-level add/remove.
    "ignore_order_threshold_collapse_paired_dict": (
        ["anchor", {"a": 1, "b": 2, "c": 3}],
        ["anchor", {"d": 4, "e": 5, "f": 6}],
        {"ignore_order": True},
    ),
    # --- datetime and date under ignore_order -------------------------
    # DeepHash normalizes a datetime to UTC before hashing
    # (_prep_datetime -> datetime_normalize), so a naive value and an aware
    # one at the same instant hash-match and pair with no finding.
    "ignore_order_datetime_naive_and_aware_hash_match": (
        [datetime(2024, 1, 1, 10), "anchor"],
        ["anchor", datetime(2024, 1, 1, 10, tzinfo=UTC)],
        {"ignore_order": True},
    ),
    # The paired items recurse through the ordinary datetime comparison, so
    # this values_changed carries the UTC-normalized pair and a new_path.
    "ignore_order_datetime_pairing_new_path": (
        [datetime(2024, 1, 1), datetime(2024, 1, 2)],
        [datetime(2024, 1, 2), datetime(2024, 1, 3)],
        {"ignore_order": True},
    ),
    # An unpaired item is reported raw, offset and all.
    "ignore_order_datetime_added_and_removed_report_raw_values": (
        [datetime(2024, 1, 1, 10, tzinfo=MINUS_FIVE)],
        [datetime(2030, 6, 1, 10, tzinfo=PLUS_THIRTY_THIRTY), "anchor"],
        {"ignore_order": True},
    ),
    "ignore_order_datetime_in_dicts": (
        [{"t": datetime(2024, 1, 1)}, {"t": datetime(2024, 1, 2)}],
        [{"t": datetime(2024, 1, 2)}, {"t": datetime(2024, 1, 3)}],
        {"ignore_order": True},
    ),
    # `_prep_date` deliberately skips normalization and formats a bare
    # `YYYY-MM-DD`, which can never equal `_prep_datetime`'s
    # `YYYY-MM-DD HH:MM:SS+00:00` — so a date and a datetime at the same
    # midnight never hash-match. They can still be *paired* by distance,
    # because `get_numeric_types_distance` measures the mixed pair with
    # `_get_date_distance` (datetime is a date subclass), which surfaces as a
    # type_changes rather than an add plus a remove.
    "ignore_order_date_and_datetime_never_hash_match": (
        [date(2024, 1, 1), "anchor"],
        ["anchor", datetime(2024, 1, 1)],
        {"ignore_order": True},
    ),
    # A datetime shares no distance family with a number
    # (`get_numeric_types_distance` finds no entry both are an isinstance of),
    # so the structural fallback measures the pair as maximally far and the
    # two stay unpaired.
    "ignore_order_datetime_and_number_never_pair": (
        [datetime(2024, 1, 1, 10), "anchor"],
        ["anchor", 5],
        {"ignore_order": True},
    ),
    # `str(datetime)`/`str(date)` are real strings, so `model.py`'s
    # `new_t1 = new_type(change.t1)` reproduces the new value, the delta
    # omits it, and the pair stays inside the pairing cutoff — a
    # `type_changes` rather than an add plus a remove. The dict wrapper is
    # what makes the rough length large enough for the distance to qualify.
    "ignore_order_datetime_pairs_with_its_own_str": (
        [{"a": datetime(2024, 1, 1)}],
        [{"a": "2024-01-01 00:00:00"}],
        {"ignore_order": True},
    ),
    "ignore_order_date_pairs_with_its_own_str": (
        [{"a": date(2024, 1, 1)}],
        [{"a": "2024-01-01"}],
        {"ignore_order": True},
    ),
    # The control: the same values unwrapped are too far apart to pair.
    "ignore_order_bare_datetime_and_its_str_are_too_far_to_pair": (
        [datetime(2024, 1, 1)],
        ["2024-01-01 00:00:00"],
        {"ignore_order": True},
    ),
    "ignore_order_date_pairing": (
        [date(2024, 1, 1), date(2024, 1, 2)],
        [date(2024, 1, 2), date(2024, 1, 3)],
        {"ignore_order": True},
    ),
    # A set containing a datetime, paired by distance like any other list
    # item: the values_changed carries the two RAW sets (a set-vs-set
    # comparison, not a datetime-vs-datetime one, so no UTC normalization
    # applies here).
    "ignore_order_set_containing_datetime_changes": (
        [{datetime(2024, 1, 1)}],
        [{datetime(2024, 1, 2)}],
        {"ignore_order": True},
    ),
    # --- combined goldens: tuple + set/frozenset + datetime/date in one
    # input, under ignore_order (issue #21) ------------------------------
    "combined_dict_with_datetime_and_set_values_under_ignore_order": (
        {"when": datetime(2024, 1, 1), "tags": {1, 2, 3}},
        {"when": datetime(2024, 1, 2), "tags": {2, 3, 4}},
        {"ignore_order": True},
    ),
    "combined_list_of_tuples_containing_datetimes_under_ignore_order": (
        [(datetime(2024, 1, 1), 1), (datetime(2024, 1, 2), 2)],
        [(datetime(2024, 1, 2), 2), (datetime(2024, 1, 3), 3)],
        {"ignore_order": True},
    ),
    # A multi-line string inside ignore_order lists: the two dicts pair by
    # distance, recursion into the changed `v` value runs _diff_str, and the
    # pairing crosses indices so the entry also carries a new_path. This is
    # the ignore_order path's own route to the diff field.
    "ignore_order_multiline_string_paired_dicts_new_path": (
        [{"id": 1, "v": "x\ny"}, {"id": 2, "v": "q"}],
        [{"id": 2, "v": "q"}, {"id": 1, "v": "x\nz"}],
        {"ignore_order": True},
    ),
    # --- time and timedelta under ignore_order (issue #61) --------------
    # `DeepHash._prep_datetime` reduces a time to `time_to_seconds`,
    # dropping BOTH the microsecond and any offset entirely (a genuine,
    # confirmed quirk) -- so a microsecond-only difference hash-matches
    # under ignore_order even though the ordinary `!=` comparison (and a
    # real Python set's own hash/eq) would call the two different.
    "ignore_order_time_microsecond_only_difference_hash_matches": (
        [time(10, 30, 0, 123_456), "anchor"],
        ["anchor", time(10, 30, 0, 999_999)],
        {"ignore_order": True},
    ),
    # An offset-only difference (same wall-clock h:m:s) hash-matches too,
    # for the identical reason -- `time_to_seconds` never reads `utcoffset()`.
    "ignore_order_time_offset_only_difference_hash_matches": (
        [time(10, 30), "anchor"],
        ["anchor", time(10, 30, tzinfo=PLUS_TWO)],
        {"ignore_order": True},
    ),
    # The paired items still recurse through the ordinary (exact) time
    # comparison, so a genuine h:m:s difference IS reported.
    "ignore_order_time_pairing_new_path": (
        [time(1, 0), time(2, 0)],
        [time(2, 0), time(3, 0)],
        {"ignore_order": True},
    ),
    "ignore_order_time_added_and_removed_report_raw_values": (
        [time(10, 0, tzinfo=MINUS_FIVE)],
        [time(23, 0, tzinfo=PLUS_THIRTY_THIRTY), "anchor"],
        {"ignore_order": True},
    ),
    # A time never shares a distance family with a datetime/date/number
    # (`TYPES_TO_DIST_FUNC` never isinstance-matches `time` against any of
    # them), so the structural fallback keeps them unpaired.
    "ignore_order_time_and_datetime_never_pair": (
        [time(10, 30), "anchor"],
        ["anchor", datetime(2024, 1, 1, 10, 30)],
        {"ignore_order": True},
    ),
    # `_prep_number` hashes a timedelta EXACTLY (no truncation), unlike
    # `time` -- a one-second difference never hash-matches.
    "ignore_order_timedelta_exact_hashing_no_truncation": (
        [timedelta(seconds=1), "anchor"],
        ["anchor", timedelta(seconds=2)],
        {"ignore_order": True},
    ),
    "ignore_order_timedelta_pairing_new_path": (
        [timedelta(seconds=1), timedelta(seconds=2)],
        [timedelta(seconds=2), timedelta(seconds=3)],
        {"ignore_order": True},
    ),
    "ignore_order_timedelta_and_number_never_pair": (
        [timedelta(seconds=1), "anchor"],
        ["anchor", 5],
        {"ignore_order": True},
    ),
    "ignore_order_time_in_dicts": (
        [{"t": time(1, 0)}, {"t": time(2, 0)}],
        [{"t": time(2, 0)}, {"t": time(3, 0)}],
        {"ignore_order": True},
    ),
    "combined_dict_with_time_and_timedelta_values_under_ignore_order": (
        {"t": time(10, 30), "d": {1, 2, 3}},
        {"t": time(12, 0), "d": {2, 3, 4}},
        {"ignore_order": True},
    ),
}

# Seeded-random ignore_order fuzz cases: the ignore_order_10k fixture
# shape (perf/generate_fixtures.py::build_ignore_order_list) at small n — a
# shuffled copy of `a` with a slice of values overwritten from a disjoint
# range, so mutated values can never accidentally collide with untouched
# originals. Small alphabet ones bias toward hash-collisions/duplicates and
# genuine tie-break scenarios, mirroring the ordered-path fuzz batch above.
_IGNORE_ORDER_FUZZ_SEED = 0xC0FF_EE01
_IGNORE_ORDER_FUZZ_CASE_COUNT = 20


def _generate_ignore_order_fuzz_cases() -> dict[str, tuple[TaggedValue, TaggedValue, dict[str, bool]]]:
    """
    Generate the seeded-random ignore_order golden cases.

    :return: A mapping of case name to `(a, b, {"ignore_order": True})`, deterministic across runs.
    """
    rng = random.Random(_IGNORE_ORDER_FUZZ_SEED)
    cases: dict[str, tuple[TaggedValue, TaggedValue, dict[str, bool]]] = {}

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


def write_json(path: Path, value: TaggedValue) -> None:
    """
    Write `value` as pretty-printed, sorted-key, deterministic JSON.

    :param path: File to write.
    :param value: The value to write; tuples are tagged on the way out.
    """
    with path.open("w", encoding="utf-8") as f:
        json.dump(encode_tags(value), f, indent=2, sort_keys=True, ensure_ascii=False)
        f.write("\n")


def read_case_input(path: Path) -> TaggedValue:
    """
    Read a just-written input fixture back as the Python value it stands for.

    :param path: The ``a.json``/``b.json`` file to read.
    :return: The decoded value, with tagged objects turned back into Python objects.
    """
    with path.open(encoding="utf-8") as f:
        return decode_tags(json.load(f))


def main() -> None:
    """Regenerate every case directory under tests/golden/ from every case dict above."""
    # Pinned to 3.14, Unicode 16.0.0; see tests/golden/README.md, Pinned versions.
    assert unicodedata.unidata_version == "16.0.0", (
        f"unicodedata.unidata_version is {unicodedata.unidata_version!r}, not '16.0.0'"
    )

    ordered_cases: dict[str, tuple[TaggedValue, TaggedValue, dict[str, bool]]] = {
        name: (a, b, {})
        for name, (a, b) in {
            **CASES,
            **_generate_fuzz_cases(),
            **_generate_time_timedelta_fuzz_cases(),
        }.items()
    }
    all_cases: dict[str, tuple[TaggedValue, TaggedValue, dict[str, bool]]] = {
        **ordered_cases,
        **IGNORE_ORDER_CASES,
        **_generate_ignore_order_fuzz_cases(),
        **_generate_time_timedelta_ignore_order_fuzz_cases(),
    }

    for name, (a, b, kwargs) in all_cases.items():
        case_dir = GOLDEN_ROOT / name
        case_dir.mkdir(parents=True, exist_ok=True)

        write_json(case_dir / "a.json", a)
        write_json(case_dir / "b.json", b)
        write_json(case_dir / "options.json", {"ignore_order": bool(kwargs.get("ignore_order", False))})

        # The committed bytes must stand for exactly the case defined above,
        # so every fixture is read back and checked before it is used as a
        # spec. DeepDiff is then run on the original objects rather than the
        # round-tripped ones: writing sorts dict keys, and one documented case
        # (path_rendering_collision) has an outcome that depends on a dict's
        # own insertion order.
        for path, value in ((case_dir / "a.json", a), (case_dir / "b.json", b)):
            assert read_case_input(path) == value, f"{path} does not decode back to its case value"

        write_json(
            case_dir / "expected.json",
            canonical_report(DeepDiff(a, b, verbose_level=2, **kwargs)),
        )

    print(f"Wrote {len(all_cases)} golden cases to {GOLDEN_ROOT}")


if __name__ == "__main__":
    main()
