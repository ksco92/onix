//! Dict (JSON object) and custom-object diffing: [`object_diff`]'s key-set
//! walk — added/removed keys become leaf findings, shared keys recurse
//! through `super::dispatch`'s [`super::diff_at`] one level deeper. The same
//! walk serves a custom object diffed by its attributes (see
//! [`crate::value::ObjectKind`]); only the path segment (`.attr` vs
//! `['key']`) and the report category (`attribute_*` vs `dictionary_item_*`)
//! differ, both selected once from [`Object::kind`].

use crate::value::{Object, ObjectKind, Value};

use crate::error::Error;
use crate::ignore_order::IgnoreOrderMemo;
use crate::path::{PathSegment, entry_path_segment, object_key_path_segment as key_segment};
use crate::report::{Report, ValuesChangedEntry};

use super::{DiffOptions, check_map_depth, check_value_depth, diff_at, scoped};

/// Diffs two dicts (JSON objects) at `path`, `depth` levels deep, matching
/// `DeepDiff`'s `_diff_dict`: keys unique to one side become
/// `dictionary_item_removed`/`added` findings, shared keys recurse one level
/// deeper, and a pair below
/// [`crate::ignore_order::is_below_threshold_to_diff_deeper`]'s ratio
/// collapses into one wholesale `values_changed` instead.
pub(crate) fn object_diff(
    path: &mut Vec<PathSegment>,
    a: &Object,
    b: &Object,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    // `threshold_to_diff_deeper` collapse — see this function's own doc.
    // The depth check runs against the borrowed maps, BEFORE either is
    // cloned into a finding: cloning first and checking after would hand
    // an attacker-controlled, arbitrarily deep `a`/`b` straight to the
    // compact `Value`'s natively recursive (but depth-guarded) `Clone` with
    // no bound in place yet — see `check_map_depth`'s own doc and
    // `docs/design/value-model.md` on why `Clone` stays recursive.
    if crate::ignore_order::is_below_threshold_to_diff_deeper(a, b) {
        check_map_depth(path, a, depth, opts.max_depth)?;
        check_map_depth(path, b, depth, opts.max_depth)?;
        let old_value = Value::Object(a.clone());
        let new_value = Value::Object(b.clone());
        let mut report = Report::new();
        report.insert_values_changed(
            path.clone(),
            ValuesChangedEntry {
                // Both sides are dicts here, never strings, so no `diff`.
                diff: None,
                old_value,
                new_value,
                new_path: None,
            },
        );
        return Ok(report);
    }

    // A non-`str` key needs python-equality matching (`object_diff_mixed`'s
    // own doc has the rule); dispatched to a separate function, kept off
    // this function's own frame, to protect the default `max_depth`
    // budget on the hot `object_diff` <-> `diff_at` recursion.
    if a.has_non_str_keys() || b.has_non_str_keys() {
        return object_diff_mixed(path, a, b, depth, opts, memo);
    }

    let mut report = Report::new();

    // A `dict` and a custom object share this walk; both sides are the same
    // class here (`diff_at` reports `type_changes` otherwise), so one side's
    // kind decides the path segment and report category for the whole walk.
    let kind = a.kind();

    // Stepping into a key — whether it recurses (shared key) or is a leaf
    // finding (added/removed) — always adds one to depth, matching the
    // module's depth-counting convention: a shared key's own recursive
    // `diff_at` call gets `depth + 1` below, and an added/removed key's
    // `check_value_depth` call needs that same `depth + 1` (the depth its
    // own path sits at), not the *parent* dict's `depth`.
    for (key, old_value) in a {
        scoped(
            path,
            entry_path_segment(kind, key),
            |path| -> Result<(), Error> {
                match b.get(key) {
                    None => {
                        check_value_depth(path, old_value, depth + 1, opts.max_depth).map(|()| {
                            insert_removed(&mut report, kind, path.clone(), old_value.clone());
                        })
                    }
                    Some(new_value) => diff_at(path, old_value, new_value, depth + 1, opts, memo)
                        .map(|sub_report| report.merge(sub_report)),
                }
            },
        )?;
    }

    for (key, new_value) in b {
        if !a.contains_key(key) {
            scoped(path, entry_path_segment(kind, key), |path| {
                check_value_depth(path, new_value, depth + 1, opts.max_depth).map(|()| {
                    insert_added(&mut report, kind, path.clone(), new_value.clone());
                })
            })?;
        }
    }

    Ok(report)
}

/// Records a removed key as the category [`ObjectKind`] selects:
/// `dictionary_item_removed` for a `dict`, `attribute_removed` for a custom
/// object.
fn insert_removed(report: &mut Report, kind: ObjectKind, path: Vec<PathSegment>, value: Value) {
    match kind {
        ObjectKind::Dict => report.insert_dictionary_item_removed(path, value),
        ObjectKind::CustomObject | ObjectKind::Opaque | ObjectKind::Cycle | ObjectKind::Failed => {
            report.insert_attribute_removed(path, value);
        }
    }
}

/// [`insert_removed`]'s added-side twin.
fn insert_added(report: &mut Report, kind: ObjectKind, path: Vec<PathSegment>, value: Value) {
    match kind {
        ObjectKind::Dict => report.insert_dictionary_item_added(path, value),
        ObjectKind::CustomObject | ObjectKind::Opaque | ObjectKind::Cycle | ObjectKind::Failed => {
            report.insert_attribute_added(path, value);
        }
    }
}

/// [`object_diff`]'s walk for the (rare) case where `a` or `b` has a
/// non-`str` key — kept out of `object_diff`'s own body; see the call
/// site's doc for why.
///
/// A non-`str` key matches across `a` and `b` by Python `==`, not this
/// crate's own structural `ObjectKey` equality, via
/// [`crate::ignore_order::match_dict_keys`] — see that function's doc for
/// the exact rule and `tests/golden/README.md`'s "A dict key matches across
/// two dicts by Python `==`" section for the confirmed example
/// (`{1: "a"}` vs `{1.0: "a2"}` reports `root[1.0]`, `b`'s key form).
///
/// The `threshold_to_diff_deeper` collapse and the depth-counting
/// convention are exactly [`object_diff`]'s own — see that function's doc.
fn object_diff_mixed(
    path: &mut Vec<PathSegment>,
    a: &Object,
    b: &Object,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    let mut report = Report::new();
    let matched = crate::ignore_order::match_dict_keys(a, b);

    for (key, old_value, new_value) in matched.shared {
        scoped(path, key_segment(key), |path| {
            diff_at(path, old_value, new_value, depth + 1, opts, memo)
                .map(|sub_report| report.merge(sub_report))
        })?;
    }
    for (key, old_value) in matched.only_a {
        scoped(path, key_segment(key), |path| {
            check_value_depth(path, old_value, depth + 1, opts.max_depth).map(|()| {
                report.insert_dictionary_item_removed(path.clone(), old_value.clone());
            })
        })?;
    }
    for (key, new_value) in matched.only_b {
        scoped(path, key_segment(key), |path| {
            check_value_depth(path, new_value, depth + 1, opts.max_depth).map(|()| {
                report.insert_dictionary_item_added(path.clone(), new_value.clone());
            })
        })?;
    }

    Ok(report)
}
