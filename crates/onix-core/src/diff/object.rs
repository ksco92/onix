//! Dict (JSON object) diffing: [`object_diff`]'s key-set walk — added/removed
//! keys become leaf findings, shared keys recurse through
//! `super::dispatch`'s [`super::diff_at`] one level deeper.

use serde_json::{Map, Value};

use crate::error::Error;
use crate::path::PathSegment;
use crate::report::{Report, ValuesChangedEntry};

use super::{DiffOptions, check_map_depth, check_value_depth, diff_at, scoped};

/// Diffs two dicts (JSON objects) at `path`, `depth` levels deep.
///
/// Keys present only in `a` become `dictionary_item_removed` findings; keys
/// present only in `b` become `dictionary_item_added` findings (both keyed by
/// the raw removed/added value, per `DeepDiff`'s `to_json()` shape); keys
/// present in both recurse through the engine's own internal dispatch one
/// level deeper with the key appended to the path, so nested
/// `values_changed`/`type_changes`/further dict findings, or a nested list
/// finding (via `array_diff`), all surface with their own deep path.
///
/// This always recurses, even for a key whose value is unchanged — it does
/// not re-check [`values_equal`] per key. That single top-level check in
/// [`diff_with_max_depth`] is enough for its documented guarantee (fully
/// equal *whole* inputs never hit the bound); re-running it per key would
/// only rescue one specific edge case (an equal subtree nested arbitrarily
/// deep under an unrelated shallow change) at the cost of an extra full
/// subtree walk on every recursion step, and — because it can only be
/// invoked here after some ancestor pair was already proven unequal — would
/// leave [`diff_at`]'s own equal-value branches permanently unreachable.
/// Simpler and covered by real test paths beats a cleverness that produces
/// dead code.
///
/// **`threshold_to_diff_deeper` collapse.** Before walking keys at all,
/// this mirrors `DeepDiff`'s own `_diff_dict` (diff.py): whenever the key
/// overlap (intersection / union) between `a` and `b` is below `DeepDiff`'s
/// default `threshold_to_diff_deeper` (`0.33`), the whole pair collapses
/// into a single wholesale `values_changed` (old/new value the entire
/// dict) instead of recursing key by key — see
/// [`crate::ignore_order::is_below_threshold_to_diff_deeper`]'s doc for the
/// exact ratio (`union_len > 1 && intersect/union < 0.33`, confirmed
/// against real `deepdiff==9.1.0` including the exact-`0.33`-boundary case,
/// which does *not* collapse). This applies unconditionally, at every
/// nesting level (root included), matching `DeepDiff`'s own behavior
/// whether or not `ignore_order` is set.
///
/// [`Report::merge`] documents why the per-key `report.merge(...)` calls
/// below never collide on a *structural* path (each key here is visited
/// once, so no traversal ever revisits the same node) — `Report` is keyed
/// by that structural path, not by [`render_path`]'s rendered string, which
/// is *not* injective on adversarial keys (see [`crate::path::quote_key`]'s
/// doc and `Report`'s module doc for how two structurally distinct paths
/// can render identically, and how that collision is handled at
/// serialization rather than here).
///
/// `path` is the single buffer shared across the whole traversal (see
/// [`diff_at`]'s doc): each key below runs its work through [`scoped`],
/// which pushes the key segment, runs the closure with that segment in
/// place, then pops it again before moving to the next key — restoring
/// `path` to exactly what it was on entry before touching any sibling key,
/// so one key's finding never leaks a stale segment into another's path.
pub(crate) fn object_diff(
    path: &mut Vec<PathSegment>,
    a: &Map<String, Value>,
    b: &Map<String, Value>,
    depth: usize,
    opts: &DiffOptions,
) -> Result<Report, Error> {
    // `threshold_to_diff_deeper` collapse — see this function's own doc.
    // The depth check runs against the borrowed maps, BEFORE either is
    // cloned into a finding: cloning first and checking after would hand
    // an attacker-controlled, arbitrarily deep `a`/`b` straight to
    // `serde_json::Value`'s natively recursive `Clone` with no bound in
    // place yet — see `check_map_depth`'s own doc.
    if crate::ignore_order::is_below_threshold_to_diff_deeper(a, b) {
        check_map_depth(path, a, depth, opts.max_depth)?;
        check_map_depth(path, b, depth, opts.max_depth)?;
        let old_value = Value::Object(a.clone());
        let new_value = Value::Object(b.clone());
        let mut report = Report::new();
        report.insert_values_changed(
            path.clone(),
            ValuesChangedEntry {
                old_value,
                new_value,
                new_path: None,
            },
        );
        return Ok(report);
    }

    let mut report = Report::new();

    // Stepping into a key — whether it recurses (shared key) or is a leaf
    // finding (added/removed) — always adds one to depth, matching the
    // module's depth-counting convention: a shared key's own recursive
    // `diff_at` call gets `depth + 1` below, and an added/removed key's
    // `check_value_depth` call needs that same `depth + 1` (the depth its
    // own path sits at), not the *parent* dict's `depth`.
    for (key, old_value) in a {
        scoped(
            path,
            PathSegment::Key(key.clone()),
            |path| -> Result<(), Error> {
                match b.get(key) {
                    None => {
                        check_value_depth(path, old_value, depth + 1, opts.max_depth).map(|()| {
                            report.insert_dictionary_item_removed(path.clone(), old_value.clone());
                        })
                    }
                    Some(new_value) => diff_at(path, old_value, new_value, depth + 1, opts)
                        .map(|sub_report| report.merge(sub_report)),
                }
            },
        )?;
    }

    for (key, new_value) in b {
        if !a.contains_key(key) {
            scoped(path, PathSegment::Key(key.clone()), |path| {
                check_value_depth(path, new_value, depth + 1, opts.max_depth).map(|()| {
                    report.insert_dictionary_item_added(path.clone(), new_value.clone());
                })
            })?;
        }
    }

    Ok(report)
}
