//! Per-diff-invocation memoization of pairwise subtree distances — the fix
//! for the otherwise-exponential cost of `ignore_order` pairing on nested
//! containers.
//!
//! `super::pairing::compute_pairs` ranks every candidate `(removed, added)`
//! pair by `super::distance::rough_distance`, and for a container pair that
//! distance is a trial diff that re-enters `ignore_order_array_diff` on the
//! sub-items — so without caching, the same subtree pair is re-diffed once
//! per candidate that embeds it, compounding to `~2x` cost per nesting level.
//! This cache collapses that: a container-pair distance is computed once and
//! reused, so the total work becomes polynomial in the number of *distinct*
//! container-pair queries.
//!
//! # Soundness (why this changes no decision)
//!
//! [`rough_distance`](super::distance::rough_distance) is a **pure function
//! of the two subtrees' content** on this pairing path:
//!
//! * The numeric fast path is `numeric_distance(numeric_value(removed),
//!   numeric_value(added), CUTOFF)` — the cutoff is the one constant
//!   [`CUTOFF_DISTANCE_FOR_PAIRS`](super::pairing::CUTOFF_DISTANCE_FOR_PAIRS),
//!   so the result depends only on the two numeric values.
//! * The structural path is `count_diff_leaves(removed, added, ...) /
//!   (rough_length(removed) + rough_length(added))`; `rough_length` is a pure
//!   content node-count, and `count_diff_leaves`'s only depth/budget
//!   dependence is `count_array_diff_leaves`'s trial-diff budget, which can
//!   change the result *only* by having the trial hit a depth bound (counted
//!   as `0`). On the `ignore_order` pairing path that never happens:
//!   `ignore_order_array_diff` runs `check_value_depth` on every paired item
//!   against `max_depth - (depth + 1)` before pairing, so each item's own
//!   nesting is `<= max_depth - depth - 1`, hence its elements' nesting is
//!   `<= max_depth - depth - 2`, strictly below the trial's budget of
//!   `max_depth - depth - 1` — the trial always completes. So the structural
//!   result depends only on content too.
//!
//! Since [`ItemKey`] is an *exact* structural identity (the full recursive
//! key, not a lossy digest — and signed zeros are normalized the same way
//! `numeric_distance` treats them), the `(removed, added)` `ItemKey` pair
//! keys `rough_distance` losslessly. Caching by it is therefore
//! observationally identical to recomputing — verified empirically by the
//! with/without differential test in `super::tests`.

use std::cell::RefCell;

use super::fxhash::HashMap;
use super::hash::ItemKey;

/// A per-top-level-diff cache of container-pair [`rough_distance`] results,
/// keyed by the `(removed, added)` [`ItemKey`] pair. Created in
/// `crate::diff::diff_with_options`, threaded (by shared reference, interior
/// mutability) through the whole recursive diff, and dropped when it returns.
/// No eviction and no tuning knobs: it is bounded by the number of distinct
/// container-pair queries one diff makes.
///
/// [`rough_distance`]: super::distance::rough_distance
pub(crate) struct DistanceMemo {
    cache: RefCell<HashMap<(ItemKey, ItemKey), f64>>,
    enabled: bool,
}

impl DistanceMemo {
    /// A live cache (production path).
    pub(crate) fn new() -> Self {
        Self {
            cache: RefCell::new(HashMap::default()),
            enabled: true,
        }
    }

    /// A cache that never stores or reads — the "without memoization" arm of
    /// the decision-equivalence differential test, so both arms run the exact
    /// same code paths and differ only in whether the cache is consulted.
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            cache: RefCell::new(HashMap::default()),
            enabled: false,
        }
    }

    /// Whether a candidate pair should be routed through the cache: only when
    /// enabled *and* both sides are containers. Scalar-involving pairs never
    /// recurse (so never re-compute), so they skip the cache entirely — no
    /// key clone, no map operation — which keeps flat `ignore_order` shapes
    /// (a list of numbers, say) free of any memoization overhead.
    pub(crate) fn should_cache(&self, removed: &ItemKey, added: &ItemKey) -> bool {
        self.enabled && is_container(removed) && is_container(added)
    }

    /// The cached distance for `key`, if present.
    pub(crate) fn get(&self, key: &(ItemKey, ItemKey)) -> Option<f64> {
        self.cache.borrow().get(key).copied()
    }

    /// Records `value` for `key` (moving the already-cloned key in).
    pub(crate) fn put(&self, key: (ItemKey, ItemKey), value: f64) {
        self.cache.borrow_mut().insert(key, value);
    }
}

/// Whether `key` is a container (list/dict) rather than a scalar — the two
/// variants whose distance is computed by a recursive trial diff.
fn is_container(key: &ItemKey) -> bool {
    matches!(key, ItemKey::List(_) | ItemKey::Dict(_))
}
