//! The caches one `ignore_order` diff shares across its whole run: the
//! pairwise subtree distances it has already computed, the digests it has
//! already assigned to hashable tuples for list-item matching, and the
//! representatives it has assigned to set members.
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
//! # Hashable digests
//!
//! The second cache is **not** an optimization: it is `DeepHash`'s own
//! observable behavior. `deephash.py::_make_hash_key` type-wraps only bare
//! numbers, so every other object — a tuple or a frozenset included — keys
//! the `hashes` dict as *itself*; `DeepHash._hash` reads that dict before
//! computing anything and writes its result back under the same key; and
//! `diff.py::_create_hashtable` builds both of a comparison's hashtables
//! against one shared `hashes` dict. A **hashable** container (a tuple, or a
//! frozenset when it appears as a set member) is therefore looked up under
//! Python's own `==`/`hash` and inherits the digest of the first Python-equal
//! container hashed anywhere in the run:
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
//! [`super::hash::item_key`] consults this cache at every hashable tuple node
//! it walks, in the order the engine hashes items (t1's list, then t2's — the
//! same order `_create_hashtable` uses), so the *first* member of a Python
//! equality class seen decides the digest for all of them. That ordering is
//! observable, and matching it is the point: `[(1.0,)]` vs `[(1, 1)]` is a
//! `type_changes` in real `DeepDiff` while `[(1,)]` vs `[(1, 1)]` is empty,
//! because in the first case the float tuple fixed the class digest before
//! the deduplicated `(1, 1)` content digest could match it.
//!
//! # Set-member digests
//!
//! [`super::hash::set_member_digest`] needs the same first-Python-equal-wins
//! cache — a member's digest must collapse a Python-equal container the way
//! `DeepHash` does — but it compares members by a *positional* digest
//! ([`super::hash::MemberDigest`]) rather than the order-insensitive
//! [`ItemKey`] the list path hashes by, so that a tuple set member matches
//! `tuple.__eq__` (onix's one deliberate divergence from `DeepHash`'s
//! order-insensitive iterable hashing — see that type's doc). It therefore
//! keeps its own interning table (same [`HashKey`] identity, same
//! first-write-wins rule, its own [`NodeId`] space) whose values are
//! `MemberDigest` representatives. A nested hashable container is named inside
//! its parent's representative by that [`NodeId`], so a `frozenset({True})`
//! and a `frozenset({1})` — one [`HashKey`] — collapse to one id, and a
//! `(naive, frozenset({True}))` member and an `(aware, frozenset({1}))` one
//! agree on that inner id, leaving the naive/aware sibling difference to
//! decide the match exactly as real `DeepDiff` decides it. Members are hashed
//! a-side in canonical order, then b-side, so the representative each Python
//! equality class settles on is deterministic.

use std::cell::RefCell;

use super::fxhash::HashMap;
use super::hash::{HashKey, ItemKey, MemberDigest, NodeId};

/// The per-top-level-diff caches described in this module's doc: container-pair
/// [`rough_distance`] results keyed by the `(removed, added)` [`ItemKey`]
/// pair, and the two hashable-node interning tables keyed by Python's own
/// equality ([`HashKey`]). Created in `crate::diff::diff_with_options`, threaded (by
/// shared reference, interior mutability) through the whole recursive diff,
/// and dropped when it returns. No eviction and no tuning knobs: both are
/// bounded by the number of distinct queries one diff makes.
///
/// [`rough_distance`]: super::distance::rough_distance
pub(crate) struct IgnoreOrderMemo {
    cache: RefCell<HashMap<(ItemKey, ItemKey), f64>>,
    /// Interns each distinct hashable-node identity (a tuple, or a frozenset
    /// hashed as a set member) to its place in `node_digests`, so a nested
    /// node can be named by one [`NodeId`] inside its parent's identity
    /// instead of by a copy of its own.
    node_ids: RefCell<HashMap<HashKey, NodeId>>,
    /// The digest assigned to each interned identity, indexed by
    /// [`NodeId::index`].
    node_digests: RefCell<Vec<ItemKey>>,
    /// The set-member interning table (see this module's "Set-member digests"
    /// section): the same [`HashKey`] identity as `node_ids`, but its own
    /// [`NodeId`] space and its own [`MemberDigest`] representatives, because a
    /// set member is compared by a positional digest rather than the
    /// order-insensitive [`ItemKey`] the list path hashes by.
    member_ids: RefCell<HashMap<HashKey, NodeId>>,
    /// The representative assigned to each interned set-member identity,
    /// indexed by its `member_ids` [`NodeId::index`].
    member_digests: RefCell<Vec<MemberDigest>>,
    enabled: bool,
}

impl IgnoreOrderMemo {
    /// A live cache (production path).
    pub(crate) fn new() -> Self {
        Self {
            cache: RefCell::new(HashMap::default()),
            node_ids: RefCell::new(HashMap::default()),
            node_digests: RefCell::new(Vec::new()),
            member_ids: RefCell::new(HashMap::default()),
            member_digests: RefCell::new(Vec::new()),
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
            node_ids: RefCell::new(HashMap::default()),
            node_digests: RefCell::new(Vec::new()),
            member_ids: RefCell::new(HashMap::default()),
            member_digests: RefCell::new(Vec::new()),
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

    /// Interns a hashable node's Python equality identity and returns its
    /// id together with its digest: the one already assigned to an earlier
    /// Python-equal node in this run, or `compute()`'s result, recorded for
    /// the rest of the run.
    ///
    /// See this module's "Hashable digests" section for why this cache is part
    /// of the observable behavior rather than a speed-up. Both the identity
    /// (whose nested nodes are named by id) and the digest (whose nested
    /// keys are shared through [`ItemKey::Tuple`]'s `Rc`) are `O(arity)` per
    /// node, so the two tables together stay linear in the number of
    /// nodes hashed rather than quadratic in nesting depth. `compute` runs
    /// with no borrow held, so it is free to recurse back into this same
    /// cache for a nested node. The `disabled()` cache still serves this
    /// method: it turns off *distance* memoization only, which is the one
    /// thing proven decision-neutral.
    pub(crate) fn intern_hashable(
        &self,
        key: HashKey,
        compute: impl FnOnce() -> ItemKey,
    ) -> (NodeId, ItemKey) {
        if let Some(&id) = self.node_ids.borrow().get(&key) {
            return (id, self.node_digests.borrow()[id.index()].clone());
        }

        let computed = compute();
        let mut digests = self.node_digests.borrow_mut();
        let id = NodeId::new(digests.len());
        digests.push(computed.clone());
        self.node_ids.borrow_mut().insert(key, id);
        (id, computed)
    }

    /// Interns a hashable set member's Python equality identity and returns its
    /// id together with its [`MemberDigest`] representative — the one already
    /// assigned to an earlier Python-equal member in this run, or `compute()`'s
    /// result. The set-member twin of [`Self::intern_hashable`], in its own
    /// [`NodeId`] space (see this module's "Set-member digests" section): a
    /// member is compared by a positional `MemberDigest`, not the
    /// order-insensitive [`ItemKey`] the list path stores. Each representative
    /// names its nested hashable children by their id, so both the identity and
    /// the representative are `O(arity)` per node and the table stays linear in
    /// the number of nodes hashed. `compute` runs with no borrow held, free to
    /// recurse back in for a nested member.
    pub(crate) fn intern_member(
        &self,
        key: HashKey,
        compute: impl FnOnce() -> MemberDigest,
    ) -> (NodeId, MemberDigest) {
        if let Some(&id) = self.member_ids.borrow().get(&key) {
            return (id, self.member_digests.borrow()[id.index()].clone());
        }

        let computed = compute();
        let mut digests = self.member_digests.borrow_mut();
        let id = NodeId::new(digests.len());
        digests.push(computed.clone());
        self.member_ids.borrow_mut().insert(key, id);
        (id, computed)
    }
}

/// Whether `key` is a container (list/tuple/dict) rather than a scalar — the
/// variants whose distance is computed by a recursive trial diff.
fn is_container(key: &ItemKey) -> bool {
    matches!(key, ItemKey::List(_) | ItemKey::Tuple(_) | ItemKey::Dict(_))
}
