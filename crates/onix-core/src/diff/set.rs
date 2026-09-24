//! Set (`set`/`frozenset`) diffing: [`set_diff`]'s membership comparison,
//! the only report shape whose findings are bare path strings, not values.

use crate::error::Error;
use crate::ignore_order::{IgnoreOrderMemo, set_difference};
use crate::path::{PathSegment, set_item_repr};
use crate::report::Report;
use crate::value::SetItems;

use super::{DiffOptions, check_value_depth, scoped};

/// Diffs two sets of the same kind at `path`, `depth` levels deep, into
/// `set_item_added`/`set_item_removed` findings, mirroring `DeepDiff`'s
/// `_diff_set` (diff.py):
///
/// - Membership is identity, not structural equality: `{1}` vs `{1.0}` is
///   add+remove, `{(1,)}` vs `{(1.0,)}` is empty (see [`set_difference`]).
/// - `ignore_order` has no effect — a set has no order to ignore.
/// - An item is reported whole, with no `diff_at` recursion into it.
/// - Each item is checked with [`check_value_depth`] before being cloned
///   into the report, at the set's path plus its own [`PathSegment::SetItem`].
pub(crate) fn set_diff(
    path: &mut Vec<PathSegment>,
    a: &SetItems,
    b: &SetItems,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    let (removed, added) = set_difference(a, b, memo);
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
