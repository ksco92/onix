//! Item hashing: the canonical equivalence key ([`ItemKey`]) and the
//! per-list hash table ([`HashedList`]) it feeds, matching `DeepHash`'s
//! default semantics for **item matching** under `ignore_order=True` — see
//! each type's own doc for the exact rules, and the parent module's doc for
//! how this fits into the algorithm end to end.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::lcs::{ScalarKey, mix_float_bits, python_scalar_key};
use crate::value::Value;

use super::IgnoreOrderMemo;
use super::fxhash::HashMap;

// ---------------------------------------------------------------------
// Distance-memo cache key
// ---------------------------------------------------------------------

/// The distance memo's cache key for one side of a candidate pair: a value's
/// **exact** structural identity.
///
/// [`ItemKey`] cannot serve here. It is deliberately order- and
/// repetition-*insensitive* for a list/tuple (its `List`/`Tuple` payload is a
/// [`BTreeSet`], matching `DeepHash`'s item-matching rules), but the distance a
/// candidate pair is ranked by reads multiplicity — [`super::distance::rough_length`]
/// counts every repeated element, and the trial diff's leaf count depends on
/// each list's first-occurrence representative — so two values that share an
/// `ItemKey` can have genuinely different distances. Keying the memo by
/// `ItemKey` handed one such value's cached distance to the other (issue #31).
///
/// This keys by [`Value`]'s own `PartialEq` instead, which is exact:
/// order- and repetition-preserving for lists and tuples, variant-sensitive
/// for numbers, by-instant for datetimes. Two entries therefore share a cache
/// slot only when their values are structurally identical and so their
/// distance is provably equal — the memo is decision-neutral by construction.
/// The [`Rc`] lets the shared value outlive the per-level [`HashedList`] that
/// produced it and keeps a cache entry two pointers wide.
#[derive(Clone)]
pub(crate) struct DistKey(Rc<Value>);

impl DistKey {
    /// Interns a copy of `value` as a cache key: the value is cloned once per
    /// distinct candidate (added/removed) entry, not once per pair, so the
    /// `A * R` cache entries one pairing records cost only a refcount bump of
    /// memory each. Per-*lookup* work is not constant, however — every probe
    /// hashes the key ([`hash_value`] walks the whole value) and, on a bucket
    /// match, compares two keys structurally — so a pairing's distance work is
    /// proportional to record size, a constant-factor cost the `ignore_order`
    /// input-size cap already covers (measured 2.3x-5.6x slower than a
    /// deduplicating `ItemKey` key only on a crafted worst case: large values
    /// whose `ItemKey` collapses to a tiny set; every realistic shape is
    /// faster). The clone recurses natively like the report's own value clones
    /// and is bounded by the same [`crate::diff::check_value_depth`] pre-pass;
    /// the hashing the key is looked up by is iterative (see [`hash_value`]).
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
        // `Value`'s own iterative structural equality — exact, and stack-safe
        // on the deep values `Rc::ptr_eq` would miss across nesting levels.
        self.0 == other.0
    }
}

impl Eq for DistKey {}

impl Hash for DistKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_value(&self.0, state);
    }
}

/// Hashes a value consistently with its structural `PartialEq` (equal values
/// hash equal): a per-variant discriminant, then the fields that equality
/// compares — numbers through [`number_key`] (so `-0.0`/`0.0` agree and an
/// int and an equal-valued float stay distinct), a datetime by its instant, a
/// date by its ordinal, a list/tuple by its length and elements (order and
/// repetition preserving), a set/frozenset by its canonical members, a dict by
/// its sorted keys and their values. The exact byte sequence is unspecified;
/// only its determinism and its agreement with [`Value`]'s equality matter.
///
/// **Iterative** (an explicit work-stack, never native recursion), like the
/// engine's own [`Value`] `Drop`/`PartialEq`: a value's nesting is
/// user-controlled and can reach the caller-raised `max_depth`, so a recursive
/// hasher would reopen the uncatchable native-stack-overflow class those
/// iterative primitives exist to close — the default budget being safe is
/// coincidence, not a property. The walk hashes each node before pushing its
/// children, so two structurally equal values drive identical stack operations
/// and hash identically; children are pushed so a container's length prefix and
/// per-variant discriminant keep distinct shapes apart.
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
            // Consistent with `Value`'s own `times_equal`-based `PartialEq`
            // for `Time`: awareness first (never collapsed), then the same
            // instant `times_equal` compares within one awareness bucket.
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
                // A `str` key carries the association and is hashed here
                // directly, in sorted order; a non-`str` key's own `Value`
                // is pushed onto the same work-stack the values use below,
                // so it gets discriminant-and-content hashed by this same
                // loop rather than needing a second, `ObjectKey`-specific
                // `Hash` impl (this type deliberately has none — see its own
                // doc). Either way, the order everything comes back in is
                // deterministic, which is all equality-consistency needs.
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ItemKey {
    Null,
    /// `true`/`false` — tagged distinctly from any integer of the same
    /// value (never collides with `Int`).
    Bool(bool),
    /// A JSON value `serde_json` parsed as an integer (no decimal point or
    /// exponent) — see [`mod@crate::diff`]'s `python_type_name` for the same
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
    /// A `time`, keyed by [`crate::datetime::Time::hash_seconds_of_day`] —
    /// `DeepHash._prep_datetime` reduces a `time` to `time_to_seconds`
    /// before formatting, dropping the microsecond *and* any offset
    /// entirely (a genuine, confirmed quirk — see `crate::datetime`'s
    /// module doc), so two times equal only in whole seconds-of-day
    /// hash-match here even when ordinary `==` would call them different.
    Time(i64),
    /// A `timedelta`, keyed by the value itself — `_prep_number` hashes a
    /// `timedelta` exactly (no truncation), and the type's own `Eq`/`Hash`
    /// already are that exact value.
    TimeDelta(crate::datetime::TimeDelta),
    /// Order- and count-insensitive: see this type's own doc.
    List(BTreeSet<ItemKey>),
    /// A tuple, keyed exactly like [`ItemKey::List`] (order- and
    /// count-insensitive) but in its own bucket, so a tuple and a list
    /// holding the same items never hash-match — see this type's own doc.
    /// A *hashable* tuple can also inherit an earlier Python-equal tuple's
    /// key outright, which is `DeepHash`'s own cache behavior: see
    /// [`item_key`] and `super::memo`'s "Tuple digests" section.
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
    /// Key-sorted, recursively keyed values, the key itself recursively
    /// keyed too — a plain `String` cannot represent a non-`str` dict key
    /// (`ItemKey` already covers every key kind this crate's dicts allow,
    /// scalar or a `tuple` of scalars, via the same recursion [`item_key`]
    /// runs on a value).
    Dict(BTreeMap<ItemKey, ItemKey>),
}

/// Hand-written to run the `Float` arm through [`mix_float_bits`] before
/// hashing: an integral or half-integer float's raw bit pattern has ~50
/// trailing zero bits, which would collide in the crate's `FxHash`-backed
/// tables (see that function's doc). Every other arm hashes as the derived
/// impl would (discriminant plus fields), and nested keys recurse through this
/// same impl, so the mixing reaches a float at any depth. Consistent with the
/// derived `Eq`: equal keys still hash equal.
impl std::hash::Hash for ItemKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            Self::Null => {}
            Self::Bool(b) => b.hash(state),
            Self::Int(i) => i.hash(state),
            Self::Float(bits) => mix_float_bits(*bits).hash(state),
            Self::Str(s) => s.hash(state),
            Self::DateTime(instant) => instant.hash(state),
            Self::Date(ordinal) => ordinal.hash(state),
            Self::Time(seconds_of_day) => seconds_of_day.hash(state),
            Self::TimeDelta(value) => value.hash(state),
            Self::List(items) | Self::Set(items) | Self::FrozenSet(items) => items.hash(state),
            Self::Tuple(items) => items.hash(state),
            Self::Dict(map) => map.hash(state),
        }
    }
}

/// One element of a hashable tuple's Python identity: a scalar by value, or
/// a nested tuple by the id its own identity was interned to.
///
/// Referring to a nested tuple by id rather than by its whole identity is
/// what keeps this `O(arity)` per node: `((((1,),),),)` is four one-element
/// identities, not four identities of size 1, 2, 3 and 4.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum PyHashPart {
    Scalar(ScalarKey),
    Tuple(TupleId),
}

/// A hashable tuple's Python identity: its elements, **positionally**
/// (Python's tuple equality is order-sensitive, unlike the
/// order-insensitive *content* digest [`item_key`] computes).
///
/// This is the key `DeepHash`'s shared `hashes` dict is looked up by (see
/// `super::memo`'s "Tuple digests" section), so it mirrors Python exactly:
/// scalars go through the crate's one definition of Python scalar equality
/// ([`python_scalar_key`], which makes `1`, `1.0` and `True` one key), and a
/// list or dict is unhashable, which also makes any tuple containing one
/// unhashable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PyHashKey(Box<[PyHashPart]>);

/// A hashable tuple identity's place in the run's interning table — see
/// [`super::IgnoreOrderMemo::tuple_digest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// A set member's *content* digest, as a small id into the run's content
/// interning table (see [`set_member_digest`]). Two members are the same member
/// exactly when their `RepId`s are equal — so [`set_difference`] compares
/// members in `O(1)`, and no comparison ever recurses into a member's structure
/// (a set member's nesting is not depth-guarded before this runs, so a
/// structural comparison could overflow the native stack).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RepId(usize);

impl RepId {
    /// The id for the entry at `index` (the interner's only constructor).
    pub(crate) fn new(index: usize) -> Self {
        Self(index)
    }
}

/// A set member's **Python-equality** class id, distinct from its content
/// [`RepId`]. One fresh id per distinct [`MemberHashKey`], so a naive and an
/// aware datetime wrapped in a tuple — Python-*un*equal, though their content
/// coincides — keep different `NodeId`s. That is what stops the content
/// collapse from leaking into the Python-equality key: `(1, (naive,))` and
/// `(1.0, (aware,))` must *not* match (the outer tuples are Python-unequal, so
/// `DeepDiff` compares content, where `1` and `1.0` are type-distinct), and
/// they don't, because the inner tuples carry different `NodeId`s here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NodeId(usize);

impl NodeId {
    /// The id for the entry at `index` (the interner's only constructor).
    pub(crate) fn new(index: usize) -> Self {
        Self(index)
    }
}

/// One element of a set member's Python-equality identity ([`MemberHashKey`]):
/// a scalar by Python `==` ([`python_scalar_key`], collapsing `1`/`1.0`/`True`
/// but keeping a naive datetime distinct from an aware one), or a nested
/// hashable container by its Python-equality [`NodeId`].
///
/// Referencing a nested container by its **[`NodeId`]** — its Python-equality
/// class, not its content [`RepId`] — is what keeps this a faithful
/// Python-equality key: `(naive,)` and `(aware,)` share a content id but not a
/// `NodeId`, so `(1, (naive,))` and `(1.0, (aware,))` get different keys and are
/// compared by content (where `1` and `1.0` differ), exactly as `DeepDiff`
/// does. `1`/`1.0` still collapse when the *whole* container is Python-equal —
/// then this whole key matches and the cache hands back one id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemberPart {
    Scalar(ScalarKey),
    Node(NodeId),
}

/// A set member's Python-equality identity — the key `DeepHash`'s shared cache
/// is looked up by, so a Python-equal container hashed earlier in the run wins
/// the digest (`1`/`1.0` collapse). A tuple is positional; a frozenset is by
/// membership.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemberHashKey {
    Tuple(Box<[MemberPart]>),
    FrozenSet(BTreeSet<MemberPart>),
}

/// A set member's *content* identity, keyed by children [`RepId`]s (and, at a
/// leaf, the type-distinct scalar [`ItemKey`]) — what a node's [`RepId`] is
/// interned by when the Python-equality cache misses. A `datetime` normalises
/// to its instant here, so a naive and an aware value at one moment share a
/// content id even though their [`MemberPart`]s differ; a tuple stays
/// positional and a frozenset by membership, onix's one deliberate divergence
/// from `DeepHash`'s order- and repetition-insensitive iterable hashing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemberContent {
    /// A scalar leaf, by its content [`ItemKey`]: numbers type-distinct
    /// (`1`/`1.0`/`True` apart), a calendar value by its instant/ordinal.
    Scalar(ItemKey),
    /// A hashable tuple, positionally (matching `tuple.__eq__`).
    Tuple(Vec<RepId>),
    /// A hashable frozenset, by membership.
    FrozenSet(BTreeSet<RepId>),
    /// A `list` (unhashable; reachable through a `list` subclass with
    /// `__hash__`), in its own variant since a list and a set of the same
    /// members are not Python-equal.
    UnhashableList(Vec<RepId>),
    /// A `set`, likewise.
    UnhashableSet(Vec<RepId>),
    /// A `dict`, likewise, keyed by each key's own [`ItemKey`] rather than a
    /// plain `String` (see [`ItemKey::Dict`]'s doc for why) — so comparing
    /// two `UnhashableDict`s (e.g. an `IgnoreOrderMemo::member_content`
    /// lookup landing on one) walks each key's own `ItemKey` tree, not a
    /// cheap string ordering.
    UnhashableDict(BTreeMap<ItemKey, RepId>),
}

/// The members of `a` that no member of `b` shares a digest with, and the
/// same the other way round — the whole of `_diff_set`'s comparison, and of
/// the distance mirror that counts what it would report.
///
/// Each member is reduced to one [`RepId`] by [`set_member_digest`]. `a`'s
/// members are digested (in [`crate::value::SetItems`]' canonical order) before
/// `b`'s, because the run's shared cache is first-write-wins: processing a
/// deterministic order makes the id each equality class settles on
/// deterministic too. Two members are the same member exactly when their
/// `RepId`s are equal.
///
/// Shared by [`crate::diff::set_diff`] and
/// `count_set_diff_leaves` so the two can never drift
/// on what "the same member" means.
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

/// The membership digest of one set member — the value `_diff_set` compares
/// members by, computed exactly as `DeepHash` computes it, by consulting the
/// run's shared cache (`memo`) at every hashable node and returning one
/// [`RepId`].
///
/// **Why a cache, and why a per-node one.** `DeepHash._hash(obj)` first looks
/// `obj` up in a run-scoped cache keyed by `_make_hash_key(obj)` — which
/// type-wraps a bare *number* (`(type(obj), obj)`, so `1` and `1.0` never share
/// an entry) but leaves a *container* (tuple, frozenset) as its own object,
/// looked up by Python `==`/`hash`. On a hit it reuses the earlier digest; on a
/// miss it builds a content digest from the children's (already-cached) digests
/// and stores it. Because both members of a comparison are hashed against one
/// shared cache (`diff.py::_create_hashtable`), a node can take the content
/// path at its root and still hit the cache at a child. `onix` reproduces this
/// with two interning tables per run (see `super::memo`'s "Set-member digests"
/// section): a Python-equality one ([`MemberHashKey`] → ([`NodeId`], [`RepId`]),
/// collapsing `1`/`1.0`) and a content one ([`MemberContent`] → [`RepId`],
/// normalising a `datetime` to its instant). A hashable container's `RepId` is
/// the cache's on a hit, its content's on a miss; a parent's Python-equality key
/// names a nested container by its `NodeId` (Python class) while its content and
/// the final comparison use the `RepId` (content class) — the two being distinct
/// is what keeps `(1, (naive,))` and `(1.0, (aware,))` apart (the inner tuples'
/// `NodeId`s differ, so the outer tuples are compared by content, where `1` and
/// `1.0` differ) while still collapsing a naive/aware difference that does not
/// break a wrapping tuple's Python-equality. Every one of these agrees with
/// `deepdiff==9.1.0` (`TZ=UTC`, `verbose_level=2`), whether the naive/aware or
/// `1`/`1.0` difference sits at the member's root or arbitrarily deep inside it:
///
/// ```text
/// {(naive, (1,))}       vs {(aware, (1.0,))}   -> {}    (inner tuple shares an id; outer content agrees)
/// {((naive,),)}         vs {((aware,),)}       -> {}    (the difference two levels down still collapses)
/// {(naive, 1)}          vs {(aware, 1.0)}      -> removal + addition (a bare number is type-distinct)
/// {(naive, 1)}          vs {(naive, 1.0)}      -> {}    (whole tuple Python-equal: one cache hit)
/// ```
///
/// **Frozensets** take part in the cache here (a frozenset is hashable in
/// Python), so a `frozenset({True})` and a `frozenset({1})` share one id the
/// way two Python-equal tuples do — the one place the cache covers frozensets;
/// in the *list*-item path ([`item_key`]) a frozenset keeps its own content
/// key, a documented divergence (see [`ItemKey::FrozenSet`]).
///
/// **One member kind still diverges from real `DeepDiff`, deliberately:** a
/// tuple matches positionally ([`MemberContent::Tuple`] is a `Vec`) and a
/// frozenset by membership, where `DeepHash._prep_iterable` hashes *every*
/// iterable order- and repetition-insensitively — so `(1, 2)` and `(2, 1)`
/// share one digest in real `_diff_set` but not here. See
/// `tests/golden/README.md`'s "Set iteration order" section (its "A tuple or a
/// frozenset set member matches order- and repetition-insensitively" point, and
/// its "A naive and an aware datetime" point for why a same-instant naive/aware
/// pair is two members here, not one).
///
/// Iterative (an explicit heap work-stack, no native recursion over value
/// depth), because a set member's own nesting is not bounded by the engine's
/// traversal depth guards before this runs — so a natively recursive walk would
/// be an unguarded overflow sink on adversarially nested input. (The
/// comparison of the results is `O(1)` per pair — they are `RepId`s — so it
/// cannot overflow either.) The walk pushes each node before its children, so
/// `order` holds parents before children and each child's subtree is
/// contiguous; assembling from the end therefore always finds a node's own
/// children as the last entries of `built`, in their original order.
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

    // Each entry: the node's content id, and its Python-equality part for a
    // parent's key (`None` when unhashable, which makes any container holding
    // it unhashable too).
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

/// [`set_member_digest`]'s tuple/frozenset case: assembles the content id and
/// the Python-equality key from the already-digested `children`. When the node
/// is hashable (every child hashable) its id comes from the run's shared cache,
/// so a Python-equal node hashed earlier wins it; otherwise it is content-only,
/// and takes no part in a parent's key.
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
        Value::Time(time) => ItemKey::Time(time.hash_seconds_of_day()),
        Value::TimeDelta(value) => ItemKey::TimeDelta(*value),
        Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => unreachable!("scalar_content_key is only called on scalar leaves"),
    }
}

/// The type-distinct key for a bare number, mirroring [`ItemKey`]'s own
/// number rule (see [`crate::value::fold_signed_zero`] for the signed-zero
/// note).
fn number_key(n: &crate::value::Number) -> ItemKey {
    if n.is_f64() {
        let f = n
            .as_f64()
            .expect("Number::is_f64 guarantees as_f64 succeeds");
        return ItemKey::Float(crate::value::fold_signed_zero(f).to_bits());
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
/// makes it inherit its digest (see `super::memo`'s "Tuple digests" section
/// for the mechanism, the source citations, and why the ordering is
/// observable).
///
/// Recurses natively — safe only because
/// every caller in this module first proves `value`'s nesting is within the
/// crate's shared depth budget via [`check_value_depth`](crate::diff::check_value_depth) (see this module's
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
        Value::Time(value) => (ItemKey::Time(value.hash_seconds_of_day()), part()),
        Value::TimeDelta(value) => (ItemKey::TimeDelta(*value), part()),
        Value::Number(n) => {
            let number = if n.is_f64() {
                let f = n
                    .as_f64()
                    .expect("Number::is_f64 guarantees as_f64 succeeds");
                // See `crate::value::fold_signed_zero`: it is the identity on
                // every float but `-0.0`, so an integral float like `2.0`
                // keeps a distinct `Float` key from the integer `2` (this
                // deliberately does NOT take the ordered path's `ScalarKey`
                // integral-to-`Int` canonicalization — the two paths have
                // genuinely different number semantics).
                ItemKey::Float(crate::value::fold_signed_zero(f).to_bits())
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
                    .map(|(k, v)| (object_key_item_key(k, memo), item_key(v, memo)))
                    .collect(),
            ),
            None,
        ),
    }
}

/// One [`ObjectKey`](crate::value::ObjectKey)'s [`ItemKey`] — a `str` key
/// keys exactly like [`keyed`]'s own `Value::Str` case, and any other key
/// recurses through [`item_key`] on its wrapped [`Value`], covering a
/// `tuple` key the same way a `tuple` value already is. Shared by
/// [`keyed`]'s dict case and [`set_member_digest`]'s, so `ItemKey::Dict` and
/// `MemberContent::UnhashableDict` key a dict identically.
///
/// This is the reason `ObjectKey` itself carries no `#[derive(Hash)]`: like
/// [`Value`], its equality is this crate's own
/// structural rule, not a field-by-field derive, so a generic `HashMap`/
/// `HashSet` cannot key by it directly — every place this crate needs a
/// content hash of one goes through this function (or `hash_value`'s own
/// inline match, for the unkeyed `DistKey` case) instead.
fn object_key_item_key(key: &crate::value::ObjectKey, memo: &IgnoreOrderMemo) -> ItemKey {
    match key {
        crate::value::ObjectKey::Str(s) => ItemKey::Str(s.to_string()),
        crate::value::ObjectKey::Other(value) => item_key(value, memo),
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

    let (id, digest) = memo.tuple_digest(PyHashKey(parts.into_boxed_slice()), || {
        ItemKey::Tuple(Rc::new(children))
    });
    (digest, Some(PyHashPart::Tuple(id)))
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
    /// order (a Python dict's insertion order). Each key sits behind an [`Rc`]
    /// shared with [`Self::info`] and the distance-memo cache keys (see
    /// `DistanceKey`).
    pub(crate) distinct_order: Vec<Rc<ItemKey>>,
    info: HashMap<Rc<ItemKey>, (usize, &'a Value)>,
}

impl<'a> HashedList<'a> {
    /// Hashes `items` in index order, threading the run's `memo` so a
    /// hashable tuple's digest is shared with every Python-equal tuple hashed
    /// earlier in this diff — including in the *other* list's table, which is
    /// built by a second call with the same `memo`, exactly as `DeepDiff`
    /// builds its two hashtables against one shared `hashes` dict.
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
    /// Panics if `key` was never produced by [`Self::build`] on this same
    /// list. Every caller in this module only ever looks up a key drawn
    /// from `self.distinct_order` itself (or a `hashes_added`/
    /// `hashes_removed` slice filtered from it), so this can never actually
    /// fire.
    pub(crate) fn get(&self, key: &ItemKey) -> (usize, &'a Value) {
        self.info[key]
    }
}
