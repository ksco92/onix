use super::{PathSegment, Report, TypeChangeEntry, ValuesChangedEntry};
use serde_json::json;

/// A one-key structural path, e.g. `key_path("a")` for `root['a']`.
fn key_path(name: &str) -> Vec<PathSegment> {
    vec![PathSegment::Key(name.to_string())]
}

/// A one-index structural path, e.g. `index_path(3)` for `root[3]`.
fn index_path(i: usize) -> Vec<PathSegment> {
    vec![PathSegment::Index(i)]
}

#[test]
fn new_report_is_empty() {
    let report = Report::new();
    assert!(report.is_empty());
}

#[test]
fn default_report_is_empty() {
    let report = Report::default();
    assert!(report.is_empty());
}

#[test]
fn empty_report_serializes_to_empty_object() {
    let report = Report::new();
    assert_eq!(report.to_json_value(), json!({}));
}

#[test]
fn values_changed_entry_is_not_empty_and_serializes_exactly() {
    let mut report = Report::new();
    report.insert_values_changed(
        Vec::new(),
        ValuesChangedEntry {
            old_value: json!(1),
            new_value: json!(2),
            new_path: None,
        },
    );

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({
            "values_changed": {
                "root": {
                    "new_value": 2,
                    "old_value": 1,
                }
            }
        })
    );
}

#[test]
fn type_change_entry_is_not_empty_and_serializes_exactly() {
    let mut report = Report::new();
    report.insert_type_change(
        Vec::new(),
        TypeChangeEntry {
            old_type: "int".to_string(),
            new_type: "str".to_string(),
            old_value: json!(1),
            new_value: json!("1"),
            new_path: None,
        },
    );

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({
            "type_changes": {
                "root": {
                    "old_type": "int",
                    "new_type": "str",
                    "old_value": 1,
                    "new_value": "1",
                }
            }
        })
    );
}

#[test]
fn both_categories_present_omits_neither() {
    let mut report = Report::new();
    report.insert_type_change(
        key_path("a"),
        TypeChangeEntry {
            old_type: "int".to_string(),
            new_type: "float".to_string(),
            old_value: json!(1),
            new_value: json!(1.5),
            new_path: None,
        },
    );
    report.insert_values_changed(
        key_path("b"),
        ValuesChangedEntry {
            old_value: json!("x"),
            new_value: json!("y"),
            new_path: None,
        },
    );

    let value = report.to_json_value();
    assert_eq!(value.as_object().unwrap().len(), 2);
    assert!(value.get("type_changes").is_some());
    assert!(value.get("values_changed").is_some());
}

#[test]
fn dictionary_item_added_entry_is_not_empty_and_serializes_to_raw_value() {
    let mut report = Report::new();
    report.insert_dictionary_item_added(key_path("c"), json!(3));

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_added": {"root['c']": 3}})
    );
}

#[test]
fn dictionary_item_removed_entry_is_not_empty_and_serializes_to_raw_value() {
    let mut report = Report::new();
    report.insert_dictionary_item_removed(key_path("b"), json!(2));

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_removed": {"root['b']": 2}})
    );
}

#[test]
fn iterable_item_added_entry_is_not_empty_and_serializes_to_raw_value() {
    let mut report = Report::new();
    report.insert_iterable_item_added(index_path(3), json!("x"));

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_added": {"root[3]": "x"}})
    );
}

#[test]
fn iterable_item_removed_entry_is_not_empty_and_serializes_to_raw_value() {
    let mut report = Report::new();
    report.insert_iterable_item_removed(index_path(2), json!("y"));

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_removed": {"root[2]": "y"}})
    );
}

#[test]
fn all_six_categories_present_omits_none() {
    let mut report = Report::new();
    report.insert_type_change(
        key_path("a"),
        TypeChangeEntry {
            old_type: "int".to_string(),
            new_type: "float".to_string(),
            old_value: json!(1),
            new_value: json!(1.5),
            new_path: None,
        },
    );
    report.insert_values_changed(
        key_path("b"),
        ValuesChangedEntry {
            old_value: json!("x"),
            new_value: json!("y"),
            new_path: None,
        },
    );
    report.insert_dictionary_item_added(key_path("c"), json!(3));
    report.insert_dictionary_item_removed(key_path("d"), json!(4));
    report.insert_iterable_item_added(index_path(0), json!(5));
    report.insert_iterable_item_removed(index_path(1), json!(6));

    let value = report.to_json_value();
    assert_eq!(value.as_object().unwrap().len(), 6);
    assert!(value.get("type_changes").is_some());
    assert!(value.get("values_changed").is_some());
    assert!(value.get("dictionary_item_added").is_some());
    assert!(value.get("dictionary_item_removed").is_some());
    assert!(value.get("iterable_item_added").is_some());
    assert!(value.get("iterable_item_removed").is_some());
}

/// Every category gets its own, mutually-distinct entry count (2..=7,
/// never 0 or 1) so a `+` mutated to `-` or `*` anywhere in
/// [`Report::finding_count`]'s summation is guaranteed to change the
/// total (a shared count, or a `0`/`1` term, could let some `+`/`*`
/// pairs coincide by accident).
#[test]
fn finding_count_sums_every_category_distinctly() {
    let mut report = Report::new();
    for i in 0..2 {
        report.insert_type_change(
            index_path(i),
            TypeChangeEntry {
                old_type: "int".to_string(),
                new_type: "float".to_string(),
                old_value: json!(1),
                new_value: json!(1.5),
                new_path: None,
            },
        );
    }
    for i in 0..3 {
        report.insert_values_changed(
            index_path(i),
            ValuesChangedEntry {
                old_value: json!("x"),
                new_value: json!("y"),
                new_path: None,
            },
        );
    }
    for i in 0..4 {
        report.insert_dictionary_item_added(index_path(i), json!(1));
    }
    for i in 0..5 {
        report.insert_dictionary_item_removed(index_path(i), json!(1));
    }
    for i in 0..6 {
        report.insert_iterable_item_added(index_path(i), json!(1));
    }
    for i in 0..7 {
        report.insert_iterable_item_removed(index_path(i), json!(1));
    }

    assert_eq!(report.finding_count(), 2 + 3 + 4 + 5 + 6 + 7);
}

#[test]
fn merge_combines_findings_from_both_reports() {
    let mut left = Report::new();
    left.insert_dictionary_item_added(key_path("a"), json!(1));

    let mut right = Report::new();
    right.insert_dictionary_item_removed(key_path("b"), json!(2));

    left.merge(right);

    assert_eq!(
        left.to_json_value(),
        json!({
            "dictionary_item_added": {"root['a']": 1},
            "dictionary_item_removed": {"root['b']": 2},
        })
    );
}

#[test]
fn merge_combines_iterable_findings_from_both_reports() {
    let mut left = Report::new();
    left.insert_iterable_item_removed(index_path(3), json!("y"));

    // Both new categories on `other` (not `left`), so `merge` exercises
    // both of its new per-category loop bodies (`other.iterable_item_added`
    // and `other.iterable_item_removed`), not just the outer `for`.
    let mut right = Report::new();
    right.insert_iterable_item_added(index_path(2), json!("x"));
    right.insert_iterable_item_removed(index_path(4), json!("z"));

    left.merge(right);

    assert_eq!(
        left.to_json_value(),
        json!({
            "iterable_item_added": {"root[2]": "x"},
            "iterable_item_removed": {"root[3]": "y", "root[4]": "z"},
        })
    );
}

#[test]
fn merging_two_empty_reports_stays_empty() {
    let mut left = Report::new();
    left.merge(Report::new());
    assert!(left.is_empty());
}

#[test]
fn multiple_paths_in_same_category_are_sorted_by_path() {
    let mut report = Report::new();
    report.insert_values_changed(
        key_path("b"),
        ValuesChangedEntry {
            old_value: json!(1),
            new_value: json!(2),
            new_path: None,
        },
    );
    report.insert_values_changed(
        key_path("a"),
        ValuesChangedEntry {
            old_value: json!(3),
            new_value: json!(4),
            new_path: None,
        },
    );

    let value = report.to_json_value();
    let keys: Vec<&String> = value["values_changed"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys, vec!["root['a']", "root['b']"]);
}

#[test]
fn retag_new_path_swaps_prefix_segment_and_keeps_suffix() {
    let mut report = Report::new();
    report.insert_values_changed(
        vec![PathSegment::Index(0), PathSegment::Key("x".to_string())],
        ValuesChangedEntry {
            old_value: json!(1),
            new_value: json!(2),
            new_path: None,
        },
    );

    report.retag_new_path(0, 3);

    let value = report.to_json_value();
    assert_eq!(
        value,
        json!({"values_changed": {"root[0]['x']": {
            "new_value": 2, "old_value": 1, "new_path": "root[3]['x']",
        }}})
    );
}

#[test]
fn retag_new_path_also_retags_type_changes() {
    let mut report = Report::new();
    report.insert_type_change(
        index_path(0),
        TypeChangeEntry {
            old_type: "int".to_string(),
            new_type: "str".to_string(),
            old_value: json!(1),
            new_value: json!("1"),
            new_path: None,
        },
    );

    report.retag_new_path(0, 5);

    let value = report.to_json_value();
    assert_eq!(value["type_changes"]["root[0]"]["new_path"], "root[5]");
}

#[test]
fn retag_new_path_composes_with_an_already_set_new_path() {
    // Mirrors a doubly-nested drift scenario: an INNER retag already ran
    // (simulating a nested pairing's own index-2 -> index-1 drift), leaving
    // new_path at [Index(0), Index(1)] ("root[0][1]") while the entry's own
    // structural key is still [Index(0), Index(2)] ("root[0][2]").  A
    // subsequent OUTER retag (prefix_depth=0, the list-level index
    // drifting 0 -> 1) must overwrite ONLY that outer segment on TOP of
    // the already-substituted vector, composing to [Index(1), Index(1)]
    // ("root[1][1]") — not leave the stale inner-only "root[0][1]", and
    // not discard the inner substitution either.
    let mut report = Report::new();
    report.insert_values_changed(
        vec![PathSegment::Index(0), PathSegment::Index(2)],
        ValuesChangedEntry {
            old_value: json!(1),
            new_value: json!(2),
            new_path: Some(vec![PathSegment::Index(0), PathSegment::Index(1)]),
        },
    );

    report.retag_new_path(0, 1);

    assert_eq!(
        report.to_json_value()["values_changed"]["root[0][2]"]["new_path"],
        "root[1][1]"
    );
}

/// The genuine bug [`insert_checked`]'s guard exists to catch: the exact
/// same *structural* path inserted twice into one category is always a
/// real engine double-visit, so it still panics in debug builds.
#[test]
#[should_panic(expected = "duplicate report path")]
fn inserting_the_same_structural_path_twice_panics_in_debug() {
    let mut report = Report::new();
    report.insert_values_changed(
        key_path("a"),
        ValuesChangedEntry {
            old_value: json!(1),
            new_value: json!(2),
            new_path: None,
        },
    );
    report.insert_values_changed(
        key_path("a"),
        ValuesChangedEntry {
            old_value: json!(3),
            new_value: json!(4),
            new_path: None,
        },
    );
}

/// The regression this module's rewrite fixes: two *different*
/// structural paths that render to the identical `DeepDiff`-style
/// string (see [`crate::path::quote_key`]'s doc for how a key's own
/// text can produce this) must NOT panic the duplicate-path guard, and
/// must collapse to a single JSON entry at serialization time rather
/// than silently vanishing or corrupting the report.
#[test]
fn two_structural_paths_rendering_identically_collapse_without_panicking() {
    // Same shape as the `tests/golden/path_rendering_collision` regression:
    // a single key whose own text contains `][` next to quote
    // characters renders identically to two nested single-quote-containing
    // keys. Both `k1`/`k2` and `flat_key` must contain a single quote so
    // `quote_key` wraps all three in double quotes (see its doc) -- that
    // shared quote character is what makes the two renderings collide.
    let mut k1_then_k2 = String::new();
    k1_then_k2.push('p');
    k1_then_k2.push('\'');
    let k1 = k1_then_k2.clone();
    let mut k2 = String::new();
    k2.push('q');
    k2.push('\'');

    let mut flat_key = String::new();
    flat_key.push('p');
    flat_key.push('\'');
    flat_key.push('"');
    flat_key.push(']');
    flat_key.push('[');
    flat_key.push('"');
    flat_key.push('q');
    flat_key.push('\'');

    let flat = key_path(&flat_key);
    let nested = vec![PathSegment::Key(k1), PathSegment::Key(k2)];
    assert_eq!(
        crate::path::render_path(&flat),
        crate::path::render_path(&nested),
        "test fixture assumption: these two structural paths must render identically",
    );

    let mut report = Report::new();
    report.insert_values_changed(
        flat,
        ValuesChangedEntry {
            old_value: json!(1),
            new_value: json!(2),
            new_path: None,
        },
    );
    report.insert_values_changed(
        nested,
        ValuesChangedEntry {
            old_value: json!(10),
            new_value: json!(20),
            new_path: None,
        },
    );

    // No panic reaching here is the primary assertion. Exactly one
    // entry survives (not two, not zero, not a malformed JSON tree).
    let value = report.to_json_value();
    let category = value["values_changed"].as_object().unwrap();
    assert_eq!(category.len(), 1);
    assert!(category.contains_key("root[\"p'\"][\"q'\"]"));
}
