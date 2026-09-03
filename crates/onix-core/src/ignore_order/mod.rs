//! `DeepDiff(..., ignore_order=True)`'s list-matching algorithm — a direct
//! port of `deepdiff/diff.py::_diff_iterable_with_deephash` plus its
//! distance/pairing helpers (`_get_most_in_common_pairs_in_iterables`,
//! `_get_rough_distance`), scoped to the default-parameter behavior only
//! (`report_repetition=False`, default `cutoff_distance_for_pairs`/
//! `cutoff_intersection_for_pairs`, no `ignore_order_func`/
//! `iterable_compare_func`/`max_passes`/`max_diffs`/numpy). Called from
//! [`crate::diff::array_diff`] instead of the ordered LCS/positional path
//! whenever [`crate::diff::DiffOptions::ignore_order`] is set — a single
//! global flag consulted independently every time a list is encountered, at
//! any depth (dicts are never affected; only list-typed values, recursively).
//!
//! Because `DeepDiff`'s own `max_passes`/`max_diffs` caps on this work are
//! unimplemented (out of scope above), [`compute_pairs`]'s greedy matching is
//! an unbounded `O(N²)` in the number of unpaired elements at each list
//! *level* — so a caller diffing untrusted input must still bound that
//! input's *width*. Across nesting *levels* the per-diff distance memo (see
//! the [`memo`] module) removes the *exponential* blowup — ranking a
//! container candidate pair is a recursive trial diff, and the memo computes
//! each distinct container-pair distance once instead of re-diffing it per
//! candidate that embeds it (a depth-25 nested list dropped from tens of
//! seconds to milliseconds). A *polynomial* (super-linear) cost in nesting
//! depth remains, though, in **both time and memory**: the memo holds one
//! entry per distinct container pair, keyed by the full recursive `ItemKey`,
//! so a caller diffing untrusted input must bound its nesting *depth* as well
//! as its *width* — a few-KB input nested many hundreds of levels deep still
//! costs seconds and hundreds of MB, all under the default `max_depth`.
//! (Scalar candidate pairs never recurse and bypass the memo entirely, so
//! flat lists pay nothing for it.)
//!
//! # The algorithm, end to end
//!
//! For one list level (`a` = `t1`/old, `b` = `t2`/new):
//!
//! 1. **Hash every item** ([`item_key`]) into a canonical equivalence key —
//!    see that function's doc for the exact `DeepHash`-matching rules
//!    (type-tagged numbers, order/count-insensitive nested lists, etc.).
//!    Reduce each list to its *distinct* keys in first-occurrence order
//!    ([`HashedList`]) — a repeated value's *other* occurrences are
//!    genuinely invisible in the `report_repetition=False` output,
//!    confirmed against real `DeepDiff` down to the exact index chosen.
//! 2. **Set-difference the distinct keys**: `hashes_added`/`hashes_removed`
//!    are exactly the keys present on only one side, each preserving *that
//!    side's* first-occurrence order (mirrors `SetOrdered.__sub__`, which is
//!    order-preserving — this is what keeps the reported ordering
//!    deterministic run to run). A key present on
//!    both sides is already "matched" and never visited again — this is why
//!    a pure shuffle, or a pure duplicate-count change, reports `{}`.
//! 3. **The get-pairs gate**: `(hashes_added.len() + hashes_removed.len()) /
//!    (t1_distinct + t2_distinct + 1) > 0.7` disables pairing entirely,
//!    falling back to raw per-hash add/remove. **The denominator is the
//!    number of *distinct hashes*, not the raw list length**
//!    (`len(full_t1_hashtable)` in `deepdiff/diff.py`). The distinction
//!    matters whenever a side has duplicates: `[1,1,2]` vs `[3,4]` has *3*
//!    raw items but only *2* distinct hashes per side, which moves the ratio
//!    from `4/7` (list length — would engage pairing) to `4/5` (distinct
//!    count — correctly disables it), matching real `deepdiff==9.1.0`.
//! 4. **If pairing is engaged**, [`compute_pairs`] runs the exact greedy,
//!    non-globally-optimal, asymmetrically-tie-broken matching described in
//!    its own doc.
//! 5. **Walk `hashes_added` (in order), then remaining `hashes_removed`**:
//!    a paired added hash gets a *real* recursive diff between its removed
//!    partner (old) and itself (new), keyed at the removed side's index,
//!    with [`crate::report::Report::retag_new_path`] applied when the old
//!    and new indices differ (see that method's doc for the *every nested
//!    finding*, not just one, subtlety it handles); an
//!    unpaired added hash is a plain `iterable_item_added` at its own index;
//!    an unpaired removed hash (whatever `hashes_removed` has left after the
//!    added-hash loop consumed every paired partner) is a plain
//!    `iterable_item_removed` at its own index.
//!
//! After the whole recursive traversal finishes, [`crate::diff`]'s existing
//! whole-tree [`crate::report::Report::merge_mutual_add_removes`] pass
//! still runs exactly once, unchanged — it is what turns many of this
//! module's own raw adds/removes into `values_changed` on typical small,
//! heavily-changed fixtures (the `get_pairs=False` common case), and this
//! module deliberately does not duplicate or special-case that logic.
//!
//! # Depth safety (this crate's own invariant, not `DeepDiff`'s)
//!
//! [`item_key`] and [`rough_length`] both recurse **natively** (no explicit
//! work-stack) — unlike the rest of this crate's traversal, which is bounded
//! via [`crate::diff::check_traversal_depth`]/[`crate::diff::check_value_depth`].
//! [`ignore_order_array_diff`] closes this gap up front: every item of both
//! `a` and `b` is validated with [`crate::diff::check_value_depth`] (the same
//! combined path-depth-plus-value-depth budget documented on
//! [`crate::diff::diff_with_max_depth`]) *before* any hashing happens, so by
//! the time [`item_key`]/[`rough_length`] run, their native recursion is
//! already proven to be no deeper than a bound this crate already trusts a
//! plain `.clone()` to recurse through safely elsewhere. The structural
//! distance fallback's own trial comparison ([`count_diff_leaves`]) is a
//! `Report`-free mirror of a diff for scalars and dicts (no further native
//! recursion risk beyond what's already covered above), except for a
//! genuinely nested array, which still runs a real, small trial
//! [`crate::diff::array_diff`] call — see [`rough_distance`]'s doc for its
//! own remaining-budget bound.
//!
//! # Two `DeepDiff` behaviors the pairing depends on
//!
//! Both are documented at the functions that implement them, not restated
//! in full here:
//!
//! 1. **Type-change coercion in the delta view.** A `type_changes` finding's
//!    `new_value` is omitted from the leaf-length measure whenever
//!    `new_type(old_value) == new_value` under Python's own coercion (e.g.
//!    `float(0) == 0.0`, `int(True) == 1`, `bool(0) == False`) — the general
//!    rule real `DeepDiff` applies
//!    (`model.py::TreeResult._from_tree_type_changes`, `DELTA_VIEW` branch).
//!    See [`coerce_for_type_change`] for the exact matrix.
//!    [`type_change_leaf_length`] and `Report::distance_leaf_length` both
//!    route through it, so this rule lives in exactly one place.
//! 2. **`new_path` composition across nesting levels.** A paired item whose
//!    own recursive diff needs a further `ignore_order` pairing with index
//!    drift must carry *both* index substitutions in its `new_path`; the
//!    inner pairing sets `new_path` before the outer pairing's own
//!    [`crate::report::Report::retag_new_path`] runs, so `retag_new_path`
//!    composes onto whatever structural `new_path` is already present rather
//!    than overwriting it — see that method's doc.
//!
//! Differential fuzzing against real `deepdiff==9.1.0`
//! (`scripts/differential_fuzz.py`) surfaces no unexplained divergences;
//! the one accepted, documented exception is the `threshold_to_diff_deeper`
//! class described on `crate::ignore_order::count_object_diff_leaves`'s own
//! doc.
//!
//! # Internal layout
//!
//! - `fxhash` — the `FxHash` non-cryptographic hasher and the
//!   `HashMap`/`HashSet` type aliases built on it (see this doc's own
//!   rationale above for why `SipHash`'s DoS-resistance is the wrong
//!   trade-off for these particular maps).
//! - `hash` — item hashing: the canonical equivalence key and the per-list
//!   hash table it feeds (algorithm step 1 above).
//! - `distance` — structural/numeric distance between two values, used
//!   only to *rank* candidate pairs; has no dependency on the hashing
//!   layer.
//! - `pairing` — the get-pairs gate threshold and the greedy matching
//!   algorithm (algorithm step 4 above) built from `hash` and `distance`.
//! - This file (`mod.rs`) is the entry point: `ignore_order_array_diff`
//!   wires the pieces above together end to end (algorithm steps 1-5
//!   above) and is the only public-to-the-crate item this module exposes.

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
use hash::{HashedList, ItemKey};
use pairing::compute_pairs;

pub(crate) use distance::{
    is_below_threshold_to_diff_deeper, item_length, type_change_leaf_length,
};

/// `cutoff_intersection_for_pairs`'s default (`DeepDiff`'s own name;
/// `CUTOFF_INTERSECTION_FOR_PAIRS_DEFAULT`, diff.py) — the get-pairs gate
/// threshold (algorithm step 3 above). Out of scope for MVP as a *tunable*
/// parameter; the default value itself is very much in scope.
const CUTOFF_INTERSECTION_FOR_PAIRS: f64 = 0.7;

/// `DeepDiff`'s `_diff_iterable_with_deephash` (diff.py) for one list level
/// — see this module's doc for the full algorithm. Called from
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
    // native recursion unless validated first. See this module's "Depth
    // safety" doc section. Checked at each side's own eventual path (its
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

    let hashes_added: Vec<ItemKey> = t2
        .distinct_order
        .iter()
        .filter(|key| !t1.contains(key))
        .cloned()
        .collect();
    let hashes_removed: Vec<ItemKey> = t1
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
    let mut consumed_removed: HashSet<ItemKey> = HashSet::default();

    for added_key in &hashes_added {
        let (new_idx, new_value) = t2.get(added_key);

        if let Some(removed_key) = pairs.get(added_key) {
            consumed_removed.insert(removed_key.clone());
            let (old_idx, old_value) = t1.get(removed_key);
            let prefix_depth = path.len();
            let sub_report = scoped(path, PathSegment::Index(old_idx), |path| {
                // `depth + 1` here (this pair's own path depth, matching
                // the "list element sits one level deeper than the list"
                // convention used throughout this crate — same as
                // `positional_array_diff`'s equivalent same-index
                // recursion) is provably redundant with, not just equal
                // to, the pre-pass's own bound: `check_value_depth` already
                // validated `old_value`/`new_value` individually against
                // this *exact* budget before pairing ever ran (see this
                // function's own pre-pass loops above), and comparing two
                // values that are both within a depth budget can never
                // need to recurse past that same budget (any point the
                // comparison reaches requires *both* sides to still be
                // containers there, which requires each side's own nesting
                // to extend that far). So a `depth + 1` -> `depth * 1`
                // mutant here is a genuinely unreachable/equivalent
                // mutation, unlike the analogous-looking but *reachable*
                // ones in `rough_distance`/`count_array_diff_leaves`/
                // `count_object_diff_leaves` (those restart the budget
                // fresh at `0` for an independent trial diff, so an
                // off-by-one there is NOT redundant with anything the
                // pre-pass already checked). Confirmed by direct mutation:
                // applying it and re-running this module's test suite
                // finds no failure.
                let mut sub = diff_at(path, old_value, new_value, depth, opts, memo)?;
                // Resolve this pair's OWN coincidental add/remove path
                // collisions before retagging: `DeepDiff`'s
                // distance/delta computation applies its whole-tree merge
                // globally too (see `count_array_diff_leaves`'s doc for the
                // same finding at the distance-probe layer), so a nested
                // `values_changed` produced *by* this merge — not by real
                // pairing — still needs `new_path` when `old_idx != new_idx`.
                // Idempotent: only ever touches entries genuinely present in
                // `sub` right now, so the *whole-tree* pass in
                // `crate::diff::diff_with_options` finds nothing left to
                // redo here, and running it now cannot merge anything a
                // sibling subtree would have prevented (that pass only ever
                // merges paths colliding within `sub` itself).
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
