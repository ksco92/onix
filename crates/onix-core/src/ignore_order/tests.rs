use super::IgnoreOrderMemo;
use crate::diff::DiffOptions;
use crate::test_support::{cdate, cdt, cdt_at, cfrozen, cobj, cset, ctup, cv, cvec};
use crate::value::{SetItems, Value as CValue};
use serde_json::json;

// Thin wrappers routing each `serde_json`-literal-based test through the real
// compact-typed engine via the shared `crate::test_support` converters.
fn diff_with_options(
    a: &serde_json::Value,
    b: &serde_json::Value,
    opts: &DiffOptions,
) -> Result<crate::report::Report, crate::error::Error> {
    crate::diff::diff_with_options(&cv(a), &cv(b), opts)
}
fn item_length(value: &serde_json::Value) -> usize {
    super::distance::item_length(&cv(value))
}
fn type_change_leaf_length(a: &serde_json::Value, b: &serde_json::Value) -> usize {
    super::distance::type_change_leaf_length(&cv(a), &cv(b))
}
fn rough_length(value: &serde_json::Value) -> usize {
    super::distance::rough_length(&cv(value))
}
fn numeric_value(value: &serde_json::Value) -> Option<f64> {
    super::distance::numeric_value(&cv(value))
}
fn count_diff_leaves(
    a: &serde_json::Value,
    b: &serde_json::Value,
    depth: usize,
    opts: &DiffOptions,
) -> usize {
    super::distance::count_diff_leaves(&cv(a), &cv(b), depth, opts, &super::IgnoreOrderMemo::new())
}
fn count_object_diff_leaves(
    a: &serde_json::Map<String, serde_json::Value>,
    b: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
    opts: &DiffOptions,
) -> usize {
    super::distance::count_object_diff_leaves(
        &cobj(a),
        &cobj(b),
        depth,
        opts,
        &super::IgnoreOrderMemo::new(),
    )
}
fn count_array_diff_leaves(
    a: &[serde_json::Value],
    b: &[serde_json::Value],
    depth: usize,
    opts: &DiffOptions,
) -> usize {
    super::distance::count_array_diff_leaves(
        &cvec(a),
        &cvec(b),
        depth,
        opts,
        &super::IgnoreOrderMemo::new(),
    )
}
fn item_key(value: &serde_json::Value) -> super::hash::ItemKey {
    super::hash::item_key(&cv(value), &IgnoreOrderMemo::new())
}

fn ignore_order_diff(a: &serde_json::Value, b: &serde_json::Value) -> serde_json::Value {
    diff_with_options(
        a,
        b,
        &DiffOptions {
            ignore_order: true,
            ..DiffOptions::default()
        },
    )
    .unwrap()
    .to_json_value()
}

// --- end-to-end, against real deepdiff==9.1.0's confirmed output -----
//
// Every expected value below was independently confirmed against a real
// deepdiff==9.1.0 run during this module's research/build (direct
// `DeepDiff(...)` probes against the library itself) — not hand-derived
// from the algorithm this module itself implements.

#[test]
fn pure_shuffle_is_empty() {
    assert_eq!(
        ignore_order_diff(&json!([1, 2, 3]), &json!([3, 2, 1])),
        json!({})
    );
}

#[test]
fn duplicates_multiplicity_change_is_invisible() {
    assert_eq!(
        ignore_order_diff(&json!([1, 1, 2]), &json!([1, 2, 2])),
        json!({})
    );
}

#[test]
fn nested_list_reorder_inside_a_shuffled_outer_list_is_empty() {
    assert_eq!(
        ignore_order_diff(&json!([[1, 2, 3], "x"]), &json!(["x", [3, 2, 1]])),
        json!({})
    );
}

#[test]
fn gate_below_threshold_engages_real_pairing() {
    // n=100, 1 value replaced by a far-away one: mirrors the shape of
    // probe9_m6_shape_100.py (this crate's own perf fixture generator),
    // just with a single mutation instead of 5 — the ratio (2/201) is
    // well under the 0.7 gate either way, so real distance-based
    // pairing engages and the single genuinely-differing pair is
    // reported as one values_changed (not a raw add + a raw remove).
    let a: Vec<i64> = (0..100).collect();
    let mut b = a.clone();
    b.reverse();
    b[0] = 100_000;
    let result = ignore_order_diff(&json!(a), &json!(b));
    let changed = result["values_changed"].as_object().unwrap();
    assert_eq!(changed.len(), 1);
    assert!(result.get("iterable_item_added").is_none());
    assert!(result.get("iterable_item_removed").is_none());
}

#[test]
fn gate_above_threshold_falls_back_to_raw_add_remove_plus_merge() {
    // a has 3 raw items but only 2 DISTINCT hashes; b has 2. Ratio uses
    // DISTINCT hash counts (2+2)/(2+2+1) = 0.8 > 0.7, disabling pairing
    // entirely — confirmed against real deepdiff (traced with the
    // pairing function monkeypatched to prove it's never called).
    assert_eq!(
        ignore_order_diff(&json!([1, 1, 2]), &json!([3, 4])),
        json!({
            "values_changed": {"root[0]": {"new_value": 3, "old_value": 1}},
            "iterable_item_added": {"root[1]": 4},
            "iterable_item_removed": {"root[2]": 2},
        })
    );
}

#[test]
fn one_sided_lists_are_all_added_or_all_removed() {
    assert_eq!(
        ignore_order_diff(&json!([]), &json!([1, 2, 3])),
        json!({"iterable_item_added": {"root[0]": 1, "root[1]": 2, "root[2]": 3}})
    );
    assert_eq!(
        ignore_order_diff(&json!([1, 2, 3]), &json!([])),
        json!({"iterable_item_removed": {"root[0]": 1, "root[1]": 2, "root[2]": 3}})
    );
}

#[test]
fn nested_dict_pairing_with_index_drift_retags_a_nested_finding() {
    // Confirmed against real deepdiff: a nested field two levels inside
    // a hash-paired dict still carries new_path with the outer index
    // swapped and the suffix path unchanged.
    let t1 = json!([{"id": 1, "meta": {"x": 1}}, "anchorA", "anchorB", "anchorC"]);
    let t2 = json!(["anchorA", "anchorB", "anchorC", {"id": 1, "meta": {"x": 2}}]);
    assert_eq!(
        ignore_order_diff(&t1, &t2),
        json!({
            "values_changed": {"root[0]['meta']['x']": {
                "new_value": 2, "old_value": 1, "new_path": "root[3]['meta']['x']",
            }}
        })
    );
}

#[test]
fn dictionary_item_added_nested_in_a_paired_container_has_no_new_path() {
    // Confirmed against real deepdiff: added/removed categories never
    // carry a second path field, even under index drift.
    let t1 = json!([{"id": 1, "meta": {"x": 1}}, "anchorA", "anchorB", "anchorC"]);
    let t2 = json!(["anchorA", "anchorB", "anchorC", {"id": 1, "meta": {"x": 1}, "extra": 9}]);
    assert_eq!(
        ignore_order_diff(&t1, &t2),
        json!({"dictionary_item_added": {"root[0]['extra']": 9}})
    );
}

#[test]
fn type_change_under_ignore_order_pairing() {
    // Confirmed against real deepdiff: [1, "2", 3.0] vs [3.0, 2, "1"] —
    // the numeric pair 1<->2 gets paired (values_changed); "2"/"1" and
    // any cross-type candidates (structural distance 0.5 >= 0.3 cutoff)
    // are left as separate add/remove.
    let result = ignore_order_diff(&json!([1, "2", 3.0]), &json!([3.0, 2, "1"]));
    let changed = &result["values_changed"];
    assert_eq!(changed["root[0]"]["old_value"], json!(1));
    assert_eq!(changed["root[0]"]["new_value"], json!(2));
}

#[test]
fn int_vs_float_single_element_pairs_and_type_changes() {
    // Unlike the ORDERED LCS path's [1] vs [1.0] (Python == matching,
    // reports nothing at all), ignore_order hashes 1 and 1.0 as
    // DIFFERENT keys (type-tagged) — so this is a real hash-different
    // pair, and the recursive diff between them reports a genuine
    // type_changes.
    assert_eq!(
        ignore_order_diff(&json!([1]), &json!([1.0])),
        json!({"type_changes": {"root[0]": {
            "old_type": "int", "new_type": "float", "old_value": 1, "new_value": 1.0,
        }}})
    );
}

#[test]
fn list_in_dict_in_list_recurses_ignore_order_at_every_level() {
    let t1 = json!([{"tags": ["x", "y", "z"]}, "anchor"]);
    let t2 = json!(["anchor", {"tags": ["z", "y", "x"]}]);
    assert_eq!(ignore_order_diff(&t1, &t2), json!({}));
}

#[test]
fn equal_inputs_of_any_shape_are_empty() {
    assert_eq!(ignore_order_diff(&json!(null), &json!(null)), json!({}));
    assert_eq!(ignore_order_diff(&json!([]), &json!([])), json!({}));
    assert_eq!(ignore_order_diff(&json!([1]), &json!([1])), json!({}));
}

#[test]
fn max_depth_exceeded_on_an_over_budget_item_is_a_clean_error() {
    let mut deep = json!(1);
    for _ in 0..10 {
        deep = json!([deep]);
    }
    let err = diff_with_options(
        &json!([0]),
        &json!([deep]),
        &DiffOptions {
            max_depth: 3,
            ignore_order: true,
        },
    )
    .unwrap_err();
    assert!(matches!(err, crate::error::Error::MaxDepthExceeded { .. }));
}

#[test]
fn max_depth_boundary_is_exact_for_an_item_on_the_a_side() {
    // max_depth=2, list itself at depth 0, so its items are checked at
    // depth+1=1: budget = 2-1 = 1. {"a":{"b":1}} has nesting exactly 2
    // (> the budget of 1), so this must fail. A `depth+1` -> `depth`
    // mutant computes depth=0 -> budget=2, under which nesting-2
    // wrongly fits (`deeper_than(_, 2)` is false) — this is the "a"-side
    // pre-pass loop specifically (deep item in `a`, shallow in `b`).
    let err = diff_with_options(
        &json!([{"a": {"b": 1}}]),
        &json!([0]),
        &DiffOptions {
            max_depth: 2,
            ignore_order: true,
        },
    )
    .unwrap_err();
    assert!(matches!(err, crate::error::Error::MaxDepthExceeded { .. }));
}

#[test]
fn max_depth_boundary_is_exact_for_an_item_on_the_b_side() {
    // Same boundary as above, mirrored onto the "b"-side pre-pass loop.
    let err = diff_with_options(
        &json!([0]),
        &json!([{"a": {"b": 1}}]),
        &DiffOptions {
            max_depth: 2,
            ignore_order: true,
        },
    )
    .unwrap_err();
    assert!(matches!(err, crate::error::Error::MaxDepthExceeded { .. }));
}

#[test]
fn get_pairs_gate_ratio_uses_the_sum_of_added_and_removed_not_their_product() {
    // 4 fully-disjoint removed items, 1 fully-disjoint added item:
    // sum=5, denominator=4+1+1=6, ratio=5/6=0.833 > 0.7 -> get_pairs
    // FALSE (raw add/remove, then the path-collision merge at root[0]).
    // A `+` -> `*` mutant computes product=4, ratio=4/6=0.667 <= 0.7 ->
    // WOULD wrongly engage real distance-based pairing instead,
    // changing the result entirely (a numeric pairing recursion instead
    // of the merge-produced values_changed below).
    assert_eq!(
        ignore_order_diff(&json!([1, 2, 3, 4]), &json!([100])),
        json!({
            "values_changed": {"root[0]": {"new_value": 100, "old_value": 1}},
            "iterable_item_removed": {"root[1]": 2, "root[2]": 3, "root[3]": 4},
        })
    );
}

#[test]
fn paired_recursion_depth_boundary_is_exact() {
    // A single unambiguous pair ({"a":{"b":1}} <-> {"a":{"b":9}}, both
    // at their own list index — anchors keep the gate/pairing trivial)
    // whose difference sits exactly 2 dict levels down, with
    // max_depth=2: correctness coverage for the paired-recursion depth
    // boundary in the common case. This does NOT kill the sibling
    // `depth + 1` -> `depth * 1` mutant at the paired `diff_at` call —
    // see that call site's own doc for why it's accepted as
    // structurally unreachable instead.
    let anchors: Vec<serde_json::Value> = (0..10).map(|i| json!(format!("anchor{i}"))).collect();
    let mut a = anchors.clone();
    a.push(json!({"a": {"b": 1}}));
    let mut b = anchors;
    b.push(json!({"a": {"b": 9}}));

    let err = diff_with_options(
        &json!(a),
        &json!(b),
        &DiffOptions {
            max_depth: 2,
            ignore_order: true,
        },
    )
    .unwrap_err();
    assert!(matches!(err, crate::error::Error::MaxDepthExceeded { .. }));
}

#[test]
fn item_key_handles_a_u64_beyond_i64_range() {
    // Confirmed against real deepdiff: large ints round-trip through
    // hashing/matching fine (Python ints are bignums).
    let a = json!([1, 18_446_744_073_709_551_615u64]);
    let b = json!([18_446_744_073_709_551_615u64, 1]);
    assert_eq!(ignore_order_diff(&a, &b), json!({}));
}

#[test]
fn numeric_pairing_at_two_distances_reuses_the_used_check_across_buckets() {
    // "5" (added) has candidates at two distinct distances: "4"
    // (closer) and "100" (farther, still under the 0.3 cutoff). It
    // pairs with the closer one first; when its farther-distance
    // bucket entry is later popped, `used` already contains it — the
    // exact branch this test targets. "100" is left as a genuine
    // unpaired removal.
    let anchors: Vec<serde_json::Value> = (0..10).map(|i| json!(format!("anchor{i}"))).collect();
    let mut a = anchors.clone();
    a.push(json!(4));
    a.push(json!(100));
    let mut b = anchors;
    b.push(json!(5));

    assert_eq!(
        ignore_order_diff(&json!(a), &json!(b)),
        json!({
            "values_changed": {"root[10]": {"new_value": 5, "old_value": 4}},
            "iterable_item_removed": {"root[11]": 100},
        })
    );
}

#[test]
fn distance_pairing_tie_between_two_records_favors_the_earliest_t1_index() {
    // Worked structural-distance tie example:
    // {"a":1,"b":1}->{"a":1,"b":3} and {"a":1,"b":2}->{"a":1,"b":3} both
    // measure the SAME distance (0.1) — diff_length only counts how
    // many fields differ, not the magnitude of the change. DeepDiff's
    // asymmetric tie-break makes the EARLIEST t1 index win.
    let anchors: Vec<serde_json::Value> = (0..10).map(|i| json!(format!("anchor{i}"))).collect();
    let mut a = anchors.clone();
    a.push(json!({"a": 1, "b": 1})); // root[10] — earliest, should win the pairing
    a.push(json!({"a": 1, "b": 2})); // root[11] — should be left as a raw removal
    let mut b = anchors;
    b.push(json!({"a": 1, "b": 3}));

    assert_eq!(
        ignore_order_diff(&json!(a), &json!(b)),
        json!({
            "values_changed": {"root[10]['b']": {"new_value": 3, "old_value": 1}},
            "iterable_item_removed": {"root[11]": {"a": 1, "b": 2}},
        })
    );
}

#[test]
fn structural_distance_of_zero_leaves_pairs_unconditionally() {
    // Single candidate on each side, differing only by an added key
    // whose value is null (item_length(null) == 0), so diff_length ==
    // 0 exactly (rough_distance's own early-return branch).
    let anchors: Vec<serde_json::Value> = (0..10).map(|i| json!(format!("anchor{i}"))).collect();
    let mut a = anchors.clone();
    a.push(json!({"a": 1}));
    let mut b = anchors;
    b.push(json!({"a": 1, "b": null}));

    let result = ignore_order_diff(&json!(a), &json!(b));
    assert_eq!(
        result["dictionary_item_added"]["root[10]['b']"],
        serde_json::Value::Null
    );
}

#[test]
fn structural_pairing_of_records_with_null_bool_and_nested_list_fields() {
    // A single, unambiguous record pair exercising count_diff_leaves's
    // Null/Bool match arms and count_object_diff_leaves's
    // removed/added-key branches, plus a nested list field routed
    // through count_array_diff_leaves — all inside one real distance
    // computation (not called directly, unlike the white-box test
    // above), to prove the wiring end to end.
    let anchors: Vec<serde_json::Value> = (0..10).map(|i| json!(format!("anchor{i}"))).collect();
    let mut a = anchors.clone();
    a.push(json!({"id": 1, "meta": null, "flag": true, "note": "x", "tags": [1, 2]}));
    let mut b = anchors;
    b.push(json!({"id": 1, "meta": null, "flag": false, "extra": 5, "tags": [1, 2, 3]}));

    let result = ignore_order_diff(&json!(a), &json!(b));
    // A single candidate on each side always pairs (no competition),
    // regardless of the exact distance value computed along the way —
    // this test's point is that the computation itself runs cleanly
    // end to end, not a specific distance number.
    assert!(result.get("values_changed").is_some());
}

#[test]
fn ignore_order_is_a_no_op_on_dict_comparison_itself() {
    // Dicts are never affected by ignore_order — only
    // list-typed VALUES inside them, recursively. The shared "z" key keeps
    // key overlap above the threshold_to_diff_deeper cutoff so this
    // exercises the normal per-key add/remove path rather than a
    // wholesale collapse.
    assert_eq!(
        ignore_order_diff(&json!({"a": 1, "z": 9}), &json!({"b": 1, "z": 9})),
        json!({
            "dictionary_item_added": {"root['b']": 1},
            "dictionary_item_removed": {"root['a']": 1},
        })
    );
}

use super::distance::{
    Distance, THRESHOLD_TO_DIFF_DEEPER, is_length_excluded_key, numeric_distance,
};
use super::fxhash::{FX_SEED, FxHasher};
use super::pairing::CUTOFF_DISTANCE_FOR_PAIRS;

// --- Distance -----------------------------------------------------
//
// `BTreeMap<Distance, _>` only ever calls `Ord::cmp` internally, never
// `PartialOrd::partial_cmp` — exercised directly here so the manual
// impls (required because `f64` has no total `Ord`) are actually
// tested, not just organically covered by map operations elsewhere.

#[test]
fn distance_partial_cmp_and_eq_and_hash_are_consistent() {
    use std::cmp::Ordering;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let a = Distance(0.1);
    let b = Distance(0.1);
    let c = Distance(0.2);

    assert_eq!(a, b);
    assert_eq!(a.partial_cmp(&b), Some(Ordering::Equal));
    assert_eq!(a.partial_cmp(&c), Some(Ordering::Less));
    assert_eq!(c.partial_cmp(&a), Some(Ordering::Greater));

    let hash_of = |d: Distance| {
        let mut hasher = DefaultHasher::new();
        d.hash(&mut hasher);
        hasher.finish()
    };
    assert_eq!(hash_of(a), hash_of(b));

    // Both kill a `PartialEq::eq -> true` mutant (a real hash function
    // will not collide two very different f64 bit patterns) and a
    // `Hash::hash -> ()` mutant (a no-op hash collapses every value to
    // the same output).
    assert_ne!(a, c);
    assert_ne!(hash_of(a), hash_of(c));
}

/// `FxHasher` (this module's own hasher, replacing the standard
/// library's default for perf — see its doc) must actually depend on
/// its input: two different byte sequences of the exact shapes this
/// module hashes through (`ItemKey`'s `write_u8` discriminant tag, an
/// `i128` `write_u128`, and a UTF-8 string via `write`) must not all
/// collapse to the same hash.
#[test]
fn fx_hasher_output_depends_on_its_input() {
    use std::hash::Hasher;

    let hash_u8 = |v: u8| {
        let mut h = FxHasher::default();
        h.write_u8(v);
        h.finish()
    };
    assert_ne!(hash_u8(1), hash_u8(2));

    let hash_u16 = |v: u16| {
        let mut h = FxHasher::default();
        h.write_u16(v);
        h.finish()
    };
    assert_ne!(hash_u16(1), hash_u16(2));

    let hash_u32 = |v: u32| {
        let mut h = FxHasher::default();
        h.write_u32(v);
        h.finish()
    };
    assert_ne!(hash_u32(1), hash_u32(2));

    let hash_u64 = |v: u64| {
        let mut h = FxHasher::default();
        h.write_u64(v);
        h.finish()
    };
    assert_ne!(hash_u64(1), hash_u64(2));

    let hash_usize = |v: usize| {
        let mut h = FxHasher::default();
        h.write_usize(v);
        h.finish()
    };
    assert_ne!(hash_usize(1), hash_usize(2));

    let hash_u128 = |v: u128| {
        let mut h = FxHasher::default();
        h.write_u128(v);
        h.finish()
    };
    assert_ne!(hash_u128(1), hash_u128(2));
    // The high 64 bits must actually be mixed in — not just discarded
    // (a `>>` -> `<<` mutant makes `(i << 64) as u64` always `0`
    // regardless of `i`'s real high bits, so both of these degenerate
    // to hashing the same (low=0, high-as-mutated=0) pair as
    // `hash_u128(0)` under that mutant, even though they're genuinely
    // different real inputs).
    assert_ne!(hash_u128(1 << 64), hash_u128(0));

    let hash_bytes = |s: &str| {
        let mut h = FxHasher::default();
        h.write(s.as_bytes());
        h.finish()
    };
    // A 9-byte string exercises write()'s 8-byte-chunk loop plus its
    // one-byte tail in a single call.
    assert_ne!(hash_bytes("abcdefghi"), hash_bytes("abcdefghz"));
    assert_ne!(hash_bytes("short"), hash_bytes(""));
}

#[test]
fn fx_hasher_mixing_step_is_xor_not_or() {
    // `1_u64.rotate_left(5) == 32`, and the word chosen below is also
    // `32` — identical operands make XOR/OR/AND maximally distinct
    // (`32^32=0`, `32|32=32`, `32&32=32`), so this genuinely
    // distinguishes all three, not just XOR-vs-one-of-them. Computed
    // directly against the documented formula
    // `(hash.rotate_left(5) ^ word).wrapping_mul(FX_SEED)`.
    assert_eq!(1_u64.rotate_left(5), 32);
    let mut h = FxHasher { hash: 1 };
    h.add_to_hash(32);
    let expected = (1_u64.rotate_left(5) ^ 32).wrapping_mul(FX_SEED);
    assert_eq!(h.hash, expected);
    assert_eq!(expected, 0, "1.rotate_left(5) ^ 32 == 32 ^ 32 == 0");
}

// --- count_array_diff_leaves ---------------------------------------

#[test]
fn count_array_diff_leaves_sums_every_report_category_via_the_ordered_path() {
    // A dict element disqualifies LCS matching, forcing the plain
    // positional path — direct control over which finding categories
    // the trial sub-diff produces, to exercise
    // Report::distance_leaf_length's full range in one shot:
    // type_changes (index 0), a nested values_changed +
    // dictionary_item_added/removed (index 1), a top-level
    // values_changed (index 2), and an iterable_item_removed (a's
    // surplus tail).
    let opts = DiffOptions {
        ignore_order: false,
        ..DiffOptions::default()
    };
    let a = vec![
        json!(1),
        json!({"x": 1, "y": 2}),
        json!("removed_str"),
        json!("tail_removed"),
    ];
    let b = vec![json!(1.5), json!({"x": 2, "z": 9}), json!("added_str")];

    // type_changes(index 0) = 1 + item_length(1.5) = 2;
    // index 1's dict diff = values_changed(1) + removed "y"(1) + added "z"(1) = 3;
    // index 2 values_changed = item_length("added_str") = 1;
    // a's surplus tail (index 3) iterable_item_removed = item_length("tail_removed") = 1.
    // Total = 2 + 3 + 1 + 1 = 7 (pins an exact value, not just >0, so a
    // `replace body with 1` mutant is caught).
    assert_eq!(count_array_diff_leaves(&a, &b, 0, &opts), 7);
}

#[test]
fn count_array_diff_leaves_of_equal_arrays_is_zero() {
    let opts = DiffOptions::default();
    assert_eq!(
        count_array_diff_leaves(&[json!(1)], &[json!(1)], 0, &opts),
        0
    );
}

// --- count_diff_leaves / count_object_diff_leaves -------------------

#[test]
fn count_diff_leaves_number_type_mismatch_uses_type_change_leaf_length() {
    let opts = DiffOptions::default();
    // int -> float mismatch: type_change_leaf_length(1, 1.5) = 1 + item_length(1.5) = 2
    // (float(1) == 1.0 != 1.5, so new_value is NOT omitted).
    assert_eq!(count_diff_leaves(&json!(1), &json!(1.5), 0, &opts), 2);
}

#[test]
fn type_change_leaf_length_omits_new_value_when_the_coercion_reproduces_it() {
    // float(0) == 0.0 -> omitted (flat 1, just the new_type).
    assert_eq!(type_change_leaf_length(&json!(0), &json!(0.0)), 1);
    // float(2) == 2.0 != 1.0 -> NOT omitted.
    assert_eq!(type_change_leaf_length(&json!(2), &json!(1.0)), 2);
    // int(1.9) == 1 (truncates toward zero) -> omitted.
    assert_eq!(type_change_leaf_length(&json!(1.9), &json!(1)), 1);
    // int(1.5) == 1 != 2 -> NOT omitted.
    assert_eq!(type_change_leaf_length(&json!(1.5), &json!(2)), 2);
    // bool(5) == True -> omitted (this is the rule the OLD special-cased
    // implementation coincidentally got right, but only for THIS
    // direction).
    assert_eq!(type_change_leaf_length(&json!(5), &json!(true)), 1);
    // bool(0) == False -> omitted (the OLD special case got this
    // WRONG: it only ever matched `new_value == true`).
    assert_eq!(type_change_leaf_length(&json!(0), &json!(false)), 1);
    // int(True) == 1 -> omitted.
    assert_eq!(type_change_leaf_length(&json!(true), &json!(1)), 1);
    // int(True) == 1 != 2 -> NOT omitted.
    assert_eq!(type_change_leaf_length(&json!(true), &json!(2)), 2);
    // str(True) == "True" -> omitted.
    assert_eq!(type_change_leaf_length(&json!(true), &json!("True")), 1);
    // str(True) == "True" != "true" -> NOT omitted.
    assert_eq!(type_change_leaf_length(&json!(true), &json!("true")), 2);
    // int("5") == 5 -> omitted.
    assert_eq!(type_change_leaf_length(&json!("5"), &json!(5)), 1);
    // int("abc") raises -> always included (a container/coercion-failure
    // case is not omitted, matching Python's `except Exception: pass`).
    assert_eq!(type_change_leaf_length(&json!("abc"), &json!(5)), 2);
    // bool(None) == False -> omitted.
    assert_eq!(
        type_change_leaf_length(&serde_json::Value::Null, &json!(false)),
        1
    );
    // str(None) == "None" -> omitted.
    assert_eq!(
        type_change_leaf_length(&serde_json::Value::Null, &json!("None")),
        1
    );
    // dict(5) has no known coercion (deliberately unimplemented) ->
    // always included.
    assert_eq!(type_change_leaf_length(&json!(5), &json!({"a": 1})), 1 + 1);
    // bool([]) == False / bool([1]) == True -> both omitted (container
    // truthiness, matching Python's own `bool()` semantics).
    assert_eq!(type_change_leaf_length(&json!([]), &json!(false)), 1);
    assert_eq!(type_change_leaf_length(&json!([1]), &json!(true)), 1);
    assert_eq!(type_change_leaf_length(&json!({}), &json!(false)), 1);
    assert_eq!(type_change_leaf_length(&json!({"a": 1}), &json!(true)), 1);
    // A huge u64-only integer (beyond i64::MAX) coerced to bool/str:
    // still exercises the u64-specific branches directly.
    assert_eq!(
        type_change_leaf_length(&json!(18_446_744_073_709_551_615u64), &json!(true)),
        1
    );
    assert_eq!(
        type_change_leaf_length(
            &json!(18_446_744_073_709_551_615u64),
            &json!("18446744073709551615")
        ),
        1
    );
    // A float grossly out of i64 range never coerces to int -> always
    // included.
    assert_eq!(type_change_leaf_length(&json!(1e300), &json!(5)), 2);
    // Same, but pinned against `i64::MAX`/`i64::MIN` specifically: a
    // range-check that degraded to `||` (rather than `&&`) would let
    // Rust's saturating `as i64` cast smuggle a grossly out-of-range
    // float through as exactly `i64::MAX`/`i64::MIN`, coincidentally
    // matching these `new_value`s and wrongly omitting them.
    assert_eq!(type_change_leaf_length(&json!(1e300), &json!(i64::MAX)), 2);
    assert_eq!(type_change_leaf_length(&json!(-1e300), &json!(i64::MIN)), 2);
    // int(5) == 5.0 -> omitted: pins `coerce_to_f64`'s `Number` branch
    // against a non-zero value (the `0`/`0.0` cases above can't
    // distinguish real coercion from a stub that always returns 0.0).
    assert_eq!(type_change_leaf_length(&json!(5), &json!(5.0)), 1);
    // bool("") == False / bool("x") == True -> both omitted (string
    // truthiness, matching Python's own `bool()` semantics) — pins
    // `is_truthy`'s `String` arm in both directions (a `!` flip there
    // would give the wrong answer for exactly one of these).
    assert_eq!(type_change_leaf_length(&json!(""), &json!(false)), 1);
    assert_eq!(type_change_leaf_length(&json!("x"), &json!(true)), 1);
    // bool("x") == True != False -> NOT omitted.
    assert_eq!(type_change_leaf_length(&json!("x"), &json!(false)), 2);
    // str(5.5) == "5.5" -> omitted: the rendered float already contains
    // a `.`, so no trailing `.0` must be appended (pins the `!` in
    // `coerce_to_python_str`'s no-append guard — a flipped guard would
    // wrongly render "5.5.0").
    assert_eq!(type_change_leaf_length(&json!(5.5), &json!("5.5")), 1);
    // str(5.5) == "5.5" != "5.5.0" -> NOT omitted (also guards against
    // the inverse mistake of never appending, since the literal
    // "5.5.0" already differs from the rendered "5.5").
    assert_eq!(type_change_leaf_length(&json!(5.5), &json!("5.5.0")), 2);
}

#[test]
fn ignore_order_pairing_rejects_a_false_negative_from_the_old_special_case() {
    // A minimal repro: a structural pair whose distance
    // depends on the general coercion rule (float(0) == 0.0), not just
    // the old `new_value == true` special case. Real deepdiff: distance
    // 0.25 < 0.3 (pairs, recursing to a nested type_changes); the old
    // inline reimplementation in Report::distance_leaf_length computed
    // 0.333 (>= 0.3, rejected), producing raw add/remove instead.
    let a = json!([[["", ""], []], {}]);
    let b = json!([[1, [], true, {"c": true}], {}]);
    assert_eq!(
        ignore_order_diff(&a, &b),
        json!({
            "iterable_item_added": {"root[0][0]": 1, "root[0][3]": {"c": true}},
            "type_changes": {"root[0][0]": {
                "new_path": "root[0][2]",
                "new_type": "bool",
                "new_value": true,
                "old_type": "list",
                "old_value": ["", ""],
            }},
        })
    );
}

#[test]
fn ignore_order_pairing_generalizes_past_the_true_literal_special_case() {
    // The sibling repro: [[0]] vs [[0.0]] recurses to a nested
    // type_changes (float(0) == 0.0, new_value omitted) in real
    // deepdiff. The old `new_value == true`-only special case couldn't
    // have handled this at all (new_value here is `0.0`, never `true`).
    let a = json!([[0]]);
    let b = json!([[0.0]]);
    assert_eq!(
        ignore_order_diff(&a, &b),
        json!({
            "type_changes": {"root[0][0]": {
                "new_type": "float", "new_value": 0.0, "old_type": "int", "old_value": 0,
            }},
        })
    );
}

#[test]
fn ignore_order_pairing_is_not_corrupted_by_a_nested_low_overlap_dict_pair() {
    // Minimized repro (2x3 distance matrix: removed = {1,
    // [{aa,bb,cc}]}, added = {0.0, 2, [{}]}): `count_array_diff_leaves`'s
    // trial sub-diff for the `[{aa,bb,cc}]` vs `[{}]` candidate pair
    // used to recurse into a nested dict-vs-dict comparison through the
    // *real* `crate::diff::object_diff` (no `threshold_to_diff_deeper`
    // awareness), inflating that candidate's measured distance past
    // `CUTOFF_DISTANCE_FOR_PAIRS` (real deepdiff: 0.1364, well under the
    // cutoff; the old inflated count: 0.3182, over it) and corrupting
    // the *pairing decision itself* — not just the reported shape. The
    // old, broken pairing wrongly matched `[{aa,bb,cc}]` (root[2])
    // straight to the scalar `0.0` (a `type_changes`: list -> float)
    // and left `[{}]` as a genuinely unpaired `iterable_item_added`,
    // producing a completely different report shape from real
    // `DeepDiff`'s.
    //
    // `count_array_diff_leaves`'s trial sub-diff now measures this nested
    // dict-vs-dict candidate through `crate::diff::object_diff`'s own
    // unconditional `threshold_to_diff_deeper` collapse, fixing the
    // pairing: this now matches real `DeepDiff`'s pairing decision exactly
    // (`1` <-> `2`, `[{aa,bb,cc}]` <-> `[{}]`, `0.0` unpaired-added) — and,
    // since the collapse is no longer trial-only, the nested `root[2][0]`
    // subtree's own reported shape now matches real `DeepDiff` exactly
    // too (a single collapsed `values_changed` with `new_path`, not
    // granular `dictionary_item_removed`s). See the sibling golden case
    // `ignore_order_nested_low_overlap_dict_pairing` for the
    // real-`DeepDiff`-generated `expected.json` this now matches
    // byte-for-byte.
    let a = json!(["y", 1, [{"aa": 1, "bb": 2, "cc": 3}]]);
    let b = json!(["y", 0.0, 2, [{}]]);
    assert_eq!(
        ignore_order_diff(&a, &b),
        json!({
            "iterable_item_added": {"root[1]": 0.0},
            "values_changed": {
                "root[1]": {
                    "new_path": "root[2]", "new_value": 2, "old_value": 1,
                },
                "root[2][0]": {
                    "new_path": "root[3][0]",
                    "new_value": {},
                    "old_value": {"aa": 1, "bb": 2, "cc": 3},
                },
            },
        })
    );
}

#[test]
fn count_diff_leaves_same_type_numbers_equal_is_zero_unequal_is_one() {
    let opts = DiffOptions::default();
    assert_eq!(count_diff_leaves(&json!(5), &json!(5), 0, &opts), 0);
    assert_eq!(count_diff_leaves(&json!(5), &json!(6), 0, &opts), 1);
    assert_eq!(count_diff_leaves(&json!(5.0), &json!(5.0), 0, &opts), 0);
    assert_eq!(count_diff_leaves(&json!(5.0), &json!(6.0), 0, &opts), 1);
}

#[test]
fn count_diff_leaves_null_null_is_zero() {
    let opts = DiffOptions::default();
    assert_eq!(
        count_diff_leaves(&serde_json::Value::Null, &serde_json::Value::Null, 0, &opts),
        0
    );
}

#[test]
fn count_diff_leaves_bool_equal_is_zero_unequal_is_one() {
    let opts = DiffOptions::default();
    assert_eq!(count_diff_leaves(&json!(true), &json!(true), 0, &opts), 0);
    assert_eq!(count_diff_leaves(&json!(true), &json!(false), 0, &opts), 1);
}

#[test]
fn count_diff_leaves_string_equal_is_zero_unequal_is_one() {
    let opts = DiffOptions::default();
    assert_eq!(count_diff_leaves(&json!("a"), &json!("a"), 0, &opts), 0);
    assert_eq!(count_diff_leaves(&json!("a"), &json!("b"), 0, &opts), 1);
}

#[test]
fn count_diff_leaves_array_dispatches_to_count_array_diff_leaves() {
    let opts = DiffOptions::default();
    // Ordered path (default opts): index-aligned, one values_changed
    // (new_value=3, item_length=1) — distinct from what the deleted
    // Array match arm's fallback (`type_change_leaf_length`, which
    // would count the WHOLE new array) would give (3).
    assert_eq!(
        count_diff_leaves(&json!([1, 2]), &json!([1, 3]), 0, &opts),
        1
    );
}

#[test]
fn count_object_diff_leaves_below_threshold_collapses_to_a_wholesale_new_value() {
    let opts = DiffOptions::default();
    let a = json!({"a": 1, "c": 2}).as_object().unwrap().clone();
    let b = json!({"b": 1, "d": 2}).as_object().unwrap().clone();
    // union={a,b,c,d}=4, intersect={}=0, ratio=0 < 0.33 -> collapses to
    // item_length_of_map(b) = item_length(1) + item_length(2) = 2 —
    // deliberately not 1, so a `replace body with 1` mutant is caught
    // too.
    assert_eq!(count_object_diff_leaves(&a, &b, 0, &opts), 2);
}

#[test]
fn count_object_diff_leaves_ratio_uses_division_not_multiplication() {
    let opts = DiffOptions::default();
    // intersect=1 ("shared"), union=4 -> ratio 1/4=0.25 < 0.33
    // (collapses, real): item_length_of_map(b) = item_length(9) +
    // item_length(3) + item_length(4) = 3. A `/` -> `*` mutant computes
    // 1*4=4 (not < 0.33, no collapse), recursing instead: differing
    // "shared" (1) + removed "x" (1) + added "y" (1) + added "z" (1) = 4.
    let a = json!({"shared": 1, "x": 2}).as_object().unwrap().clone();
    let b = json!({"shared": 9, "y": 3, "z": 4})
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(count_object_diff_leaves(&a, &b, 0, &opts), 3);
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "asserting the runtime division is bit-identical to the compile-time literal is the test's own point"
)]
fn count_object_diff_leaves_ratio_at_exactly_the_threshold_does_not_collapse() {
    // A ⊇ B: 100 keys in `a` (33 shared with `b`, 67 exclusive to `a`),
    // 33 keys in `b` (all shared). union = 100, intersect = 33, ratio =
    // 33.0/100.0 — bit-identical to the `0.33` literal (both round the
    // same exact decimal value to the nearest f64). `< 0.33` is false at
    // an exact match (no collapse); a `<` -> `<=` mutant would wrongly
    // collapse instead.
    let mut a = serde_json::Map::new();
    let mut b = serde_json::Map::new();
    for i in 0..33 {
        a.insert(format!("shared{i}"), json!(1));
        b.insert(format!("shared{i}"), json!(1));
    }
    for i in 0..67 {
        a.insert(format!("only_a{i}"), json!(2));
    }
    assert_eq!(33.0_f64 / 100.0_f64, THRESHOLD_TO_DIFF_DEEPER);

    // No collapse -> every shared key recurses (all equal, 0 each) and
    // every `a`-only key is a removed leaf (item_length(2) = 1 each,
    // 67 of them) = 67, not item_length_of_map(b) = 33 (all `1`s).
    assert_eq!(
        count_object_diff_leaves(&a, &b, 0, &DiffOptions::default()),
        67
    );
}

#[test]
fn count_object_diff_leaves_shared_key_recursion_depth_boundary_is_exact() {
    // Shared key "x" holds arrays whose one element (a dict) needs 3
    // levels of recursion (array-element -> dict "a" -> dict "b") to
    // reach the actual leaf difference. With max_depth=3 and this call
    // itself at depth=0, the recursive `count_diff_leaves(..., depth +
    // 1, ...)` call for "x" must use depth=1, so
    // `count_array_diff_leaves`'s own fresh-restart budget
    // (`max_depth.saturating_sub(depth)`) is `3 - 1 = 2` — one short of
    // the 3 needed, so the trial is rejected (0). A `+` -> `*` mutant
    // computes depth=0 instead, handing the trial the full budget of 3
    // (exactly enough to succeed), giving a nonzero total instead.
    let opts = DiffOptions {
        max_depth: 3,
        ignore_order: false,
    };
    let a = json!({"x": [{"a": {"b": 1}}]}).as_object().unwrap().clone();
    let b = json!({"x": [{"a": {"b": 9}}]}).as_object().unwrap().clone();
    assert_eq!(count_object_diff_leaves(&a, &b, 0, &opts), 0);
}

#[test]
fn count_object_diff_leaves_at_or_above_threshold_recurses_normally() {
    let opts = DiffOptions::default();
    // Full key overlap (ratio 1.0, well above 0.33): must recurse
    // key-by-key, not collapse — also kills an `&&` -> `||` mutant
    // (union_len=2 > 1 alone would wrongly satisfy `||`).
    let a = json!({"a": 1, "b": 2}).as_object().unwrap().clone();
    let b = json!({"a": 1, "b": 3}).as_object().unwrap().clone();
    // Shared "a" equal (0) + shared "b" differs (1) = 1, not
    // item_length_of_map(b) = 1 + 3 = 4.
    assert_eq!(count_object_diff_leaves(&a, &b, 0, &opts), 1);
}

#[test]
fn count_object_diff_leaves_union_len_one_never_collapses() {
    let opts = DiffOptions::default();
    // union_len == 1 (the `> 1` boundary): must never collapse
    // regardless of the (zero) intersection — kills a `> 1` -> `>= 1`
    // mutant.
    let a = json!({"a": 1}).as_object().unwrap().clone();
    let b = serde_json::Map::new();
    // Removed-only "a": item_length(1) = 1, not item_length_of_map({}) = 0.
    assert_eq!(count_object_diff_leaves(&a, &b, 0, &opts), 1);
}

#[test]
fn count_object_diff_leaves_accumulates_distinct_contributions_by_addition() {
    let opts = DiffOptions::default();
    // Three keys each contributing a DIFFERENT, non-0/1 leaf count so a
    // `+=` -> `*=` mutant (which would multiply instead of sum, and
    // start from a multiplicative identity of 1 rather than 0) changes
    // the total: shared "a" differs by a nested list (item_length([1,2])
    // = 2), removed-only "b" is a 3-element list (item_length = 3),
    // added-only "c" is a 4-element list (item_length = 4). Sum = 9;
    // any `*=` variant gives a different number (e.g. 2*3*4=24, or
    // 1*2*3*4=24 if the running total also starts at 1).
    let a = json!({"a": [9, 9], "b": [1, 2, 3]})
        .as_object()
        .unwrap()
        .clone();
    let b = json!({"a": [1, 2], "c": [1, 2, 3, 4]})
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(count_object_diff_leaves(&a, &b, 0, &opts), 2 + 3 + 4);
}

// --- item_key ---------------------------------------------------

#[test]
fn int_float_bool_never_share_a_key_even_at_equal_value() {
    assert_ne!(item_key(&json!(1)), item_key(&json!(1.0)));
    assert_ne!(item_key(&json!(1)), item_key(&json!(true)));
    assert_ne!(item_key(&json!(1.0)), item_key(&json!(true)));
    assert_ne!(item_key(&json!(0)), item_key(&json!(false)));
}

#[test]
fn signed_zero_floats_share_a_key_but_stay_distinct_from_the_integer_zero() {
    // Signed zeros share a key; an integral float stays distinct from the
    // integer of the same value. See `super::hash::item_key`'s float branch
    // for the deepdiff-9.1.0 provenance behind both.
    assert_eq!(item_key(&json!(0.0)), item_key(&json!(-0.0)));
    assert_ne!(item_key(&json!(2.0)), item_key(&json!(2)));
    assert_ne!(item_key(&json!(0.0)), item_key(&json!(0)));
}

#[test]
fn signed_zero_floats_share_a_set_member_digest_too() {
    // The same normalization, but through `set_member_digest`'s own
    // `number_key` (its scalar content path): confirmed against
    // `deepdiff==9.1.0`, `DeepDiff({0.0}, {-0.0})` is `{}` -- two
    // otherwise-unrelated sets, each holding one signed zero, are the same
    // set. A `+0.0` normalization mutated away (e.g. `f + 0.0` -> `f - 0.0`,
    // the identity on every float) would keep the two bit patterns distinct
    // here.
    let memo = IgnoreOrderMemo::new();
    let key = |value: &CValue| super::set_member_digest(value, &memo);
    assert_eq!(key(&cv(&json!(0.0))), key(&cv(&json!(-0.0))));
    assert_ne!(key(&cv(&json!(2.0))), key(&cv(&json!(2))));
    // A `f + 0.0` -> `f * 0.0` mutant would collapse every float to `0.0`'s
    // bit pattern regardless of its own value; two distinct nonzero floats
    // must keep distinct keys.
    assert_ne!(key(&cv(&json!(1.5))), key(&cv(&json!(2.5))));
}

#[test]
fn signed_zero_floats_dedup_to_one_removal_under_ignore_order() {
    // Full-diff regression for the signed-zero item_key normalization (see
    // `super::hash::item_key`'s float branch for the deepdiff-9.1.0 provenance).
    assert_eq!(
        ignore_order_diff(&json!([0.0, -0.0]), &json!([])),
        json!({"iterable_item_removed": {"root[0]": 0.0}})
    );
}

#[test]
fn equal_ints_share_a_key_regardless_of_representation() {
    assert_eq!(item_key(&json!(5)), item_key(&json!(5)));
}

#[test]
fn nested_list_hashes_order_and_count_insensitively() {
    assert_eq!(item_key(&json!([1, 2, 3])), item_key(&json!([3, 2, 1])));
    assert_eq!(item_key(&json!([1, 1, 2])), item_key(&json!([1, 2, 2])));
    assert_ne!(item_key(&json!([1, 2])), item_key(&json!([1, 2, 3])));
}

#[test]
fn nested_dict_hashes_key_order_insensitively() {
    assert_eq!(
        item_key(&json!({"a": 1, "b": 2})),
        item_key(&json!({"b": 2, "a": 1}))
    );
    assert_ne!(
        item_key(&json!({"a": 1})),
        item_key(&json!({"a": 1, "b": 2}))
    );
}

#[test]
fn different_strings_and_null_have_distinct_keys() {
    assert_eq!(item_key(&json!("a")), item_key(&json!("a")));
    assert_ne!(item_key(&json!("a")), item_key(&json!("b")));
    assert_ne!(item_key(&serde_json::Value::Null), item_key(&json!("a")));
    assert_eq!(
        item_key(&serde_json::Value::Null),
        item_key(&serde_json::Value::Null)
    );
}

#[test]
fn item_key_is_orderable_for_use_as_a_nested_set_element() {
    // Exercises the Ord derive path a List/Dict key relies on
    // (BTreeSet<ItemKey>/BTreeMap<String, ItemKey>) — a list containing
    // dicts, nested inside another list, must hash without panicking.
    let a = item_key(&json!([{"a": 1}, {"b": [1, 2]}]));
    let b = item_key(&json!([{"b": [2, 1]}, {"a": 1}]));
    assert_eq!(a, b);
}

// --- numeric_value / numeric_distance ----------------------------

#[test]
fn numeric_value_covers_bool_and_number_only() {
    assert_eq!(numeric_value(&json!(true)), Some(1.0));
    assert_eq!(numeric_value(&json!(false)), Some(0.0));
    assert_eq!(numeric_value(&json!(3)), Some(3.0));
    assert_eq!(numeric_value(&json!(3.5)), Some(3.5));
    assert_eq!(numeric_value(&json!("3")), None);
    assert_eq!(numeric_value(&serde_json::Value::Null), None);
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "exact output of our own deterministic arithmetic against literal expected constants"
)]
fn numeric_distance_of_equal_values_is_zero() {
    assert_eq!(numeric_distance(5.0, 5.0, 0.3), 0.0);
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "exact output of our own deterministic arithmetic against literal expected constants"
)]
fn numeric_distance_opposite_sign_zero_sum_is_always_rejected() {
    // divisor == 0.0 without num1 == num2: DeepDiff returns max_ itself
    // (always >= cutoff, i.e. always rejected as a pairing candidate).
    assert_eq!(numeric_distance(-5.0, 5.0, 0.3), 0.3);
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "exact output of our own deterministic arithmetic against literal expected constants"
)]
fn numeric_distance_bool_vs_number_is_always_at_the_cutoff() {
    // Confirmed against real deepdiff==9.1.0: get_numeric_types_distance(0, True) == 0.3.
    assert_eq!(numeric_distance(0.0, 1.0, 0.3), 0.3);
}

#[test]
fn numeric_distance_matches_the_probed_n_equals_100_shape() {
    // probe9_m6_shape_100.py's worked pair: 251650 -> 2870137.
    let d = numeric_distance(251_650.0, 2_870_137.0, 0.3);
    assert!(
        d < 0.3,
        "expected an accepted (self-cancellation) candidate, got {d}"
    );
}

// --- rough_distance -------------------------------------------------

#[test]
fn rough_distance_structural_formula_is_diff_length_over_summed_rough_lengths() {
    let opts = DiffOptions::default();
    // removed=[1,2] (rough_length=3), added=[1,2,3] (rough_length=4):
    // diff_length=1 (one iterable_item_added, item_length(3)=1).
    // distance = 1 / (3 + 4) = 1/7, distinct from 1/(3*4) = 1/12 (an
    // `rough_len = a + b` -> `a * b` mutant) and from other simple
    // arithmetic mistakes.
    let removed = json!([1, 2]);
    let added = json!([1, 2, 3]);
    let d = super::distance::rough_distance(
        &cv(&removed),
        &cv(&added),
        CUTOFF_DISTANCE_FOR_PAIRS,
        0,
        &opts,
        &super::IgnoreOrderMemo::new(),
    );
    assert!((d - 1.0 / 7.0).abs() < 1e-12, "expected 1/7, got {d}");
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "the exact-zero early return (diff_length == 0) is deterministic, not an arithmetic result"
)]
fn rough_distance_depth_boundary_is_exact() {
    // removed/added are single-element arrays whose element (a dict)
    // needs 3 levels of recursion (array-element -> dict "a" -> dict
    // "b") to reach the actual leaf difference. With max_depth=3 and
    // the pairing list itself at depth=0, `rough_distance`'s own
    // `count_diff_leaves(..., depth + 1, ...)` call must use depth=1,
    // so `count_array_diff_leaves`'s fresh-restart budget
    // (`max_depth.saturating_sub(depth)`) is `3 - 1 = 2` — one short of
    // the 3 needed, so `diff_length` is 0 and this returns exactly
    // `0.0`. A `+` -> `*` mutant computes depth=0 instead, handing the
    // trial the full budget of 3 (exactly enough to succeed), giving a
    // nonzero distance instead.
    let opts = DiffOptions {
        max_depth: 3,
        ignore_order: false,
    };
    let removed = json!([{"a": {"b": 1}}]);
    let added = json!([{"a": {"b": 9}}]);
    let d = super::distance::rough_distance(
        &cv(&removed),
        &cv(&added),
        CUTOFF_DISTANCE_FOR_PAIRS,
        0,
        &opts,
        &super::IgnoreOrderMemo::new(),
    );
    assert_eq!(d, 0.0);
}

/// Pins the exact scale `distance_family` measures a `datetime` pair by:
/// its instant in *seconds* (microseconds divided by `1_000_000`), the same
/// value `DeepDiff`'s own `_get_datetime_distance` reads from
/// `datetime.timestamp()`. Compares `rough_distance`'s actual output
/// against the identical formula computed independently from `instant()`
/// here, bypassing `distance_family` entirely.
///
/// Catches a `/` mutated to `%` (a non-linear rescale, changing which
/// candidates fall within the pairing cutoff). It does **not** catch a `/`
/// mutated to `*`: `numeric_distance`'s own formula, `cutoff * (n1 - n2) /
/// (n1 + n2)`, is a ratio that is invariant *in the reals* under scaling
/// both operands by the same nonzero constant, and `timestamp` here is used
/// nowhere else — but that is an argument about real-number algebra, not
/// `f64`: exact-integer `/` and `*` are not bit-exact inverses in floating
/// point in general, so this is an empirical finding (no reachable input
/// has been observed to distinguish `/ 1_000_000.0` from `* 1_000_000.0`
/// here, i.e. the two agree up to `f64` rounding for every case this suite
/// exercises), not an algebraic proof of equivalence.
#[test]
#[allow(
    clippy::cast_precision_loss,
    reason = "mirrors distance_family's own allow: Python's int-to-float timestamp() \
              conversion is likewise inexact past 2^53"
)]
fn rough_distance_datetime_pair_measures_seconds_not_microseconds() {
    let a = cdt_at(2024, 1, 1, 0, 0, 0, 0, None);
    let b = cdt_at(2024, 1, 1, 0, 0, 10, 0, None);
    let (CValue::DateTime(a_dt), CValue::DateTime(b_dt)) = (&a, &b) else {
        panic!("cdt_at builds a DateTime")
    };
    let a_seconds = a_dt.instant() as f64 / 1_000_000.0;
    let b_seconds = b_dt.instant() as f64 / 1_000_000.0;
    let expected = numeric_distance(a_seconds, b_seconds, CUTOFF_DISTANCE_FOR_PAIRS);

    let d = super::distance::rough_distance(
        &a,
        &b,
        CUTOFF_DISTANCE_FOR_PAIRS,
        0,
        &DiffOptions::default(),
        &super::IgnoreOrderMemo::new(),
    );
    assert!(
        (d - expected).abs() < 1e-12,
        "expected {expected} (seconds-scale), got {d}"
    );
}

// --- rough_length / item_length -----------------------------------

#[test]
fn rough_length_matches_deephash_counts_for_scalars_and_containers() {
    assert_eq!(rough_length(&json!(1)), 1);
    assert_eq!(rough_length(&json!("x")), 1);
    assert_eq!(rough_length(&serde_json::Value::Null), 1);
    assert_eq!(rough_length(&json!([1, 2])), 1 + 1 + 1);
    assert_eq!(rough_length(&json!({"a": 1})), 1 + (1 + 1));
}

#[test]
fn item_length_of_null_is_zero() {
    // Confirmed against real deepdiff: _get_item_length(None) == 0.
    assert_eq!(item_length(&serde_json::Value::Null), 0);
}

#[test]
fn item_length_excludes_special_dict_keys() {
    // Confirmed against real deepdiff:
    // _get_item_length({"old_value": 5, "x": 3}) == 1.
    assert_eq!(item_length(&json!({"old_value": 5, "x": 3})), 1);
}

#[test]
fn is_length_excluded_key_matches_the_literal_set() {
    assert!(is_length_excluded_key("old_value"));
    assert!(is_length_excluded_key("old_type"));
    assert!(is_length_excluded_key("new_path"));
    assert!(is_length_excluded_key("deep_distance"));
    assert!(is_length_excluded_key("_internal"));
    assert!(!is_length_excluded_key("new_value"));
    assert!(!is_length_excluded_key("new_type"));
    assert!(!is_length_excluded_key("x"));
}

#[test]
fn item_length_of_scalars_and_containers() {
    assert_eq!(item_length(&json!(1)), 1);
    assert_eq!(item_length(&json!("x")), 1);
    assert_eq!(item_length(&json!(true)), 1);
    assert_eq!(item_length(&json!([1, 2, "x"])), 3);
    assert_eq!(item_length(&json!({"a": 1, "b": [1, 2]})), 3);
}

#[test]
fn item_key_ord_is_consistent_with_eq() {
    let a = item_key(&json!(1));
    let b = item_key(&json!(1));
    assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);
}

// --- distance-memoization decision equivalence --------------------------

use proptest::prelude::*;

/// Generates a nested JSON value biased toward the shapes `ignore_order`
/// pairing exercises: lists (some nested), dicts, and scalars, up to a
/// modest depth.
fn arb_nested() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i32>().prop_map(|i| json!(i)),
        "[a-z]{0,3}".prop_map(serde_json::Value::String),
    ];
    leaf.prop_recursive(6, 48, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::Array),
            prop::collection::hash_map("[a-c]", inner, 0..3)
                .prop_map(|map| serde_json::Value::Object(map.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]

    /// The distance memo must change no decision: an `ignore_order` diff run
    /// with the memo enabled produces a byte-identical report to one run with
    /// it disabled, over generated nested shapes. This is the empirical
    /// counterpart to the purity argument in `super::memo`'s module doc.
    #[test]
    fn memoized_and_unmemoized_reports_are_byte_identical(
        a in arb_nested(),
        b in arb_nested(),
    ) {
        let a = crate::value::Value::from(a);
        let b = crate::value::Value::from(b);
        let opts = DiffOptions {
            ignore_order: true,
            max_depth: 1_000,
        };
        let with = crate::diff::diff_with_options_memo(&a, &b, &opts, &super::IgnoreOrderMemo::new());
        let without =
            crate::diff::diff_with_options_memo(&a, &b, &opts, &super::IgnoreOrderMemo::disabled());
        prop_assert_eq!(
            with.map(|report| report.to_json_value().to_string()),
            without.map(|report| report.to_json_value().to_string()),
        );
    }
}

#[test]
fn deep_nested_ignore_order_completes_quickly_with_memoization() {
    use std::time::Instant;

    // A single-element nested list of depth `d` is `d` nodes — tiny, legal
    // input. Unmemoized, `ignore_order` pairing re-diffs each level twice
    // (once to score the pair's distance, once to record it), compounding to
    // `~2x` cost per level: this used to take ~1-2s at depth 20 and hang for
    // tens of seconds by depth 25. The distance memo collapses it to linear.
    let opts = DiffOptions {
        ignore_order: true,
        max_depth: 100_000,
    };
    let build = |depth: usize, leaf: i64| {
        let mut value = json!(leaf);
        for _ in 0..depth {
            value = json!([value]);
        }
        crate::value::Value::from(value)
    };

    let (a, b) = (build(20, 1), build(20, 2));
    let started = Instant::now();
    let report = crate::diff::diff_with_options(&a, &b, &opts).expect("depth 20 diffs");
    let depth_20 = started.elapsed();
    assert!(
        !report.is_empty(),
        "depth-20 unequal input must report a change"
    );
    assert!(
        depth_20.as_millis() < 500,
        "depth-20 nested ignore_order took {depth_20:?}, over the 500ms bound"
    );

    // Depth 25 previously hung (>30s); it must now complete at all.
    let (a, b) = (build(25, 1), build(25, 2));
    let started = Instant::now();
    let _ = crate::diff::diff_with_options(&a, &b, &opts).expect("depth 25 diffs");
    let depth_25 = started.elapsed();
    assert!(
        depth_25.as_secs() < 5,
        "depth-25 nested ignore_order took {depth_25:?}, over the 5s bound"
    );
}

// --- tuples under ignore_order -------------------------------------------
//
// Every expected value below was confirmed against a real
// `deepdiff==9.1.0` probe. Tuples cannot be written as JSON literals, so
// these build compact values directly and route through one local helper.

/// `ignore_order_diff` for values that are already compact (a tuple has no
/// `serde_json` literal form).
fn ignore_order_diff_compact(a: &CValue, b: &CValue) -> serde_json::Value {
    crate::diff::diff_with_options(
        a,
        b,
        &DiffOptions {
            ignore_order: true,
            ..DiffOptions::default()
        },
    )
    .unwrap()
    .to_json_value()
}

/// A compact array of compact values, for a list holding a tuple.
fn carr(items: Vec<CValue>) -> CValue {
    CValue::Array(items.into_boxed_slice())
}

#[test]
fn a_tuple_and_a_list_with_the_same_items_never_hash_match() {
    // DeepHash carries the type, so `[(1, 2)]` vs `[[1, 2]]` does not match
    // as equal: the two items pair by distance and the type change is
    // reported.
    assert_ne!(
        super::hash::item_key(&ctup(&[json!(1), json!(2)]), &IgnoreOrderMemo::new()),
        super::hash::item_key(&cv(&json!([1, 2])), &IgnoreOrderMemo::new()),
    );
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![ctup(&[json!(1), json!(2)])]),
            &cv(&json!([[1, 2]])),
        ),
        json!({"type_changes": {"root[0]": {
            "old_type": "tuple", "new_type": "list",
            "old_value": [1, 2], "new_value": [1, 2],
        }}})
    );
}

#[test]
fn a_tuple_nested_in_a_list_hashes_order_insensitively() {
    // `DeepHash`'s `ignore_iterable_order` default applies to tuples too:
    // `[(1, 2)]` vs `[(2, 1)]` reports nothing at all.
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![ctup(&[json!(1), json!(2)])]),
            &carr(vec![ctup(&[json!(2), json!(1)])]),
        ),
        json!({})
    );
}

#[test]
fn a_tuple_is_itself_paired_by_hash_under_ignore_order() {
    // The container being diffed is the tuple: `(1, 2, 3)` vs `(3, 2, 5)`
    // pairs 1 <-> 5 across drifted indices, carrying `new_path`.
    assert_eq!(
        ignore_order_diff_compact(
            &ctup(&[json!(1), json!(2), json!(3)]),
            &ctup(&[json!(3), json!(2), json!(5)]),
        ),
        json!({"values_changed": {"root[0]": {
            "new_value": 5, "old_value": 1, "new_path": "root[2]",
        }}})
    );
}

#[test]
fn a_changed_tuple_inside_a_list_pairs_and_carries_new_path() {
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![cv(&json!("anchor")), ctup(&[json!(1), json!(2)])]),
            &carr(vec![ctup(&[json!(1), json!(3)]), cv(&json!("anchor"))]),
        ),
        json!({"values_changed": {"root[1][1]": {
            "new_value": 3, "old_value": 2, "new_path": "root[0][1]",
        }}})
    );
}

#[test]
fn a_tuple_and_a_list_whose_items_differ_fall_back_to_raw_add_remove() {
    // The pairing distance hinges on `list(t1) == t2`: with differing items
    // the delta view keeps `new_value`, the distance crosses the cutoff, and
    // real DeepDiff reports a whole-value change rather than a type change.
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![ctup(&[json!(1), json!(2)])]),
            &cv(&json!([[1, 3]])),
        ),
        json!({"values_changed": {"root[0]": {
            "new_value": [1, 3], "old_value": [1, 2],
        }}})
    );
}

// --- DeepHash's shared cache: Python-equal hashable tuples collide ---------
//
// `DeepHash` keys its cache by the object itself and shares one cache across
// both hashtables of a run, so a hashable tuple inherits the digest of an
// earlier Python-equal one (see `super::memo`'s "Tuple digests" section for
// the source citations). Every expected value below was confirmed against a
// real `deepdiff==9.1.0` probe.

#[test]
fn a_hashable_tuple_inherits_the_digest_of_an_earlier_python_equal_one() {
    for (a, b) in [
        (ctup(&[json!(1)]), ctup(&[json!(1.0)])),
        (ctup(&[json!(1.0)]), ctup(&[json!(1)])),
        (ctup(&[json!(true)]), ctup(&[json!(1)])),
        (ctup(&[json!(0)]), ctup(&[json!(false)])),
        (
            ctup(&[json!("a"), json!(1)]),
            ctup(&[json!("a"), json!(1.0)]),
        ),
    ] {
        assert_eq!(
            ignore_order_diff_compact(&carr(vec![a]), &carr(vec![b])),
            json!({}),
        );
    }
}

#[test]
fn the_digest_collision_reaches_tuples_nested_inside_other_containers() {
    // The cache is consulted at every tuple node, so a tuple wrapped in
    // another tuple, in a dict, or in a list collides just the same.
    let wrapped = |item: CValue| {
        (
            carr(vec![CValue::Tuple(vec![item.clone()].into_boxed_slice())]),
            carr(vec![cobj_of("k", item.clone())]),
            carr(vec![carr(vec![item])]),
        )
    };
    let (a_tuple, a_dict, a_list) = wrapped(ctup(&[json!(1)]));
    let (b_tuple, b_dict, b_list) = wrapped(ctup(&[json!(1.0)]));

    assert_eq!(ignore_order_diff_compact(&a_tuple, &b_tuple), json!({}));
    assert_eq!(ignore_order_diff_compact(&a_dict, &b_dict), json!({}));
    assert_eq!(ignore_order_diff_compact(&a_list, &b_list), json!({}));
}

#[test]
fn colliding_tuples_in_one_list_collapse_to_a_single_distinct_item() {
    // `[(1,), (1.0,)]` holds two items with one digest between them, so
    // removing the whole list reports one removal, at the first index.
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![ctup(&[json!(1)]), ctup(&[json!(1.0)])]),
            &carr(vec![]),
        ),
        json!({"iterable_item_removed": {"root[0]": [1]}})
    );
}

#[test]
fn a_tuple_digest_cache_hit_reads_its_own_index_not_the_first_ones() {
    // Three hashable tuples share one run's cache: `(9,)` gets index 0
    // (fresh), `(1,)` gets index 1 (fresh), and `(1.0,)` -- Python-equal to
    // `(1,)` -- is a cache HIT reading `node_digests[id.index()]`. A
    // `NodeId::index` mutant that always returns `0` would make every
    // cache-hit read index 0's digest (`(9,)`'s) instead of its own tuple's
    // -- invisible for a repeat of the FIRST tuple ever hashed (index 0
    // already equals 0), so this needs a repeat of the SECOND one, on
    // both sides of the diff (the shared memo spans the whole run).
    let a = carr(vec![
        ctup(&[json!(9)]),
        ctup(&[json!(1)]),
        ctup(&[json!(1.0)]),
    ]);
    let b = carr(vec![ctup(&[json!(9)]), ctup(&[json!(1)])]);
    assert_eq!(ignore_order_diff_compact(&a, &b), json!({}));
}

#[test]
fn an_unhashable_tuple_never_collides() {
    // A tuple holding a list or a dict cannot be a Python dict key, so it
    // misses the cache entirely and keeps its own type-strict digest.
    let list_inside = |first: serde_json::Value| {
        carr(vec![CValue::Tuple(
            vec![cv(&first), cv(&json!([1]))].into_boxed_slice(),
        )])
    };
    assert_eq!(
        ignore_order_diff_compact(&list_inside(json!(1)), &list_inside(json!(1.0))),
        json!({"type_changes": {"root[0][0]": {
            "old_type": "int", "new_type": "float",
            "old_value": 1, "new_value": 1.0,
        }}})
    );

    let dict_inside = |first: serde_json::Value| {
        carr(vec![CValue::Tuple(
            vec![cv(&first), cv(&json!({"k": 1}))].into_boxed_slice(),
        )])
    };
    assert_eq!(
        ignore_order_diff_compact(&dict_inside(json!(1)), &dict_inside(json!(1.0))),
        json!({"type_changes": {"root[0][0]": {
            "old_type": "int", "new_type": "float",
            "old_value": 1, "new_value": 1.0,
        }}})
    );
}

#[test]
fn the_collision_is_positional_not_the_order_insensitive_content_digest() {
    // Python tuple equality is positional, so a reordered pair of the other
    // numeric type does NOT inherit: `(1, 2)` and `(2.0, 1.0)` are neither
    // Python-equal nor equal in content digest (int keys vs float keys).
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![ctup(&[json!(1), json!(2)])]),
            &carr(vec![ctup(&[json!(2.0), json!(1.0)])]),
        ),
        json!({"values_changed": {"root[0]": {
            "new_value": [2.0, 1.0], "old_value": [1, 2],
        }}})
    );
}

#[test]
fn which_member_of_an_equality_class_is_hashed_first_is_observable() {
    // The content digest deduplicates, so `(1, 1)` and `(1,)` share one. The
    // float tuple is not Python-equal to `(1, 1)`, so when it is hashed first
    // it fixes the class digest as the float one and the two no longer match
    // — real DeepDiff behaves exactly this way round.
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![ctup(&[json!(1)])]),
            &carr(vec![ctup(&[json!(1), json!(1)])]),
        ),
        json!({})
    );
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![ctup(&[json!(1.0)])]),
            &carr(vec![ctup(&[json!(1), json!(1)])]),
        ),
        json!({"type_changes": {"root[0][0]": {
            "old_type": "float", "new_type": "int",
            "old_value": 1.0, "new_value": 1,
        }}})
    );
}

#[test]
fn a_collided_element_drops_out_of_its_parents_own_comparison() {
    // The tuples at index 0 of each paired item match on their inherited
    // digest, so only the sibling difference is reported.
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![CValue::Tuple(
                vec![ctup(&[json!(1)]), cv(&json!("a"))].into_boxed_slice()
            )]),
            &carr(vec![CValue::Tuple(
                vec![ctup(&[json!(1.0)]), cv(&json!("b"))].into_boxed_slice()
            )]),
        ),
        json!({"values_changed": {"root[0][1]": {
            "new_value": "b", "old_value": "a",
        }}})
    );
}

// --- the list(t1) == t2 coercion test uses Python equality ----------------

#[test]
fn a_tuple_and_a_list_whose_items_are_python_equal_still_pair() {
    // `list((1,)) == [1.0]` in Python, so DeepDiff's delta view omits
    // new_value and the pair stays inside the pairing cutoff.
    for (a, b) in [
        (ctup(&[json!(1)]), cv(&json!([1.0]))),
        (ctup(&[json!(1), json!(2)]), cv(&json!([1.0, 2.0]))),
        (ctup(&[json!(1)]), cv(&json!([true]))),
        (ctup(&[json!(0)]), cv(&json!([false]))),
    ] {
        let expected_old = a.to_serde_json();
        let expected_new = b.to_serde_json();
        assert_eq!(
            ignore_order_diff_compact(&carr(vec![a]), &carr(vec![b])),
            json!({"type_changes": {"root[0]": {
                "old_type": "tuple", "new_type": "list",
                "old_value": expected_old, "new_value": expected_new,
            }}})
        );
    }
}

#[test]
fn python_equality_keeps_container_kinds_distinct_inside_the_coercion_test() {
    // `list((1, (2,))) == [1, [2]]` is False in Python — a tuple never
    // equals a list, at any depth — so this pair keeps new_value, misses the
    // cutoff, and reports as a whole-value change instead of a type change.
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![CValue::Tuple(
                vec![cv(&json!(1)), ctup(&[json!(2)])].into_boxed_slice()
            )]),
            &cv(&json!([[1, [2]]])),
        ),
        json!({"values_changed": {"root[0]": {
            "new_value": [1, [2]], "old_value": [1, [2]],
        }}})
    );

    // With a real list in the same position it does equal, and pairs.
    assert_eq!(
        ignore_order_diff_compact(
            &carr(vec![CValue::Tuple(
                vec![cv(&json!(1)), cv(&json!([2]))].into_boxed_slice()
            )]),
            &cv(&json!([[1, [2]]])),
        ),
        json!({"type_changes": {"root[0]": {
            "old_type": "tuple", "new_type": "list",
            "old_value": [1, [2]], "new_value": [1, [2]],
        }}})
    );
}

#[test]
fn tuple_and_list_leaf_lengths_follow_python_equality() {
    // The rule directly, at its boundary: Python-equal items omit new_value
    // (leaf length 1) in either direction; a differing item keeps it.
    assert_eq!(
        super::distance::type_change_leaf_length(&ctup(&[json!(1)]), &cv(&json!([1.0]))),
        1
    );
    assert_eq!(
        super::distance::type_change_leaf_length(&cv(&json!([1.0])), &ctup(&[json!(1)])),
        1
    );
    assert_eq!(
        super::distance::type_change_leaf_length(&ctup(&[json!(1)]), &cv(&json!([2]))),
        2
    );
    assert_eq!(
        super::distance::type_change_leaf_length(&ctup(&[json!(1), json!(2)]), &cv(&json!([1, 3]))),
        3
    );

    // A dict element compares by keys and by Python-equal values, so an equal
    // dict omits new_value while a changed value or a changed key keeps it.
    assert_eq!(
        super::distance::type_change_leaf_length(
            &ctup(&[json!({"k": 1})]),
            &cv(&json!([{"k": 1.0}]))
        ),
        1
    );
    assert_eq!(
        super::distance::type_change_leaf_length(
            &ctup(&[json!({"k": 1})]),
            &cv(&json!([{"k": 2}]))
        ),
        2
    );
    assert_eq!(
        super::distance::type_change_leaf_length(
            &ctup(&[json!({"k": 1})]),
            &cv(&json!([{"j": 1}]))
        ),
        2
    );
}

/// A compact one-key object, for a test that needs a dict around a value a
/// `serde_json` literal cannot express.
fn cobj_of(key: &str, value: CValue) -> CValue {
    CValue::Object(crate::value::Object::from_pairs(vec![(
        std::sync::Arc::from(key),
        value,
    )]))
}

// --- datetimes and dates -------------------------------------------------

#[test]
fn a_naive_and_an_aware_datetime_at_one_instant_hash_match() {
    // `DeepHash._prep_datetime` normalizes to UTC before formatting its
    // digest, so these pair with no finding at all.
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    let a = CValue::Array(vec![cdt_at(2024, 1, 1, 10, 0, 0, 0, None)].into_boxed_slice());
    let b = CValue::Array(vec![cdt_at(2024, 1, 1, 12, 0, 0, 0, Some(2 * 3600))].into_boxed_slice());

    assert!(
        crate::diff::diff_with_options(&a, &b, &opts)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_date_and_a_datetime_at_one_midnight_never_hash_match() {
    // `_prep_date` skips normalization and formats a bare `YYYY-MM-DD`,
    // which can never collide with `_prep_datetime`'s
    // `YYYY-MM-DD HH:MM:SS+00:00` — the two are paired by distance instead,
    // which surfaces as a type change rather than as nothing.
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    let a = CValue::Array(vec![cdate(2024, 1, 1)].into_boxed_slice());
    let b = CValue::Array(vec![cdt(2024, 1, 1, None)].into_boxed_slice());

    assert_eq!(
        crate::diff::diff_with_options(&a, &b, &opts)
            .unwrap()
            .to_json_value(),
        json!({"type_changes": {"root[0]": {
            "old_type": "date",
            "new_type": "datetime",
            "old_value": "2024-01-01",
            "new_value": "2024-01-01T00:00:00",
        }}})
    );
}

#[test]
fn a_paired_datetime_is_reported_normalized_while_an_unpaired_one_stays_raw() {
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    let a = CValue::Array(
        vec![
            cdt_at(2024, 1, 1, 10, 0, 0, 0, Some(-5 * 3600)),
            cv(&json!("anchor")),
        ]
        .into_boxed_slice(),
    );
    let b = CValue::Array(
        vec![
            cv(&json!("anchor")),
            cdt_at(2024, 1, 2, 10, 0, 0, 0, Some(-5 * 3600)),
            cv(&json!("extra")),
        ]
        .into_boxed_slice(),
    );

    assert_eq!(
        crate::diff::diff_with_options(&a, &b, &opts)
            .unwrap()
            .to_json_value(),
        json!({
            "values_changed": {"root[0]": {
                "new_path": "root[1]",
                "new_value": "2024-01-02T15:00:00+00:00",
                "old_value": "2024-01-01T15:00:00+00:00",
            }},
            "iterable_item_added": {"root[2]": "extra"},
        })
    );
}

#[test]
fn calendar_values_count_as_one_structural_node() {
    // Both `_get_item_length` and `DeepHash`'s own node count treat a
    // datetime and a date as a single leaf, like any other scalar.
    assert_eq!(super::distance::item_length(&cdt(2024, 1, 1, None)), 1);
    assert_eq!(super::distance::item_length(&cdate(2024, 1, 1)), 1);
    assert_eq!(super::distance::rough_length(&cdt(2024, 1, 1, None)), 1);
    assert_eq!(super::distance::rough_length(&cdate(2024, 1, 1)), 1);
}

#[test]
fn a_datetime_leaf_counts_as_changed_only_when_the_instants_differ() {
    let opts = DiffOptions::default();
    let memo = IgnoreOrderMemo::new();
    let count = |a: &CValue, b: &CValue| super::distance::count_diff_leaves(a, b, 0, &opts, &memo);

    assert_eq!(
        count(
            &cdt_at(2024, 1, 1, 10, 0, 0, 0, None),
            &cdt_at(2024, 1, 1, 12, 0, 0, 0, Some(2 * 3600))
        ),
        0
    );
    assert_eq!(count(&cdt(2024, 1, 1, None), &cdt(2024, 1, 2, None)), 1);
    assert_eq!(count(&cdate(2024, 1, 1), &cdate(2024, 1, 1)), 0);
    assert_eq!(count(&cdate(2024, 1, 1), &cdate(2024, 1, 2)), 1);
}

#[test]
fn a_date_and_a_datetime_pair_by_ordinal_distance_in_either_direction() {
    // `get_numeric_types_distance` walks `TYPES_TO_DIST_FUNC` in order and
    // matches the mixed pair on its `datetime.date` entry, because `datetime`
    // is a `date` subclass — so the pair is measured in ordinals and lands
    // well inside the pairing cutoff whichever side each type is on.
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    let date_side = CValue::Array(vec![cdate(2024, 1, 1)].into_boxed_slice());
    let datetime_side = CValue::Array(vec![cdt(2024, 3, 5, None)].into_boxed_slice());

    let forward = crate::diff::diff_with_options(&date_side, &datetime_side, &opts).unwrap();
    let backward = crate::diff::diff_with_options(&datetime_side, &date_side, &opts).unwrap();

    assert_eq!(
        forward.to_json_value(),
        json!({"type_changes": {"root[0]": {
            "old_type": "date",
            "new_type": "datetime",
            "old_value": "2024-01-01",
            "new_value": "2024-03-05T00:00:00",
        }}})
    );
    assert_eq!(
        backward.to_json_value(),
        json!({"type_changes": {"root[0]": {
            "old_type": "datetime",
            "new_type": "date",
            "old_value": "2024-03-05T00:00:00",
            "new_value": "2024-01-01",
        }}})
    );
}

#[test]
fn a_calendar_value_is_truthy_when_a_type_change_coerces_it_to_bool() {
    // `_from_tree_type_changes` omits `new_value` when `new_type(old_value)`
    // reproduces it, and `bool(datetime(...))`/`bool(date(...))` is always
    // True — confirmed against real `deepdiff==9.1.0`:
    // `DeepDiff(datetime(2024, 1, 1), True, view="_delta")` has no
    // `new_value` (length 1), while the same pair against `False` does
    // (length 2).
    let leaf = |a: &CValue, b: &CValue| super::distance::type_change_leaf_length(a, b);

    assert_eq!(leaf(&cdt(2024, 1, 1, None), &cv(&json!(true))), 1);
    assert_eq!(leaf(&cdate(2024, 1, 1), &cv(&json!(true))), 1);
    assert_eq!(leaf(&cdt(2024, 1, 1, None), &cv(&json!(false))), 2);
    assert_eq!(leaf(&cdt(2024, 1, 1, None), &cv(&json!("x"))), 2);
}

#[test]
fn a_calendar_value_and_a_number_share_no_distance_family_and_never_pair() {
    // `get_numeric_types_distance` returns `not_found` unless both values are
    // an `isinstance` of the *same* entry, so this pair takes the structural
    // fallback, measures as maximally far, and is reported as an add plus a
    // remove rather than a type change.
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    let a = CValue::Array(
        vec![cdt_at(2024, 1, 1, 10, 0, 0, 0, None), cv(&json!("anchor"))].into_boxed_slice(),
    );
    let b = CValue::Array(vec![cv(&json!("anchor")), cv(&json!(5))].into_boxed_slice());

    assert_eq!(
        crate::diff::diff_with_options(&a, &b, &opts)
            .unwrap()
            .to_json_value(),
        json!({
            "iterable_item_added": {"root[1]": 5},
            "iterable_item_removed": {"root[0]": "2024-01-01T10:00:00"},
        })
    );
}

#[test]
fn a_calendar_value_against_its_own_python_str_is_reproduced_by_coercion() {
    // `str(datetime)` uses a space separator, not a `T`, so only the
    // space-separated string is reproducible — confirmed against real
    // `deepdiff==9.1.0` with `view="_delta"`, whose `_get_item_length` is
    // `1` for the first two pairs (no `new_value` key) and `2` for the
    // third.
    let leaf = |a: &CValue, b: &CValue| super::distance::type_change_leaf_length(a, b);

    assert_eq!(
        leaf(&cdt(2024, 1, 1, None), &cv(&json!("2024-01-01 00:00:00"))),
        1
    );
    assert_eq!(leaf(&cdate(2024, 1, 1), &cv(&json!("2024-01-01"))), 1);
    assert_eq!(
        leaf(&cdt(2024, 1, 1, None), &cv(&json!("2024-01-01T00:00:00"))),
        2
    );
    assert_eq!(
        leaf(
            &cdt_at(2024, 1, 1, 10, 0, 0, 0, Some(1830)),
            &cv(&json!("2024-01-01 10:00:00+00:30:30"))
        ),
        1
    );
}

#[test]
fn a_calendar_value_pairs_with_its_own_python_str_under_ignore_order() {
    // The end-to-end consequence of the coercion above: because `str()`
    // reproduces the new value, the delta omits it, the pair stays inside
    // the pairing cutoff, and the result is a `type_changes` rather than an
    // add plus a remove merged into an unrelated `values_changed`. The
    // values sit inside a dict because that is what makes the distance small
    // enough to pair: two bare scalars have a rough length of 2 between
    // them, so even a `diff_length` of 1 lands on 0.5, over the 0.3 cutoff —
    // real `DeepDiff` reports a plain `values_changed` for the bare pair
    // too, which the sibling case below pins.
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    let wrapped = |value: CValue| {
        let mut builder = crate::value::Builder::new();
        CValue::Array(vec![builder.object(vec![("a".to_string(), value)])].into_boxed_slice())
    };

    assert_eq!(
        crate::diff::diff_with_options(
            &wrapped(cdt(2024, 1, 1, None)),
            &wrapped(cv(&json!("2024-01-01 00:00:00"))),
            &opts,
        )
        .unwrap()
        .to_json_value(),
        json!({"type_changes": {"root[0]['a']": {
            "old_type": "datetime",
            "new_type": "str",
            "old_value": "2024-01-01T00:00:00",
            "new_value": "2024-01-01 00:00:00",
        }}})
    );
    assert_eq!(
        crate::diff::diff_with_options(
            &wrapped(cdate(2024, 1, 1)),
            &wrapped(cv(&json!("2024-01-01"))),
            &opts,
        )
        .unwrap()
        .to_json_value(),
        json!({"type_changes": {"root[0]['a']": {
            "old_type": "date",
            "new_type": "str",
            "old_value": "2024-01-01",
            "new_value": "2024-01-01",
        }}})
    );
}

#[test]
fn two_bare_scalars_are_too_far_apart_to_pair_even_when_str_reproduces_one() {
    // The control for the case above: same values, no surrounding dict, so
    // the rough length is 2 and a `diff_length` of 1 gives 0.5 — over the
    // cutoff. Real `DeepDiff` likewise reports a `values_changed` here (an
    // add and a remove at one path, merged by
    // `mutual_add_removes_to_become_value_changes`), not a `type_changes`.
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    let a = CValue::Array(vec![cdt(2024, 1, 1, None)].into_boxed_slice());
    let b = CValue::Array(vec![cv(&json!("2024-01-01 00:00:00"))].into_boxed_slice());

    assert_eq!(
        crate::diff::diff_with_options(&a, &b, &opts)
            .unwrap()
            .to_json_value(),
        json!({"values_changed": {"root[0]": {
            "old_value": "2024-01-01T00:00:00",
            "new_value": "2024-01-01 00:00:00",
        }}})
    );
}

#[test]
fn a_pre_epoch_datetime_pairs_with_a_date_by_ordinal_not_by_timestamp() {
    // The two measures disagree here, which is what makes this case able to
    // notice a swap: the ordinal distance is ~0.003 (well inside the 0.3
    // cutoff, so the pair forms), while the timestamp distance is exactly
    // the cutoff — `_get_numbers_distance`'s divisor is `(n1 + n2) / max_`,
    // and a negative timestamp against a positive one nearly cancels it, so
    // the self-cancellation quirk that keeps same-sign numbers close does
    // not apply. Measuring this pair by timestamp would reject it.
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    let a = CValue::Array(vec![cdt(1950, 1, 1, None), cv(&json!("anchor"))].into_boxed_slice());
    let b = CValue::Array(vec![cv(&json!("anchor")), cdate(1990, 1, 1)].into_boxed_slice());

    assert_eq!(
        crate::diff::diff_with_options(&a, &b, &opts)
            .unwrap()
            .to_json_value(),
        json!({"type_changes": {"root[0]": {
            "old_type": "datetime",
            "new_type": "date",
            "new_path": "root[1]",
            "old_value": "1950-01-01T00:00:00",
            "new_value": "1990-01-01",
        }}})
    );
}

// --- sets ----------------------------------------------------------------

/// An `ignore_order` diff of two already-compact values, for the set cases
/// whose inputs no JSON literal can express.
fn compact_ignore_order_diff(
    a: &CValue,
    b: &CValue,
) -> Result<crate::report::Report, crate::error::Error> {
    crate::diff::diff_with_options(
        a,
        b,
        &DiffOptions {
            ignore_order: true,
            ..DiffOptions::default()
        },
    )
}

/// A one-item list holding `value`, the shape that puts a set through the
/// `ignore_order` hashing path rather than through a direct set diff.
fn listed(value: CValue) -> CValue {
    CValue::Array(Box::new([value]))
}

/// Each container kind hashes into its own bucket, so two of them holding
/// the same items never hash-match — the pairing recurses instead and finds
/// a type change (golden: `ignore_order_set_vs_list_never_hash_match`).
#[test]
fn a_set_never_hash_matches_another_container_kind() {
    let items = [json!(1), json!(2)];
    for (other, new_type) in [
        (cv(&json!([1, 2])), "list"),
        (ctup(&items), "tuple"),
        (cfrozen(&items), "frozenset"),
    ] {
        let report = compact_ignore_order_diff(&listed(cset(&items)), &listed(other))
            .expect("shallow values diff cleanly");
        assert_eq!(
            report.to_json_value()["type_changes"]["root[0]"]["new_type"],
            json!(new_type),
        );
    }
}

/// A set's own items hash order-insensitively, so a reordered set pairs as
/// equal (golden: `ignore_order_list_of_sets_pairs`).
#[test]
fn sets_holding_the_same_items_hash_match() {
    let report = compact_ignore_order_diff(
        &listed(cset(&[json!(1), json!(2)])),
        &listed(cset(&[json!(2), json!(1)])),
    )
    .expect("shallow values diff cleanly");

    assert!(report.is_empty());
}

/// Neither set kind consults the run's digest cache: a `set` is unhashable
/// in Python, and a `frozenset` is deliberately kept out of it, so both keep
/// their own content key. Real `DeepDiff` lets a frozenset inherit an
/// earlier Python-equal one's digest, which makes its answer depend on
/// hashing order; `onix` is deterministic instead (see
/// `tests/golden/README.md`'s "Set iteration order" section).
#[test]
fn neither_set_kind_inherits_another_items_digest() {
    let frozen = compact_ignore_order_diff(
        &listed(cfrozen(&[json!(1)])),
        &listed(cfrozen(&[json!(1.0)])),
    )
    .expect("shallow values diff cleanly");
    assert_eq!(
        frozen.to_json_value(),
        json!({"values_changed": {"root[0]": {"old_value": [1], "new_value": [1.0]}}}),
        "the two frozensets are distinct items, and too far apart to pair"
    );

    let plain = compact_ignore_order_diff(
        &CValue::Array(Box::new([cset(&[json!(1)]), cset(&[json!(1.0)])])),
        &CValue::Array(Box::new([])),
    )
    .expect("shallow values diff cleanly");
    assert_eq!(
        plain.to_json_value(),
        json!({"iterable_item_removed": {"root[0]": [1], "root[1]": [1.0]}})
    );
}

/// A frozenset hashes by membership, so a reordered one is the same item —
/// and a tuple holding the same members is not, since the two are not
/// Python-equal (golden:
/// `set_tuple_and_frozenset_items_never_share_a_digest`).
#[test]
fn a_frozenset_hashes_by_membership_and_apart_from_a_tuple() {
    let report = compact_ignore_order_diff(
        &listed(cfrozen(&[json!(1), json!(2)])),
        &listed(cfrozen(&[json!(2), json!(1)])),
    )
    .expect("shallow values diff cleanly");
    assert!(report.is_empty());

    let memo = IgnoreOrderMemo::new();
    assert_ne!(
        super::hash::item_key(&ctup(&[json!(1)]), &memo),
        super::hash::item_key(&cfrozen(&[json!(1)]), &memo),
        "a tuple and a frozenset holding the same members are not one item"
    );
}

/// `_get_item_length` of a set diff's delta view is the number of added
/// plus removed *items*, each measured by `item_length` — verified against
/// real `deepdiff==9.1.0` (`{1, 2}` vs `{1, 2, 3, 4, 5}` measures 3).
#[test]
fn count_diff_leaves_of_two_sets_counts_added_and_removed_items() {
    let memo = IgnoreOrderMemo::new();
    let opts = DiffOptions::default();
    let count = |a: &CValue, b: &CValue| super::distance::count_diff_leaves(a, b, 0, &opts, &memo);

    assert_eq!(
        count(
            &cset(&[json!(1), json!(2)]),
            &cset(&[json!(1), json!(2), json!(3), json!(4), json!(5)]),
        ),
        3
    );
    assert_eq!(count(&cset(&[json!(1)]), &cset(&[json!(1.0)])), 2);
    assert_eq!(count(&cset(&[json!(1)]), &cset(&[json!(1)])), 0);
}

/// `_prep_iterable` counts a set exactly like a list — verified with real
/// `DeepHash` (`{1, 2}` and `[1, 2]` both count 3).
#[test]
fn rough_length_of_a_set_matches_a_list_of_the_same_items() {
    assert_eq!(
        super::distance::rough_length(&cset(&[json!(1), json!(2)])),
        3
    );
    assert_eq!(super::distance::rough_length(&cfrozen(&[])), 1);
    assert_eq!(
        super::distance::item_length(&cset(&[json!(1), json!(2)])),
        2
    );
}

/// `DeepDiff`'s delta view omits a `type_changes` entry's `new_value`
/// whenever applying the new side's own type to the old value reproduces it
/// — for the set kinds, `set(x)`/`frozenset(x)` keeps only distinct members,
/// so the answer never depends on order.
#[test]
fn a_set_type_change_omits_its_new_value_when_the_constructor_reproduces_it() {
    let leaves = super::distance::type_change_leaf_length;
    let items = [json!(1), json!(2)];

    // Reproduced: the entry costs the single `new_type` leaf.
    assert_eq!(leaves(&cv(&json!([1, 2])), &cset(&items)), 1);
    assert_eq!(leaves(&ctup(&items), &cfrozen(&items)), 1);
    assert_eq!(leaves(&cset(&items), &cfrozen(&items)), 1);
    assert_eq!(leaves(&cset(&items), &cv(&json!([1, 2]))), 1);
    assert_eq!(leaves(&cfrozen(&items), &ctup(&items)), 1);
    // Python-equal members, not identical ones: `set([1, 1.0])` is `{1}`.
    assert_eq!(leaves(&cv(&json!([1, 1.0])), &cset(&[json!(1)])), 1);
    // A nested set member compares by membership too, one level down, for
    // either set kind.
    for nested in [cfrozen(&items), cset(&items)] {
        assert_eq!(
            leaves(
                &CValue::Set(SetItems::new(vec![nested.clone()])),
                &CValue::Set(SetItems::new(vec![nested])),
            ),
            1
        );
    }

    // Reproduced whichever order the sequence is in: `onix` answers this by
    // membership, where Python answers it by the set's iteration order.
    assert_eq!(leaves(&cset(&items), &cv(&json!([2, 1]))), 1);

    // Not reproduced: the entry additionally costs `new_value`'s own length.
    assert_eq!(leaves(&cset(&items), &cfrozen(&[json!(1), json!(3)])), 3);

    // A proper subset must not be "reproduced" either: `unordered_python_eq`
    // requires membership BOTH ways, not either way. `{1}` (new) has every
    // member in `{1, 2}` (old), but not the reverse, so the constructor does
    // not reproduce `old` as `new` -- an `&&` -> `||` mutant would accept
    // this one-directional match and wrongly cost it `1`.
    assert_eq!(leaves(&cset(&items), &cfrozen(&[json!(1)])), 2);
}

/// Python's `bool(a_set)` is emptiness, so a `type_changes` from a
/// non-empty set to `true` is reproduced by coercion.
#[test]
fn a_set_coerces_to_its_own_truthiness() {
    let leaves = super::distance::type_change_leaf_length;

    assert_eq!(leaves(&cset(&[json!(1)]), &cv(&json!(true))), 1);
    assert_eq!(leaves(&cfrozen(&[]), &cv(&json!(false))), 1);
    // Every non-`bool` target refuses a set outright, as it does a list.
    assert_eq!(leaves(&cset(&[json!(1)]), &cv(&json!(2))), 2);
    assert_eq!(leaves(&cset(&[json!(1)]), &cv(&json!(2.5))), 2);
    assert_eq!(leaves(&cset(&[json!(1)]), &cv(&json!("x"))), 2);
}

/// A nested-sequence element's `python_eq` must reject a length mismatch
/// outright, not answer from however many elements the shorter side has.
///
/// `((1, 2, 3),)` vs `[[1, 2]]`: the outer tuple-vs-list pair is length-1 on
/// both sides, so `sequences_python_eq`'s own top-level length check passes
/// through to a per-element `python_eq` -- which is where the mismatched
/// INNER lengths (3 vs 2) must be caught. An `&&` -> `||` mutant in
/// `python_eq`'s array/tuple arm would let the two shorter, pairwise-equal
/// elements ([1, 2] against the first two of [1, 2, 3]) pass regardless of
/// the length check, wrongly reproducing `new_value`.
#[test]
fn python_eq_rejects_mismatched_nested_sequence_lengths() {
    let leaves = super::distance::type_change_leaf_length;

    assert_eq!(
        leaves(&ctup(&[json!([1, 2, 3])]), &cv(&json!([[1, 2]]))),
        1 + 2,
        "not reproduced: the mismatched inner lengths must cost new_value's own length"
    );
    // The control: equal-length, equal-content inner sequences ARE
    // reproduced, so the assertion above is testing the length check, not
    // an unrelated content mismatch.
    assert_eq!(
        leaves(&ctup(&[json!([1, 2, 3])]), &cv(&json!([[1, 2, 3]]))),
        1
    );
}

/// A frozenset holding an unhashable value cannot exist in Python, but the
/// value model can express one; it still keys by its own content.
#[test]
fn a_frozenset_holding_an_unhashable_value_keeps_its_own_digest() {
    let memo = IgnoreOrderMemo::new();
    let with_list = CValue::FrozenSet(SetItems::new(vec![cv(&json!([1]))]));
    let with_other_list = CValue::FrozenSet(SetItems::new(vec![cv(&json!([1.0]))]));

    assert_ne!(
        super::hash::item_key(&with_list, &memo),
        super::hash::item_key(&with_other_list, &memo),
        "an unhashable frozenset never inherits a Python-equal one's digest"
    );
}

/// A member Python cannot hash — a list, a set or a dict — has no Python
/// identity to compare by, so it keys structurally; only a directly-built
/// value can carry one.
#[test]
fn an_unhashable_set_member_keys_structurally() {
    let memo = IgnoreOrderMemo::new();
    let key = |value: &CValue| super::set_member_digest(value, &memo);
    let mut builder = crate::value::Builder::new();
    let object = builder.object(vec![("a".to_string(), cv(&json!(1)))]);
    let other_object = builder.object(vec![("a".to_string(), cv(&json!(2)))]);

    assert_ne!(key(&cv(&json!([1]))), key(&cv(&json!([2]))));
    assert_eq!(key(&cv(&json!([1]))), key(&cv(&json!([1]))));
    assert_ne!(key(&cset(&[json!(1)])), key(&cset(&[json!(2)])));
    assert_ne!(key(&object), key(&other_object));
    assert_ne!(key(&cv(&json!([1]))), key(&ctup(&[json!(1)])));

    // A nested unhashable value still keys the container it sits in.
    assert_ne!(
        key(&ctup(&[json!([1])])),
        key(&ctup(&[json!([2])])),
        "an unhashable element still distinguishes its tuple"
    );

    // A number too large for an `i64` keys apart from every other integer.
    assert_ne!(
        key(&CValue::Number(crate::value::Number::from_u64(u64::MAX))),
        key(&cv(&json!(1)))
    );
}

/// Two unhashable containers of different kinds are different members: a
/// `list` and a `set` holding the same values are not Python-equal, so they
/// must not share one identity even though neither can be hashed.
#[test]
fn unhashable_set_members_of_different_kinds_stay_distinct() {
    let memo = IgnoreOrderMemo::new();
    let key = |value: &CValue| super::set_member_digest(value, &memo);
    let listed = |value: CValue| CValue::Tuple(Box::new([value]));

    assert_ne!(
        key(&listed(cv(&json!([1])))),
        key(&listed(cset(&[json!(1)]))),
        "a list and a set holding the same members are not one identity"
    );
    assert_ne!(
        key(&listed(cv(&json!([1])))),
        key(&listed(cfrozen(&[json!(1)])))
    );

    // The pair the missing kind tag made invisible, end to end.
    let with_set = CValue::Set(SetItems::new(vec![listed(cset(&[json!(1)]))]));
    let with_list = CValue::Set(SetItems::new(vec![listed(cv(&json!([1])))]));
    assert_eq!(
        crate::diff::diff(&with_set, &with_list)
            .expect("shallow sets diff cleanly")
            .to_json_value(),
        json!({
            "set_item_added": ["root[([1],)]"],
            "set_item_removed": ["root[({1},)]"],
        })
    );
}

/// The per-node cache decision has to hold whether a naive/aware (or int/float)
/// difference sits at the member's own root or nested below it. A member's
/// digest is built through the shared cache at every node, so both families
/// collapse. Pins the root-level rows (a control the below-root rows are read
/// against) and the below-root rows in one place; every pairing but the
/// bare-number sibling is `{}` in real `deepdiff==9.1.0`.
#[test]
fn a_set_member_collapses_a_calendar_difference_at_the_root_and_below_it() {
    let n = || cdt(2024, 1, 1, None);
    let a = || cdt(2024, 1, 1, Some(0));
    let i = |x: i64| cv(&json!(x));
    let f = |x: f64| cv(&json!(x));
    let tup = |items: Vec<CValue>| CValue::Tuple(items.into_boxed_slice());
    let fz = |items: Vec<CValue>| CValue::FrozenSet(SetItems::new(items));
    let set = |items: Vec<CValue>| CValue::Set(SetItems::new(items));
    let empty = |x: CValue, y: CValue| {
        crate::diff::diff(&x, &y)
            .expect("shallow sets diff cleanly")
            .is_empty()
    };

    // Root level: the difference is a direct element of the member.
    assert!(empty(
        set(vec![tup(vec![n(), tup(vec![i(1)])])]),
        set(vec![tup(vec![a(), tup(vec![f(1.0)])])]),
    ));
    assert!(
        !empty(
            set(vec![tup(vec![n(), i(1)])]),
            set(vec![tup(vec![a(), f(1.0)])]),
        ),
        "a bare-number sibling is type-distinct with no shared cache entry"
    );

    // Below the root: `((N,),)` vs `((A,),)`, one level deeper than the rows above.
    assert!(empty(
        set(vec![tup(vec![tup(vec![n()])])]),
        set(vec![tup(vec![tup(vec![a()])])]),
    ));
    // Deeper still: `(((N,),),)` vs `(((A,),),)`.
    assert!(empty(
        set(vec![tup(vec![tup(vec![tup(vec![n()])])])]),
        set(vec![tup(vec![tup(vec![tup(vec![a()])])])]),
    ));
    // Through a frozenset: `fs({(N,)})` vs `fs({(A,)})`.
    assert!(empty(
        set(vec![fz(vec![tup(vec![n()])])]),
        set(vec![fz(vec![tup(vec![a()])])]),
    ));
    // A twin naive/aware, one at the root and one below it, both collapsing.
    assert!(empty(
        set(vec![tup(vec![n(), tup(vec![n(), i(1)])])]),
        set(vec![tup(vec![a(), tup(vec![a(), i(1)])])]),
    ));
}

/// A set member's digest walk is iterative and its result compares in `O(1)`
/// (a `RepId`), so a deeply nested member neither overflows the native stack
/// while it is hashed nor while two members are compared — a naive structural
/// digest with a derived comparison would overflow this small stack. The two
/// members share one deep chain and differ only naive/aware at the outer tuple,
/// so they match: the walk runs and the comparison genuinely fires (the two
/// sets are unequal as wholes, so no fast path short-circuits).
#[test]
fn a_deeply_nested_set_member_hashes_and_compares_without_native_recursion() {
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            const DEPTH: usize = 200_000;
            let chain = |leaf: CValue| {
                let mut value = leaf;
                for _ in 0..DEPTH {
                    value = CValue::FrozenSet(SetItems::new(vec![value]));
                }
                value
            };
            let member = |dt: CValue| {
                CValue::Set(SetItems::new(vec![CValue::Tuple(
                    vec![dt, chain(cv(&json!(0)))].into_boxed_slice(),
                )]))
            };
            let a = member(cdt(2024, 1, 1, None));
            let b = member(cdt(2024, 1, 1, Some(0)));
            assert!(
                crate::diff::diff(&a, &b)
                    .expect("deep set members diff cleanly")
                    .is_empty()
            );
        })
        .expect("probe thread spawns");
    handle
        .join()
        .expect("set-member hashing and comparison complete on a small stack");
}

/// Interning `K` set members must stay near linear in `K`, never quadratic.
///
/// - **Float bit-pattern collision (`SetFloat*`, `ListFloat`) — the runtime
///   guard.** A float carrying an integer or half-integer has ~50 trailing zero
///   bits; on the `FxHash` tables the crate keeps (e.g. `HashedList` for an
///   `ignore_order` list), a run of them collides unless the float bits are
///   mixed first ([`crate::lcs::mix_float_bits`]). Reverting the mixing turns
///   the float rows here red, so they genuinely guard it.
/// - **Benign near-linearity (`SetIntPair`).** A run of plain int 2-tuples
///   exercises the set-member tables on the *shape* of the crafted
///   hash-flooding attack, but does **not** stand in for the attack: sequential
///   ints do not collide under `FxHash`, so this row stays green even if the
///   tables were reverted to `FxHash`. The adversarial hazard — the tables are
///   keyed by attacker-controlled content and reached with the default
///   `ignore_order=false` — is guarded at the **type level** instead: the tables
///   are [`BTreeMap`]s, and [`super::hash::MemberHashKey`]/
///   [`super::hash::MemberContent`] no longer derive `Hash`, so putting them
///   back on an `FxHash` map fails to compile (`E0599`). This row is a plain
///   regression check that the `BTreeMap` path itself scales.
///
/// Each asserts the `K -> 2K` diff-time ratio stays under `3.0` — a linear (or
/// `n log n`) pass is `~2x`, a quadratic one `~4x`. Sized to run well under a
/// second.
#[test]
fn set_and_list_member_interning_scales_near_linearly() {
    use crate::value::Number;

    #[derive(Clone, Copy)]
    enum Shape {
        SetIntFloat,
        SetHalfFloat,
        SetIntPair,
        ListFloat,
    }

    let f = |x: f64| CValue::Number(Number::from_f64(x).expect("finite"));
    let i = |n: i64| CValue::Number(Number::from_i64(n));

    let build = |k: usize, shape: Shape| -> (CValue, CValue, DiffOptions) {
        #[allow(clippy::cast_precision_loss)]
        let member = |n: usize| -> CValue {
            let n_i = i64::try_from(n).expect("test sizes fit i64");
            match shape {
                Shape::SetIntFloat => CValue::Tuple(vec![f(n as f64)].into_boxed_slice()),
                Shape::SetHalfFloat => CValue::Tuple(vec![f(n as f64 + 0.5)].into_boxed_slice()),
                Shape::SetIntPair => CValue::Tuple(vec![i(n_i), i(n_i)].into_boxed_slice()),
                Shape::ListFloat => f(n as f64),
            }
        };
        let side = |extra: bool| {
            let mut items: Vec<CValue> = (0..k).map(member).collect();
            if extra {
                items.push(CValue::Str("sentinel".to_string().into_boxed_str()));
            }
            match shape {
                Shape::ListFloat => CValue::Array(items.into_boxed_slice()),
                _ => CValue::Set(SetItems::new(items)),
            }
        };
        let opts = DiffOptions {
            ignore_order: matches!(shape, Shape::ListFloat),
            ..DiffOptions::default()
        };
        // Differ by one member so the whole-value fast path can't short-circuit.
        (side(false), side(true), opts)
    };

    let best_diff = |k: usize, shape: Shape| -> f64 {
        let (a, b, opts) = build(k, shape);
        let _ = crate::diff::diff_with_options(&a, &b, &opts).expect("diffs cleanly"); // warm
        (0..5)
            .map(|_| {
                let start = std::time::Instant::now();
                let _ = crate::diff::diff_with_options(&a, &b, &opts).expect("diffs cleanly");
                start.elapsed().as_secs_f64()
            })
            .fold(f64::INFINITY, f64::min)
    };

    for (name, shape) in [
        ("set int-float", Shape::SetIntFloat),
        ("set half-float", Shape::SetHalfFloat),
        ("set int-pair", Shape::SetIntPair),
        ("ignore_order list float", Shape::ListFloat),
    ] {
        let k = 10_000;
        let t1 = best_diff(k, shape);
        let t2 = best_diff(2 * k, shape);
        let ratio = t2 / t1;
        assert!(
            ratio < 3.0,
            "{name}: K->2K ratio {ratio:.2} (t1={t1:.4}s t2={t2:.4}s) is super-linear — \
             keys are colliding in their interning table"
        );
    }
}

// --- distance-memo repetition-collision regression (issue #31) ---

/// Two sibling subtrees whose list elements share an `ItemKey` (order- and
/// repetition-insensitive) but differ in element repetition have different
/// distances, so the distance memo must not hand one's cached answer to the
/// other. `[3, 4]` and `[3]*8 + [4]*8` both key as the set `{3, 4}`; paired
/// against `[9, 8]`, the short list is close enough to pair (a whole-element
/// `values_changed`) while the long list is not (it recurses). Keying the memo
/// by `ItemKey` conflated them; keying by the exact structural `DistKey` does
/// not. Verified by the memo being decision-neutral (enabled == disabled) and
/// by the two sibling keys never contaminating each other.
#[test]
fn memo_does_not_conflate_lists_sharing_itemkey_but_differing_repetition() {
    let short = json!([3, 4]);
    let long = json!([3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4]);
    let other = json!([9, 8]);
    let opts = DiffOptions {
        ignore_order: true,
        max_depth: 1_000,
    };

    // Both key orders: the memo processes candidates in order, so a short-then-
    // long and a long-then-short sibling arrangement stress it differently.
    for (p_a, q_a) in [(&short, &long), (&long, &short)] {
        let a = cv(&json!({"p": [p_a], "q": [q_a]}));
        let b = cv(&json!({"p": [other], "q": [other]}));

        let memoized = crate::diff::diff_with_options_memo(&a, &b, &opts, &IgnoreOrderMemo::new())
            .expect("memoized diff succeeds");
        let unmemoized =
            crate::diff::diff_with_options_memo(&a, &b, &opts, &IgnoreOrderMemo::disabled())
                .expect("unmemoized diff succeeds");
        assert_eq!(
            memoized.to_json_value().to_string(),
            unmemoized.to_json_value().to_string(),
            "the memo changed the report for arrangement p={p_a}, q={q_a}"
        );

        // No cross-key contamination: the whole diff must equal the two
        // sibling subtrees diffed in isolation and merged. If the memo leaked
        // one sibling's distance into the other, this would differ.
        let mut isolated = crate::diff::diff_with_options(
            &cv(&json!({"p": [p_a]})),
            &cv(&json!({"p": [other]})),
            &opts,
        )
        .expect("p-only diff succeeds");
        let q_only = crate::diff::diff_with_options(
            &cv(&json!({"q": [q_a]})),
            &cv(&json!({"q": [other]})),
            &opts,
        )
        .expect("q-only diff succeeds");
        isolated.merge(q_only);
        assert_eq!(
            memoized.to_json_value(),
            isolated.to_json_value(),
            "sibling subtrees contaminated each other for arrangement p={p_a}, q={q_a}"
        );
    }
}

/// A list of 1..12 elements drawn from a tiny scalar alphabet, so distinct
/// lists frequently share an `ItemKey` (the deduplicated set of members) while
/// differing in element repetition — exactly the shape that made the distance
/// memo unsound when keyed by `ItemKey`.
fn arb_repeating_list() -> impl Strategy<Value = serde_json::Value> {
    prop::collection::vec(
        prop_oneof![Just(json!(0)), Just(json!(1)), Just(json!(2))],
        1..12,
    )
    .prop_map(serde_json::Value::Array)
}

/// An `(a, b)` pair engineered to stress the distance memo: `a` is a dict of
/// four sibling keys each wrapping its own repetition-varying list, while `b`
/// gives *every* sibling the same "other" list. So each sibling pairs its inner
/// list against one shared other list, and two siblings whose lists share a
/// member set (frequent over a 3-symbol alphabet) present the same `(removed,
/// added)` `ItemKey` pair with genuinely different distances — the exact
/// collision the memo must not act on. The list lengths make those distances
/// straddle the 0.3 cutoff.
fn arb_repeating_siblings_pair() -> impl Strategy<Value = (serde_json::Value, serde_json::Value)> {
    let siblings = (
        arb_repeating_list(),
        arb_repeating_list(),
        arb_repeating_list(),
        arb_repeating_list(),
    );
    (siblings, arb_repeating_list()).prop_map(|((p, q, r, s), other)| {
        let wrap = |v: serde_json::Value| serde_json::Value::Array(vec![v]);
        let a = serde_json::json!({
            "p": wrap(p),
            "q": wrap(q),
            "r": wrap(r),
            "s": wrap(s),
        });
        let b = serde_json::json!({
            "p": wrap(other.clone()),
            "q": wrap(other.clone()),
            "r": wrap(other.clone()),
            "s": wrap(other),
        });
        (a, b)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(800))]

    /// Targeted at the repetition-collision regression (issue #31): over dicts
    /// of single-element lists wrapping repetition-varying lists — the shape
    /// where sibling candidates
    /// share an `ItemKey` but not a distance, and where those distances
    /// straddle the 0.3 pairing cutoff — the distance memo must still change no
    /// decision. Fails on the pre-fix `ItemKey`-keyed cache.
    #[test]
    fn memo_neutral_on_repetition_varying_siblings(
        (a, b) in arb_repeating_siblings_pair(),
    ) {
        let a = crate::value::Value::from(a);
        let b = crate::value::Value::from(b);
        let opts = DiffOptions {
            ignore_order: true,
            max_depth: 1_000,
        };
        let with = crate::diff::diff_with_options_memo(&a, &b, &opts, &IgnoreOrderMemo::new());
        let without =
            crate::diff::diff_with_options_memo(&a, &b, &opts, &IgnoreOrderMemo::disabled());
        prop_assert_eq!(
            with.map(|report| report.to_json_value().to_string()),
            without.map(|report| report.to_json_value().to_string()),
        );
    }
}

/// The distance memo's two caching conditions are load-bearing, so pin each:
/// a scalar-only `ignore_order` diff must cache nothing (scalar distances never
/// recurse, so `is_container` gates them out), and a `disabled()` memo must
/// cache nothing regardless of shape (so the with/without differential tests
/// genuinely exercise the uncached path). Both also guard the caching gate
/// against being widened to "always cache", which would make the memo do
/// redundant work.
#[test]
fn distance_memo_only_caches_container_pairs_when_enabled() {
    let opts = DiffOptions {
        ignore_order: true,
        max_depth: 1_000,
    };

    // Scalars never recurse, so an enabled memo caches nothing for them.
    let scalar_memo = IgnoreOrderMemo::new();
    crate::diff::diff_with_options_memo(
        &cv(&json!([1, 2, 3])),
        &cv(&json!([3, 4, 5])),
        &opts,
        &scalar_memo,
    )
    .expect("scalar diff succeeds");
    assert_eq!(
        scalar_memo.cache_len(),
        0,
        "scalar-only ignore_order diff must not populate the distance cache"
    );

    // Container pairs do populate an enabled memo...
    let container_a = cv(&json!([[1, 2], [3, 4], "anchor"]));
    let container_b = cv(&json!(["anchor", [1, 9], [3, 8]]));
    let enabled_memo = IgnoreOrderMemo::new();
    crate::diff::diff_with_options_memo(&container_a, &container_b, &opts, &enabled_memo)
        .expect("container diff succeeds");
    assert!(
        enabled_memo.cache_len() > 0,
        "container ignore_order diff should populate the distance cache"
    );

    // ...but a disabled memo caches nothing, whatever the shape.
    let disabled_memo = IgnoreOrderMemo::disabled();
    crate::diff::diff_with_options_memo(&container_a, &container_b, &opts, &disabled_memo)
        .expect("container diff succeeds");
    assert_eq!(
        disabled_memo.cache_len(),
        0,
        "a disabled memo must never populate the distance cache"
    );
}

// --- DistKey hashing stack safety (issue #31) -------------------------

/// [`DistKey`]'s `Hash` walks the value with an explicit stack, never native
/// recursion, so hashing a distance-cache key can never overflow the native
/// stack however deep the value — the same posture the engine's `Value`
/// `Drop`/`PartialEq` hold. Isolated from the key's own value clone (which,
/// like the report's clones, recurses): the chain is built iteratively and
/// wrapped without copying, so the only thing exercised on the deliberately
/// tiny 256 KiB stack is the hash. A recursive hasher overflows here.
#[test]
fn dist_key_hashing_does_not_overflow_the_native_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            const DEPTH: usize = 200_000;
            let mut value = cv(&json!(0));
            for _ in 0..DEPTH {
                value = CValue::Array(vec![value].into_boxed_slice());
            }
            let key = super::hash::DistKey::from_rc(std::rc::Rc::new(value));
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&key, &mut hasher);
            let _ = std::hash::Hasher::finish(&hasher);
        })
        .expect("probe thread spawns");
    handle
        .join()
        .expect("DistKey hashing completes on a small stack");
}

/// The same memo-driven `ignore_order` path at the default budget on an
/// ordinary main-thread-sized (8 MiB) stack — the common case a caller hits
/// without raising `max_depth` or spawning a special thread. (Rust runs each
/// `#[test]` on a 2 MiB worker thread, smaller than a real main thread, so the
/// probe sizes the stack to an ordinary 8 MiB rather than relying on the test
/// harness's own reduced default.)
#[test]
fn ignore_order_memo_at_default_budget_on_plain_thread() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let build = |leaf: i64| {
                let mut value = cv(&json!(leaf));
                for _ in 0..200 {
                    value = CValue::Array(vec![value].into_boxed_slice());
                }
                value
            };
            let opts = DiffOptions {
                ignore_order: true,
                max_depth: crate::diff::DEFAULT_MAX_DEPTH,
            };
            let report = crate::diff::diff_with_options(&build(1), &build(2), &opts)
                .expect("default-budget ignore_order diff succeeds");
            assert!(!report.is_empty());
        })
        .expect("probe thread spawns");
    handle
        .join()
        .expect("default-budget ignore_order diff completes on an ordinary stack");
}

/// The distance-cache key's hash of a value, for the agreement tests below.
fn dist_hash(value: &CValue) -> u64 {
    use std::hash::{Hash, Hasher};
    let key = super::hash::DistKey::from_rc(std::rc::Rc::new(value.clone()));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// `DistKey`'s `Hash` must agree with its `Eq` (equal values hash equal), or the
/// distance memo silently stops hitting and the pairing goes exponential. The
/// explicit-stack rewrite is the usual place this breaks (a differing child
/// visit order, or a length hashed at the wrong spot), so pin the equal-but-
/// differently-built cases the value model treats as equal: signed-zero and
/// integral floats, sets/frozensets whose members arrive in different insertion
/// orders (`SetItems` canonicalizes them equal), and datetimes equal by instant
/// though built with different offsets (a naive value read as UTC, and an
/// aware pair shifted by its offset to the same moment).
#[test]
fn dist_key_hash_agrees_with_equality_on_tricky_equal_values() {
    let float = |f: f64| CValue::Number(crate::value::Number::from_f64(f).unwrap());
    let nested_set = |order: [i64; 3]| {
        CValue::Set(SetItems::new(vec![
            ctup(&[json!(order[0])]),
            ctup(&[json!(order[1])]),
            ctup(&[json!(order[2])]),
        ]))
    };
    let pairs: &[(CValue, CValue)] = &[
        // Signed zero: Value equality treats +0.0 == -0.0.
        (float(0.0), float(-0.0)),
        // Set members in different insertion orders canonicalize equal.
        (
            cset(&[json!(1), json!(2), json!(3)]),
            cset(&[json!(3), json!(1), json!(2)]),
        ),
        (
            cfrozen(&[json!("b"), json!("a")]),
            cfrozen(&[json!("a"), json!("b")]),
        ),
        // A set of tuples, members reordered: exercises the nested walk.
        (nested_set([1, 2, 3]), nested_set([3, 2, 1])),
        // A naive datetime (read as UTC) and an aware one at the same instant:
        // Value equality compares datetimes by instant, so these are equal and
        // must hash equal — they would not if the hash mixed in the offset.
        (cdt(2024, 6, 1, None), cdt(2024, 6, 1, Some(0))),
        // Two aware datetimes at the same instant but different wall clock and
        // offset: 12:00+00:00 == 13:00+01:00.
        (
            cdt_at(2024, 6, 1, 12, 0, 0, 0, Some(0)),
            cdt_at(2024, 6, 1, 13, 0, 0, 0, Some(3600)),
        ),
    ];
    for (a, b) in pairs {
        assert_eq!(a, b, "test inputs must be Value-equal: {a:?} vs {b:?}");
        assert_eq!(
            dist_hash(a),
            dist_hash(b),
            "equal values must hash equal: {a:?} vs {b:?}"
        );
    }
}

/// An `arbitrary` compact value covering every equality class the distance-key
/// hash must respect — including the ones JSON cannot express, so they are
/// actually generated: tuples, sets, frozensets, datetimes (naive and aware),
/// dates, and floats (signed zero and integral values among them).
fn arb_cvalue() -> impl Strategy<Value = CValue> {
    let arb_datetime = (
        2000i32..2025,
        1u8..=12,
        1u8..=28,
        0u8..24,
        0u8..60,
        0u8..60,
        prop_oneof![
            Just(None),
            Just(Some(0)),
            Just(Some(3600)),
            Just(Some(-3600))
        ],
    )
        .prop_map(|(y, mo, d, h, mi, s, off)| cdt_at(y, mo, d, h, mi, s, 0, off));
    let arb_date = (2000i32..2025, 1u8..=12, 1u8..=28).prop_map(|(y, m, d)| cdate(y, m, d));
    let arb_float = prop_oneof![
        Just(0.0f64),
        Just(-0.0f64),
        Just(1.0f64),
        Just(2.0f64),
        any::<f64>(),
    ]
    .prop_filter_map("finite floats only", |f| {
        crate::value::Number::from_f64(f).map(CValue::Number)
    });
    let leaf = prop_oneof![
        Just(CValue::Null),
        any::<bool>().prop_map(CValue::Bool),
        any::<i64>().prop_map(|i| CValue::Number(crate::value::Number::from_i64(i))),
        arb_float,
        "[a-z]{0,3}".prop_map(|s| CValue::Str(s.into_boxed_str())),
        arb_datetime,
        arb_date,
    ];
    leaf.prop_recursive(5, 40, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4)
                .prop_map(|v| CValue::Array(v.into_boxed_slice())),
            prop::collection::vec(inner.clone(), 0..4)
                .prop_map(|v| CValue::Tuple(v.into_boxed_slice())),
            prop::collection::vec(inner.clone(), 0..4).prop_map(|v| CValue::Set(SetItems::new(v))),
            prop::collection::vec(inner.clone(), 0..4)
                .prop_map(|v| CValue::FrozenSet(SetItems::new(v))),
            prop::collection::vec(("[a-c]", inner), 0..3)
                .prop_map(|entries| crate::value::Builder::new().object(entries)),
        ]
    })
}

/// Rebuilds `value` into a twin that is equal by [`Value`]'s rules but differs
/// **structurally**, so the hash-agreement property has real power. The
/// load-bearing arms are the two places `Value` equality is coarser than
/// structure:
///
/// - a **datetime** is re-expressed at the same instant with different fields —
///   a naive value (read as UTC) becomes aware `+00:00`, and an aware value's
///   wall clock and offset shift together by one hour (e.g. `12:00+00:00` ->
///   `13:00+01:00`), which `Value::eq` compares equal by instant;
/// - a **signed zero** flips sign (`+0.0` <-> `-0.0`), which `Value::eq`
///   compares equal though the bit patterns differ.
///
/// Set/frozenset members and dict entries are also reversed before rebuilding
/// (they re-canonicalize to the same stored order, so this is only a
/// construction-path check, not where the power comes from). Recurses over
/// proptest-bounded depth (safe).
///
/// The datetime and signed-zero perturbations are suppressed once the walk is
/// **below a set or frozenset** (`in_set`, sticky through nested arrays, tuples
/// and dicts). A set's canonical storage order is *finer* than value equality
/// for signed zero (`-0.0` sorts before `0.0` in [`SetItems`], and `Value::eq`
/// on sets zips stored order), so perturbing a member there would reorder the
/// enclosing set and make the twin genuinely unequal — a false failure. Above
/// any set the perturbations run and give the property its power; the reversal
/// still runs everywhere.
fn structural_twin(value: &CValue, in_set: bool) -> CValue {
    match value {
        CValue::Array(items) => CValue::Array(
            items
                .iter()
                .map(|item| structural_twin(item, in_set))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        CValue::Tuple(items) => CValue::Tuple(
            items
                .iter()
                .map(|item| structural_twin(item, in_set))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        CValue::Set(items) => {
            let mut members: Vec<CValue> = items
                .iter()
                .map(|item| structural_twin(item, true))
                .collect();
            members.reverse();
            CValue::Set(SetItems::new(members))
        }
        CValue::FrozenSet(items) => {
            let mut members: Vec<CValue> = items
                .iter()
                .map(|item| structural_twin(item, true))
                .collect();
            members.reverse();
            CValue::FrozenSet(SetItems::new(members))
        }
        CValue::Object(map) => {
            let mut entries: Vec<(String, CValue)> = map
                .iter()
                .map(|(key, child)| (key.to_string(), structural_twin(child, in_set)))
                .collect();
            entries.reverse();
            crate::value::Builder::new().object(entries)
        }
        CValue::DateTime(_) | CValue::Number(_) if in_set => value.clone(),
        CValue::DateTime(dt) => {
            let date = dt.date();
            match dt.utc_offset_seconds() {
                None => cdt_at(
                    date.year(),
                    date.month(),
                    date.day(),
                    dt.hour(),
                    dt.minute(),
                    dt.second(),
                    dt.microsecond(),
                    Some(0),
                ),
                Some(offset) => {
                    let (hour, offset) = if dt.hour() < 23 {
                        (dt.hour() + 1, offset + 3600)
                    } else {
                        (dt.hour() - 1, offset - 3600)
                    };
                    cdt_at(
                        date.year(),
                        date.month(),
                        date.day(),
                        hour,
                        dt.minute(),
                        dt.second(),
                        dt.microsecond(),
                        Some(offset),
                    )
                }
            }
        }
        CValue::Number(n) if n.is_f64() => {
            let f = n.as_f64().expect("is_f64 guarantees as_f64");
            let flipped = if f == 0.0 { -f } else { f };
            CValue::Number(
                crate::value::Number::from_f64(flipped).expect("finite float stays finite"),
            )
        }
        scalar => scalar.clone(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Over generated nested shapes spanning every equality class (see
    /// `arb_cvalue`), a value hashes identically to a [`structural_twin`] that
    /// is `Value`-equal but structurally different: an equal-instant datetime
    /// at a different offset and a sign-flipped zero (the two places `Value`
    /// equality is coarser than structure), plus reversed set/dict order.
    /// Guards the `DistKey` `Hash`/`Eq` agreement the memo's soundness depends
    /// on — a hash that mixed in the UTC offset or the raw signed-zero bits
    /// would fail here.
    #[test]
    fn dist_key_hash_equal_for_equal_values(value in arb_cvalue()) {
        let twin = structural_twin(&value, false);
        prop_assert_eq!(&value, &twin);
        prop_assert_eq!(dist_hash(&value), dist_hash(&twin));

        // A set of the value wrapped, built from two orders, stays equal.
        let s1 = CValue::Set(SetItems::new(vec![value.clone(), CValue::Null, cv(&json!("z"))]));
        let s2 = CValue::Set(SetItems::new(vec![cv(&json!("z")), value, CValue::Null]));
        prop_assert_eq!(&s1, &s2);
        prop_assert_eq!(dist_hash(&s1), dist_hash(&s2));
    }
}
