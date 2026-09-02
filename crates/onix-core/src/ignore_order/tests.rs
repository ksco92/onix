use crate::diff::DiffOptions;
use crate::test_support::{cobj, cv, cvec};
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
    super::distance::count_diff_leaves(&cv(a), &cv(b), depth, opts)
}
fn count_object_diff_leaves(
    a: &serde_json::Map<String, serde_json::Value>,
    b: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
    opts: &DiffOptions,
) -> usize {
    super::distance::count_object_diff_leaves(&cobj(a), &cobj(b), depth, opts)
}
fn count_array_diff_leaves(
    a: &[serde_json::Value],
    b: &[serde_json::Value],
    depth: usize,
    opts: &DiffOptions,
) -> usize {
    super::distance::count_array_diff_leaves(&cvec(a), &cvec(b), depth, opts)
}
fn item_key(value: &serde_json::Value) -> super::hash::ItemKey {
    super::hash::item_key(&cv(value))
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
    );
    assert_eq!(d, 0.0);
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
