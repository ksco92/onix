//! The two caches one `ignore_order` diff shares across its whole run: the
//! pairwise subtree distances it has already computed, and the digests it has
//! already assigned to hashable tuples.
//!
//! # Pairwise distances
//!
//! Memoizing them is the fix
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
//!
//! # Tuple digests
//!
//! The second cache is **not** an optimization: it is `DeepHash`'s own
//! observable behavior. `deephash.py::_make_hash_key` type-wraps only bare
//! numbers, so every other object — a tuple included — keys the `hashes`
//! dict as *itself*; `DeepHash._hash` reads that dict before computing
//! anything and writes its result back under the same key; and
//! `diff.py::_create_hashtable` builds both of a comparison's hashtables
//! against one shared `hashes` dict. A **hashable** tuple is therefore looked
//! up under Python's own `==`/`hash` and inherits the digest of the first
//! Python-equal tuple hashed anywhere in the run:
//!
//! ```text
//! DeepDiff([(1,)],     [(1.0,)],     ignore_order=True) -> {}
//! DeepDiff([(1, [1])], [(1.0, [1])], ignore_order=True) -> type_changes at root[0][0]
//! ```
//!
//! The second line is the boundary: a tuple holding a list or a dict cannot
//! be a dict key at all, so both the lookup and the store raise `TypeError`,
//! `DeepHash` falls back to object identity, and the tuple keeps its own
//! type-strict digest.
//!
//! [`super::hash::item_key`] consults this cache at every tuple node it
//! walks, in the order the engine hashes items (t1's list, then t2's — the
//! same order `_create_hashtable` uses), so the *first* member of a Python
//! equality class seen decides the digest for all of them. That ordering is
//! observable, and matching it is the point: `[(1.0,)]` vs `[(1, 1)]` is a
//! `type_changes` in real `DeepDiff` while `[(1,)]` vs `[(1, 1)]` is empty,
//! because in the first case the float tuple fixed the class digest before
//! the deduplicated `(1, 1)` content digest could match it.

use std::cell::RefCell;

use super::fxhash::HashMap;
use super::hash::{ItemKey, PyHashKey, TupleId};

/// The per-top-level-diff caches described in this module's doc: container-pair
/// [`rough_distance`] results keyed by the `(removed, added)` [`ItemKey`]
/// pair, and tuple digests keyed by Python's own equality
/// ([`PyHashKey`]). Created in `crate::diff::diff_with_options`, threaded (by
/// shared reference, interior mutability) through the whole recursive diff,
/// and dropped when it returns. No eviction and no tuning knobs: both are
/// bounded by the number of distinct queries one diff makes.
///
/// [`rough_distance`]: super::distance::rough_distance
pub(crate) struct IgnoreOrderMemo {
    cache: RefCell<HashMap<(ItemKey, ItemKey), f64>>,
    /// Interns each distinct hashable-tuple identity to its place in
    /// `tuple_digests`, so a nested tuple can be named by one [`TupleId`]
    /// inside its parent's identity instead of by a copy of its own.
    tuple_ids: RefCell<HashMap<PyHashKey, TupleId>>,
    /// The digest assigned to each interned identity, indexed by
    /// [`TupleId::index`].
    tuple_digests: RefCell<Vec<ItemKey>>,
    enabled: bool,
}

impl IgnoreOrderMemo {
    /// A live cache (production path).
    pub(crate) fn new() -> Self {
        Self {
            cache: RefCell::new(HashMap::default()),
            tuple_ids: RefCell::new(HashMap::default()),
            tuple_digests: RefCell::new(Vec::new()),
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
            tuple_ids: RefCell::new(HashMap::default()),
            tuple_digests: RefCell::new(Vec::new()),
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

    /// Interns a hashable tuple's Python equality identity and returns its
    /// id together with its digest: the one already assigned to an earlier
    /// Python-equal tuple in this run, or `compute()`'s result, recorded for
    /// the rest of the run.
    ///
    /// See this module's "Tuple digests" section for why this cache is part
    /// of the observable behavior rather than a speed-up. Both the identity
    /// (whose nested tuples are named by id) and the digest (whose nested
    /// keys are shared through [`ItemKey::Tuple`]'s `Rc`) are `O(arity)` per
    /// tuple node, so the two tables together stay linear in the number of
    /// nodes hashed rather than quadratic in nesting depth. `compute` runs
    /// with no borrow held, so it is free to recurse back into this same
    /// cache for a nested tuple. The `disabled()` cache still serves this
    /// method: it turns off *distance* memoization only, which is the one
    /// thing proven decision-neutral.
    pub(crate) fn tuple_digest(
        &self,
        key: PyHashKey,
        compute: impl FnOnce() -> ItemKey,
    ) -> (TupleId, ItemKey) {
        if let Some(&id) = self.tuple_ids.borrow().get(&key) {
            return (id, self.tuple_digests.borrow()[id.index()].clone());
        }

        let computed = compute();
        let mut digests = self.tuple_digests.borrow_mut();
        let id = TupleId::new(digests.len());
        digests.push(computed.clone());
        self.tuple_ids.borrow_mut().insert(key, id);
        (id, computed)
    }
}

/// Whether `key` is a container (list/tuple/dict) rather than a scalar — the
/// variants whose distance is computed by a recursive trial diff.
fn is_container(key: &ItemKey) -> bool {
    matches!(key, ItemKey::List(_) | ItemKey::Tuple(_) | ItemKey::Dict(_))
}
