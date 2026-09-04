//! The caches one `ignore_order` diff shares across its whole run: the
//! pairwise subtree distances it has already computed, the digests it has
//! already assigned to hashable tuples for list-item matching, and the ids it
//! has assigned to set members.
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
//! numbers, so every other object — a tuple included — keys
//! the `hashes` dict as *itself*; `DeepHash._hash` reads that dict before
//! computing anything and writes its result back under the same key; and
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
//! [`super::hash::set_member_digest`] reduces each set member to one content id
//! ([`super::hash::RepId`]), reproducing `DeepHash`'s per-node cache decision
//! with two run-scoped tables. `node_table`
//! ([`super::hash::MemberHashKey`] → ([`super::hash::NodeId`], `RepId`)) is the
//! first-Python-equal-wins cache the tuple digests also use: a container
//! Python-equal to one hashed earlier in the run wins both ids, so `1` and
//! `1.0` inside an otherwise-equal tuple collapse. `member_content`
//! ([`super::hash::MemberContent`] → `RepId`) interns each distinct *content* —
//! children content ids plus, at a leaf, the type-distinct scalar [`ItemKey`],
//! with a `datetime` normalised to its instant — so a naive and an aware
//! datetime at one moment collapse to one content id.
//!
//! The two ids are distinct on purpose. A parent's Python-equality key names a
//! nested container by its `NodeId`, so `(naive,)` and `(aware,)` — different
//! `NodeId`s though one content id — keep `(1, (naive,))` and `(1.0, (aware,))`
//! Python-*un*equal, and those are then compared by content, where `1` and
//! `1.0` differ: exactly what `DeepDiff` reports. A parent's content and the
//! final comparison use the `RepId`, so a naive/aware difference that does not
//! break Python-equality of a wrapping tuple still collapses, at the root or
//! arbitrarily deep. A member is compared by its `RepId` — an `O(1)`,
//! stack-safe comparison, which matters because a set member's nesting is not
//! depth-guarded before this runs.
//!
//! **Both set-member tables are [`BTreeMap`]s, not `FxHash` maps** — a
//! deliberate, security-motivated exception to the rest of the crate. They are
//! keyed by *attacker-controlled member content* and reached for **every**
//! set/frozenset comparison, including with the default `ignore_order=false`.
//! `FxHash` uses a fixed, public seed and an invertible step, so collisions can
//! be crafted in closed form (see `super::fxhash`'s hash-flooding note); an
//! `FxHash` table here would let a crafted set drive interning to `O(n^2)` — a
//! denial-of-service vector. A `BTreeMap` has no hash to attack: lookups are
//! `O(log n)` in the worst case regardless of input, so the walk is
//! `O(n log n)`. The `FxHash` tables the crate keeps (`HashedList`, the tuple
//! digests, the distance memo) are only reached under `ignore_order=true` and
//! are the pairing hot path, so they stay on `FxHash`; their float-carrying keys
//! are protected from the *non-adversarial* bit-pattern collision of integral
//! and half-integer floats by mixing (see `crate::lcs::mix_float_bits`), but a
//! deterministic adversary is out of scope there.
//!
//! A tuple stays positional and a frozenset by membership (onix's one
//! deliberate divergence from `DeepHash`'s order-insensitive iterable hashing —
//! see [`super::hash::set_member_digest`]'s doc). Members are hashed a-side in
//! canonical order, then b-side, so the id each equality class settles on is
//! deterministic.

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::fxhash::HashMap;
use super::hash::{ItemKey, MemberContent, MemberHashKey, NodeId, PyHashKey, RepId, TupleId};

/// The per-top-level-diff caches described in this module's doc: container-pair
/// [`rough_distance`] results keyed by the `(removed, added)` [`ItemKey`]
/// pair, the tuple-digest interning table for list-item matching, and the two
/// set-member interning tables. Created in `crate::diff::diff_with_options`,
/// threaded (by shared reference, interior mutability) through the whole
/// recursive diff, and dropped when it returns. No eviction and no tuning
/// knobs: each is bounded by the number of distinct queries one diff makes.
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
    /// Set-member Python-equality cache (see this module's "Set-member digests"
    /// section): each distinct [`MemberHashKey`] gets a fresh [`NodeId`]
    /// (its Python-equality class) paired with its content [`RepId`], so a
    /// container Python-equal to one hashed earlier wins both — collapsing
    /// `1`/`1.0` — while a naive/aware difference keeps distinct `NodeId`s. A
    /// [`BTreeMap`], not an `FxHash` map: it is keyed by attacker-controlled
    /// content and reached with default options, so it must be
    /// collision-immune (module doc, "Set-member digests").
    node_table: RefCell<BTreeMap<MemberHashKey, (NodeId, RepId)>>,
    /// Set-member content interning: each distinct [`MemberContent`] gets one
    /// [`RepId`] (its `usize` index), so content-equal members — a naive and an
    /// aware datetime at one instant included — collapse to one id. A
    /// [`BTreeMap`] for the same collision-immunity reason as `node_table`.
    member_content: RefCell<BTreeMap<MemberContent, RepId>>,
    enabled: bool,
}

impl IgnoreOrderMemo {
    /// A live cache (production path).
    pub(crate) fn new() -> Self {
        Self {
            cache: RefCell::new(HashMap::default()),
            tuple_ids: RefCell::new(HashMap::default()),
            tuple_digests: RefCell::new(Vec::new()),
            node_table: RefCell::new(BTreeMap::new()),
            member_content: RefCell::new(BTreeMap::new()),
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
            node_table: RefCell::new(BTreeMap::new()),
            member_content: RefCell::new(BTreeMap::new()),
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

    /// Interns one set-member content identity to its [`RepId`] (its `usize`
    /// index): the id already assigned to an equal [`MemberContent`], or a
    /// fresh one. This is the content half of [`super::hash::set_member_digest`]
    /// (see this module's "Set-member digests" section) — where a naive and an
    /// aware datetime at one instant collapse, their `MemberContent::Scalar`
    /// being one and the same.
    pub(crate) fn content_rep(&self, content: MemberContent) -> RepId {
        let mut map = self.member_content.borrow_mut();
        if let Some(&id) = map.get(&content) {
            return id;
        }
        let id = RepId::new(map.len());
        map.insert(content, id);
        id
    }

    /// The Python-equality half of [`super::hash::set_member_digest`]: returns
    /// the ([`NodeId`], [`RepId`]) of the container Python-equal to `key` hashed
    /// earlier in the run (collapsing `1`/`1.0`), or, on a miss, a fresh
    /// `NodeId` paired with `content()`'s interned `RepId`, recorded under `key`.
    /// The `NodeId` is the container's Python-equality class (a parent names it
    /// by that, keeping a naive/aware difference distinct); the `RepId` is its
    /// content class (a parent's content and the final comparison use that,
    /// collapsing a naive/aware difference). `content` runs with no borrow held,
    /// free to recurse back in for a nested member.
    pub(crate) fn member_rep(
        &self,
        key: MemberHashKey,
        content: impl FnOnce() -> MemberContent,
    ) -> (NodeId, RepId) {
        if let Some(&pair) = self.node_table.borrow().get(&key) {
            return pair;
        }
        let rep = self.content_rep(content());
        let node = NodeId::new(self.node_table.borrow().len());
        self.node_table.borrow_mut().insert(key, (node, rep));
        (node, rep)
    }
}

/// Whether `key` is a container (list/tuple/dict) rather than a scalar — the
/// variants whose distance is computed by a recursive trial diff.
fn is_container(key: &ItemKey) -> bool {
    matches!(key, ItemKey::List(_) | ItemKey::Tuple(_) | ItemKey::Dict(_))
}
