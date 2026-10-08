//! Item hashing: the canonical equivalence key ([`ItemKey`]) and per-list hash table
//! ([`HashedList`]) matching `DeepHash`'s item matching under `ignore_order=True`; see
//! `docs/design/ignore-order.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use num_bigint::BigInt;

use crate::lcs::{ScalarKey, mix_float_bits, python_scalar_key};
use crate::value::{ObjectKind, Value};

use super::IgnoreOrderMemo;
use super::fxhash::HashMap;

// ---------------------------------------------------------------------
// Distance-memo cache key
// ---------------------------------------------------------------------

/// The distance memo's cache key for one side of a candidate pair: the value's exact structural
/// identity (`Value`'s `PartialEq`), not [`ItemKey`], which ignores the order and repetition the
/// distance reads (issue #31).
#[derive(Clone)]
pub(crate) struct DistKey(Rc<Value>);

impl DistKey {
    /// Clones `value` into a shared key; the whole value is hashed and compared per probe (cost:
    /// [`IgnoreOrderMemo`]'s doc). The clone recurses natively, bounded by the
    /// [`crate::diff::check_value_depth`] pre-pass.
    pub(crate) fn new(value: &Value) -> Self {
        Self(Rc::new(value.clone()))
    }

    /// Wraps an already-owned value with no clone — the test hook that lets the
    /// stack-safety probe hash a value deeper than a native clone could build.
    #[cfg(test)]
    pub(crate) fn from_rc(value: Rc<Value>) -> Self {
        Self(value)
    }
}

impl PartialEq for DistKey {
    fn eq(&self, other: &Self) -> bool {
        // Iterative structural equality, stack-safe on deep values.
        self.0 == other.0
    }
}

impl Eq for DistKey {}

impl Hash for DistKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_value(&self.0, state);
    }
}

/// Hashes a value consistently with its structural `PartialEq` (equal values hash equal) for
/// [`DistKey`] (issue #31). Iterative; nesting is user-controlled up to `max_depth`, so a
/// recursive hasher would overflow the native stack.
fn hash_value<H: Hasher>(root: &Value, state: &mut H) {
    let mut stack: Vec<&Value> = vec![root];
    while let Some(value) = stack.pop() {
        core::mem::discriminant(value).hash(state);
        match value {
            Value::Null => {}
            Value::Bool(b) => b.hash(state),
            Value::Number(n) => number_key(n).hash(state),
            Value::Str(s) => s.hash(state),
            Value::DateTime(dt) => dt.instant().hash(state),
            Value::Date(date) => date.ordinal().hash(state),
            // Awareness first, then the instant `times_equal` compares within one bucket.
            Value::Time(time) => {
                time.utc_offset_seconds().is_some().hash(state);
                time.sort_instant().hash(state);
            }
            Value::TimeDelta(value) => value.hash(state),
            Value::Array(items) | Value::Tuple(items) => {
                items.len().hash(state);
                stack.extend(items.iter());
            }
            Value::Set(items) | Value::FrozenSet(items) => {
                items.len().hash(state);
                stack.extend(items.iter());
            }
            Value::Object(map) => {
                map.len().hash(state);
                // Tagged apart from a `dict` by kind and class `__name__`; same-named classes
                // collide here and equality separates them.
                map.is_custom_object().hash(state);
                map.type_name().hash(state);
                // A non-`str` key is pushed onto the work-stack: `ObjectKey` has no `Hash`.
                for (key, _) in map {
                    match key {
                        crate::value::ObjectKey::Str(s) => s.hash(state),
                        crate::value::ObjectKey::Other(value) => stack.push(value),
                    }
                }
                stack.extend(map.values());
            }
        }
    }
}

/// A canonical hash-equivalence key for one value under `ignore_order=True`, matching
/// `DeepHash`'s item matching, not [`crate::lcs::all_basic_scalars`]'s:
///
/// - Numbers are type-tagged: `1`, `1.0` and `true` are three keys.
/// - A nested list's key is order- and count-insensitive (`DeepHash`'s defaults), so `[[1, 2]]`
///   and `[[2, 1]]` match.
/// - A nested dict's key sorts by key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ItemKey {
    Null,
    /// Tagged apart from any integer.
    Bool(bool),
    /// An integer that fits `i128`; every `i64` and `u64` lands here.
    Int(i128),
    /// An integer beyond `i128`, boxed so this arm does not widen the enum.
    BigInt(Box<BigInt>),
    /// A float by [`deephash_float_bits`]; `5.0` never collides with `Int(5)`.
    Float(u64),
    /// WTF-8 bytes, so a lone surrogate keeps a distinct key.
    Str(Vec<u8>),
    /// By instant, a naive value read as UTC, as `DeepHash` normalizes it.
    DateTime(i64),
    /// By ordinal, in its own bucket: a date never pairs with a datetime.
    Date(i64),
    /// By [`crate::datetime::Time::hash_seconds_of_day`]: microseconds and offset are dropped
    /// (`docs/design/value-model.md`).
    Time(i64),
    /// By exact value.
    TimeDelta(crate::datetime::TimeDelta),
    /// Order- and count-insensitive: see this type's own doc.
    List(BTreeSet<ItemKey>),
    /// Keyed like [`ItemKey::List`] in its own bucket. A hashable tuple can inherit an earlier
    /// Python-equal tuple's key (`docs/design/ignore-order.md`, "Distance memo"). Behind an
    /// [`Rc`] so a nested key is shared with the digest cache: `O(D)` keys for a `D`-deep nest.
    Tuple(Rc<BTreeSet<ItemKey>>),
    /// In its own bucket, so it never matches a list, tuple or frozenset of the same members.
    /// Unhashable in Python, so it never uses the digest cache.
    Set(BTreeSet<ItemKey>),
    /// Keyed like [`ItemKey::Set`], always by its own membership, never a cached digest
    /// (`tests/golden/README.md`, "Set iteration order").
    FrozenSet(BTreeSet<ItemKey>),
    /// Key-sorted; keys are keyed recursively, so a non-`str` key or a lone surrogate works.
    Dict(BTreeMap<ItemKey, ItemKey>),
    /// A custom object keyed like [`ItemKey::Dict`], tagged by class `__name__` as
    /// `DeepHash._prep_obj` does; classes sharing a name match when attributes do. Cost:
    /// `fxhash.rs`.
    Object(Box<str>, BTreeMap<ItemKey, ItemKey>),
    /// An [`ObjectKind::Opaque`](crate::value::ObjectKind) token, by its Python object's identity
    /// (cost: `fxhash.rs`).
    Opaque(Box<str>),
}

/// Hand-written so the `Float` arm runs through [`mix_float_bits`]; every other arm hashes as
/// the derive would.
impl std::hash::Hash for ItemKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            Self::Null => {}
            Self::Bool(b) => b.hash(state),
            Self::Int(i) => i.hash(state),
            Self::BigInt(b) => b.hash(state),
            Self::Float(bits) => mix_float_bits(*bits).hash(state),
            Self::Str(s) => s.hash(state),
            Self::DateTime(instant) => instant.hash(state),
            Self::Date(ordinal) => ordinal.hash(state),
            Self::Time(seconds_of_day) => seconds_of_day.hash(state),
            Self::TimeDelta(value) => value.hash(state),
            Self::List(items) | Self::Set(items) | Self::FrozenSet(items) => items.hash(state),
            Self::Tuple(items) => items.hash(state),
            Self::Dict(map) => map.hash(state),
            Self::Opaque(identity) => identity.hash(state),
            Self::Object(class, map) => {
                class.hash(state);
                map.hash(state);
            }
        }
    }
}

/// One element of a hashable tuple's Python identity: a scalar by value, or a nested tuple by
/// its interned id, which keeps the key `O(arity)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PyHashPart {
    Scalar(ScalarKey),
    Tuple(TupleId),
}

/// A hashable tuple's Python identity, positional: the key `DeepHash`'s shared cache is looked
/// up by. Scalars go through [`python_scalar_key`]; a list or dict makes the tuple unhashable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PyHashKey(Box<[PyHashPart]>);

/// A hashable tuple identity's id in the run's interning table
/// ([`super::IgnoreOrderMemo::tuple_digest`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TupleId(usize);

impl TupleId {
    /// The id for the entry at `index` (the interner's only constructor).
    pub(crate) fn new(index: usize) -> Self {
        Self(index)
    }

    /// This id's index into the digest table.
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

// ---------------------------------------------------------------------
// Set-member digests
// ---------------------------------------------------------------------

/// A set member's content digest, an id into the run's interning table ([`set_member_digest`]).
/// Equal ids mean the same member, so no comparison recurses into a member's structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RepId(usize);

impl RepId {
    /// The id for the entry at `index` (the interner's only constructor).
    pub(crate) fn new(index: usize) -> Self {
        Self(index)
    }
}

/// A set member's Python-equality class id, distinct from its content [`RepId`]: a naive and an
/// aware datetime inside a tuple share content but not a `NodeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NodeId(usize);

impl NodeId {
    /// The id for the entry at `index` (the interner's only constructor).
    pub(crate) fn new(index: usize) -> Self {
        Self(index)
    }
}

/// One element of a [`MemberHashKey`]: a scalar by Python `==` ([`python_scalar_key`]; a naive
/// datetime stays distinct from an aware one), or a nested container by its [`NodeId`], not its
/// content [`RepId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemberPart {
    Scalar(ScalarKey),
    Node(NodeId),
}

/// A set member's Python-equality identity, the key of `DeepHash`'s shared cache: a tuple is
/// positional, a frozenset by membership.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemberHashKey {
    Tuple(Box<[MemberPart]>),
    FrozenSet(BTreeSet<MemberPart>),
}

/// A set member's content identity, keyed by children's [`RepId`]s and, at a leaf, the scalar
/// [`ItemKey`]. A datetime normalizes to its instant; a tuple stays positional and a frozenset
/// by membership (`tests/golden/README.md`, "Set iteration order").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemberContent {
    /// A scalar leaf, type-distinct.
    Scalar(ItemKey),
    /// A hashable tuple, positional.
    Tuple(Vec<RepId>),
    /// A hashable frozenset, by membership.
    FrozenSet(BTreeSet<RepId>),
    /// A `list` (unhashable; reachable through a `list` subclass with `__hash__`).
    UnhashableList(Vec<RepId>),
    /// A `set`.
    UnhashableSet(Vec<RepId>),
    /// A `dict`, keyed by each key's [`ItemKey`]; comparing two walks those trees (cost:
    /// `fxhash.rs`).
    UnhashableDict(BTreeMap<ItemKey, RepId>),
}

/// The members of `a` that no member of `b` shares a digest with, and the reverse: the whole of
/// `_diff_set`'s comparison and of its distance mirror. Two members are the same exactly when
/// their [`RepId`]s are equal: a Python-equal container hashed earlier in the run lends its id,
/// otherwise content decides (a datetime by its instant). `a` is digested before `b` because the
/// shared cache is first-write-wins.
pub(crate) fn set_difference<'a>(
    a: &'a [Value],
    b: &'a [Value],
    memo: &IgnoreOrderMemo,
) -> (Vec<&'a Value>, Vec<&'a Value>) {
    let a_keys: Vec<RepId> = a.iter().map(|v| set_member_digest(v, memo)).collect();
    let b_keys: Vec<RepId> = b.iter().map(|v| set_member_digest(v, memo)).collect();

    let a_lookup: BTreeSet<RepId> = a_keys.iter().copied().collect();
    let b_lookup: BTreeSet<RepId> = b_keys.iter().copied().collect();

    let only_in = |items: &'a [Value], keys: &[RepId], other: &BTreeSet<RepId>| {
        items
            .iter()
            .zip(keys)
            .filter(|(_, key)| !other.contains(key))
            .map(|(item, _)| item)
            .collect()
    };

    (
        only_in(a, &a_keys, &b_lookup),
        only_in(b, &b_keys, &a_lookup),
    )
}

/// The digest of one set member as a single [`RepId`], through the run's Python-equality and
/// content tables (`docs/design/ignore-order.md`, "Distance memo"). Iterative: member nesting
/// is not depth-checked before this runs.
pub(crate) fn set_member_digest(root: &Value, memo: &IgnoreOrderMemo) -> RepId {
    let mut order: Vec<&Value> = Vec::new();
    let mut stack: Vec<&Value> = vec![root];

    while let Some(value) = stack.pop() {
        order.push(value);
        match value {
            Value::Array(items) | Value::Tuple(items) => stack.extend(items.iter()),
            Value::Set(items) | Value::FrozenSet(items) => stack.extend(items.iter()),
            Value::Object(map) => stack.extend(map.values()),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Str(_)
            | Value::DateTime(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::TimeDelta(_) => {}
        }
    }

    // Each entry: the node's content id and its Python-equality part (`None` when unhashable,
    // which makes any container holding it unhashable). `order` holds parents before children,
    // so a node's children are the last entries of `built`.
    let mut built: Vec<(RepId, Option<MemberPart>)> = Vec::with_capacity(order.len());
    for value in order.iter().rev() {
        let out = match value {
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Str(_)
            | Value::DateTime(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::TimeDelta(_) => {
                let rep = memo.content_rep(MemberContent::Scalar(scalar_content_key(value)));
                let part = python_scalar_key(value)
                    .map(MemberPart::Scalar)
                    .expect("python_scalar_key covers every scalar");
                (rep, Some(part))
            }
            Value::Tuple(items) => {
                let children = built.split_off(built.len() - items.len());
                build_container(memo, children, ContainerKind::Tuple)
            }
            Value::FrozenSet(items) => {
                let children = built.split_off(built.len() - items.len());
                build_container(memo, children, ContainerKind::FrozenSet)
            }
            Value::Array(items) => {
                let reps = child_reps(&mut built, items.len());
                (memo.content_rep(MemberContent::UnhashableList(reps)), None)
            }
            Value::Set(items) => {
                let reps = child_reps(&mut built, items.len());
                (memo.content_rep(MemberContent::UnhashableSet(reps)), None)
            }
            Value::Object(map) => {
                let reps = child_reps(&mut built, map.len());
                let content = MemberContent::UnhashableDict(
                    map.keys()
                        .map(|key| object_key_item_key(key, memo))
                        .zip(reps)
                        .collect(),
                );
                (memo.content_rep(content), None)
            }
        };
        built.push(out);
    }

    built
        .pop()
        .expect("the walk pushes at least the root's own entry")
        .0
}

/// Which hashable container [`build_container`] assembles.
#[derive(Debug, Clone, Copy)]
enum ContainerKind {
    Tuple,
    FrozenSet,
}

/// Pops the last `count` built entries and returns their content [`RepId`]s,
/// in original child order.
fn child_reps(built: &mut Vec<(RepId, Option<MemberPart>)>, count: usize) -> Vec<RepId> {
    built
        .split_off(built.len() - count)
        .into_iter()
        .map(|(rep, _)| rep)
        .collect()
}

/// [`set_member_digest`]'s tuple/frozenset case: a hashable node's id comes from the run's shared
/// cache, so a Python-equal node hashed earlier wins it; otherwise it is content-only and takes
/// no part in a parent's key.
fn build_container(
    memo: &IgnoreOrderMemo,
    children: Vec<(RepId, Option<MemberPart>)>,
    kind: ContainerKind,
) -> (RepId, Option<MemberPart>) {
    let mut reps = Vec::with_capacity(children.len());
    let mut parts = Vec::with_capacity(children.len());
    let mut hashable = true;

    for (rep, part) in children {
        reps.push(rep);
        match part {
            Some(part) => parts.push(part),
            None => hashable = false,
        }
    }

    let content = |reps: Vec<RepId>| match kind {
        ContainerKind::Tuple => MemberContent::Tuple(reps),
        ContainerKind::FrozenSet => MemberContent::FrozenSet(reps.into_iter().collect()),
    };

    if !hashable {
        return (memo.content_rep(content(reps)), None);
    }

    let hash_key = match kind {
        ContainerKind::Tuple => MemberHashKey::Tuple(parts.into_boxed_slice()),
        ContainerKind::FrozenSet => MemberHashKey::FrozenSet(parts.into_iter().collect()),
    };
    let (node, rep) = memo.member_rep(hash_key, || content(reps));
    (rep, Some(MemberPart::Node(node)))
}

/// The content digest of one scalar leaf, matching [`keyed`]'s scalar arms.
fn scalar_content_key(value: &Value) -> ItemKey {
    match value {
        Value::Null => ItemKey::Null,
        Value::Bool(b) => ItemKey::Bool(*b),
        Value::Number(n) => number_key(n),
        Value::Str(s) => ItemKey::Str(s.as_bytes().to_vec()),
        Value::DateTime(dt) => ItemKey::DateTime(dt.instant()),
        Value::Date(date) => ItemKey::Date(date.ordinal()),
        Value::Time(time) => ItemKey::Time(time.hash_seconds_of_day()),
        Value::TimeDelta(value) => ItemKey::TimeDelta(value.value()),
        Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => unreachable!("scalar_content_key is only called on scalar leaves"),
    }
}

/// The bit pattern [`ItemKey::Float`] keys a float by: signed zero folded
/// ([`crate::value::fold_signed_zero`]), and one fixed representative for every `NaN`, since
/// `DeepHash` digests a `NaN` by `str()`, the same three characters whatever its bits.
fn deephash_float_bits(f: f64) -> u64 {
    if f.is_nan() {
        f64::NAN.to_bits()
    } else {
        crate::value::fold_signed_zero(f).to_bits()
    }
}

/// The type-distinct key for a bare number ([`deephash_float_bits`] for a float).
fn number_key(n: &crate::value::Number) -> ItemKey {
    if n.is_f64() {
        let f = n
            .as_f64()
            .expect("Number::is_f64 guarantees as_f64 succeeds");
        return ItemKey::Float(deephash_float_bits(f));
    }
    if let Some(i) = n.as_i128() {
        return ItemKey::Int(i);
    }
    ItemKey::BigInt(Box::new(
        n.as_big()
            .expect("a non-float Number that overflows i128 is an arbitrary-precision integer")
            .clone(),
    ))
}

/// Computes `value`'s [`ItemKey`]; a tuple Python-equal to one hashed earlier in the diff inherits
/// its key through `memo` (`docs/design/ignore-order.md`, "Distance memo"). Recurses natively:
/// callers first prove the nesting within [`check_value_depth`](crate::diff::check_value_depth)
/// (`docs/design/ignore-order.md`, "Depth safety").
pub(crate) fn item_key(value: &Value, memo: &IgnoreOrderMemo) -> ItemKey {
    keyed(value, memo, false).0
}

/// [`item_key`]'s recursion, also returning the value's [`PyHashPart`] when `want_part` is set (a
/// tuple's own elements) and it is hashable; one walk, since a nested tuple's identity is known
/// only once it is interned.
fn keyed(value: &Value, memo: &IgnoreOrderMemo, want_part: bool) -> (ItemKey, Option<PyHashPart>) {
    let part = || {
        want_part
            .then(|| python_scalar_key(value).map(PyHashPart::Scalar))
            .flatten()
    };

    match value {
        Value::Null => (ItemKey::Null, part()),
        Value::Bool(b) => (ItemKey::Bool(*b), part()),
        Value::Str(s) => (ItemKey::Str(s.as_bytes().to_vec()), part()),
        Value::DateTime(value) => (ItemKey::DateTime(value.instant()), part()),
        Value::Date(value) => (ItemKey::Date(value.ordinal()), part()),
        Value::Time(value) => (ItemKey::Time(value.hash_seconds_of_day()), part()),
        Value::TimeDelta(value) => (ItemKey::TimeDelta(value.value()), part()),
        Value::Number(n) => (number_key(n), part()),
        Value::Array(items) => (
            ItemKey::List(items.iter().map(|i| item_key(i, memo)).collect()),
            None,
        ),
        Value::Tuple(items) => tuple_keyed(items, memo),
        // Neither set kind consults the digest cache.
        Value::Set(items) => (
            ItemKey::Set(items.iter().map(|i| item_key(i, memo)).collect()),
            None,
        ),
        Value::FrozenSet(items) => (
            ItemKey::FrozenSet(items.iter().map(|i| item_key(i, memo)).collect()),
            None,
        ),
        Value::Object(map) => {
            if let Some(identity) = map
                .opaque_identity()
                .filter(|_| map.kind() == ObjectKind::Opaque)
            {
                return (ItemKey::Opaque(Box::from(identity)), None);
            }
            let attrs = map
                .iter()
                .map(|(k, v)| (object_key_item_key(k, memo), item_key(v, memo)))
                .collect();
            // A `dict` subclass keys as a bare `dict`, as `DeepHash` digests it.
            let key = if matches!(map.kind(), ObjectKind::CustomObject | ObjectKind::Failed) {
                ItemKey::Object(Box::from(map.type_name().unwrap_or_default()), attrs)
            } else {
                ItemKey::Dict(attrs)
            };
            (key, None)
        }
    }
}

/// One [`ObjectKey`](crate::value::ObjectKey)'s [`ItemKey`], shared by [`keyed`]'s dict case and
/// [`set_member_digest`]'s so `ItemKey::Dict` and `MemberContent::UnhashableDict` key a dict
/// identically. `ObjectKey` has no `Hash` derive; every content hash of one goes through here.
fn object_key_item_key(key: &crate::value::ObjectKey, memo: &IgnoreOrderMemo) -> ItemKey {
    match key {
        crate::value::ObjectKey::Str(s) => ItemKey::Str(s.as_bytes().to_vec()),
        crate::value::ObjectKey::Other(value) => item_key(value, memo),
    }
}

/// [`keyed`]'s tuple case: keys elements bottom-up, then takes the key from the digest cache (an
/// earlier Python-equal tuple's wins), or keeps the content key if any element is unhashable.
fn tuple_keyed(items: &[Value], memo: &IgnoreOrderMemo) -> (ItemKey, Option<PyHashPart>) {
    let mut children = BTreeSet::new();
    let mut parts = Vec::with_capacity(items.len());
    let mut hashable = true;

    for item in items {
        let (child_key, child_part) = keyed(item, memo, hashable);
        children.insert(child_key);
        match child_part {
            Some(child_part) => parts.push(child_part),
            None => hashable = false,
        }
    }

    if !hashable {
        return (ItemKey::Tuple(Rc::new(children)), None);
    }

    let (id, digest) = memo.tuple_digest(PyHashKey(parts.into_boxed_slice()), || {
        ItemKey::Tuple(Rc::new(children))
    });
    (digest, Some(PyHashPart::Tuple(id)))
}

// ---------------------------------------------------------------------
// Per-list hash tables
// ---------------------------------------------------------------------

/// One list's items hashed via [`item_key`] and reduced to first-occurrence distinct entries, as
/// `DeepDiff`'s `_create_hashtable` does; no `report_repetition=False` path reads another index.
pub(crate) struct HashedList<'a> {
    /// Distinct keys in first-occurrence order, shared by [`Rc`] with [`Self::info`].
    pub(crate) distinct_order: Vec<Rc<ItemKey>>,
    info: HashMap<Rc<ItemKey>, (usize, &'a Value)>,
}

impl<'a> HashedList<'a> {
    /// Hashes `items` in index order with the run's `memo`, so a hashable tuple's digest is shared
    /// with the other list's table, built by a second call.
    pub(crate) fn build(items: &'a [Value], memo: &IgnoreOrderMemo) -> Self {
        let mut distinct_order = Vec::new();
        let mut info: HashMap<Rc<ItemKey>, (usize, &'a Value)> = HashMap::default();

        for (idx, item) in items.iter().enumerate() {
            let key = Rc::new(item_key(item, memo));

            if let std::collections::hash_map::Entry::Vacant(entry) = info.entry(Rc::clone(&key)) {
                distinct_order.push(key);
                entry.insert((idx, item));
            }
        }

        Self {
            distinct_order,
            info,
        }
    }

    pub(crate) fn contains(&self, key: &ItemKey) -> bool {
        self.info.contains_key(key)
    }

    /// The first-occurrence `(index, value)` for `key`.
    ///
    /// # Panics
    ///
    /// Panics if `key` is not in this list's table.
    pub(crate) fn get(&self, key: &ItemKey) -> (usize, &'a Value) {
        self.info[key]
    }
}
