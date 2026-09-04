//! Set (`set`/`frozenset`) diffing: [`set_diff`]'s membership comparison,
//! the one report shape whose findings are bare path strings rather than
//! path-keyed values.

use crate::error::Error;
use crate::ignore_order::set_difference;
use crate::path::{PathSegment, set_item_repr};
use crate::report::Report;
use crate::value::SetItems;

use super::{DiffOptions, check_value_depth, scoped};

/// Diffs two sets of the same kind at `path`, `depth` levels deep, into
/// `set_item_added`/`set_item_removed` findings.
///
/// This mirrors `DeepDiff`'s `_diff_set` (diff.py), and the mirroring is
/// what makes it look unlike every other comparison here:
///
/// - **Membership is an identity, not structural equality.** `_diff_set`
///   builds a hashtable per side and compares the two *key sets* — so `{1}`
///   vs `{1.0}` is a removal plus an addition while `{(1,)}` vs `{(1.0,)}`
///   is empty. [`set_difference`]'s own doc has the full matching rule this
///   reproduces, and the one place `onix` is deliberately more
///   deterministic than `DeepDiff` here.
/// - **`ignore_order` changes nothing.** A set has no order to ignore, and
///   `DeepDiff` dispatches to this same `_diff_set` either way; confirmed
///   against `deepdiff==9.1.0`.
/// - **An item is never recursed into.** A differing item is reported whole,
///   as one entry naming it, with no sub-path beneath it — so unlike every
///   other container there is no `diff_at` recursion from here at all.
///
/// Each finding's path is the set's own path plus a
/// [`PathSegment::SetItem`] carrying the item's rendered text; `depth` grows
/// by one for that segment, and the item is checked with
/// [`check_value_depth`] against the remaining budget before being cloned
/// into the report, exactly like every other value-carrying finding.
pub(crate) fn set_diff(
    path: &mut Vec<PathSegment>,
    a: &SetItems,
    b: &SetItems,
    depth: usize,
    opts: &DiffOptions,
) -> Result<Report, Error> {
    let (removed, added) = set_difference(a, b);
    let mut report = Report::new();

    for item in removed {
        insert_set_finding(path, &mut report, item, depth, opts.max_depth, false)?;
    }
    for item in added {
        insert_set_finding(path, &mut report, item, depth, opts.max_depth, true)?;
    }

    Ok(report)
}

/// Records one set item as an addition (`added`) or a removal, at the set's
/// `path` extended by the item's own rendered segment.
fn insert_set_finding(
    path: &mut Vec<PathSegment>,
    report: &mut Report,
    item: &crate::value::Value,
    depth: usize,
    max_depth: usize,
    added: bool,
) -> Result<(), Error> {
    scoped(path, PathSegment::SetItem(set_item_repr(item)), |path| {
        check_value_depth(path, item, depth + 1, max_depth)?;
        if added {
            report.insert_set_item_added(path.clone(), item.clone());
        } else {
            report.insert_set_item_removed(path.clone(), item.clone());
        }
        Ok(())
    })
}
