use super::DEFAULT_MAX_DEPTH;
use crate::error::Error;
use crate::path::PathSegment;
use crate::report::Report;
use crate::test_support::{cdate, cdt, cdt_at, cfrozen, cnum, cobj, cset, ctup, cv};
use crate::value::{Object as CObject, ObjectKey, SetItems, Value as CValue};
use serde_json::{Map, Number, Value, json};

// Thin wrappers routing each `serde_json`-literal-based test through the real
// compact-typed engine via the shared `crate::test_support` converters.
fn diff(a: &Value, b: &Value) -> Result<Report, Error> {
    super::diff(&cv(a), &cv(b))
}
fn diff_with_max_depth(a: &Value, b: &Value, max_depth: usize) -> Result<Report, Error> {
    super::diff_with_max_depth(&cv(a), &cv(b), max_depth)
}
fn python_type_name(value: &Value) -> &'static str {
    super::python_type_name(&cv(value))
}
fn values_equal(a: &Value, b: &Value) -> bool {
    super::values_equal(&cv(a), &cv(b))
}
fn scalar_diff(
    path: &[PathSegment],
    equal: bool,
    a: &Value,
    b: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<Report, Error> {
    super::scalar_diff(path, equal, &cv(a), &cv(b), depth, max_depth)
}
fn deeper_than(value: &Value, limit: usize) -> bool {
    super::dispatch::deeper_than(&cv(value), limit)
}
fn map_deeper_than(map: &Map<String, Value>, limit: usize) -> bool {
    super::dispatch::map_deeper_than(&cobj(map), limit)
}
fn number_as_i128(n: &Number) -> Option<i128> {
    super::scalar::number_as_i128(&cnum(n))
}

/// Wraps `leaf` in `depth` single-key (`"k"`) nested dicts, so `leaf`
/// itself sits at nesting depth `depth` (root, i.e. `depth == 0`, is
/// `leaf` unwrapped).
fn nested_dict(depth: usize, leaf: Value) -> Value {
    let mut value = leaf;
    for _ in 0..depth {
        let mut map = Map::new();
        map.insert("k".to_string(), value);
        value = Value::Object(map);
    }
    value
}

/// Wraps `leaf` in `depth` single-element nested arrays, so `leaf` sits
/// at nesting depth `depth`.
fn nested_array(depth: usize, leaf: Value) -> Value {
    let mut value = leaf;
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    value
}

/// A depth just past [`DEFAULT_MAX_DEPTH`], for tests whose point is
/// "behaves correctly once past the guard" rather than "handles an
/// absolutely enormous structure" — small enough to build, diff, and
/// drop entirely on a plain default-stack test thread — seeing past the
/// guard needs only a small margin, not a `5_000`/`100_000`-scale
/// structure.
const PAST_DEFAULT_MAX_DEPTH: usize = DEFAULT_MAX_DEPTH + 100;

/// A depth that reliably overflows a *native-recursive* equivalent of
/// an iterative function on this crate's test thread, reusing the
/// threshold this file already independently establishes via
/// `compounding_depth_regression_max_depth_20_000_...`'s traversal (see
/// `run_on_a_large_stack`'s doc): `19_999` native `diff_at` recursion
/// frames alone, with no deep value involved at all, is empirically
/// confirmed to overflow a default thread's stack. Used only by tests
/// whose actual point is proving a function (`deeper_than`,
/// `values_equal`) is genuinely iterative — not merely correct — so a
/// smaller depth would prove nothing beyond what the boundary tests
/// elsewhere in this file already cover.
const RECURSION_OVERFLOW_DEPTH: usize = 20_000;

/// Runs `f` on a dedicated thread with a generously large stack, then
/// propagates any panic from it.
///
/// Two distinct reasons a test below reaches for this: (1) the *diff
/// call itself* needs more stack, because a `max_depth` as large as
/// `20_000` means up to `20_000` native `diff_at`/`object_diff`
/// recursion frames just to *reach* a finding, which empirically
/// overflows an unmodified default thread's stack — confirmed to
/// happen even for a perfectly ordinary, bug-free diff with no deep
/// values anywhere (a plain unequal scalar leaf at depth `19_999`);
/// or (2) a genuinely-deep fixture (see [`RECURSION_OVERFLOW_DEPTH`])
/// needs to be safely *dropped* at the end of the test — `serde_json`'s
/// derived `Drop` recurses natively with no depth bound (an
/// independent limitation of the JSON value model itself, orthogonal to
/// this crate's own depth-guarded traversal) — without leaking it. Either
/// way this is a *test-fixture*
/// concern, not a production one: `max_depth` is a caller-chosen knob,
/// and a caller who raises it this far is responsible for sizing their
/// own thread's stack accordingly, the same way any deep native
/// recursion in Rust requires.
fn run_on_a_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .expect("failed to spawn large-stack test thread")
        .join()
        .expect("large-stack test thread panicked");
}

#[test]
fn equal_nulls_are_empty() {
    let report = diff(&Value::Null, &Value::Null).unwrap();
    assert!(report.is_empty());
}

#[test]
fn null_vs_scalar_is_type_change() {
    let report = diff(&Value::Null, &json!(0)).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "NoneType", "new_type": "int",
            "old_value": null, "new_value": 0,
        }}})
    );
}

#[test]
fn equal_ints_are_empty() {
    let report = diff(&json!(1), &json!(1)).unwrap();
    assert!(report.is_empty());
}

#[test]
fn unequal_ints_are_values_changed() {
    let report = diff(&json!(1), &json!(2)).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root": {"new_value": 2, "old_value": 1}}})
    );
}

#[test]
fn int_vs_float_is_always_type_change_even_when_numerically_equal() {
    let report = diff(&json!(1), &json!(1.0)).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "int", "new_type": "float",
            "old_value": 1, "new_value": 1.0,
        }}})
    );
}

#[test]
fn unequal_floats_are_values_changed() {
    let report = diff(&json!(1.5), &json!(2.5)).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root": {"new_value": 2.5, "old_value": 1.5}}})
    );
}

#[test]
fn equal_floats_are_empty() {
    let report = diff(&json!(1.5), &json!(1.5)).unwrap();
    assert!(report.is_empty());
}

#[test]
fn positive_zero_and_negative_zero_are_equal() {
    let report = diff(&json!(0.0), &json!(-0.0)).unwrap();
    assert!(report.is_empty());
}

#[test]
fn same_value_within_i64_range_is_equal_regardless_of_i64_or_u64_representation() {
    // Both 9_000_000_000_000_000_000u64 and its i64 counterpart fit
    // within i64::MAX, so this does not by itself exercise the
    // u64-only fallback in `number_as_i128` (see the
    // `u64_max_*` tests below for that).
    let a = Value::Number(Number::from(9_000_000_000_000_000_000u64));
    let b = Value::Number(Number::from(9_000_000_000_000_000_000i64));
    let report = diff(&a, &b).unwrap();
    assert!(report.is_empty());
}

#[test]
fn u64_max_equal_to_itself() {
    let a = Value::Number(Number::from(u64::MAX));
    let b = Value::Number(Number::from(u64::MAX));
    let report = diff(&a, &b).unwrap();
    assert!(report.is_empty());
}

#[test]
fn u64_max_vs_negative_one_is_values_changed_not_false_equal() {
    // u64::MAX has no i64 representation (as_i64() is None, forcing the
    // as_u64() fallback); -1i64 has no u64 representation. Neither side
    // panics, and the two are correctly reported as different, not
    // accidentally treated as equal.
    let a = Value::Number(Number::from(u64::MAX));
    let b = Value::Number(Number::from(-1i64));
    let report = diff(&a, &b).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root": {
            "new_value": -1, "old_value": u64::MAX,
        }}})
    );
}

#[test]
fn unequal_ints_straddling_the_i64_max_boundary_are_values_changed() {
    let i64_max_as_u64 = u64::try_from(i64::MAX).expect("i64::MAX fits in u64");
    let just_over_i64_max = i64_max_as_u64 + 1;
    let a = Value::Number(Number::from(i64::MAX));
    let b = Value::Number(Number::from(just_over_i64_max));
    let report = diff(&a, &b).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root": {
            "new_value": just_over_i64_max, "old_value": i64::MAX,
        }}})
    );
}

#[test]
fn bool_vs_int_is_type_change() {
    let report = diff(&json!(true), &json!(1)).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "bool", "new_type": "int",
            "old_value": true, "new_value": 1,
        }}})
    );
}

#[test]
fn true_vs_false_is_values_changed() {
    let report = diff(&json!(true), &json!(false)).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root": {"new_value": false, "old_value": true}}})
    );
}

#[test]
fn equal_bools_are_empty() {
    let report = diff(&json!(true), &json!(true)).unwrap();
    assert!(report.is_empty());
}

#[test]
fn null_vs_zero_is_type_change() {
    let report = diff(&Value::Null, &json!(0)).unwrap();
    assert!(!report.is_empty());
}

#[test]
fn changed_strings_are_values_changed() {
    let report = diff(&json!("a"), &json!("b")).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root": {"new_value": "b", "old_value": "a"}}})
    );
}

#[test]
fn equal_strings_are_empty() {
    let report = diff(&json!("a"), &json!("a")).unwrap();
    assert!(report.is_empty());
}

#[test]
fn str_vs_int_is_type_change() {
    let report = diff(&json!("1"), &json!(1)).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "str", "new_type": "int",
            "old_value": "1", "new_value": 1,
        }}})
    );
}

#[test]
fn dict_vs_list_is_type_change() {
    let report = diff(&json!({"a": 1}), &json!([1])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "dict", "new_type": "list",
            "old_value": {"a": 1}, "new_value": [1],
        }}})
    );
}

#[test]
fn equal_dicts_are_empty() {
    let report = diff(&json!({"a": 1, "b": 2}), &json!({"b": 2, "a": 1})).unwrap();
    assert!(report.is_empty());
}

#[test]
fn unequal_dicts_now_recurse_instead_of_erroring() {
    // Regression test: this used to
    // return Error::UnsupportedContainerDiff.
    let report = diff(&json!({"a": 1}), &json!({"a": 2})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root['a']": {"new_value": 2, "old_value": 1}}})
    );
}

#[test]
fn empty_dict_vs_empty_dict_is_empty() {
    let report = diff(&json!({}), &json!({})).unwrap();
    assert_eq!(report.to_json_value(), json!({}));
}

#[test]
fn empty_dict_vs_nonempty_dict_is_all_added() {
    // A single added key keeps the key-overlap union at exactly 1, below
    // the `threshold_to_diff_deeper` guard's `union_len > 1` floor, so
    // this stays on the granular add path rather than collapsing into a
    // wholesale `values_changed` (see the `threshold_collapse_*` tests
    // for the >= 2-key case, which does collapse, matching real
    // `DeepDiff` exactly).
    let report = diff(&json!({}), &json!({"a": 1})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_added": {"root['a']": 1}})
    );
}

#[test]
fn nonempty_dict_vs_empty_dict_is_all_removed() {
    // Mirror of `empty_dict_vs_nonempty_dict_is_all_added` above.
    let report = diff(&json!({"a": 1}), &json!({})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_removed": {"root['a']": 1}})
    );
}

#[test]
fn mixed_dict_diff_reports_every_category_exactly() {
    let a = json!({"same": 1, "changed": 2, "type_changed": 3, "removed": 4});
    let b = json!({"same": 1, "changed": 20, "type_changed": "3", "added": 5});

    let report = diff(&a, &b).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({
            "values_changed": {"root['changed']": {"new_value": 20, "old_value": 2}},
            "type_changes": {"root['type_changed']": {
                "old_type": "int", "new_type": "str",
                "old_value": 3, "new_value": "3",
            }},
            "dictionary_item_added": {"root['added']": 5},
            "dictionary_item_removed": {"root['removed']": 4},
        })
    );
}

#[test]
fn unicode_key_is_rendered_correctly_in_added_path() {
    let report = diff(&json!({}), &json!({"héllo世界": 1})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_added": {"root['héllo世界']": 1}})
    );
}

#[test]
fn key_containing_a_quote_is_rendered_correctly() {
    let report = diff(&json!({}), &json!({"it's": 1})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_added": {r#"root["it's"]"#: 1}})
    );
}

#[test]
fn nested_values_changed_reports_deep_path() {
    let report = diff(&json!({"a": {"b": 1}}), &json!({"a": {"b": 2}})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root['a']['b']": {"new_value": 2, "old_value": 1}}})
    );
}

#[test]
fn nested_add_and_remove_at_depth() {
    let a = json!({"a": {"keep": 1, "removed": 2}});
    let b = json!({"a": {"keep": 1, "added": 3}});

    let report = diff(&a, &b).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({
            "dictionary_item_added": {"root['a']['added']": 3},
            "dictionary_item_removed": {"root['a']['removed']": 2},
        })
    );
}

#[test]
fn nested_dict_vs_scalar_is_type_change_with_dict_type() {
    let report = diff(&json!({"a": {"b": 1}}), &json!({"a": 5})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root['a']": {
            "old_type": "dict", "new_type": "int",
            "old_value": {"b": 1}, "new_value": 5,
        }}})
    );
}

#[test]
fn identical_deep_nested_dicts_are_empty() {
    let value = json!({"a": {"b": {"c": [1, 2, 3], "d": "x"}}});
    let report = diff(&value, &value).unwrap();
    assert!(report.is_empty());
}

#[test]
fn nested_unequal_list_inside_dict_now_recurses_instead_of_erroring() {
    // Regression test: this used to
    // return Error::UnsupportedContainerDiff.
    let a = json!({"a": {"b": [1, 2]}});
    let b = json!({"a": {"b": [1, 3]}});

    let report = diff(&a, &b).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root['a']['b'][1]": {"new_value": 3, "old_value": 2}}})
    );
}

#[test]
fn nested_equal_int_repr_shortcut_is_still_correct() {
    // Same numeric value at a nested key, stored as different
    // serde_json Number representations (u64 vs i64). The whole diff
    // must still come out empty, exercising the numeric_diff logic
    // through recursion rather than only at the root.
    let mut a_inner = Map::new();
    a_inner.insert(
        "b".to_string(),
        Value::Number(Number::from(9_000_000_000_000_000_000u64)),
    );
    let mut a = Map::new();
    a.insert("a".to_string(), Value::Object(a_inner));

    let mut b_inner = Map::new();
    b_inner.insert(
        "b".to_string(),
        Value::Number(Number::from(9_000_000_000_000_000_000i64)),
    );
    let mut b = Map::new();
    b.insert("a".to_string(), Value::Object(b_inner));

    let report = diff(&Value::Object(a), &Value::Object(b)).unwrap();
    assert!(report.is_empty());
}

#[test]
fn dictionary_item_added_value_is_a_whole_nested_container_verbatim() {
    let report = diff(&json!({}), &json!({"a": {"nested": {"deep": [1, 2, 3]}}})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_added": {"root['a']": {"nested": {"deep": [1, 2, 3]}}}})
    );
}

#[test]
fn dictionary_item_removed_value_is_a_whole_nested_container_verbatim() {
    let report = diff(&json!({"a": {"nested": {"deep": [1, 2, 3]}}}), &json!({})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_removed": {"root['a']": {"nested": {"deep": [1, 2, 3]}}}})
    );
}

#[test]
fn quoted_key_combined_with_recursion_depth_two() {
    let report = diff(
        &json!({"it's": {"also's": 1}}),
        &json!({"it's": {"also's": 2}}),
    )
    .unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {
            r#"root["it's"]["also's"]"#: {"new_value": 2, "old_value": 1}
        }})
    );
}

#[test]
fn equal_lists_are_empty() {
    let report = diff(&json!([1, 2, 3]), &json!([1, 2, 3])).unwrap();
    assert!(report.is_empty());
}

#[test]
fn unequal_lists_now_diff_index_aligned_instead_of_erroring() {
    // Regression test: this used to
    // return Error::UnsupportedContainerDiff.
    let report = diff(&json!([1, 2]), &json!([1, 3])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root[1]": {"new_value": 3, "old_value": 2}}})
    );
}

#[test]
fn python_type_name_covers_every_json_variant() {
    assert_eq!(python_type_name(&Value::Null), "NoneType");
    assert_eq!(python_type_name(&json!(true)), "bool");
    assert_eq!(python_type_name(&json!(1)), "int");
    assert_eq!(python_type_name(&json!(1.0)), "float");
    assert_eq!(python_type_name(&json!("s")), "str");
    assert_eq!(python_type_name(&json!([1])), "list");
    assert_eq!(python_type_name(&json!({"a": 1})), "dict");
}

#[test]
fn number_as_i128_uses_i64_when_available() {
    assert_eq!(number_as_i128(&Number::from(-5i64)), Some(-5i128));
}

#[test]
fn number_as_i128_falls_back_to_u64_beyond_i64_range() {
    let n = Number::from(u64::MAX);
    assert_eq!(number_as_i128(&n), Some(i128::from(u64::MAX)));
    // A float has no integer representation.
    assert_eq!(
        number_as_i128(&Number::from_f64(1.5).expect("finite")),
        None
    );
}

// --- Recursion depth guard -----------------------------------------

#[test]
fn nested_equal_null_alongside_an_unrelated_change_produces_no_finding_for_it() {
    // The equal-inputs-of-any-depth fast path only fires when the whole
    // top-level pair is equal, so a *nested* equal Null (with an
    // unrelated key differing elsewhere) still reaches diff_at's own
    // (Null, Null) dispatch arm directly, rather than being filtered
    // out before ever getting there.
    let report = diff(
        &json!({"n": null, "changed": 1}),
        &json!({"n": null, "changed": 2}),
    )
    .unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root['changed']": {"new_value": 2, "old_value": 1}}})
    );
}

#[test]
fn nested_equal_list_alongside_an_unrelated_change_produces_no_finding_for_it() {
    // Same reasoning as above, for array_diff recursing into an equal
    // nested (not top-level) list: every same-index pair is equal, so
    // nothing is reported for it.
    let report = diff(
        &json!({"list": [1, 2], "changed": 1}),
        &json!({"list": [1, 2], "changed": 2}),
    )
    .unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root['changed']": {"new_value": 2, "old_value": 1}}})
    );
}

#[test]
fn equal_deeply_nested_dicts_do_not_hit_the_depth_bound() {
    // PAST_DEFAULT_MAX_DEPTH is far beyond DEFAULT_MAX_DEPTH (512); this
    // would overflow the native stack pre-guard (both via unbounded
    // object_diff recursion and via serde_json's recursive derived
    // PartialEq) if `values_equal`'s top-level fast path didn't
    // short-circuit first. Built twice independently (not `.clone()`'d)
    // so this test only exercises the engine under test, not
    // serde_json's own recursive Clone. The point here is "past the
    // guard", not "absolutely enormous" (see `PAST_DEFAULT_MAX_DEPTH`'s
    // doc), so it drops normally with no large-stack accommodation.
    let a = nested_dict(PAST_DEFAULT_MAX_DEPTH, json!("leaf"));
    let b = nested_dict(PAST_DEFAULT_MAX_DEPTH, json!("leaf"));
    let report = diff(&a, &b).unwrap();
    assert!(report.is_empty());
}

#[test]
fn equal_deeply_nested_arrays_do_not_hit_the_depth_bound() {
    let a = nested_array(PAST_DEFAULT_MAX_DEPTH, json!(1));
    let b = nested_array(PAST_DEFAULT_MAX_DEPTH, json!(1));
    let report = diff(&a, &b).unwrap();
    assert!(report.is_empty());
}

#[test]
fn values_equal_handles_deeply_nested_equal_dicts_without_crashing() {
    // RECURSION_OVERFLOW_DEPTH (see its doc) is deep enough that a
    // native-recursive `values_equal` would overflow; the whole test,
    // construction through drop, runs on `run_on_a_large_stack` so the
    // fixture is genuinely reclaimed afterwards instead of leaked.
    run_on_a_large_stack(|| {
        let a = nested_dict(RECURSION_OVERFLOW_DEPTH, json!("leaf"));
        let b = nested_dict(RECURSION_OVERFLOW_DEPTH, json!("leaf"));
        assert!(values_equal(&a, &b));
    });
}

#[test]
fn values_equal_handles_deeply_nested_equal_arrays_without_crashing() {
    run_on_a_large_stack(|| {
        let a = nested_array(RECURSION_OVERFLOW_DEPTH, json!(1));
        let b = nested_array(RECURSION_OVERFLOW_DEPTH, json!(1));
        assert!(values_equal(&a, &b));
    });
}

#[test]
fn values_equal_handles_deeply_nested_unequal_dicts_without_crashing() {
    run_on_a_large_stack(|| {
        let a = nested_dict(RECURSION_OVERFLOW_DEPTH, json!("a"));
        let b = nested_dict(RECURSION_OVERFLOW_DEPTH, json!("b"));
        assert!(!values_equal(&a, &b));
    });
}

#[test]
fn deeper_than_true_for_value_one_level_past_limit() {
    let value = nested_array(4, json!(1));
    assert!(deeper_than(&value, 3));
}

#[test]
fn deeper_than_false_for_value_exactly_at_limit() {
    let value = nested_array(3, json!(1));
    assert!(!deeper_than(&value, 3));
}

#[test]
fn deeper_than_counts_object_nesting_the_same_way_as_array_nesting() {
    // Found by mutation testing: no test above exercised `deeper_than`'s
    // `Value::Object` arm, so a mutant that made that arm behave like a
    // scalar leaf survived undetected.
    assert!(deeper_than(&nested_dict(4, json!(1)), 3));
    assert!(!deeper_than(&nested_dict(3, json!(1)), 3));
}

#[test]
fn deeper_than_counts_frozenset_nesting_the_same_way_as_array_nesting() {
    // A set/frozenset member has no JSON literal, so this builds the
    // compact `Value` directly instead of going through the `cv`-based
    // local `deeper_than` wrapper. Mutation-found (mirrors the object case
    // above): a `depth + 1` -> `depth * 1` mutant in this arm would count
    // every level of set/frozenset nesting as depth `0`, never past the
    // limit.
    let mut value = cv(&json!(1));
    for _ in 0..4 {
        value = CValue::FrozenSet(SetItems::new(vec![value]));
    }
    assert!(super::dispatch::deeper_than(&value, 3));

    let mut value = cv(&json!(1));
    for _ in 0..3 {
        value = CValue::FrozenSet(SetItems::new(vec![value]));
    }
    assert!(!super::dispatch::deeper_than(&value, 3));
}

#[test]
fn deeper_than_false_for_a_scalar_regardless_of_limit() {
    assert!(!deeper_than(&json!(1), 0));
    assert!(!deeper_than(&json!("s"), 0));
    assert!(!deeper_than(&Value::Null, 0));
}

#[test]
fn deeper_than_false_for_empty_containers() {
    assert!(!deeper_than(&json!([]), 0));
    assert!(!deeper_than(&json!({}), 0));
}

#[test]
fn map_deeper_than_true_for_a_map_one_level_past_limit() {
    // Mirrors `deeper_than_true_for_value_one_level_past_limit`: a map
    // whose one field wraps a leaf 3 levels deep has combined nesting 4
    // (1 for the field itself + 3), one past a limit of 3.
    let mut map = Map::new();
    map.insert("x".to_string(), nested_dict(3, json!(1)));
    assert!(map_deeper_than(&map, 3));
}

#[test]
fn map_deeper_than_false_for_a_map_exactly_at_limit() {
    // Kills both `>` -> `==` and `>` -> `>=` mutants in `map_deeper_than`:
    // nesting is exactly 3 (1 + 2), which must NOT count as deeper than a
    // limit of 3 (the check is strictly `>`).
    let mut map = Map::new();
    map.insert("x".to_string(), nested_dict(2, json!(1)));
    assert!(!map_deeper_than(&map, 3));
}

#[test]
fn map_deeper_than_true_for_any_nonempty_map_at_limit_zero() {
    // Kills the `!map.is_empty()` -> `map.is_empty()` mutant: at
    // `limit == 0`, a single scalar field already sits one level too
    // deep, regardless of its own content.
    let mut map = Map::new();
    map.insert("x".to_string(), json!(1));
    assert!(map_deeper_than(&map, 0));
}

#[test]
fn map_deeper_than_false_for_an_empty_map_at_limit_zero() {
    assert!(!map_deeper_than(&Map::new(), 0));
}

#[test]
fn map_deeper_than_recurses_with_incrementing_not_constant_depth() {
    // Kills the `depth + 1` -> `depth * 1` mutant in `map_deeper_than`'s
    // `Value::Object` recursion arm: without the increment, the leaf at
    // depth 2 would be (wrongly) checked at depth 1, never exceeding a
    // limit of 1.
    let mut inner = Map::new();
    inner.insert("y".to_string(), json!(1));
    let mut map = Map::new();
    map.insert("x".to_string(), Value::Object(inner));
    assert!(map_deeper_than(&map, 1));
}

#[test]
fn deeper_than_handles_a_deeply_nested_value_without_crashing() {
    // Guards `deeper_than` itself against being vulnerable to the very
    // depth it is measuring — RECURSION_OVERFLOW_DEPTH (see its doc)
    // reuses this file's own established native-recursion-overflow
    // threshold. Built iteratively (a flat loop, not recursion); the
    // whole test runs on `run_on_a_large_stack` so it is genuinely
    // dropped afterwards instead of leaked.
    run_on_a_large_stack(|| {
        let value = nested_array(RECURSION_OVERFLOW_DEPTH, json!(1));
        assert!(deeper_than(&value, DEFAULT_MAX_DEPTH));
    });
}

// --- A shallow finding carrying a deep VALUE
// (dictionary_item_added/removed/type_changes/values_changed) must
// error cleanly via check_value_depth, never clone the deep value. ---

#[test]
fn added_value_deeper_than_max_depth_errors_cleanly_instead_of_cloning_it() {
    let deep = nested_array(20, json!(1));
    let mut b = Map::new();
    b.insert("x".to_string(), deep);

    let err = diff_with_max_depth(&json!({}), &Value::Object(b), 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['x']".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn removed_value_deeper_than_max_depth_errors_cleanly_instead_of_cloning_it() {
    let deep = nested_array(20, json!(1));
    let mut a = Map::new();
    a.insert("x".to_string(), deep);

    let err = diff_with_max_depth(&Value::Object(a), &json!({}), 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['x']".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn threshold_collapse_rejects_a_deep_side_cleanly_instead_of_cloning_it_first() {
    // Zero key overlap so this hits the threshold_to_diff_deeper collapse
    // branch (not the granular add/remove path); "deep" pushes `a`'s own
    // nesting to 11 (1 for the key itself + the array's own depth 10),
    // past a max_depth of 5. If the collapse cloned `a`/`b` into the
    // report before checking depth, this would either wrongly succeed
    // (the depth check running against the wrong value) or (on a
    // sufficiently deep input) crash outright rather than returning a
    // clean error.
    let mut a = Map::new();
    a.insert("a".to_string(), json!(1));
    a.insert("deep".to_string(), nested_array(10, json!(1)));

    let err = diff_with_max_depth(&Value::Object(a), &json!({"x": 1, "y": 2}), 5).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root".to_string(),
            max_depth: 5,
        }
    );
}

#[test]
fn removed_value_at_root_is_checked_against_its_own_plus_one_depth_not_the_parents() {
    // Found by mutation testing: pins the exact boundary the removed-key
    // sink's `depth + 1` check needs (a `depth + 1` -> `depth` mutant
    // survives without this test), which
    // `removed_value_deeper_than_max_depth_...` above can't catch on its
    // own.
    let deep = nested_array(10, json!(1)); // one past the correct budget of 9
    let mut a = Map::new();
    a.insert("x".to_string(), deep);

    let err = diff_with_max_depth(&Value::Object(a), &json!({}), 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['x']".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn type_changed_value_deeper_than_max_depth_errors_cleanly_instead_of_cloning_it() {
    let deep = nested_array(20, json!(1));
    let mut a = Map::new();
    a.insert("x".to_string(), deep);
    let mut b = Map::new();
    b.insert("x".to_string(), json!(5));

    let err = diff_with_max_depth(&Value::Object(a), &Value::Object(b), 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['x']".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn type_changed_deep_value_on_the_new_side_alone_errors_cleanly() {
    // Round-2 only ever put the deep value on the old/a side; this
    // covers type_change_report's *second* check_value_depth call
    // (the `b` side) specifically — a is trivially shallow so only the
    // b-side check can be what trips here.
    let deep = nested_array(11, json!(1)); // one past the root (depth 0) budget of 10
    let err = diff_with_max_depth(&json!(5), &deep, 10).unwrap_err();
    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn scalar_diff_rejects_a_value_whose_own_nesting_exceeds_max_depth_on_the_old_side() {
    // scalar_diff's real callers only ever pass scalars (inherently
    // depth 0), so there is no way to reach this guard through the
    // public diff/diff_with_max_depth API today — it is exercised
    // directly here as a defensive unit test of the internal
    // contract (see scalar_diff's doc). depth 20 can't overflow
    // anything on its own, so it is dropped normally.
    let deep = nested_array(20, json!(1));
    let err = scalar_diff(&[], false, &deep, &json!(1), 0, 10).unwrap_err();
    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn scalar_diff_rejects_a_value_whose_own_nesting_exceeds_max_depth_on_the_new_side() {
    // Covers scalar_diff's *second* check_value_depth call (the `b`
    // side): a is trivially shallow so only the b-side check can be
    // what trips here.
    let deep = nested_array(20, json!(1));
    let err = scalar_diff(&[], false, &json!(1), &deep, 0, 10).unwrap_err();
    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root".to_string(),
            max_depth: 10,
        }
    );
}

// --- Compounding-depth regression: a value reached through a shallow
// finding must not be allowed its own full max_depth on top of the path
// depth already consumed to reach it. ---

#[test]
fn value_depth_full_budget_at_a_shallow_finding_is_accepted() {
    // At a root-level finding (path depth 0, via type_change_report),
    // the value gets the FULL max_depth budget: exactly max_depth deep
    // is accepted. Also doubles as the to_json_value-on-a-max-legal-
    // report regression: nothing here should fail to serialize.
    let deep = nested_array(10, json!(1)); // == max_depth
    let report = diff_with_max_depth(&deep, &json!(5), 10).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "list", "new_type": "int",
            "old_value": deep, "new_value": 5,
        }}})
    );
}

#[test]
fn value_depth_one_past_full_budget_at_a_shallow_finding_errors() {
    let deep = nested_array(11, json!(1)); // == max_depth + 1
    let err = diff_with_max_depth(&deep, &json!(5), 10).unwrap_err();
    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn value_depth_reduced_budget_at_a_deep_finding_is_accepted_at_the_reduced_bound() {
    // The finding ("x" added under "p") sits at path depth 2, so its
    // budget is max_depth(10) - 2 = 8, not the full 10. A value of
    // exactly that reduced depth is still accepted.
    let deep = nested_array(8, json!(1));
    let a = json!({"p": {}});
    let mut inner_b = Map::new();
    inner_b.insert("x".to_string(), deep.clone());
    let b = json!({"p": Value::Object(inner_b)});

    let report = diff_with_max_depth(&a, &b, 10).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_added": {"root['p']['x']": deep}})
    );
}

#[test]
fn value_depth_one_past_reduced_budget_at_a_deep_finding_errors() {
    // Same shape as above, but one level deeper than the reduced
    // budget (9 > 8) — this is exactly the compounding case: a flat
    // (path-depth-unaware) check against max_depth(10) would have
    // wrongly accepted this, since 9 <= 10.
    let deep = nested_array(9, json!(1));
    let a = json!({"p": {}});
    let mut inner_b = Map::new();
    inner_b.insert("x".to_string(), deep);
    let b = json!({"p": Value::Object(inner_b)});

    let err = diff_with_max_depth(&a, &b, 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['p']['x']".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn many_dict_siblings_with_nested_findings_report_correct_paths_at_scale() {
    // Regression for the shared path-buffer refactor: object_diff now
    // pushes/pops one shared `Vec<PathSegment>` instead of cloning the
    // whole path per key. A leaked push (forgetting to pop after a
    // sibling's own recursion) would make every *later* sibling inherit
    // an earlier sibling's stale segment(s) — a leak surfaces starting
    // at the second sibling, so 3 siblings is enough to prove both "the
    // first sibling after a leaky one is wrong" and "the leak keeps
    // compounding into a third", with no discriminating power gained
    // from going wider. Each sibling carries its own one-level-nested
    // `values_changed` finding; every single path is asserted exactly
    // correct with no leakage between siblings.
    const SIBLINGS: usize = 3;

    let mut a = Map::new();
    let mut b = Map::new();
    let mut expected_changes = Map::new();

    for i in 0..SIBLINGS {
        let key = format!("k{i}");

        let mut a_inner = Map::new();
        a_inner.insert("changed".to_string(), json!(i));
        let mut b_inner = Map::new();
        b_inner.insert("changed".to_string(), json!(i + 1000));

        a.insert(key.clone(), Value::Object(a_inner));
        b.insert(key.clone(), Value::Object(b_inner));

        expected_changes.insert(
            format!("root['{key}']['changed']"),
            json!({"new_value": i + 1000, "old_value": i}),
        );
    }

    let report = diff(&Value::Object(a), &Value::Object(b)).unwrap();

    let mut expected = Map::new();
    expected.insert(
        "values_changed".to_string(),
        Value::Object(expected_changes),
    );
    assert_eq!(report.to_json_value(), Value::Object(expected));
}

#[test]
fn many_array_siblings_with_nested_findings_report_correct_paths_at_scale() {
    // Array counterpart of the dict siblings test above, stressing
    // array_diff's own push/pop of `PathSegment::Index` the same way —
    // see that test's comment for why 3 siblings is enough.
    const SIBLINGS: usize = 3;

    let mut a_items = Vec::new();
    let mut b_items = Vec::new();
    let mut expected_changes = Map::new();

    for i in 0..SIBLINGS {
        let mut a_inner = Map::new();
        a_inner.insert("changed".to_string(), json!(i));
        let mut b_inner = Map::new();
        b_inner.insert("changed".to_string(), json!(i + 1000));

        a_items.push(Value::Object(a_inner));
        b_items.push(Value::Object(b_inner));

        expected_changes.insert(
            format!("root[{i}]['changed']"),
            json!({"new_value": i + 1000, "old_value": i}),
        );
    }

    let report = diff(&Value::Array(a_items), &Value::Array(b_items)).unwrap();

    let mut expected = Map::new();
    expected.insert(
        "values_changed".to_string(),
        Value::Object(expected_changes),
    );
    assert_eq!(report.to_json_value(), Value::Object(expected));
}

#[test]
fn sibling_after_a_deeply_nested_key_gets_its_own_shallow_path_not_the_deep_ones() {
    // After object_diff recurses all the way down "first"'s nested
    // chain (many push/pop pairs unwinding back up the shared buffer),
    // the buffer must be back to exactly its pre-"first" state before
    // "second" pushes its own segment — proving a deep pop-unwind
    // restores the buffer correctly, not just a single-level pop
    // (covered by the siblings tests above).
    let depth = 50;
    let a = json!({
        "first": nested_dict(depth, json!("a")),
        "second": 1,
    });
    let b = json!({
        "first": nested_dict(depth, json!("b")),
        "second": 2,
    });

    let report = diff(&a, &b).unwrap();

    let expected_first_path = format!("root['first']{}", "['k']".repeat(depth));
    let mut expected_changes = Map::new();
    expected_changes.insert(
        expected_first_path,
        json!({"new_value": "b", "old_value": "a"}),
    );
    expected_changes.insert(
        "root['second']".to_string(),
        json!({"new_value": 2, "old_value": 1}),
    );
    let mut expected = Map::new();
    expected.insert(
        "values_changed".to_string(),
        Value::Object(expected_changes),
    );
    assert_eq!(report.to_json_value(), Value::Object(expected));
}

#[test]
fn shallow_finding_with_a_value_past_the_guard_errors_cleanly() {
    // Exercises the shallow-finding-with-a-too-deep-value shape:
    // diff({}, {"x": <deep array>}) at DEFAULT_MAX_DEPTH.
    // `check_value_depth`/`deeper_than` reject a
    // too-deep value by walking at most `max_depth + 1` levels of it
    // before short-circuiting (see `check_value_depth`'s doc), so
    // PAST_DEFAULT_MAX_DEPTH exercises identical behavior to the
    // original 100_000-deep fixture at a fraction of the memory.
    // Runs entirely on the default test thread (no large-stack helper):
    // `nested_array` builds iteratively (a flat loop), diff_with_max_depth
    // never clones this value (check_value_depth rejects it before any
    // clone happens), and the fixture drops normally afterwards — this
    // is itself the proof that the production path needs no special
    // stack.
    let deep = nested_array(PAST_DEFAULT_MAX_DEPTH, json!(1));
    let mut b = Map::new();
    b.insert("x".to_string(), deep);
    let b = Value::Object(b);

    let err = diff_with_max_depth(&json!({}), &b, DEFAULT_MAX_DEPTH).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['x']".to_string(),
            max_depth: DEFAULT_MAX_DEPTH,
        }
    );
}

#[test]
fn compounding_depth_regression_max_depth_20_000_traversal_plus_deep_added_value() {
    // Found during review: a ~19_999-deep traversal (native
    // diff_at/object_diff recursion, forced by an asymmetric key only
    // at the bottom dict, so the top-level equal-inputs fast path can't
    // short-circuit it) reaching a dict whose one extra key carries a
    // 20_000-deep added value, at max_depth = 20_000. Before threading
    // `depth` into check_value_depth, the value was checked against
    // the flat max_depth (20_000) and accepted (its own depth is
    // exactly 20_000, not "deeper than" 20_000), so a `.clone()` of a
    // 20_000-deep value ran on top of a native call stack already
    // ~19_999 diff_at/object_diff frames deep — a real, reproduced
    // SIGABRT (empirically confirmed: reverting just the
    // `.saturating_sub(depth)` to a flat `max_depth` reproduces the
    // crash at this exact shape). After the fix, the value's budget at
    // this position is max_depth.saturating_sub(20_000) = 0, so it is
    // rejected before any clone happens: a clean error, never a crash.
    //
    // Run on `run_on_a_large_stack`: at this max_depth, the *baseline*
    // traversal alone (no deep value at all) already needs more than a
    // default thread's stack, empirically confirmed independent of
    // this bug or its fix — see that helper's doc. The fixture drops
    // normally at the end of this closure, which is safe precisely
    // because it's still running on that same large stack.
    run_on_a_large_stack(|| {
        let depth = 19_999;
        let deep_added = nested_array(20_000, json!(1));
        let a_bottom = Map::new();
        let mut b_bottom = Map::new();
        b_bottom.insert("added".to_string(), deep_added);

        let mut a = Value::Object(a_bottom);
        let mut b = Value::Object(b_bottom);
        for _ in 0..depth {
            let mut a_map = Map::new();
            a_map.insert("k".to_string(), a);
            a = Value::Object(a_map);

            let mut b_map = Map::new();
            b_map.insert("k".to_string(), b);
            b = Value::Object(b_map);
        }

        let err = diff_with_max_depth(&a, &b, 20_000).unwrap_err();

        let expected_path = format!("root{}['added']", "['k']".repeat(depth));
        assert_eq!(
            err,
            Error::MaxDepthExceeded {
                path: expected_path,
                max_depth: 20_000,
            }
        );
    });
}

#[test]
fn threshold_collapse_rejects_a_deep_side_on_a_constrained_stack_instead_of_crashing() {
    // Guards the `threshold_to_diff_deeper` collapse:
    // the collapse clones the whole `a`/`b` dict into a finding,
    // so cloning before checking depth would hand an attacker-controlled,
    // `RECURSION_OVERFLOW_DEPTH`-deep value straight to `serde_json::Value`'s
    // natively recursive `Clone` with no bound in place yet. Run on a
    // DELIBERATELY CONSTRAINED 8 MiB stack (not `run_on_a_large_stack`):
    // empirically, reverting `object_diff`'s check-before-clone ordering
    // reproduces a real SIGABRT at exactly this depth/stack combination,
    // while `map_deeper_than`'s iterative (heap-stack, not native-stack)
    // check clears it cleanly at the same size — a much larger stack (256
    // MiB, tried while developing this test) masks the bug entirely, since
    // `serde_json::Value::clone`'s per-frame cost is small enough that even
    // the buggy ordering survives on a generous stack; 8 MiB is the
    // smallest size found where the two orderings genuinely diverge.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // Build the deep side as a COMPACT value ITERATIVELY (a flat
            // loop, no native recursion) and call the real compact engine
            // directly: the whole point is that the engine's iterative
            // `map_deeper_than` guard rejects it on this constrained stack
            // *before* any recursive clone. Going through the serde `From`
            // bridge (the other tests' shim) would itself recurse here and
            // defeat the test, so this one bypasses it.
            let deep = {
                let mut value = CValue::from(json!(1));
                for _ in 0..RECURSION_OVERFLOW_DEPTH {
                    value = CValue::Array(vec![value].into_boxed_slice());
                }
                value
            };
            let a = CValue::Object(CObject::from_pairs(vec![
                (
                    ObjectKey::Str(std::sync::Arc::from("p")),
                    CValue::from(json!(1)),
                ),
                (
                    ObjectKey::Str(std::sync::Arc::from("q")),
                    CValue::from(json!(2)),
                ),
            ]));
            let b = CValue::Object(CObject::from_pairs(vec![
                (
                    ObjectKey::Str(std::sync::Arc::from("r")),
                    CValue::from(json!(3)),
                ),
                (ObjectKey::Str(std::sync::Arc::from("deep")), deep),
            ]));

            let err = super::diff_with_max_depth(&a, &b, 1).unwrap_err();
            assert_eq!(
                err,
                Error::MaxDepthExceeded {
                    path: "root".to_string(),
                    max_depth: 1,
                }
            );
        })
        .expect("failed to spawn constrained-stack test thread")
        .join()
        .expect("constrained-stack test thread panicked (crashed or asserted wrong)");
}

#[test]
fn unequal_structure_deeper_than_default_max_depth_errors_via_diff() {
    // depth == DEFAULT_MAX_DEPTH + 1 is exactly one past the bound.
    let a = nested_dict(DEFAULT_MAX_DEPTH + 1, json!("a"));
    let b = nested_dict(DEFAULT_MAX_DEPTH + 1, json!("b"));

    let err = diff(&a, &b).unwrap_err();

    let expected_path = format!("root{}", "['k']".repeat(DEFAULT_MAX_DEPTH + 1));
    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: expected_path,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    );
}

#[test]
fn structure_exactly_at_configured_max_depth_diffs_successfully() {
    let a = nested_dict(3, json!("a"));
    let b = nested_dict(3, json!("b"));

    let report = diff_with_max_depth(&a, &b, 3).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {
            "root['k']['k']['k']": {"new_value": "b", "old_value": "a"}
        }})
    );
}

/// `object_diff_mixed`'s shared-key recursion steps `depth` by exactly one.
#[test]
fn mixed_dict_shared_key_recursion_steps_depth_by_one_not_by_multiplication() {
    let a = CValue::Object(CObject::from_pairs(vec![(
        ObjectKey::Other(Box::new(CValue::from(json!(1)))),
        cv(&json!("a")),
    )]));
    let b = CValue::Object(CObject::from_pairs(vec![(
        ObjectKey::Other(Box::new(CValue::from(json!(1)))),
        cv(&json!("b")),
    )]));

    let err = super::diff_with_max_depth(&a, &b, 0).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root[1]".to_string(),
            max_depth: 0,
        }
    );
}

/// `object_diff_mixed`'s removed-key (`only_a`) sink checks the removed
/// value against its own path depth plus one, not the parent's.
#[test]
fn mixed_dict_removed_key_value_is_checked_against_its_own_plus_one_depth() {
    let a = CValue::Object(CObject::from_pairs(vec![(
        ObjectKey::Other(Box::new(CValue::from(json!(5)))),
        cv(&nested_array(10, json!(1))), // one past the correct budget of 9
    )]));
    let b = CValue::Object(CObject::from_pairs(vec![]));

    let err = super::diff_with_max_depth(&a, &b, 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root[5]".to_string(),
            max_depth: 10,
        }
    );
}

/// [`mixed_dict_removed_key_value_is_checked_against_its_own_plus_one_depth`]'s
/// twin for `object_diff_mixed`'s added-key (`only_b`) sink.
#[test]
fn mixed_dict_added_key_value_is_checked_against_its_own_plus_one_depth() {
    let a = CValue::Object(CObject::from_pairs(vec![]));
    let b = CValue::Object(CObject::from_pairs(vec![(
        ObjectKey::Other(Box::new(CValue::from(json!(5)))),
        cv(&nested_array(10, json!(1))), // one past the correct budget of 9
    )]));

    let err = super::diff_with_max_depth(&a, &b, 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root[5]".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn structure_one_level_past_configured_max_depth_errors() {
    let a = nested_dict(4, json!("a"));
    let b = nested_dict(4, json!("b"));

    let err = diff_with_max_depth(&a, &b, 3).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['k']['k']['k']['k']".to_string(),
            max_depth: 3,
        }
    );
}

#[test]
fn equal_inputs_deeper_than_a_tiny_configured_max_depth_still_succeed() {
    // The equal-inputs-of-any-depth guarantee holds even when max_depth
    // is far smaller than the actual nesting depth.
    let value = nested_dict(50, json!("leaf"));
    let report = diff_with_max_depth(&value, &value.clone(), 1).unwrap();
    assert!(report.is_empty());
}

#[test]
fn equal_inputs_containing_a_null_leaf_use_the_equality_fast_path_even_past_max_depth() {
    // Found by mutation testing: no equal-inputs-of-any-depth test above
    // used a Null leaf, so a mutant special-casing `Value::Null` in the
    // equality fast path survived undetected.
    let value = nested_dict(50, Value::Null);
    let report = diff_with_max_depth(&value, &value.clone(), 1).unwrap();
    assert!(report.is_empty());
}

#[test]
fn equal_inputs_containing_a_bool_leaf_use_the_equality_fast_path_even_past_max_depth() {
    // Same test gap as the Null case above, for `values_equal`'s
    // `(Value::Bool(x), Value::Bool(y))` match arm.
    let value = nested_dict(50, json!(true));
    let report = diff_with_max_depth(&value, &value.clone(), 1).unwrap();
    assert!(report.is_empty());
}

#[test]
fn equal_subtree_nested_under_an_unrelated_shallow_change_still_hits_the_bound() {
    // Documents the accepted edge case from diff_with_max_depth's doc:
    // the equal-inputs-of-any-depth guarantee only checks the *whole*
    // top-level pair once. The top-level inputs here differ (an
    // unrelated "shallow" key), so that check does not fire, and the
    // "deep" key's subtree — identical on both sides, but past
    // max_depth — still recurses natively and trips the bound. This is
    // still *safe* (a clean error, not a crash); an iterative rewrite
    // would remove this limitation entirely.
    let deep_equal_a = nested_dict(50, json!("same"));
    let deep_equal_b = nested_dict(50, json!("same"));
    let mut a = Map::new();
    a.insert("shallow".to_string(), json!(1));
    a.insert("deep".to_string(), deep_equal_a);
    let mut b = Map::new();
    b.insert("shallow".to_string(), json!(2));
    b.insert("deep".to_string(), deep_equal_b);

    let err = diff_with_max_depth(&Value::Object(a), &Value::Object(b), 1).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['deep']['k']".to_string(),
            max_depth: 1,
        }
    );
}

#[test]
fn values_equal_true_for_equal_objects_regardless_of_key_order() {
    assert!(values_equal(
        &json!({"a": 1, "b": 2}),
        &json!({"b": 2, "a": 1})
    ));
}

#[test]
fn values_equal_false_for_objects_with_different_key_sets() {
    assert!(!values_equal(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
}

#[test]
fn values_equal_false_for_arrays_of_different_length() {
    assert!(!values_equal(&json!([1, 2]), &json!([1, 2, 3])));
}

#[test]
fn values_equal_false_for_different_json_variants() {
    assert!(!values_equal(&json!(1), &json!("1")));
}

#[test]
fn values_equal_true_for_deeply_nested_mixed_structure() {
    let value = json!({"a": [1, {"b": 2, "c": [3, 4]}], "d": "x"});
    assert!(values_equal(&value, &value.clone()));
}

#[test]
fn values_equal_false_when_mixed_structure_differs_at_a_leaf() {
    let a = json!({"a": [1, {"b": 2}]});
    let b = json!({"a": [1, {"b": 3}]});
    assert!(!values_equal(&a, &b));
}

/// `values_equal(x, y)` must agree with `diff(x, y).is_empty()` on the
/// tricky numeric matrix already covered by the individual `diff` tests
/// above: int/float type mismatches, exact float equality (incl.
/// signed zero), and int equality across `i64`/`u64` representations
/// (incl. values that only have one of the two representations).
#[test]
fn values_equal_agrees_with_diff_emptiness_across_numeric_matrix() {
    let i64_max_as_u64 = u64::try_from(i64::MAX).expect("i64::MAX fits in u64");
    let cases: Vec<(Value, Value)> = vec![
        (json!(1), json!(1)),
        (json!(1), json!(2)),
        (json!(1), json!(1.0)),
        (json!(1.5), json!(1.5)),
        (json!(1.5), json!(2.5)),
        (json!(0.0), json!(-0.0)),
        (
            Value::Number(Number::from(9_000_000_000_000_000_000u64)),
            Value::Number(Number::from(9_000_000_000_000_000_000i64)),
        ),
        (
            Value::Number(Number::from(u64::MAX)),
            Value::Number(Number::from(u64::MAX)),
        ),
        (
            Value::Number(Number::from(u64::MAX)),
            Value::Number(Number::from(-1i64)),
        ),
        (
            Value::Number(Number::from(i64::MAX)),
            Value::Number(Number::from(i64_max_as_u64 + 1)),
        ),
    ];

    for (a, b) in cases {
        let diff_is_empty = diff(&a, &b).unwrap().is_empty();
        assert_eq!(
            values_equal(&a, &b),
            diff_is_empty,
            "values_equal disagreed with diff emptiness for a={a:?} b={b:?}"
        );
    }
}

// --- List (JSON array) diffing -------------------------------------

#[test]
fn empty_lists_are_empty() {
    let report = diff(&json!([]), &json!([])).unwrap();
    assert!(report.is_empty());
}

#[test]
fn equal_nested_lists_are_empty() {
    let report = diff(&json!([[1, 2], [3, 4]]), &json!([[1, 2], [3, 4]])).unwrap();
    assert!(report.is_empty());
}

#[test]
fn same_length_lists_with_a_type_changed_element_report_it_at_the_right_index() {
    let report = diff(&json!([1, "a", 3]), &json!([1, 2, 3])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root[1]": {
            "old_type": "str", "new_type": "int",
            "old_value": "a", "new_value": 2,
        }}})
    );
}

#[test]
fn shorter_a_reports_bs_tail_as_iterable_item_added_at_absolute_indices() {
    let report = diff(&json!([1, 2]), &json!([1, 2, 3, 4])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_added": {"root[2]": 3, "root[3]": 4}})
    );
}

#[test]
fn shorter_b_reports_as_tail_as_iterable_item_removed_at_absolute_indices() {
    let report = diff(&json!([1, 2, 3, 4]), &json!([1, 2])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_removed": {"root[2]": 3, "root[3]": 4}})
    );
}

#[test]
fn mixed_list_diff_reports_changed_indices_and_added_tail_exactly() {
    // Index 0 same, index 1 changed, index 2 type-changed, then b has a
    // two-element surplus tail.
    let a = json!([1, 2, 3]);
    let b = json!([1, 20, "3", 4, 5]);

    let report = diff(&a, &b).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({
            "values_changed": {"root[1]": {"new_value": 20, "old_value": 2}},
            "type_changes": {"root[2]": {
                "old_type": "int", "new_type": "str",
                "old_value": 3, "new_value": "3",
            }},
            "iterable_item_added": {"root[3]": 4, "root[4]": 5},
        })
    );
}

#[test]
fn nested_list_in_list_reports_deep_index_path() {
    let report = diff(&json!([[1, 2]]), &json!([[1, 3]])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root[0][1]": {"new_value": 3, "old_value": 2}}})
    );
}

#[test]
fn list_inside_dict_recurses_with_correct_path() {
    let report = diff(&json!({"a": [1, 2]}), &json!({"a": [1, 3]})).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root['a'][1]": {"new_value": 3, "old_value": 2}}})
    );
}

#[test]
fn dict_inside_list_recurses_with_correct_path() {
    let report = diff(&json!([{"a": 1}]), &json!([{"a": 2}])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root[0]['a']": {"new_value": 2, "old_value": 1}}})
    );
}

#[test]
fn type_change_at_an_index_from_dict_to_scalar() {
    let report = diff(&json!([{"a": 1}]), &json!([5])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root[0]": {
            "old_type": "dict", "new_type": "int",
            "old_value": {"a": 1}, "new_value": 5,
        }}})
    );
}

#[test]
fn type_change_at_an_index_from_scalar_to_list() {
    let report = diff(&json!([5]), &json!([[1, 2]])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root[0]": {
            "old_type": "int", "new_type": "list",
            "old_value": 5, "new_value": [1, 2],
        }}})
    );
}

#[test]
fn int_vs_float_single_element_list_matches_via_lcs_python_equality() {
    // This used to assert `type_changes` here,
    // matching this engine's own (and every *other*) numeric-comparison
    // rule (see `int_vs_float_is_always_type_change_even_when_numerically_equal`
    // and the sibling test below, both still `type_changes`). Real
    // `DeepDiff` diverges specifically on the LCS list-matching path:
    // `[1]` and `[1.0]` both qualify for basic-hashable list matching
    // (see `crate::lcs::all_basic_scalars`), Python's `==` treats `1`
    // and `1.0` as equal, and a `difflib` `'equal'` opcode is never
    // diffed further — so real `DeepDiff` reports this pair as
    // *completely empty*, confirmed against `deepdiff==9.1.0`. See
    // `crate::diff`'s "List diffing" module doc and `crate::lcs`'s doc
    // for the full write-up.
    let report = diff(&json!([1]), &json!([1.0])).unwrap();
    assert!(report.is_empty());
}

#[test]
fn int_vs_float_at_an_index_is_still_a_type_change_outside_a_hashable_only_list() {
    // The ordinary int/float type-change rule still holds whenever the
    // LCS path does not apply: a sibling dict element disqualifies the
    // whole list from hashable-list matching (`crate::lcs::all_basic_scalars`
    // is false), falling back to `positional_array_diff` — confirmed
    // against real `DeepDiff`, which applies the identical
    // disqualification rule (a dict is never a "basic hashable" type).
    let report = diff(&json!([1, {"k": 1}]), &json!([1.0, {"k": 1}])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root[0]": {
            "old_type": "int", "new_type": "float",
            "old_value": 1, "new_value": 1.0,
        }}})
    );
}

#[test]
fn i64_and_u64_same_value_at_an_index_are_equal() {
    let a = json!([Value::Number(Number::from(9_000_000_000_000_000_000u64))]);
    let b = json!([Value::Number(Number::from(9_000_000_000_000_000_000i64))]);
    let report = diff(&a, &b).unwrap();
    assert!(report.is_empty());
}

// --- Depth guard: array recursion and iterable-sink clones must
// respect the same combined path-depth-plus-value-depth budget as
// object_diff (see check_value_depth's and diff_with_max_depth's doc).
// ---

#[test]
fn deeply_nested_unequal_list_at_the_bottom_hits_the_depth_bound() {
    // depth == DEFAULT_MAX_DEPTH + 1 is exactly one past the bound, same
    // shape as unequal_structure_deeper_than_default_max_depth_errors_via_diff
    // but for arrays instead of dicts.
    //
    // This runs on the test thread's own default (~2 MiB) stack, with no
    // large-stack accommodation: `array_diff` keeps its scalar-only-list
    // candidate computation (two extra `Report` locals) in a separate
    // non-recursive helper (`lcs_or_positional_array_diff`, see
    // `array_diff`'s "Stack-footprint note"), so those locals don't inflate
    // every frame of this native list-of-list recursion in a debug build,
    // and the bound is reached cleanly. The dedicated
    // `array_diff_at_depth_512_on_a_default_stack_completes_without_crashing`
    // test below pins the exact bound this test's shape only implies.
    let a = nested_array(DEFAULT_MAX_DEPTH + 1, json!(1));
    let b = nested_array(DEFAULT_MAX_DEPTH + 1, json!(2));

    let err = diff(&a, &b).unwrap_err();

    let expected_path = format!("root{}", "[0]".repeat(DEFAULT_MAX_DEPTH + 1));
    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: expected_path,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    );
}

#[test]
fn array_diff_at_depth_512_on_a_default_stack_completes_without_crashing() {
    // Runs the diff call directly on
    // this test's own (default-size, ~2 MiB) thread — no
    // `run_on_a_large_stack` for the diff call itself, which is
    // exactly the point (a library consumer calling `diff`/
    // `diff_with_max_depth` on an ordinary thread must not crash
    // before `Error::MaxDepthExceeded` can fire). Pure nested-list
    // traversal (every level dispatches through `array_diff`, the hot
    // path the fix targeted) one level past `DEFAULT_MAX_DEPTH`
    // completes with a clean `Err`, not a `SIGABRT`.
    let a = nested_array(DEFAULT_MAX_DEPTH + 1, json!("left"));
    let b = nested_array(DEFAULT_MAX_DEPTH + 1, json!("right"));

    let err = diff(&a, &b).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: format!("root{}", "[0]".repeat(DEFAULT_MAX_DEPTH + 1)),
            max_depth: DEFAULT_MAX_DEPTH,
        }
    );
}

#[test]
fn added_tail_element_deeper_than_the_remaining_budget_errors_cleanly() {
    // Mirrors added_value_deeper_than_max_depth_errors_cleanly_instead_of_cloning_it
    // (object_diff's dictionary_item_added sink), but for array_diff's
    // iterable_item_added sink: a surplus tail element one level deeper
    // than the max_depth budget must be rejected before it is cloned.
    let deep = nested_array(11, json!(1)); // one past the root (depth 0) budget of 10
    let a = json!([]);
    let b = json!([deep]);

    let err = diff_with_max_depth(&a, &b, 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root[0]".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn removed_tail_element_deeper_than_the_remaining_budget_errors_cleanly() {
    // Mirrors the added case above, but for array_diff's
    // iterable_item_removed sink.
    let deep = nested_array(11, json!(1)); // one past the root (depth 0) budget of 10
    let a = json!([deep]);
    let b = json!([]);

    let err = diff_with_max_depth(&a, &b, 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root[0]".to_string(),
            max_depth: 10,
        }
    );
}

// --- Review follow-up: the two surplus-tail check_value_depth
// calls in array_diff use `depth + 1`, i.e. the *finding's own* path
// depth, not the parent list's `depth`. Every test above only ever put
// the surplus tail at the root (depth 0), so mutating `depth + 1` to
// `depth` there is unobservable (root's `depth` is 0, so `depth + 1`
// and a hypothetical off-by-one both still land on a value budget of
// `max_depth`, or the mutation just shifts which of two equal-looking
// numbers is used). These pin the +1 at a non-root path (a list nested
// inside a dict, so the surplus finding sits at path depth 2), mirroring
// value_depth_reduced_budget_at_a_deep_finding_is_accepted_at_the_reduced_bound
// for object_diff's own leaf sinks. ---

#[test]
fn added_tail_element_at_a_non_root_path_is_accepted_at_the_reduced_budget() {
    // "root['p'][1]" is at path depth 2, so its budget is
    // max_depth(10) - 2 = 8, not the full 10. A value of exactly that
    // reduced depth is still accepted.
    let deep = nested_array(8, json!(1));
    let a = json!({"p": [1]});
    let b = json!({"p": [1, deep.clone()]});

    let report = diff_with_max_depth(&a, &b, 10).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_added": {"root['p'][1]": deep}})
    );
}

#[test]
fn added_tail_element_at_a_non_root_path_one_past_the_reduced_budget_errors() {
    // Same shape as above, but one level deeper than the reduced budget
    // (9 > 8) — a flat (path-depth-unaware) check against max_depth(10)
    // would have wrongly accepted this, since 9 <= 10.
    let deep = nested_array(9, json!(1));
    let a = json!({"p": [1]});
    let b = json!({"p": [1, deep]});

    let err = diff_with_max_depth(&a, &b, 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['p'][1]".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn removed_tail_element_at_a_non_root_path_is_accepted_at_the_reduced_budget() {
    // Mirrors the added case above, but for array_diff's
    // iterable_item_removed sink.
    let deep = nested_array(8, json!(1));
    let a = json!({"p": [1, deep.clone()]});
    let b = json!({"p": [1]});

    let report = diff_with_max_depth(&a, &b, 10).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_removed": {"root['p'][1]": deep}})
    );
}

#[test]
fn removed_tail_element_at_a_non_root_path_one_past_the_reduced_budget_errors() {
    let deep = nested_array(9, json!(1));
    let a = json!({"p": [1, deep]});
    let b = json!({"p": [1]});

    let err = diff_with_max_depth(&a, &b, 10).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root['p'][1]".to_string(),
            max_depth: 10,
        }
    );
}

#[test]
fn equal_deeply_nested_lists_at_a_tiny_max_depth_still_succeed() {
    // Same equal-inputs-of-any-depth guarantee as
    // equal_inputs_deeper_than_a_tiny_configured_max_depth_still_succeed,
    // for arrays instead of dicts.
    let value = nested_array(50, json!("leaf"));
    let report = diff_with_max_depth(&value, &value.clone(), 1).unwrap();
    assert!(report.is_empty());
}

// --- The tie-break's positional_array_diff call must itself respect the
// traversal depth bound. ---

#[test]
fn lcs_tie_break_positional_candidate_hits_the_depth_bound_at_the_root() {
    // `[1.0, 2]` vs `[2, 1]` is exactly the tie-break example this
    // module's doc walks through: the LCS pass finds 2 findings (an
    // add + a remove, via `1.0`/`1`'s cross-type match), which is
    // `> 1`, so `array_diff` also computes `positional_array_diff` to
    // compare counts. That candidate recurses into same-index pairs
    // through `diff_at` at `depth + 1` exactly like any other list —
    // at `max_depth == 0` (this pair sits at the *root*, depth 0),
    // `depth + 1 == 1` trips the bound before the tie-break can even
    // finish computing, and the whole `diff` call must surface that
    // `Error::MaxDepthExceeded` cleanly rather than silently returning
    // the (uncompared, so unreliable) LCS result instead.
    let a = json!([1.0, 2]);
    let b = json!([2, 1]);

    let err = diff_with_max_depth(&a, &b, 0).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root[0]".to_string(),
            max_depth: 0,
        }
    );
}

#[test]
fn lcs_replace_pair_itself_hits_the_depth_bound_not_via_the_positional_fallback() {
    // Distinct from `lcs_tie_break_positional_candidate_hits_the_depth_bound_at_the_root`
    // above: that test's input (`[1.0, 2]` vs `[2, 1]`) opcodes are
    // insert+equal+delete — no `Replace` opcode at all — so its error
    // comes from `positional_array_diff`'s own `diff_at` recursion
    // (the tie-break candidate), never from `insert_lcs_pair_finding`'s
    // own `check_traversal_depth` call. `[1, 2, 3]` vs `[1, 5, 3]`
    // opcodes are equal+replace+equal (a single, aligned `Replace`
    // pair at index 1, real DeepDiff: `values_changed` at `root[1]`) —
    // `lcs_report.finding_count()` here is exactly `1`, so
    // `array_diff` never even reaches the tie-break/positional branch;
    // the error can only come from `insert_lcs_pair_finding` itself.
    let a = json!([1, 2, 3]);
    let b = json!([1, 5, 3]);

    let err = diff_with_max_depth(&a, &b, 0).unwrap_err();

    assert_eq!(
        err,
        Error::MaxDepthExceeded {
            path: "root[1]".to_string(),
            max_depth: 0,
        }
    );
}

#[test]
fn lcs_tie_break_positional_candidate_succeeds_at_a_sufficient_max_depth() {
    // Same shape as the test above, one level of budget higher (1
    // instead of 0) — the tie-break's positional candidate now fits,
    // and (per this module's doc) wins the tie, producing the plain
    // index-aligned `type_changes`/`values_changed` result rather than
    // the LCS add/remove one.
    let a = json!([1.0, 2]);
    let b = json!([2, 1]);

    let report = diff_with_max_depth(&a, &b, 1).unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({
            "type_changes": {"root[0]": {
                "old_type": "float", "new_type": "int",
                "old_value": 1.0, "new_value": 2,
            }},
            "values_changed": {"root[1]": {"new_value": 1, "old_value": 2}},
        })
    );
}

// --- tuples -------------------------------------------------------------
//
// Every expected value in this section was confirmed against a real
// `deepdiff==9.1.0` probe (`DeepDiff(t1, t2, verbose_level=2).to_json()`),
// not derived from this engine's own behavior.

#[test]
fn tuple_vs_tuple_diffs_positionally_like_a_list() {
    let report = super::diff(
        &ctup(&[json!(1), json!(2), json!(3)]),
        &ctup(&[json!(1), json!(2), json!(4)]),
    )
    .unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root[2]": {"new_value": 4, "old_value": 3}}})
    );
}

#[test]
fn equal_tuples_are_empty() {
    let report = super::diff(&ctup(&[json!(1), json!(2)]), &ctup(&[json!(1), json!(2)])).unwrap();
    assert!(report.is_empty());
}

#[test]
fn tuple_vs_list_at_root_is_a_type_change() {
    let report = super::diff(&ctup(&[json!(1), json!(2)]), &cv(&json!([1, 2]))).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "tuple", "new_type": "list",
            "old_value": [1, 2], "new_value": [1, 2],
        }}})
    );
}

#[test]
fn list_vs_tuple_nested_in_a_dict_is_a_type_change() {
    let mut a = Map::new();
    a.insert("a".to_string(), json!([1, 2]));
    let b = CValue::Object(CObject::from_pairs(vec![(
        ObjectKey::Str(std::sync::Arc::from("a")),
        ctup(&[json!(1), json!(2)]),
    )]));
    let report = super::diff(&cv(&Value::Object(a)), &b).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root['a']": {
            "old_type": "list", "new_type": "tuple",
            "old_value": [1, 2], "new_value": [1, 2],
        }}})
    );
}

#[test]
fn empty_tuple_vs_empty_list_is_a_type_change() {
    let report = super::diff(&ctup(&[]), &cv(&json!([]))).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "tuple", "new_type": "list",
            "old_value": [], "new_value": [],
        }}})
    );
}

#[test]
fn tuple_growing_reports_the_surplus_tail_as_added() {
    let report = super::diff(
        &ctup(&[json!(1), json!(2)]),
        &ctup(&[json!(1), json!(2), json!(3)]),
    )
    .unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_added": {"root[2]": 3}})
    );
}

#[test]
fn tuple_of_dicts_recurses_into_the_dict() {
    let report = super::diff(&ctup(&[json!({"a": 1})]), &ctup(&[json!({"a": 2})])).unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root[0]['a']": {"new_value": 2, "old_value": 1}}})
    );
}

#[test]
fn tuple_of_scalars_uses_the_same_lcs_match_a_list_would() {
    // The exact pair (and expected output) of the
    // `list_lcs_new_path_after_index_drift` golden case, as a tuple: real
    // DeepDiff runs its difflib match inside a tuple exactly as inside a
    // list.
    let report = super::diff(
        &ctup(&[
            json!(0),
            json!(0),
            json!(3),
            json!(3),
            json!(0),
            json!(1),
            json!(0),
        ]),
        &ctup(&[json!(4), json!(3), json!(0), json!(4)]),
    )
    .unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({
            "values_changed": {
                "root[0]": {"new_value": 4, "old_value": 0},
                "root[5]": {"new_value": 4, "old_value": 1, "new_path": "root[3]"},
            },
            "iterable_item_removed": {"root[1]": 0, "root[2]": 3, "root[6]": 0},
        })
    );
}

#[test]
fn a_tuple_element_disqualifies_a_list_from_lcs_matching() {
    // `[(1, 2), 3]` vs `[3]`: a tuple is not a basic-hashable scalar, so the
    // list falls back to index-aligned comparison (a type change at index 0
    // plus a removed tail), exactly like a nested list or dict element.
    let report = super::diff(
        &CValue::Array(vec![ctup(&[json!(1), json!(2)]), cv(&json!(3))].into_boxed_slice()),
        &cv(&json!([3])),
    )
    .unwrap();
    assert_eq!(
        report.to_json_value(),
        json!({
            "type_changes": {"root[0]": {
                "old_type": "tuple", "new_type": "int",
                "old_value": [1, 2], "new_value": 3,
            }},
            "iterable_item_removed": {"root[1]": 3},
        })
    );
}

#[test]
fn python_type_name_of_a_tuple_is_tuple() {
    assert_eq!(super::python_type_name(&ctup(&[json!(1)])), "tuple");
}

#[test]
fn a_tuple_never_equals_a_list_with_the_same_items() {
    assert!(!super::values_equal(
        &ctup(&[json!(1), json!(2)]),
        &cv(&json!([1, 2]))
    ));
    assert!(super::values_equal(
        &ctup(&[json!(1), json!(2)]),
        &ctup(&[json!(1), json!(2)])
    ));
}

#[test]
fn tuple_nesting_counts_toward_the_value_depth_guard() {
    // Tuples nest exactly like arrays, so `deeper_than` must see through
    // them: `((1,),)` is depth 2.
    let nested = CValue::Tuple(vec![ctup(&[json!(1)])].into_boxed_slice());
    assert!(super::dispatch::deeper_than(&nested, 1));
    assert!(!super::dispatch::deeper_than(&nested, 2));
}

#[test]
fn a_dict_value_that_is_a_too_deep_tuple_errors_instead_of_being_cloned() {
    // The clone-into-report guard applies to a tuple-shaped added value the
    // same as to an array-shaped one.
    let mut deep = CValue::Null;
    for _ in 0..5 {
        deep = CValue::Tuple(vec![deep].into_boxed_slice());
    }
    let b = CValue::Object(CObject::from_pairs(vec![(
        ObjectKey::Str(std::sync::Arc::from("a")),
        deep,
    )]));
    let error = super::diff_with_max_depth(&cv(&json!({})), &b, 4).unwrap_err();
    assert!(matches!(error, Error::MaxDepthExceeded { .. }));
}

// --- datetimes and dates -------------------------------------------------

/// A one-key dict wrapping `value`, so a datetime case can be exercised at
/// depth as well as at the root.
fn wrapped(value: CValue) -> CValue {
    let mut builder = crate::value::Builder::new();
    builder.object(vec![("t".to_string(), value)])
}

#[test]
fn changed_datetimes_report_the_pair_normalized_to_utc() {
    let report = super::diff(
        &cdt_at(2024, 1, 1, 10, 0, 0, 0, Some(-5 * 3600)),
        &cdt_at(2024, 1, 2, 10, 0, 0, 0, Some(-5 * 3600)),
    )
    .unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({"values_changed": {"root": {
            "new_value": "2024-01-02T15:00:00+00:00",
            "old_value": "2024-01-01T15:00:00+00:00",
        }}})
    );
}

#[test]
fn datetimes_at_the_same_instant_report_nothing_however_they_are_written() {
    for new in [
        cdt_at(2024, 1, 1, 10, 0, 0, 0, Some(0)),
        cdt_at(2024, 1, 1, 12, 0, 0, 0, Some(2 * 3600)),
        cdt_at(2024, 1, 1, 5, 0, 0, 0, Some(-5 * 3600)),
    ] {
        let old = cdt_at(2024, 1, 1, 10, 0, 0, 0, None);

        assert!(super::diff(&old, &new).unwrap().is_empty());
        assert!(
            super::diff(&wrapped(old), &wrapped(new))
                .unwrap()
                .is_empty(),
            "the same rule must hold one level down"
        );
    }
}

#[test]
fn a_datetime_outside_values_changed_keeps_its_raw_rendering() {
    let added = super::diff(
        &cv(&json!({})),
        &wrapped(cdt_at(2024, 1, 1, 10, 0, 0, 0, Some(1830))),
    )
    .unwrap();
    let type_changed = super::diff(
        &cdt_at(2024, 1, 1, 0, 0, 0, 0, None),
        &cv(&json!("2024-01-01")),
    )
    .unwrap();

    assert_eq!(
        added.to_json_value(),
        json!({"dictionary_item_added": {"root['t']": "2024-01-01T10:00:00+00:30:30"}})
    );
    assert_eq!(
        type_changed.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "datetime",
            "new_type": "str",
            "old_value": "2024-01-01T00:00:00",
            "new_value": "2024-01-01",
        }}})
    );
}

#[test]
fn dates_compare_by_value_and_are_never_a_datetime() {
    let changed = super::diff(&cdate(2024, 1, 1), &cdate(2024, 1, 2)).unwrap();
    let retyped = super::diff(&cdate(2024, 1, 1), &cdt(2024, 1, 1, None)).unwrap();

    assert_eq!(
        changed.to_json_value(),
        json!({"values_changed": {"root": {
            "new_value": "2024-01-02", "old_value": "2024-01-01",
        }}})
    );
    assert_eq!(
        retyped.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "date",
            "new_type": "datetime",
            "old_value": "2024-01-01",
            "new_value": "2024-01-01T00:00:00",
        }}})
    );
    assert!(
        super::diff(&cdate(2024, 1, 1), &cdate(2024, 1, 1))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn calendar_type_names_are_datetime_and_date() {
    assert_eq!(super::python_type_name(&cdt(2024, 1, 1, None)), "datetime");
    assert_eq!(super::python_type_name(&cdate(2024, 1, 1)), "date");
}

#[test]
fn a_list_of_datetimes_takes_the_difflib_path() {
    // `datetime` is in DeepDiff's `basic_types`, so a shifted list of them
    // aligns by LCS: one delete plus one insert, not three values_changed.
    let report = super::diff(
        &CValue::Array(
            vec![
                cdt(2024, 1, 1, None),
                cdt(2024, 1, 2, None),
                cdt(2024, 1, 3, None),
            ]
            .into_boxed_slice(),
        ),
        &CValue::Array(
            vec![
                cdt(2024, 1, 2, None),
                cdt(2024, 1, 3, None),
                cdt(2024, 1, 4, None),
            ]
            .into_boxed_slice(),
        ),
    )
    .unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({
            "iterable_item_removed": {"root[0]": "2024-01-01T00:00:00"},
            "iterable_item_added": {"root[2]": "2024-01-04T00:00:00"},
        })
    );
}

#[test]
fn a_naive_and_aware_pair_matched_by_difflib_replace_reports_nothing_at_a_drifted_index() {
    // Two lists that differ *only* in a naive/aware pair at one instant are
    // equal by this engine's own rules, so `diff_with_options`'s top-level
    // fast path would answer them before `array_diff` ran at all. The
    // unmatched leading element here keeps the lists genuinely unequal, so
    // the difflib 'replace' opcode really is what decides the datetime pair,
    // and the finding it must *not* record is the whole point.
    let report = super::diff(
        &CValue::Array(
            vec![
                cv(&json!("x")),
                cv(&json!("y")),
                cdt_at(2024, 1, 1, 10, 0, 0, 0, None),
            ]
            .into_boxed_slice(),
        ),
        &CValue::Array(
            vec![
                cv(&json!("y")),
                cdt_at(2024, 1, 1, 12, 0, 0, 0, Some(2 * 3600)),
            ]
            .into_boxed_slice(),
        ),
    )
    .unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_removed": {"root[0]": "x"}})
    );
}

#[test]
fn a_datetime_pair_matched_by_difflib_replace_is_normalized_and_keeps_new_path() {
    // Same 'replace' path, with the instants genuinely different and an
    // earlier delete drifting the new-side index, which attaches `new_path`.
    let report = super::diff(
        &CValue::Array(
            vec![
                cv(&json!("x")),
                cv(&json!("y")),
                cdt_at(2024, 1, 1, 10, 0, 0, 0, None),
            ]
            .into_boxed_slice(),
        ),
        &CValue::Array(
            vec![
                cv(&json!("y")),
                cdt_at(2024, 1, 2, 12, 0, 0, 0, Some(2 * 3600)),
            ]
            .into_boxed_slice(),
        ),
    )
    .unwrap();

    assert_eq!(
        report.to_json_value(),
        json!({
            "values_changed": {"root[2]": {
                "new_value": "2024-01-02T10:00:00+00:00",
                "old_value": "2024-01-01T10:00:00+00:00",
                "new_path": "root[1]",
            }},
            "iterable_item_removed": {"root[0]": "x"},
        })
    );
}

#[test]
fn comparing_two_datetimes_that_cannot_normalize_to_utc_is_an_error_naming_the_path() {
    // `9999-12-31T23:00-01:00` is a real Python datetime whose UTC wall
    // clock is `10000-01-01T00:00`, outside the year range a datetime can
    // hold. Real `DeepDiff` raises `OverflowError: date value out of range`
    // on exactly this comparison, so there is no report to match.
    let extreme = cdt_at(9999, 12, 31, 23, 0, 0, 0, Some(-3600));
    let ordinary = cdt_at(2024, 1, 1, 10, 0, 0, 0, None);

    let error = super::diff(&wrapped(extreme.clone()), &wrapped(ordinary))
        .expect_err("an unnormalizable pair has no report");

    assert_eq!(
        error,
        Error::DateTimeOutOfRange {
            path: "root['t']".to_string(),
        }
    );
}

#[test]
fn an_unnormalizable_datetime_still_diffs_when_it_is_never_compared_to_another_one() {
    // On the ordered path real `DeepDiff` normalizes only inside
    // `_diff_datetime`, so the same value added, or type-changed against a
    // non-datetime, reports its raw rendering rather than raising in both
    // tools — verified live. The `ignore_order` path differs; see the test
    // below.
    let extreme = cdt_at(9999, 12, 31, 23, 0, 0, 0, Some(-3600));

    let added = super::diff(&cv(&json!({})), &wrapped(extreme.clone())).unwrap();
    let retyped = super::diff(&extreme, &cv(&json!(5))).unwrap();

    assert_eq!(
        added.to_json_value(),
        json!({"dictionary_item_added": {"root['t']": "9999-12-31T23:00:00-01:00"}})
    );
    assert_eq!(
        retyped.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "datetime",
            "new_type": "int",
            "old_value": "9999-12-31T23:00:00-01:00",
            "new_value": 5,
        }}})
    );
}

#[test]
fn an_unnormalizable_datetime_under_ignore_order_is_reported_raw() {
    // `ignore_order` hashes every item, and this engine's hash key is the
    // instant, which every datetime has — so an extreme aware value that has
    // no UTC form is still hashed, paired, and reported raw.
    //
    // Real `DeepDiff` diverges here, and deliberately so: its
    // `deephash.py::_prep_datetime` runs `datetime_normalize` on every
    // datetime it hashes, so it raises `OverflowError: date value out of
    // range` for both cases below — the added one and the pure shuffle that
    // has no finding at all. Reproducing a crash is not a semantic worth
    // matching (see `tests/golden/README.md`), so onix keeps the
    // deterministic report.
    let extreme = cdt_at(9999, 12, 31, 23, 0, 0, 0, Some(-3600));
    let opts = super::DiffOptions {
        ignore_order: true,
        ..super::DiffOptions::default()
    };
    let list = |items: Vec<CValue>| CValue::Array(items.into_boxed_slice());

    let added = super::diff_with_options(
        &list(vec![cv(&json!(1))]),
        &list(vec![cv(&json!(1)), extreme.clone()]),
        &opts,
    )
    .unwrap();
    let shuffled = super::diff_with_options(
        &list(vec![cv(&json!(1)), extreme.clone()]),
        &list(vec![extreme, cv(&json!(1))]),
        &opts,
    )
    .unwrap();

    assert_eq!(
        added.to_json_value(),
        json!({"iterable_item_added": {"root[1]": "9999-12-31T23:00:00-01:00"}})
    );
    assert!(shuffled.is_empty());
}

// --- sets ----------------------------------------------------------------

/// Diffs two already-compact values, for the set cases whose inputs no
/// JSON literal can express.
fn diff_compact(a: &CValue, b: &CValue) -> Result<Report, Error> {
    super::diff(a, b)
}

#[test]
fn set_reports_added_and_removed_items() {
    let report = diff_compact(
        &cset(&[json!(1), json!(2), json!(3)]),
        &cset(&[json!(2), json!(3), json!(4)]),
    )
    .expect("shallow sets diff cleanly");

    assert_eq!(
        report.to_json_value(),
        json!({"set_item_added": ["root[4]"], "set_item_removed": ["root[1]"]})
    );
}

#[test]
fn a_set_with_the_same_items_in_another_order_reports_nothing() {
    let report = diff_compact(&cset(&[json!(1), json!(2)]), &cset(&[json!(2), json!(1)]))
        .expect("shallow sets diff cleanly");

    assert!(report.is_empty());
}

#[test]
fn set_findings_carry_the_sets_own_path() {
    let mut builder = crate::value::Builder::new();
    let a = builder.object(vec![("a".to_string(), cset(&[json!(1), json!(2)]))]);
    let b = builder.object(vec![("a".to_string(), cset(&[json!(1)]))]);

    assert_eq!(
        diff_compact(&a, &b)
            .expect("shallow sets diff cleanly")
            .to_json_value(),
        json!({"set_item_removed": ["root['a'][2]"]})
    );
}

/// Membership is `DeepHash` identity, which is type-aware for bare numbers
/// — the same pairs that are one Python object inside a real set are still
/// two distinct items here.
#[test]
fn set_membership_is_type_aware_for_numbers() {
    for (a, b, removed, added) in [
        (json!(1), json!(1.0), "root[1]", "root[1.0]"),
        (json!(true), json!(1), "root[True]", "root[1]"),
    ] {
        assert_eq!(
            diff_compact(
                &cset(std::slice::from_ref(&a)),
                &cset(std::slice::from_ref(&b)),
            )
            .expect("shallow sets diff cleanly")
            .to_json_value(),
            json!({"set_item_added": [added], "set_item_removed": [removed]}),
            "{a} vs {b}"
        );
    }
}

/// The other side of that rule: only a *bare* number is type-wrapped, so a
/// container that Python's `==` calls equal is one member — the tuple pair
/// wrapping the very same numbers reports nothing at all (golden:
/// `set_tuple_item_python_equality`).
#[test]
fn a_container_set_item_is_compared_by_python_equality() {
    let tuples = diff_compact(
        &CValue::Set(SetItems::new(vec![ctup(&[json!(1)])])),
        &CValue::Set(SetItems::new(vec![ctup(&[json!(1.0)])])),
    )
    .expect("shallow sets diff cleanly");
    assert!(tuples.is_empty());

    let frozensets = diff_compact(
        &CValue::Set(SetItems::new(vec![cfrozen(&[json!(1)])])),
        &CValue::Set(SetItems::new(vec![cfrozen(&[json!(1.0)])])),
    )
    .expect("shallow sets diff cleanly");
    assert!(frozensets.is_empty());
}

#[test]
fn a_set_and_a_frozenset_are_a_type_change() {
    let report =
        diff_compact(&cset(&[json!(1)]), &cfrozen(&[json!(1)])).expect("shallow sets diff cleanly");

    assert_eq!(
        report.to_json_value(),
        json!({"type_changes": {"root": {
            "old_type": "set", "new_type": "frozenset",
            "old_value": [1], "new_value": [1],
        }}})
    );
}

#[test]
fn python_type_name_names_both_set_kinds() {
    assert_eq!(super::python_type_name(&cset(&[])), "set");
    assert_eq!(super::python_type_name(&cfrozen(&[])), "frozenset");
}

/// A set element disqualifies its list from the difflib match, the way a
/// nested list or a tuple element does (golden:
/// `set_element_disqualifies_list_lcs`).
#[test]
fn a_set_element_disqualifies_a_list_from_the_lcs_match() {
    assert!(!crate::lcs::all_basic_scalars(&[cset(&[json!(1)])]));
    assert!(!crate::lcs::all_basic_scalars(&[cfrozen(&[json!(1)])]));
}

/// Membership is an identity, not a cache lookup, so the report does not
/// depend on which member of a Python-equality class was seen first — where
/// real `DeepDiff`'s does. Both orders give two removals and one addition.
#[test]
fn a_sets_report_does_not_depend_on_its_member_order() {
    let float_tuple = CValue::Tuple(Box::new([ctup(&[json!(1.0)])]));
    let int_tuple = CValue::Tuple(Box::new([ctup(&[json!(1)]), cv(&json!(0))]));
    let b = CValue::Set(SetItems::new(vec![CValue::Tuple(Box::new([ctup(&[
        json!(1),
        json!(1),
    ])]))]));

    let expected = json!({
        "set_item_added": ["root[((1, 1),)]"],
        "set_item_removed": ["root[((1,), 0)]", "root[((1.0,),)]"],
    });
    for members in [
        vec![float_tuple.clone(), int_tuple.clone()],
        vec![int_tuple, float_tuple],
    ] {
        assert_eq!(
            diff_compact(&CValue::Set(SetItems::new(members)), &b)
                .expect("shallow sets diff cleanly")
                .to_json_value(),
            expected
        );
    }
}

/// `list(a_set) == some_list` is answered by membership, so an
/// `ignore_order` set-versus-list pairing stays a type change whichever
/// order either side happens to hold.
#[test]
fn a_set_versus_list_type_change_does_not_depend_on_order() {
    let opts = super::DiffOptions {
        ignore_order: true,
        ..super::DiffOptions::default()
    };
    let listed = |value: CValue| CValue::Array(Box::new([value]));
    let members = || vec![cv(&json!(75)), cv(&json!(47))];
    let mut reversed = members();
    reversed.reverse();

    for set_members in [members(), reversed] {
        for list in [json!([75, 47]), json!([47, 75])] {
            let report = super::diff_with_options(
                &listed(CValue::Set(SetItems::new(set_members.clone()))),
                &listed(cv(&list)),
                &opts,
            )
            .expect("shallow values diff cleanly");
            assert_eq!(
                report.to_json_value()["type_changes"]["root[0]"]["new_type"],
                json!("list"),
                "for set {set_members:?} against {list}"
            );
        }
    }
}

/// A member that survived the conversion boundary as a duplicate is dropped
/// once, at construction, so it can never become a second finding at the
/// same report path.
#[test]
fn a_duplicate_set_member_is_reported_once() {
    let with_duplicate = CValue::Set(SetItems::new(vec![
        cv(&json!("x")),
        cv(&json!("x")),
        cv(&json!("y")),
    ]));

    assert_eq!(
        diff_compact(&with_duplicate, &cset(&[json!("y")]))
            .expect("shallow sets diff cleanly")
            .to_json_value(),
        json!({"set_item_removed": ["root['x']"]})
    );
}

/// A set item's own nesting shares the traversal's `max_depth` budget, so
/// a finding carrying an over-deep item is a clean error rather than a
/// native-stack clone.
#[test]
fn a_too_deep_set_item_reports_max_depth_exceeded() {
    // A set holding one tuple nested `nesting` levels deep.
    let deep = |nesting: usize| {
        let mut value = ctup(&[json!(1)]);
        for _ in 1..nesting {
            value = CValue::Tuple(Box::new([value]));
        }
        CValue::Set(SetItems::new(vec![value]))
    };

    // The item sits one level under the set, so at `max_depth == 3` its own
    // nesting may reach 2 but not 3.
    assert!(super::diff_with_max_depth(&deep(2), &cset(&[json!(9)]), 3).is_ok());
    assert!(matches!(
        super::diff_with_max_depth(&deep(3), &cset(&[json!(9)]), 3),
        Err(Error::MaxDepthExceeded { .. })
    ));
}
