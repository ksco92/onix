//! Property tests for the diff engine: reflexivity, that every reported path resolves in its
//! expected side, per-category membership rules, and determinism. Dict keys exclude quotes,
//! backslashes and control characters so `parse_path` finds each closing quote. All
//! properties share one fixed seed and case count.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestCaseError};
use serde_json::{Number, Value};

use onix_core::diff;

/// Recursion bound for generated values, far below [`onix_core::DEFAULT_MAX_DEPTH`].
const MAX_GENERATED_DEPTH: u32 = 6;

/// Rough cap on the total number of nodes `prop_recursive` aims for per
/// generated value.
const MAX_GENERATED_NODES: u32 = 64;

/// Cap on elements per generated array/object, keeping cases small and fast.
const MAX_COLLECTION_BRANCH: u32 = 6;

/// Cap on proptest cases per property, keeping `make check` fast.
const PROPTEST_CASES: u32 = 256;

/// Fixed seed so every run explores the identical sequence of generated
/// values (see this module's doc).
const PROPTEST_SEED: u64 = 0x0451_1745_0999_0001;

/// The shared [`Config`] every property in this file runs under.
fn config() -> Config {
    Config {
        cases: PROPTEST_CASES,
        rng_seed: RngSeed::Fixed(PROPTEST_SEED),
        ..Config::default()
    }
}

/// A dict key without quotes, backslashes or control characters (see this module's doc).
fn arb_key() -> impl Strategy<Value = String> {
    r#"[^'"\\\x00-\x1f\x7f]{0,8}"#
}

/// A single JSON leaf: null, bool, int (spanning both `i64` and
/// `u64`-only-representable ranges), finite float, or unicode string.
fn arb_json_leaf() -> impl Strategy<Value = Value> {
    let u64_only_range = (i64::MAX as u64 + 1)..=u64::MAX;
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(Number::from(n))),
        u64_only_range.prop_map(|n| Value::Number(Number::from(n))),
        any::<f64>()
            .prop_filter("JSON has no NaN/Infinity", |f| f.is_finite())
            .prop_map(|f| Value::Number(Number::from_f64(f).expect("filtered to finite above"))),
        ".*".prop_map(Value::String),
    ]
}

/// An arbitrary JSON-shaped value: a leaf, or an array/object recursing into
/// more of the same, bounded to [`MAX_GENERATED_DEPTH`].
fn arb_json_value() -> impl Strategy<Value = Value> {
    arb_json_leaf().prop_recursive(
        MAX_GENERATED_DEPTH,
        MAX_GENERATED_NODES,
        MAX_COLLECTION_BRANCH,
        |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..MAX_COLLECTION_BRANCH as usize)
                    .prop_map(Value::Array),
                prop::collection::hash_map(arb_key(), inner, 0..MAX_COLLECTION_BRANCH as usize)
                    .prop_map(|map| Value::Object(map.into_iter().collect())),
            ]
        },
    )
}

/// One parsed step of a DeepDiff-style path: a dict key or a list index.
#[derive(Debug)]
enum PathStep {
    Key(String),
    Index(usize),
}

/// Parses a DeepDiff-style path string (e.g. `root['a'][0]`) back into [`PathStep`]s.
/// Handles only the quote-free keys [`arb_key`] produces.
fn parse_path(path: &str) -> Vec<PathStep> {
    let rest = path
        .strip_prefix("root")
        .expect("every rendered path starts with \"root\"");

    let bytes = rest.as_bytes();
    let mut steps = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        assert_eq!(bytes[i], b'[', "malformed path segment in {path:?}");
        if bytes[i + 1] == b'\'' {
            let start = i + 2;
            let end = start
                + rest[start..]
                    .find('\'')
                    .expect("arb_key never generates a single quote, so a closing one exists");
            steps.push(PathStep::Key(rest[start..end].to_string()));
            i = end + 2; // skip over "']"
        } else {
            let start = i + 1;
            let end = start
                + rest[start..]
                    .find(']')
                    .expect("index segment has a closing ]");
            let index: usize = rest[start..end].parse().expect("index segment is numeric");
            steps.push(PathStep::Index(index));
            i = end + 1;
        }
    }
    steps
}

/// Navigates `value` by `steps`, returning `None` as soon as a step doesn't
/// resolve (wrong container kind, missing key, or out-of-range index).
fn resolve<'v>(value: &'v Value, steps: &[PathStep]) -> Option<&'v Value> {
    let mut current = value;
    for step in steps {
        current = match step {
            PathStep::Key(key) => current.as_object()?.get(key)?,
            PathStep::Index(index) => current.as_array()?.get(*index)?,
        };
    }
    Some(current)
}

/// Returns `true` if the DeepDiff-style path `path` resolves to a node in
/// `value`.
fn resolves_in(value: &Value, path: &str) -> bool {
    resolve(value, &parse_path(path)).is_some()
}

/// The parent path (everything but the last step) and the last step of
/// `path`, e.g. `root['a']['b']` splits into `root['a']` and `Key("b")`.
fn split_last_step(path: &str) -> (Vec<PathStep>, PathStep) {
    let mut steps = parse_path(path);
    let last = steps
        .pop()
        .expect("a report path always has at least one step");
    (steps, last)
}

/// A JSON value's DeepDiff-reported "type" category, coarse enough to prove (in)equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Null,
    Bool,
    Int,
    Float,
    Str,
    List,
    Dict,
}

fn kind_of(value: &Value) -> Kind {
    match value {
        Value::Null => Kind::Null,
        Value::Bool(_) => Kind::Bool,
        Value::Number(n) if n.is_f64() => Kind::Float,
        Value::Number(_) => Kind::Int,
        Value::String(_) => Kind::Str,
        Value::Array(_) => Kind::List,
        Value::Object(_) => Kind::Dict,
    }
}

/// Runs `diff` and renders the report; the depth bound makes `Err` a bug.
fn diff_ok(a: &Value, b: &Value) -> Value {
    diff(
        &onix_core::Value::from(a.clone()),
        &onix_core::Value::from(b.clone()),
    )
    .expect("generated depth is far below DEFAULT_MAX_DEPTH")
    .to_json_value()
}

/// The membership-delta invariant shared by `dictionary_item_added`'s and
/// `dictionary_item_removed`'s per-path assertions: `key` exists in
/// `has_key`'s parent object at `parent_steps`, and not in `lacks_key`'s
/// (either that side's parent isn't even an object, or it is but lacks the
/// key).
fn assert_membership_delta(
    has_key: &Value,
    lacks_key: &Value,
    parent_steps: &[PathStep],
    key: &str,
    path: &str,
) -> Result<(), TestCaseError> {
    let parent_with = resolve(has_key, parent_steps).and_then(Value::as_object);
    prop_assert!(
        parent_with.is_some_and(|map| map.contains_key(key)),
        "{key} at {path} does not exist in the expected parent object"
    );

    let parent_without = resolve(lacks_key, parent_steps).and_then(Value::as_object);
    prop_assert!(
        !parent_without.is_some_and(|map| map.contains_key(key)),
        "{key} at {path} unexpectedly exists in the other side's parent object"
    );
    Ok(())
}

/// `index` resolves in `present_side`'s parent array to `recorded_value`; shared by the
/// `iterable_item_added`/`removed` properties. No tail-surplus check: LCS matching can
/// report either in range on both sides.
fn assert_index_resolves_to_recorded_value(
    present_side: &Value,
    parent_steps: &[PathStep],
    index: usize,
    recorded_value: &Value,
    path: &str,
) -> Result<(), TestCaseError> {
    let actual = resolve(present_side, parent_steps)
        .and_then(Value::as_array)
        .and_then(|items| items.get(index));
    prop_assert!(
        actual.is_some(),
        "index {index} at {path} is not in range for the expected parent array"
    );
    prop_assert_eq!(
        actual,
        Some(recorded_value),
        "recorded value at {} does not match the actual value at that index",
        path
    );
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn diff_of_a_value_with_itself_is_always_empty(value in arb_json_value()) {
        let report = diff_ok(&value, &value);
        prop_assert_eq!(report, serde_json::json!({}));
    }

    /// An identical `shared` value alongside an unrelated difference reports nothing for it.
    #[test]
    fn shared_identical_substructure_alongside_an_unrelated_difference_reports_nothing_for_it(
        shared in arb_json_value(),
    ) {
        let a = serde_json::json!({"shared": shared.clone(), "differs": true});
        let b = serde_json::json!({"shared": shared, "differs": false});

        let report = diff_ok(&a, &b);
        let categories = report.as_object().expect("to_json_value always returns an object");
        for entries in categories.values() {
            let entries = entries.as_object().expect("every category is a path-keyed map");
            for path in entries.keys() {
                prop_assert!(
                    !path.starts_with("root['shared']"),
                    "unexpected finding under the identical shared substructure at {path}"
                );
            }
        }
    }

    /// Every reported path resolves in its expected side: added/iterable-added in `b`,
    /// removed/iterable-removed in `a`, and values_changed/type_changes in `a`, with the
    /// `b`-side location (`new_path` when present, else `path`) in `b`.
    ///
    /// `path` itself need not resolve in `b`: `new_path` records the drift of an LCS-paired index.
    #[test]
    fn every_reported_path_resolves_in_its_expected_side(
        a in arb_json_value(),
        b in arb_json_value(),
    ) {
        let report = diff_ok(&a, &b);
        let categories = report.as_object().expect("to_json_value always returns an object");

        for (category, entries) in categories {
            let entries = entries.as_object().expect("every category is a path-keyed map");
            for (path, entry) in entries {
                match category.as_str() {
                    "dictionary_item_added" | "iterable_item_added" => {
                        prop_assert!(
                            resolves_in(&b, path),
                            "{category} path {path} does not resolve in b"
                        );
                    }
                    "dictionary_item_removed" | "iterable_item_removed" => {
                        prop_assert!(
                            resolves_in(&a, path),
                            "{category} path {path} does not resolve in a"
                        );
                    }
                    "values_changed" | "type_changes" => {
                        prop_assert!(
                            resolves_in(&a, path),
                            "{category} path {path} does not resolve in a"
                        );
                        let b_side_path = entry
                            .get("new_path")
                            .and_then(Value::as_str)
                            .unwrap_or(path.as_str());
                        prop_assert!(
                            resolves_in(&b, b_side_path),
                            "{category} path {path}'s b-side location \
                             {b_side_path} does not resolve in b"
                        );
                    }
                    other => panic!("unexpected report category {other}"),
                }
            }
        }
    }

    #[test]
    fn dictionary_item_added_keys_exist_in_b_parent_and_not_in_a_parent(
        a in arb_json_value(),
        b in arb_json_value(),
    ) {
        let report = diff_ok(&a, &b);
        if let Some(added) = report.get("dictionary_item_added").and_then(Value::as_object) {
            for path in added.keys() {
                let (parent_steps, last) = split_last_step(path);
                let PathStep::Key(key) = last else {
                    panic!("dictionary_item_added path {path} does not end in a key");
                };
                assert_membership_delta(&b, &a, &parent_steps, &key, path)?;
            }
        }
    }

    #[test]
    fn dictionary_item_removed_keys_exist_in_a_parent_and_not_in_b_parent(
        a in arb_json_value(),
        b in arb_json_value(),
    ) {
        let report = diff_ok(&a, &b);
        if let Some(removed) = report.get("dictionary_item_removed").and_then(Value::as_object) {
            for path in removed.keys() {
                let (parent_steps, last) = split_last_step(path);
                let PathStep::Key(key) = last else {
                    panic!("dictionary_item_removed path {path} does not end in a key");
                };
                assert_membership_delta(&a, &b, &parent_steps, &key, path)?;
            }
        }
    }

    #[test]
    fn iterable_item_added_indices_resolve_to_the_recorded_value_in_b(
        a in arb_json_value(),
        b in arb_json_value(),
    ) {
        let report = diff_ok(&a, &b);
        if let Some(added) = report.get("iterable_item_added").and_then(Value::as_object) {
            for (path, recorded_value) in added {
                let (parent_steps, last) = split_last_step(path);
                let PathStep::Index(index) = last else {
                    panic!("iterable_item_added path {path} does not end in an index");
                };
                assert_index_resolves_to_recorded_value(
                    &b,
                    &parent_steps,
                    index,
                    recorded_value,
                    path,
                )?;
            }
        }
    }

    #[test]
    fn iterable_item_removed_indices_resolve_to_the_recorded_value_in_a(
        a in arb_json_value(),
        b in arb_json_value(),
    ) {
        let report = diff_ok(&a, &b);
        if let Some(removed) = report.get("iterable_item_removed").and_then(Value::as_object) {
            for (path, recorded_value) in removed {
                let (parent_steps, last) = split_last_step(path);
                let PathStep::Index(index) = last else {
                    panic!("iterable_item_removed path {path} does not end in an index");
                };
                assert_index_resolves_to_recorded_value(
                    &a,
                    &parent_steps,
                    index,
                    recorded_value,
                    path,
                )?;
            }
        }
    }

    /// `type_changes` entries change kind (never int/int, dict/dict, etc).
    #[test]
    fn type_changes_entries_have_different_kinds(
        a in arb_json_value(),
        b in arb_json_value(),
    ) {
        let report = diff_ok(&a, &b);
        if let Some(entries) = report.get("type_changes").and_then(Value::as_object) {
            for entry in entries.values() {
                let old_kind = kind_of(&entry["old_value"]);
                let new_kind = kind_of(&entry["new_value"]);
                prop_assert_ne!(old_kind, new_kind);
            }
        }
    }

    /// `values_changed` entries keep the same kind but differ in value.
    #[test]
    fn values_changed_entries_keep_kind_but_differ_in_value(
        a in arb_json_value(),
        b in arb_json_value(),
    ) {
        let report = diff_ok(&a, &b);
        if let Some(entries) = report.get("values_changed").and_then(Value::as_object) {
            for entry in entries.values() {
                let old_value = &entry["old_value"];
                let new_value = &entry["new_value"];
                prop_assert_eq!(kind_of(old_value), kind_of(new_value));
                prop_assert_ne!(old_value, new_value);
            }
        }
    }

    #[test]
    fn diff_result_is_deterministic_across_repeated_calls(
        a in arb_json_value(),
        b in arb_json_value(),
    ) {
        let first = diff_ok(&a, &b);
        let second = diff_ok(&a, &b);
        prop_assert_eq!(first, second);
    }
}

/// `parse_path`/`resolve` on hand-written examples.
#[test]
fn path_resolver_helper_matches_hand_written_examples() {
    let value = serde_json::json!({"a": [1, {"b c": 2}], "": 3});

    assert_eq!(resolve(&value, &parse_path("root")), Some(&value));
    assert_eq!(resolve(&value, &parse_path("root['a']")), value.get("a"));
    assert_eq!(
        resolve(&value, &parse_path("root['a'][1]['b c']")),
        Some(&Value::from(2))
    );
    assert_eq!(
        resolve(&value, &parse_path("root['']")),
        Some(&Value::from(3))
    );
    assert_eq!(resolve(&value, &parse_path("root['missing']")), None);
    assert_eq!(resolve(&value, &parse_path("root['a'][99]")), None);
    assert_eq!(
        resolve(&value, &parse_path("root['a']['not-an-index']")),
        None
    );
}
