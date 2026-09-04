//! Item hashing: the canonical equivalence key ([`ItemKey`]) and the
//! per-list hash table ([`HashedList`]) it feeds, matching `DeepHash`'s
//! default semantics for **item matching** under `ignore_order=True` — see
//! each type's own doc for the exact rules, and the parent module's doc for
//! how this fits into the algorithm end to end.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::lcs::{ScalarKey, python_scalar_key};
use crate::value::Value;

use super::IgnoreOrderMemo;
use super::fxhash::HashMap;

/// A canonical hash-equivalence key for one JSON value, matching
/// `DeepHash`'s default semantics for **item matching** under
/// `ignore_order=True` — deliberately **not** the same
/// equivalence [`crate::lcs::all_basic_scalars`]'s scalar-only ordered-list
/// matcher uses:
///
/// - **Numbers are type-tagged**: an `Int`, a `Float`, and a `Bool` never
///   share a key even at the same numeric value (`1`, `1.0`, and `true` are
///   three distinct keys) — the *opposite* of `crate::lcs::ScalarKey`'s
///   Python-`==` collapsing rule, and confirmed against real `DeepDiff`:
///   `[1]` vs `[1.0]` under `ignore_order=True` is `type_changes` (a real
///   pairing recurses and finds a type mismatch), unlike the *ordered* LCS
///   path's `[1]` vs `[1.0]` (which reports nothing at all — the two rules
///   are independent and both faithfully reproduced, in their own modules).
/// - **A nested list's key is order- and count-insensitive**: `[[1,2,3]]`
///   and `[[3,2,1]]` hash identically as list ELEMENTS (their `List` key is
///   a deduplicated `BTreeSet` of child keys), because `DeepHash`'s own
///   `ignore_iterable_order`/`ignore_repetition` default to `True`
///   regardless of the outer `DeepDiff`'s own `ignore_order` flag —
///   this can make two items with genuinely different *contents*
///   (different order, or different duplicate counts) compare as fully
///   "matched" (no report at all) once nested one level inside an
///   `ignore_order` list; that is real, confirmed `DeepDiff` behavior, not
///   a bug in this port.
/// - **A nested dict's key sorts by key**, recursively keying each value the
///   same way — dict *comparison* itself is never affected by
///   `ignore_order`, but a dict nested as a list *element* still
///   needs a canonical, insertion-order-independent key to be hashed at all.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ItemKey {
    Null,
    /// `true`/`false` — tagged distinctly from any integer of the same
    /// value (never collides with `Int`).
    Bool(bool),
    /// A JSON value `serde_json` parsed as an integer (no decimal point or
    /// exponent) — see [`crate::diff`]'s `python_type_name` for the same
    /// int/float split used throughout this crate. Always exact: a
    /// `serde_json::Number`'s non-float representation is always an `i64`
    /// or `u64`, both of which fit losslessly in `i128`.
    Int(i128),
    /// A JSON value `serde_json` parsed as a float, keyed by its exact bit
    /// pattern — kept as its own bucket even when whole-numbered (`5.0`
    /// never collides with `Int(5)`; see this type's own doc).
    Float(u64),
    Str(String),
    /// A `datetime`, keyed by its instant with a naive value read as UTC —
    /// `DeepHash._prep_datetime` runs `datetime_normalize` before formatting
    /// its digest string, so a naive and an aware value at the same moment
    /// hash identically and pair under `ignore_order`.
    DateTime(i64),
    /// A `date`, keyed by its ordinal, in its own bucket: `_prep_date`
    /// deliberately skips normalization and formats a bare `YYYY-MM-DD`,
    /// which can never collide with `_prep_datetime`'s
    /// `YYYY-MM-DD HH:MM:SS+00:00`, so a date and a datetime never pair.
    Date(i64),
    /// Order- and count-insensitive: see this type's own doc.
    List(BTreeSet<ItemKey>),
    /// A tuple, keyed exactly like [`ItemKey::List`] (order- and
    /// count-insensitive) but in its own bucket, so a tuple and a list
    /// holding the same items never hash-match — see this type's own doc.
    /// A *hashable* tuple can also inherit an earlier Python-equal tuple's
    /// key outright, which is `DeepHash`'s own cache behavior: see
    /// [`item_key`] and `super::memo`'s "Hashable digests" section.
    ///
    /// The entries sit behind an [`Rc`] so that a nested tuple's key is
    /// *shared* between the parent key that contains it and the digest cache
    /// that keeps it, instead of being deep-copied into each: a `D`-deep
    /// tuple nest costs `O(D)` keys in total rather than `O(D^2)`, and every
    /// clone of a tuple key — into a parent, into the cache, into a hash
    /// table — is a refcount bump. `Rc` derives the same `Eq`/`Ord`/`Hash`
    /// as the set it points at, so the key's *semantics* are exactly what
    /// holding the set inline gave. Only this variant needs the sharing;
    /// lists, sets and dicts are never cached, so theirs stay inline.
    Tuple(Rc<BTreeSet<ItemKey>>),
    /// A `set`, in its own bucket so it never hash-matches a list, a tuple
    /// or a `frozenset` holding the same members — confirmed against real
    /// `deepdiff==9.1.0`, where `[{1, 2}]` vs `[frozenset({1, 2})]` under
    /// `ignore_order` is a `type_changes` (a pairing that recursed), not an
    /// empty report. A `set` is unhashable in Python, so unlike
    /// [`ItemKey::FrozenSet`] it never participates in the digest cache and
    /// always keeps its own content key.
    Set(BTreeSet<ItemKey>),
    /// A `frozenset`, keyed like [`ItemKey::Set`] but in its own bucket.
    ///
    /// A `frozenset` *is* hashable in Python, so real `DeepDiff` lets it
    /// inherit an earlier Python-equal frozenset's digest the way a tuple
    /// does — which makes its result depend on which member of an equality
    /// class the process happened to hash first. `onix` deliberately does
    /// not reproduce that: a frozenset keys by its own membership, always.
    /// See `tests/golden/README.md`'s "Set iteration order" section.
    FrozenSet(BTreeSet<ItemKey>),
    /// Key-sorted, recursively keyed values.
    Dict(BTreeMap<String, ItemKey>),
}

/// One element of a hashable node's Python identity: a scalar by value
/// (numbers collapsed by Python `==` via [`python_scalar_key`]; a calendar
/// value by Python's own `datetime`/`date` equality — a naive value never
/// equal to an aware one, two aware values equal by instant, a `date` never
/// equal to a `datetime`), or a nested hashable node (a tuple or a frozenset)
/// by the id its own identity was interned to.
///
/// Referring to a nested node by id rather than by its whole identity is
/// what keeps this `O(arity)` per node: `((((1,),),),)` is four one-element
/// identities, not four identities of size 1, 2, 3 and 4.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum PyHashPart {
    Scalar(ScalarKey),
    Node(NodeId),
}

/// A hashable node's Python identity — the key `DeepHash`'s shared `hashes`
/// dict is looked up by (see `super::memo`'s "Hashable digests" section), so
/// it mirrors Python exactly. Scalars go through the crate's one definition of
/// Python scalar equality ([`python_scalar_key`], which makes `1`, `1.0` and
/// `True` one key), and a list, set or dict is unhashable, which also makes
/// any container holding one unhashable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum HashKey {
    /// A tuple, keyed **positionally** (Python's tuple equality is
    /// order-sensitive, unlike the order-insensitive *content* digest each
    /// node's [`ItemKey`] also carries).
    Tuple(Box<[PyHashPart]>),
    /// A frozenset, keyed by its **member set** (Python's frozenset equality
    /// is order- and repetition-insensitive; the `BTreeSet` collapses
    /// Python-equal members — `frozenset({1, True})` is `frozenset({1})`).
    FrozenSet(BTreeSet<PyHashPart>),
}

/// A hashable node identity's place in the run's interning table — see
/// [`super::IgnoreOrderMemo::intern_hashable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct NodeId(usize);

impl NodeId {
    /// The id for the entry at `index` (the interner's only constructor).
    pub(crate) fn new(index: usize) -> Self {
        Self(index)
    }

    /// This id's index into the digest table.
    pub(crate) fn index(self) -> usize {
        self.0
    }
}

/// One set member's membership digest — the value `_diff_set` compares
/// members by, computed as `DeepHash` computes it but kept **positional** for
/// a tuple (onix's one deliberate divergence; see [`set_member_digest`]).
///
/// A hashable container is compared by its own representative when it is the
/// member being matched (`Tuple`/`FrozenSet` here), but *named by its interned
/// id* ([`Self::Node`]) when it sits inside a parent — so a parent's digest is
/// `O(arity)` and two Python-equal nested containers, which share one id,
/// leave a parent's digest unchanged. See [`set_member_digest`] for the walk
/// that builds these.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemberDigest {
    /// A scalar leaf, by its content [`ItemKey`]: a number type-distinct
    /// (`1`/`1.0`/`True` stay apart), a calendar value by its instant/ordinal,
    /// null/str by value.
    Scalar(ItemKey),
    /// A nested hashable container (tuple or frozenset), named by its interned
    /// [`NodeId`] — one id per Python equality class, so a Python-equal nested
    /// container never changes its parent's digest.
    Node(NodeId),
    /// A hashable tuple, **positionally** (matching `tuple.__eq__`), used when
    /// the tuple is itself the member being matched.
    Tuple(Vec<MemberDigest>),
    /// A hashable frozenset, by membership, used when the frozenset is itself
    /// the member being matched.
    FrozenSet(BTreeSet<MemberDigest>),
    /// A `list`, which Python cannot hash — reachable through a `list` subclass
    /// with `__hash__`, keyed structurally (its own variant, since a list and a
    /// set holding the same members are not Python-equal).
    UnhashableList(Vec<MemberDigest>),
    /// A `set`, likewise unhashable and likewise in its own variant.
    UnhashableSet(Vec<MemberDigest>),
    /// A `dict`, likewise.
    UnhashableDict(BTreeMap<String, MemberDigest>),
}

/// The members of `a` that no member of `b` shares a digest with, and the
/// same the other way round — the whole of `_diff_set`'s comparison, and of
/// the distance mirror that counts what it would report.
///
/// Each member is reduced to one [`MemberDigest`] by [`set_member_digest`].
/// `a`'s members are digested (in [`crate::value::SetItems`]' canonical order)
/// before `b`'s, because the run's shared set-member cache is first-write-wins:
/// processing a deterministic order makes the representative each Python
/// equality class settles on deterministic too. Two members are the same
/// member exactly when their digests are equal.
///
/// Shared by [`crate::diff::set_diff`] and
/// [`crate::ignore_order::count_set_diff_leaves`] so the two can never drift
/// on what "the same member" means.
pub(crate) fn set_difference<'a>(
    a: &'a [Value],
    b: &'a [Value],
    memo: &IgnoreOrderMemo,
) -> (Vec<&'a Value>, Vec<&'a Value>) {
    let a_keys: Vec<MemberDigest> = a.iter().map(|v| set_member_digest(v, memo)).collect();
    let b_keys: Vec<MemberDigest> = b.iter().map(|v| set_member_digest(v, memo)).collect();

    let a_lookup: BTreeSet<&MemberDigest> = a_keys.iter().collect();
    let b_lookup: BTreeSet<&MemberDigest> = b_keys.iter().collect();

    let only_in = |items: &'a [Value], keys: &[MemberDigest], other: &BTreeSet<&MemberDigest>| {
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

/// The membership digest of one set member — the value `_diff_set` compares
/// members by, computed exactly as `DeepHash` computes it, by consulting the
/// run's shared set-member cache (`memo`) at every hashable node.
///
/// **Why the cache, not a standalone key.** `DeepHash._hash(obj)` first looks
/// `obj` up in a run-scoped cache keyed by `_make_hash_key(obj)` — which
/// type-wraps a bare *number* (`(type(obj), obj)`, so `1` and `1.0` never
/// share an entry) but leaves a *container* (tuple, frozenset) as its own
/// object, looked up by Python `==`/`hash`. On a hit it reuses the earlier
/// digest; on a miss it builds a content digest from the children's digests
/// (each of which also went through the cache) and stores it. Because both of
/// a comparison's set members are hashed against one shared cache
/// (`diff.py::_create_hashtable`), a node can take the content path at its
/// root and still hit the cache at a child — which is the whole subtlety this
/// reproduces. For example `{(naive, (1,))}` vs `{(aware, (1.0,))}` is empty
/// in real `DeepDiff`: the outer tuples are not Python-equal (`naive != aware`
/// blocks the cache), so each outer digest is built fresh — but the inner
/// `(1,)` and `(1.0,)` *are* Python-equal, share one interned id, and the two
/// datetimes normalize to one instant, so the two fresh outer digests coincide
/// anyway. A bare-number sibling instead of a nested tuple — `{(naive, 1)}` vs
/// `{(aware, 1.0)}` — is a removal plus an addition, because `1` and `1.0`
/// have their own type-distinct content ([`MemberDigest::Scalar`]) and no
/// cache entry to share. Confirmed against `deepdiff==9.1.0` (`TZ=UTC`,
/// `verbose_level=2`).
///
/// **Calendar values** are normalized to their instant in the content digest
/// (`_prep_datetime` runs `datetime_normalize` unconditionally; a `date` keys
/// by its ordinal and never joins a `datetime`'s class), and, in a node's
/// Python-equality key ([`PyHashPart::Scalar`] via [`python_scalar_key`]),
/// carry Python's own strict datetime equality — a naive value never equal to
/// an aware one, two aware values equal by instant, a `date` never equal to a
/// `datetime`. So a naive/aware difference blocks the whole-container cache
/// while still normalizing away inside a content digest, which is exactly the
/// split the examples above turn on.
///
/// **Frozensets** take part in the cache here (a frozenset is hashable in
/// Python), so a `frozenset({True})` and a `frozenset({1})` share one interned
/// id the way two Python-equal tuples do. This is the one place the digest
/// cache covers frozensets: in the *list*-item path ([`item_key`]) a frozenset
/// keeps its own content key, a documented divergence — see
/// [`ItemKey::FrozenSet`].
///
/// **One member kind still diverges from real `DeepDiff`, deliberately:** a
/// tuple or frozenset here matches by Python's own `==` (positional for a
/// tuple — [`MemberDigest::Tuple`] is a `Vec`, not the order-insensitive
/// [`ItemKey::Tuple`] — and by membership for a frozenset), where
/// `DeepHash._prep_iterable` hashes *every* iterable order- and
/// repetition-insensitively — so `(1, 2)` and `(2, 1)` share one digest in
/// real `_diff_set` but not here. Reproducing that would mean matching by
/// something other than Python `==`; this crate keeps the honest `==` answer.
/// See `tests/golden/README.md`'s "Set iteration order" section, its "A tuple
/// or a frozenset set member matches order- and repetition-insensitively"
/// point (and its "A naive and an aware datetime" point for why a same-instant
/// naive/aware pair is two members here, not one).
///
/// Iterative (an explicit heap work-stack, no native recursion over value
/// depth), because a set member's own nesting is not bounded by the engine's
/// traversal depth guards before this runs — so a natively recursive walk
/// would be an unguarded overflow sink on adversarially nested input. The
/// walk pushes each node before its children, so `order` holds parents before
/// children and each child's subtree is contiguous; assembling from the end
/// therefore always finds a node's own children as the last entries of
/// `built`, in their original order.
pub(crate) fn set_member_digest(root: &Value, memo: &IgnoreOrderMemo) -> MemberDigest {
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
            | Value::Date(_) => {}
        }
    }

    let mut built: Vec<NodeOut> = Vec::with_capacity(order.len());
    for value in order.iter().rev() {
        let out = match value {
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Str(_)
            | Value::DateTime(_)
            | Value::Date(_) => {
                let digest = MemberDigest::Scalar(scalar_content_key(value));
                let part = python_scalar_key(value)
                    .map(PyHashPart::Scalar)
                    .expect("python_scalar_key covers every scalar");
                NodeOut {
                    child_ref: digest.clone(),
                    rep: digest,
                    part: Some(part),
                }
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
                let refs = child_refs(&mut built, items.len());
                unhashable(MemberDigest::UnhashableList(refs))
            }
            Value::Set(items) => {
                let refs = child_refs(&mut built, items.len());
                unhashable(MemberDigest::UnhashableSet(refs))
            }
            Value::Object(map) => {
                let refs = child_refs(&mut built, map.len());
                unhashable(MemberDigest::UnhashableDict(
                    map.keys().map(str::to_owned).zip(refs).collect(),
                ))
            }
        };
        built.push(out);
    }

    built
        .pop()
        .expect("the walk pushes at least the root's own entry")
        .rep
}

/// One node's contribution to [`set_member_digest`]'s assembly.
struct NodeOut {
    /// The digest a *parent* embeds for this node: a nested hashable container
    /// collapses to [`MemberDigest::Node`], everything else is its own digest.
    child_ref: MemberDigest,
    /// This node's digest when it is itself the member being compared (the walk
    /// returns the root's `rep`). Equal to `child_ref` for every kind but a
    /// hashable container, where `rep` is the positional content and
    /// `child_ref` is the interned id.
    rep: MemberDigest,
    /// This node's Python-equality part for a parent's [`HashKey`]; `None` when
    /// the node is unhashable, which makes any container holding it unhashable.
    part: Option<PyHashPart>,
}

/// Which hashable container [`build_container`] assembles.
#[derive(Debug, Clone, Copy)]
enum ContainerKind {
    Tuple,
    FrozenSet,
}

/// Pops the last `count` built entries and returns their parent-facing
/// digests, in original child order.
fn child_refs(built: &mut Vec<NodeOut>, count: usize) -> Vec<MemberDigest> {
    built
        .split_off(built.len() - count)
        .into_iter()
        .map(|out| out.child_ref)
        .collect()
}

/// A node the cache never sees: it keeps its own digest and takes no part in a
/// parent's Python-equality key.
fn unhashable(digest: MemberDigest) -> NodeOut {
    NodeOut {
        child_ref: digest.clone(),
        rep: digest,
        part: None,
    }
}

/// [`set_member_digest`]'s tuple/frozenset case: assembles the positional (or
/// membership) content and the Python-equality key from the already-digested
/// `children`, then interns it in the run's shared set-member cache when it is
/// hashable (every child hashable) so a Python-equal node hashed earlier in
/// the run wins the representative. An unhashable child makes the node
/// unhashable: it keeps its own content and takes no part in the cache.
fn build_container(memo: &IgnoreOrderMemo, children: Vec<NodeOut>, kind: ContainerKind) -> NodeOut {
    let mut refs = Vec::with_capacity(children.len());
    let mut parts = Vec::with_capacity(children.len());
    let mut hashable = true;

    for child in children {
        refs.push(child.child_ref);
        match child.part {
            Some(part) => parts.push(part),
            None => hashable = false,
        }
    }

    let content = match kind {
        ContainerKind::Tuple => MemberDigest::Tuple(refs),
        ContainerKind::FrozenSet => MemberDigest::FrozenSet(refs.into_iter().collect()),
    };

    if !hashable {
        return unhashable(content);
    }

    let hash_key = match kind {
        ContainerKind::Tuple => HashKey::Tuple(parts.into_boxed_slice()),
        ContainerKind::FrozenSet => HashKey::FrozenSet(parts.into_iter().collect()),
    };
    let (id, rep) = memo.intern_member(hash_key, || content);
    NodeOut {
        child_ref: MemberDigest::Node(id),
        rep,
        part: Some(PyHashPart::Node(id)),
    }
}

/// The content digest of one scalar leaf, matching [`keyed`]'s own scalar
/// arms: numbers stay type-distinct, calendar values normalize to their
/// instant/ordinal.
fn scalar_content_key(value: &Value) -> ItemKey {
    match value {
        Value::Null => ItemKey::Null,
        Value::Bool(b) => ItemKey::Bool(*b),
        Value::Number(n) => number_key(n),
        Value::Str(s) => ItemKey::Str(s.to_string()),
        Value::DateTime(dt) => ItemKey::DateTime(dt.instant()),
        Value::Date(date) => ItemKey::Date(date.ordinal()),
        Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => unreachable!("scalar_content_key is only called on scalar leaves"),
    }
}

/// The type-distinct key for a bare number, mirroring [`ItemKey`]'s own
/// number rule (see [`keyed`]'s number branch for the signed-zero note).
fn number_key(n: &crate::value::Number) -> ItemKey {
    if n.is_f64() {
        let f = n
            .as_f64()
            .expect("Number::is_f64 guarantees as_f64 succeeds");
        return ItemKey::Float((f + 0.0).to_bits());
    }
    if let Some(i) = n.as_i64() {
        return ItemKey::Int(i128::from(i));
    }
    ItemKey::Int(i128::from(
        n.as_u64()
            .expect("a non-f64 Number always has an i64 or u64 repr"),
    ))
}

/// Computes `value`'s [`ItemKey`], consulting `memo` for every hashable
/// tuple it walks — a tuple that is Python-equal to one hashed earlier in
/// this diff inherits that tuple's key, exactly as `DeepHash`'s shared cache
/// makes it inherit its digest (see `super::memo`'s "Hashable digests" section
/// for the mechanism, the source citations, and why the ordering is
/// observable).
///
/// Recurses natively — safe only because
/// every caller in this module first proves `value`'s nesting is within the
/// crate's shared depth budget via [`check_value_depth`] (see this module's
/// "Depth safety" doc section).
pub(crate) fn item_key(value: &Value, memo: &IgnoreOrderMemo) -> ItemKey {
    keyed(value, memo, false).0
}

/// [`item_key`]'s recursion, additionally returning `value`'s Python
/// identity as one [`PyHashPart`] when the caller is a tuple that needs it
/// (`want_part`) and `value` is hashable.
///
/// The two are computed in one walk rather than two: a nested tuple's
/// identity is only knowable once it has been interned, which happens in the
/// same step that assigns its digest, so a second pass would re-walk (and
/// re-intern) every level. `want_part` is `false` everywhere except a
/// tuple's own elements, so a scalar sitting in a list or a dict never pays
/// for the Python-identity key it would not be asked for.
fn keyed(value: &Value, memo: &IgnoreOrderMemo, want_part: bool) -> (ItemKey, Option<PyHashPart>) {
    let part = || {
        want_part
            .then(|| python_scalar_key(value).map(PyHashPart::Scalar))
            .flatten()
    };

    match value {
        Value::Null => (ItemKey::Null, part()),
        Value::Bool(b) => (ItemKey::Bool(*b), part()),
        Value::Str(s) => (ItemKey::Str(s.to_string()), part()),
        Value::DateTime(value) => (ItemKey::DateTime(value.instant()), part()),
        Value::Date(value) => (ItemKey::Date(value.ordinal()), part()),
        Value::Number(n) => {
            let number = if n.is_f64() {
                let f = n
                    .as_f64()
                    .expect("Number::is_f64 guarantees as_f64 succeeds");
                // Normalize signed zeros so `-0.0` and `+0.0` produce one
                // key: Python's `DeepHash` treats them equal (confirmed
                // against `deepdiff==9.1.0`: `DeepDiff([0.0, -0.0], [],
                // ignore_order=True)` removes a single item). `f + 0.0` maps
                // `-0.0` to `+0.0` and is the identity on every other float,
                // so all other bit patterns are preserved: an integral float
                // like `2.0` keeps a distinct `Float` key from the integer
                // `2` (deepdiff reports that pairing as a `type_changes`,
                // never a hash match), which is why this deliberately does
                // NOT take the ordered path's `ScalarKey` integral-to-`Int`
                // canonicalization — the two paths have genuinely different
                // number semantics.
                ItemKey::Float((f + 0.0).to_bits())
            } else if let Some(i) = n.as_i64() {
                ItemKey::Int(i128::from(i))
            } else {
                let u = n
                    .as_u64()
                    .expect("a non-f64 serde_json::Number always has an i64 or u64 repr");
                ItemKey::Int(i128::from(u))
            };
            (number, part())
        }
        Value::Array(items) => (
            ItemKey::List(items.iter().map(|i| item_key(i, memo)).collect()),
            None,
        ),
        Value::Tuple(items) => tuple_keyed(items, memo),
        // Neither set kind consults the digest cache: a `set` is unhashable
        // in Python, and a `frozenset` is deliberately kept out of it (see
        // [`ItemKey::FrozenSet`]), so both key by content like a list does.
        Value::Set(items) => (
            ItemKey::Set(items.iter().map(|i| item_key(i, memo)).collect()),
            None,
        ),
        Value::FrozenSet(items) => (
            ItemKey::FrozenSet(items.iter().map(|i| item_key(i, memo)).collect()),
            None,
        ),
        Value::Object(map) => (
            ItemKey::Dict(
                map.iter()
                    .map(|(k, v)| (k.to_string(), item_key(v, memo)))
                    .collect(),
            ),
            None,
        ),
    }
}

/// [`keyed`]'s tuple case: keys every element first (bottom-up, so a nested
/// tuple is interned before this one asks for its id), then either hands the
/// assembled identity to the run's digest cache — where an earlier
/// Python-equal tuple's key wins — or, if any element was unhashable, keeps
/// the content key it just built.
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

    let (id, digest) = memo.intern_hashable(HashKey::Tuple(parts.into_boxed_slice()), || {
        ItemKey::Tuple(Rc::new(children))
    });
    (digest, Some(PyHashPart::Node(id)))
}

// ---------------------------------------------------------------------
// Per-list hash tables
// ---------------------------------------------------------------------

/// One list's items, hashed via [`item_key`] and reduced to first-occurrence
/// distinct entries — mirrors `DeepDiff`'s own `full_t1_hashtable`/
/// `full_t2_hashtable` (`_create_hashtable`, `{hash: (item, [indexes])}`).
/// Only the *first* index per distinct hash is kept: every
/// `report_repetition=False` code path in `deepdiff/diff.py` reads
/// `.indexes[0]` exclusively (confirmed by direct source reading), so a
/// hash's other occurrences are provably never used for anything in this
/// module's scope.
pub(crate) struct HashedList<'a> {
    /// Distinct keys, in first-occurrence (ascending original index) order
    /// — this is `SetOrdered(full_t{1,2}_hashtable.keys())`'s own iteration
    /// order (a Python dict's insertion order).
    pub(crate) distinct_order: Vec<ItemKey>,
    info: HashMap<ItemKey, (usize, &'a Value)>,
}

impl<'a> HashedList<'a> {
    /// Hashes `items` in index order, threading the run's `memo` so a
    /// hashable tuple's digest is shared with every Python-equal tuple hashed
    /// earlier in this diff — including in the *other* list's table, which is
    /// built by a second call with the same `memo`, exactly as `DeepDiff`
    /// builds its two hashtables against one shared `hashes` dict.
    pub(crate) fn build(items: &'a [Value], memo: &IgnoreOrderMemo) -> Self {
        let mut distinct_order = Vec::new();
        let mut info: HashMap<ItemKey, (usize, &'a Value)> = HashMap::default();

        for (idx, item) in items.iter().enumerate() {
            let key = item_key(item, memo);

            if let std::collections::hash_map::Entry::Vacant(entry) = info.entry(key.clone()) {
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
    /// Panics if `key` was never produced by [`Self::build`] on this same
    /// list. Every caller in this module only ever looks up a key drawn
    /// from `self.distinct_order` itself (or a `hashes_added`/
    /// `hashes_removed` slice filtered from it), so this can never actually
    /// fire.
    pub(crate) fn get(&self, key: &ItemKey) -> (usize, &'a Value) {
        self.info[key]
    }
}
