//! `DeepDiff(..., ignore_order=True)`'s list-matching algorithm, called
//! from [`crate::diff::array_diff`] whenever
//! [`crate::diff::DiffOptions::ignore_order`] is set. See
//! `docs/design/ignore-order.md` for the full algorithm.
//!
//! - **Hash** — reduce each list to a canonical equivalence key per item.
//! - **Pair** — gate on overlap, then greedily match by structural distance.
//! - **Distance** — rank candidate pairs; never an equality check.

mod distance;
mod fxhash;
mod hash;
mod memo;
mod pairing;

pub(crate) use memo::IgnoreOrderMemo;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use crate::value::Value;

use crate::diff::{DiffOptions, check_value_depth, diff_at, scoped};
use crate::error::Error;
use crate::path::PathSegment;
use crate::report::Report;

use fxhash::{HashMap, HashSet};
use std::rc::Rc;

use hash::{HashedList, ItemKey};
use pairing::compute_pairs;

pub(crate) use distance::{
    is_below_threshold_to_diff_deeper, item_length, match_dict_keys, type_change_leaf_length,
};
pub(crate) use hash::set_difference;
#[cfg(test)]
pub(crate) use hash::set_member_digest;

/// `cutoff_intersection_for_pairs`'s default (`DeepDiff`'s own name;
/// `CUTOFF_INTERSECTION_FOR_PAIRS_DEFAULT`, diff.py) — the get-pairs gate
/// threshold (see `docs/design/ignore-order.md`'s "Pair" stage). Out of
/// scope for MVP as a *tunable* parameter; the default value itself is
/// very much in scope.
const CUTOFF_INTERSECTION_FOR_PAIRS: f64 = 0.7;

/// `DeepDiff`'s `_diff_iterable_with_deephash` (diff.py) for one list level
/// — see `docs/design/ignore-order.md` for the full algorithm. Called from
/// [`crate::diff::array_diff`] whenever
/// [`crate::diff::DiffOptions::ignore_order`] is set.
pub(crate) fn ignore_order_array_diff(
    path: &mut Vec<PathSegment>,
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    // Every item will be structurally hashed (`item_key`, native recursion)
    // and, if left unpaired, cloned whole into the Report — both unbounded
    // native recursion unless validated first. See `docs/design/ignore-order.md`'s
    // "Depth safety" section. Checked at each side's own eventual path (its
    // own original index) so a MaxDepthExceeded error points at exactly the
    // item that tripped it.
    for (idx, item) in a.iter().enumerate() {
        scoped(path, PathSegment::Index(idx), |path| {
            check_value_depth(path, item, depth + 1, opts.max_depth)
        })?;
    }
    for (idx, item) in b.iter().enumerate() {
        scoped(path, PathSegment::Index(idx), |path| {
            check_value_depth(path, item, depth + 1, opts.max_depth)
        })?;
    }

    let t1 = HashedList::build(a, memo);
    let t2 = HashedList::build(b, memo);

    let hashes_added: Vec<Rc<ItemKey>> = t2
        .distinct_order
        .iter()
        .filter(|key| !t1.contains(key))
        .cloned()
        .collect();
    let hashes_removed: Vec<Rc<ItemKey>> = t1
        .distinct_order
        .iter()
        .filter(|key| !t2.contains(key))
        .cloned()
        .collect();

    #[allow(
        clippy::cast_precision_loss,
        reason = "distinct-hash counts are bounded by list length, far under f64's exact-integer range"
    )]
    let get_pairs = {
        let ratio = (hashes_added.len() + hashes_removed.len()) as f64
            / (t1.distinct_order.len() + t2.distinct_order.len() + 1) as f64;
        ratio <= CUTOFF_INTERSECTION_FOR_PAIRS
    };

    let pairs = if get_pairs {
        compute_pairs(&hashes_added, &hashes_removed, &t1, &t2, depth, opts, memo)
    } else {
        HashMap::default()
    };

    let mut report = Report::new();
    let mut consumed_removed: HashSet<Rc<ItemKey>> = HashSet::default();

    for added_key in &hashes_added {
        let (new_idx, new_value) = t2.get(added_key);

        if let Some(removed_key) = pairs.get(added_key) {
            consumed_removed.insert(Rc::clone(removed_key));
            let (old_idx, old_value) = t1.get(removed_key);
            let prefix_depth = path.len();
            let sub_report = scoped(path, PathSegment::Index(old_idx), |path| {
                let mut sub = diff_at(path, old_value, new_value, depth, opts, memo)?;
                // Merges this pair's own add/remove collision before
                // retagging; independent of the whole-tree merge pass.
                sub.merge_mutual_add_removes();
                if old_idx != new_idx {
                    sub.retag_new_path(prefix_depth, new_idx);
                }
                Ok::<Report, Error>(sub)
            })?;
            report.merge(sub_report);
        } else {
            scoped(path, PathSegment::Index(new_idx), |path| {
                report.insert_iterable_item_added(path.clone(), new_value.clone());
            });
        }
    }

    for removed_key in &hashes_removed {
        if consumed_removed.contains(removed_key) {
            continue;
        }
        let (old_idx, old_value) = t1.get(removed_key);
        scoped(path, PathSegment::Index(old_idx), |path| {
            report.insert_iterable_item_removed(path.clone(), old_value.clone());
        });
    }

    Ok(report)
}
