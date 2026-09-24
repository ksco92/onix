//! The per-diff caches `ignore_order` pairing shares across one run:
//! container-pair distances, tuple digests for list-item matching, and
//! set-member digests for set/frozenset comparison. See
//! `docs/design/ignore-order.md`'s "Distance memo" section for the
//! caching rationale, soundness condition, and the digest rules.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::diff::{Resolution, Resolver};
use crate::value::Value;

use super::fxhash::HashMap;
use super::hash::{
    DistKey, ItemKey, MemberContent, MemberHashKey, NodeId, PyHashKey, RepId, TupleId,
};

/// A `(removed, added)` container-pair cache key, each side a value's
/// exact structural identity (not the order/repetition-insensitive
/// `ItemKey`). See `docs/design/ignore-order.md`.
type DistanceKey = (DistKey, DistKey);

/// The per-diff caches described in `docs/design/ignore-order.md`'s
/// "Distance memo" section: pairwise container distances, tuple
/// digests, and set-member digests, scoped to one diff run. `DistKey`
/// keys hash and compare by a full-tree walk; `MemberContent` keys
/// compare by one (it has no `Hash`); a `PyHashKey` lookup makes `O(log n)`
/// comparisons, each `O(the probed key's element bytes)`, a nested tuple
/// comparing as one id. A big-integer leaf costs
/// `O(digits)` per lookup; `super::fxhash`'s doc enumerates each key's cost.
pub(crate) struct IgnoreOrderMemo<'r> {
    cache: RefCell<HashMap<DistanceKey, f64>>,
    /// Interns each hashable-tuple identity to its digest, shared
    /// across the run — see `docs/design/ignore-order.md`'s "Distance
    /// memo" section. A `BTreeMap`, reached on the default path through a
    /// set member's tuple dict keys; its key type carries no `Hash` derive.
    tuple_ids: RefCell<BTreeMap<PyHashKey, TupleId>>,
    /// The digest assigned to each interned identity, indexed by
    /// [`TupleId::index`].
    tuple_digests: RefCell<Vec<ItemKey>>,
    /// Set-member Python-equality cache; see `docs/design/ignore-order.md`.
    /// A `BTreeMap`, not `FxHash`, reached on the default path against
    /// attacker-controlled content; its key type carries no `Hash` derive.
    node_table: RefCell<BTreeMap<MemberHashKey, (NodeId, RepId)>>,
    /// Set-member content interning; see `docs/design/ignore-order.md`.
    /// A `BTreeMap`, not `FxHash`, reached on the default path against
    /// attacker-controlled content; its key type carries no `Hash` derive.
    member_content: RefCell<BTreeMap<MemberContent, RepId>>,
    /// The caller's resolver (see [`crate::diff::diff_with_resolver`]), and
    /// what it returned for each token identity it was called with.
    resolver: RefCell<Option<&'r mut Resolver<'r>>>,
    resolutions: RefCell<BTreeMap<Box<str>, Option<Resolution<'r>>>>,
    enabled: bool,
    /// Count of [`Self::put`] calls, not just distinct entries: the
    /// signal that rises if a caller recomputes a distance instead of
    /// reusing the cache. Test-only.
    #[cfg(test)]
    puts: std::cell::Cell<usize>,
}

impl<'r> IgnoreOrderMemo<'r> {
    /// A live cache (production path).
    pub(crate) fn new() -> Self {
        Self {
            cache: RefCell::new(HashMap::default()),
            tuple_ids: RefCell::new(BTreeMap::new()),
            tuple_digests: RefCell::new(Vec::new()),
            node_table: RefCell::new(BTreeMap::new()),
            member_content: RefCell::new(BTreeMap::new()),
            resolver: RefCell::new(None),
            resolutions: RefCell::new(BTreeMap::new()),
            enabled: true,
            #[cfg(test)]
            puts: std::cell::Cell::new(0),
        }
    }

    /// A cache that never stores or reads, so a caller can run the same
    /// code path with memoization off.
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            cache: RefCell::new(HashMap::default()),
            tuple_ids: RefCell::new(BTreeMap::new()),
            tuple_digests: RefCell::new(Vec::new()),
            node_table: RefCell::new(BTreeMap::new()),
            member_content: RefCell::new(BTreeMap::new()),
            resolver: RefCell::new(None),
            resolutions: RefCell::new(BTreeMap::new()),
            enabled: false,
            puts: std::cell::Cell::new(0),
        }
    }

    /// A live memo that compares a token as the value `resolver` returns for
    /// it.
    pub(crate) fn with_resolver(resolver: &'r mut Resolver<'r>) -> Self {
        IgnoreOrderMemo {
            resolver: RefCell::new(Some(resolver)),
            ..IgnoreOrderMemo::new()
        }
    }

    /// Whether the pair `(a, b)` reports nothing without a walk: the identical
    /// Python object on both sides, or a cycle token on the first, as
    /// `DeepDiff`'s `t1 is t2` and `parents_ids` checks skip them.
    pub(crate) fn skips(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) if x.same_instance(y) => true,
            (Value::Object(x), _) => x.is_cycle(),
            _ => false,
        }
    }

    /// The value the diff compares in place of the token `value` against
    /// `other`, `None` when `value` is not a token or stays one. A cycle
    /// token resolves only against an object of the class it points back
    /// at, unless `any_other` is set.
    pub(crate) fn resolve(
        &self,
        value: &Value,
        other: &Value,
        any_other: bool,
    ) -> Option<Resolution<'r>> {
        let Value::Object(token) = value else {
            return None;
        };
        let identity = token.token_identity()?;
        let known = self.resolutions.borrow().get(identity).cloned();
        let resolution = known.unwrap_or_else(|| {
            let resolution = self
                .resolver
                .borrow_mut()
                .as_mut()
                .and_then(|resolver| resolver(identity));
            self.resolutions
                .borrow_mut()
                .insert(Box::from(identity), resolution.clone());
            resolution
        })?;
        if token.is_cycle() && !any_other && !crate::value::same_class(&resolution, other) {
            return None;
        }
        Some(resolution)
    }

    /// Whether distance memoization is live for this run; a candidate
    /// pair is cached only when both sides are containers
    /// ([`is_container`]).
    pub(crate) fn caching_enabled(&self) -> bool {
        self.enabled
    }

    /// The number of distinct container-pair distances currently memoized.
    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.cache.borrow().len()
    }

    /// The number of times [`Self::put`] has run. Test-only.
    #[cfg(test)]
    pub(crate) fn put_count(&self) -> usize {
        self.puts.get()
    }

    /// The cached distance for `key`, if present.
    pub(crate) fn get(&self, key: &DistanceKey) -> Option<f64> {
        self.cache.borrow().get(key).copied()
    }

    /// Records `value` for `key` (moving the already-cloned key in).
    pub(crate) fn put(&self, key: DistanceKey, value: f64) {
        #[cfg(test)]
        self.puts.set(self.puts.get() + 1);
        self.cache.borrow_mut().insert(key, value);
    }

    /// Interns a hashable tuple's Python equality identity and returns its
    /// id together with its digest, computing it on a first sighting. See
    /// `docs/design/ignore-order.md`'s "Distance memo" section.
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

    /// Interns one set-member content identity to its [`RepId`]: the id
    /// already assigned to an equal [`MemberContent`], or a fresh one.
    /// See `docs/design/ignore-order.md`'s "Distance memo" section.
    pub(crate) fn content_rep(&self, content: MemberContent) -> RepId {
        let mut map = self.member_content.borrow_mut();
        if let Some(&id) = map.get(&content) {
            return id;
        }
        let id = RepId::new(map.len());
        map.insert(content, id);
        id
    }

    /// The Python-equality half of [`super::hash::set_member_digest`]: the
    /// `(NodeId, RepId)` of the container Python-equal to `key` hashed
    /// earlier in the run, or a fresh pair on a miss. See
    /// `docs/design/ignore-order.md`'s "Distance memo" section.
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

/// Whether `key` is a container (list/tuple/dict/custom object) rather than a
/// scalar — the variants whose distance is computed by a recursive trial
/// diff, and so the only ones worth memoizing.
pub(crate) fn is_container(key: &ItemKey) -> bool {
    matches!(
        key,
        ItemKey::List(_) | ItemKey::Tuple(_) | ItemKey::Dict(_) | ItemKey::Object(..)
    )
}
