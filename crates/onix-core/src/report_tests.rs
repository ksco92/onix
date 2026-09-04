use super::{PathSegment, Report, TypeChangeEntry, ValuesChangedEntry};
use crate::test_support::cv;
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
            old_value: cv(&json!(1)),
            new_value: cv(&json!(2)),
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
            old_value: cv(&json!(1)),
            new_value: cv(&json!("1")),
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
            old_value: cv(&json!(1)),
            new_value: cv(&json!(1.5)),
            new_path: None,
        },
    );
    report.insert_values_changed(
        key_path("b"),
        ValuesChangedEntry {
            old_value: cv(&json!("x")),
            new_value: cv(&json!("y")),
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
    report.insert_dictionary_item_added(key_path("c"), cv(&json!(3)));

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_added": {"root['c']": 3}})
    );
}

#[test]
fn dictionary_item_removed_entry_is_not_empty_and_serializes_to_raw_value() {
    let mut report = Report::new();
    report.insert_dictionary_item_removed(key_path("b"), cv(&json!(2)));

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({"dictionary_item_removed": {"root['b']": 2}})
    );
}

#[test]
fn iterable_item_added_entry_is_not_empty_and_serializes_to_raw_value() {
    let mut report = Report::new();
    report.insert_iterable_item_added(index_path(3), cv(&json!("x")));

    assert!(!report.is_empty());
    assert_eq!(
        report.to_json_value(),
        json!({"iterable_item_added": {"root[3]": "x"}})
    );
}

#[test]
fn iterable_item_removed_entry_is_not_empty_and_serializes_to_raw_value() {
    let mut report = Report::new();
    report.insert_iterable_item_removed(index_path(2), cv(&json!("y")));

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
            old_value: cv(&json!(1)),
            new_value: cv(&json!(1.5)),
            new_path: None,
        },
    );
    report.insert_values_changed(
        key_path("b"),
        ValuesChangedEntry {
            old_value: cv(&json!("x")),
            new_value: cv(&json!("y")),
            new_path: None,
        },
    );
    report.insert_dictionary_item_added(key_path("c"), cv(&json!(3)));
    report.insert_dictionary_item_removed(key_path("d"), cv(&json!(4)));
    report.insert_iterable_item_added(index_path(0), cv(&json!(5)));
    report.insert_iterable_item_removed(index_path(1), cv(&json!(6)));

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
                old_value: cv(&json!(1)),
                new_value: cv(&json!(1.5)),
                new_path: None,
            },
        );
    }
    for i in 0..3 {
        report.insert_values_changed(
            index_path(i),
            ValuesChangedEntry {
                old_value: cv(&json!("x")),
                new_value: cv(&json!("y")),
                new_path: None,
            },
        );
    }
    for i in 0..4 {
        report.insert_dictionary_item_added(index_path(i), cv(&json!(1)));
    }
    for i in 0..5 {
        report.insert_dictionary_item_removed(index_path(i), cv(&json!(1)));
    }
    for i in 0..6 {
        report.insert_iterable_item_added(index_path(i), cv(&json!(1)));
    }
    for i in 0..7 {
        report.insert_iterable_item_removed(index_path(i), cv(&json!(1)));
    }

    assert_eq!(report.finding_count(), 2 + 3 + 4 + 5 + 6 + 7);
}

#[test]
fn merge_combines_findings_from_both_reports() {
    let mut left = Report::new();
    left.insert_dictionary_item_added(key_path("a"), cv(&json!(1)));

    let mut right = Report::new();
    right.insert_dictionary_item_removed(key_path("b"), cv(&json!(2)));

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
    left.insert_iterable_item_removed(index_path(3), cv(&json!("y")));

    // Both new categories on `other` (not `left`), so `merge` exercises
    // both of its new per-category loop bodies (`other.iterable_item_added`
    // and `other.iterable_item_removed`), not just the outer `for`.
    let mut right = Report::new();
    right.insert_iterable_item_added(index_path(2), cv(&json!("x")));
    right.insert_iterable_item_removed(index_path(4), cv(&json!("z")));

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
            old_value: cv(&json!(1)),
            new_value: cv(&json!(2)),
            new_path: None,
        },
    );
    report.insert_values_changed(
        key_path("a"),
        ValuesChangedEntry {
            old_value: cv(&json!(3)),
            new_value: cv(&json!(4)),
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
            old_value: cv(&json!(1)),
            new_value: cv(&json!(2)),
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
            old_value: cv(&json!(1)),
            new_value: cv(&json!("1")),
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
            old_value: cv(&json!(1)),
            new_value: cv(&json!(2)),
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
            old_value: cv(&json!(1)),
            new_value: cv(&json!(2)),
            new_path: None,
        },
    );
    report.insert_values_changed(
        key_path("a"),
        ValuesChangedEntry {
            old_value: cv(&json!(3)),
            new_value: cv(&json!(4)),
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
            old_value: cv(&json!(1)),
            new_value: cv(&json!(2)),
            new_path: None,
        },
    );
    report.insert_values_changed(
        nested,
        ValuesChangedEntry {
            old_value: cv(&json!(10)),
            new_value: cv(&json!(20)),
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

#[test]
fn to_value_preserves_a_tuple_that_to_json_value_flattens_to_an_array() {
    use crate::test_support::ctup;
    use crate::value::Value as CValue;

    let mut report = Report::new();
    report.insert_dictionary_item_added(key_path("s"), ctup(&[json!(1), json!(2)]));
    report.insert_values_changed(
        index_path(0),
        ValuesChangedEntry {
            old_value: ctup(&[json!(1)]),
            new_value: cv(&json!([1])),
            new_path: None,
        },
    );

    // The type-preserving rendering keeps both tuples...
    let rendered = report.to_value();
    let CValue::Object(root) = &rendered else {
        panic!("a report renders to an object");
    };
    let CValue::Object(added) = root.get("dictionary_item_added").expect("category present") else {
        panic!("a category renders to an object");
    };
    assert!(matches!(
        added.get("root['s']").expect("path present"),
        CValue::Tuple(_)
    ));
    let CValue::Object(changed) = root.get("values_changed").expect("category present") else {
        panic!("a category renders to an object");
    };
    let CValue::Object(entry) = changed.get("root[0]").expect("path present") else {
        panic!("an entry renders to an object");
    };
    assert!(matches!(
        entry.get("old_value").expect("old_value present"),
        CValue::Tuple(_)
    ));
    assert!(matches!(
        entry.get("new_value").expect("new_value present"),
        CValue::Array(_)
    ));

    // ...while the JSON rendering shows both as arrays, as DeepDiff's own
    // to_json() does.
    assert_eq!(
        report.to_json_value(),
        json!({
            "dictionary_item_added": {"root['s']": [1, 2]},
            "values_changed": {"root[0]": {"new_value": [1], "old_value": [1]}},
        })
    );
}

#[test]
fn the_two_renderings_agree_on_every_category() {
    use crate::test_support::ctup;

    // One report touching every category, both entry shapes, `new_path`, and
    // a tuple: `to_json_value`'s direct walk must land exactly where
    // `to_value`'s compact rendering does once converted.
    let mut report = Report::new();
    report.insert_values_changed(
        index_path(0),
        ValuesChangedEntry {
            old_value: cv(&json!(1)),
            new_value: cv(&json!(2)),
            new_path: Some(index_path(3)),
        },
    );
    report.insert_type_change(
        key_path("t"),
        TypeChangeEntry {
            old_type: "tuple".to_string(),
            new_type: "list".to_string(),
            old_value: ctup(&[json!(1)]),
            new_value: cv(&json!([1])),
            new_path: Some(key_path("t2")),
        },
    );
    report.insert_dictionary_item_added(key_path("a"), ctup(&[json!(1), json!("x")]));
    report.insert_dictionary_item_removed(key_path("r"), cv(&json!({"k": [1, 2]})));
    report.insert_iterable_item_added(index_path(7), cv(&json!(null)));
    report.insert_iterable_item_removed(index_path(8), ctup(&[]));

    assert_eq!(report.to_json_value(), report.to_value().to_serde_json());
}

// --- set categories ------------------------------------------------------

/// A one-set-item structural path, e.g. `root[1]`.
fn set_item_path(item: &str) -> Vec<PathSegment> {
    vec![PathSegment::SetItem(item.to_string())]
}

#[test]
fn set_categories_serialize_as_arrays_of_path_strings() {
    let mut report = Report::new();
    report.insert_set_item_removed(set_item_path("1"), cv(&json!(1)));
    report.insert_set_item_added(set_item_path("'b'"), cv(&json!("b")));

    let expected = json!({
        "set_item_added": ["root['b']"],
        "set_item_removed": ["root[1]"],
    });
    assert_eq!(report.to_json_value(), expected);
    assert_eq!(report.to_value().to_serde_json(), expected);
}

#[test]
fn empty_set_categories_are_omitted() {
    let mut report = Report::new();
    report.insert_set_item_added(set_item_path("1"), cv(&json!(1)));

    assert_eq!(
        report.to_json_value(),
        json!({"set_item_added": ["root[1]"]})
    );
}

/// The documented order: ascending by rendered path string, which is not
/// the same as the structural key order whenever a key's own quoting
/// character differs (`"` sorts before `'`).
#[test]
fn set_entries_are_sorted_by_rendered_path_string() {
    let mut report = Report::new();
    for key in ["it's", "a", "b"] {
        report.insert_set_item_added(
            vec![
                PathSegment::Key(key.to_string()),
                PathSegment::SetItem("1".to_string()),
            ],
            cv(&json!(1)),
        );
    }

    assert_eq!(
        report.to_json_value(),
        json!({"set_item_added": ["root[\"it's\"][1]", "root['a'][1]", "root['b'][1]"]})
    );
}

/// Two structurally distinct paths that render identically collapse to one
/// entry, the same way every path-keyed category does.
#[test]
fn set_entries_rendering_identically_collapse_to_one() {
    // The same colliding pair as
    // `two_structural_paths_rendering_identically_collapse_without_panicking`
    // (see its comment for the mechanism), with a set-item segment appended
    // to each side.
    let mut flat_key = String::new();
    flat_key.push('p');
    flat_key.push('\'');
    flat_key.push('"');
    flat_key.push(']');
    flat_key.push('[');
    flat_key.push('"');
    flat_key.push('q');
    flat_key.push('\'');

    let item = PathSegment::SetItem("1".to_string());
    let flat = vec![PathSegment::Key(flat_key), item.clone()];
    let nested = vec![
        PathSegment::Key("p'".to_string()),
        PathSegment::Key("q'".to_string()),
        item,
    ];
    assert_eq!(
        crate::path::render_path(&flat),
        crate::path::render_path(&nested),
        "test fixture assumption: these two structural paths must render identically",
    );

    let mut report = Report::new();
    report.insert_set_item_added(flat, cv(&json!(1)));
    report.insert_set_item_added(nested, cv(&json!(1)));

    let rendered = report.to_json_value();
    assert_eq!(
        rendered["set_item_added"]
            .as_array()
            .expect("an array")
            .len(),
        1
    );
    assert_eq!(rendered, report.to_value().to_serde_json());
}

#[test]
fn set_categories_count_toward_emptiness_and_finding_count() {
    let mut report = Report::new();
    assert!(report.is_empty());

    report.insert_set_item_added(set_item_path("1"), cv(&json!(1)));
    assert!(!report.is_empty());
    assert_eq!(report.finding_count(), 1);

    report.insert_set_item_removed(set_item_path("2"), cv(&json!(2)));
    assert_eq!(report.finding_count(), 2);
}

#[test]
fn merging_carries_set_categories_over() {
    let mut report = Report::new();
    report.insert_set_item_added(set_item_path("1"), cv(&json!(1)));

    let mut other = Report::new();
    other.insert_set_item_removed(set_item_path("2"), cv(&json!(2)));
    report.merge(other);

    assert_eq!(
        report.to_json_value(),
        json!({"set_item_added": ["root[1]"], "set_item_removed": ["root[2]"]})
    );
}

/// A set finding contributes the item's own `item_length` to the distance
/// numerator — the item is kept in the report solely for this.
#[test]
fn set_findings_contribute_their_item_length_to_the_distance() {
    let mut report = Report::new();
    report.insert_set_item_added(set_item_path("(1, 2)"), cv(&json!([1, 2])));
    report.insert_set_item_removed(set_item_path("1"), cv(&json!(1)));

    assert_eq!(report.distance_leaf_length(), 3);
}
