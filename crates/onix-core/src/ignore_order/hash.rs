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
    /// Key-sorted, recursively keyed values.
    Dict(BTreeMap<String, ItemKey>),
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

/// The identity a **set member** is compared by in onix: Python's own `==`
/// for a bare scalar (numbers kept type-distinct) or a tuple (positional,
/// exactly `tuple.__eq__`), by instant for a calendar value, and by
/// membership for a frozenset.
///
/// This is `DeepHash`'s `_make_hash_key` rule (deephash.py) applied
/// directly for a bare, hashable scalar, rather than through the shared
/// `hashes` cache real `DeepDiff` looks it up in: `_make_hash_key`
/// type-wraps a number (`(type(obj), obj)`) and returns every other object
/// unchanged, so a bare member's identity *is* Python equality except that
/// `1`, `1.0` and `True` stay three members. Reading the rule instead of the
/// cache is what makes this deterministic — the cache's answer additionally
/// depends on which member of an equality class the process hashed first,
/// which follows the set's own hash order. See `tests/golden/README.md`'s
/// "Set iteration order" section for the difference that buys and costs.
///
/// Confirmed against `deepdiff==9.1.0`: `{1}` vs `{1.0}` and `{True}` vs
/// `{1}` are each a removal plus an addition (bare numbers, type-wrapped),
/// while `{(1,)}` vs `{(1.0,)}` and `{frozenset({1})}` vs
/// `{frozenset({1.0})}` are empty (a Python-equal *number* inside a
/// container is not type-wrapped).
///
/// Two more member kinds diverge from real `DeepDiff` here, deliberately —
/// each is `_diff_set` hashing through the *same* `DeepHash` this crate's
/// `ignore_order` item matching already reproduces
/// ([`ItemKey`]), not through plain Python `==`, and onix picks the
/// simpler, always-deterministic rule instead of replicating either:
///
/// - **A calendar value matches by instant**, at any nesting depth
///   (`_prep_datetime` normalizes every `datetime` to UTC before hashing,
///   and a `date` never joins a `datetime`'s equivalence class) — so a naive
///   and an aware value at one instant are the *same* member for matching
///   here, unlike Python's own stricter `==` (never equal across
///   naive/aware). This only decides which member of `a` pairs with which
///   member of `b`; it does not merge two structurally distinct members
///   *within* one set — see [`crate::value::SetItems::new`]'s doc for that
///   split, and `tests/golden/README.md`'s "Set iteration order" §4 for the
///   consequence: a set holding both a naive and an aware value at one
///   instant reports both as members where `DeepDiff`'s own hashtable can
///   report only whichever one it happened to keep.
/// - **A tuple or a frozenset containing one matches order- and
///   repetition-insensitively in real `DeepDiff`**, because
///   `_prep_iterable` runs with `ignore_iterable_order`/`ignore_repetition`
///   for every iterable it hashes (not only the list-nested-in-an-
///   `ignore_order`-list case that motivates the default) — so `(1, 2)` and
///   `(2, 1)`, or `(1, 1, 2)` and `(1, 2, 2)`, share one digest in
///   `_diff_set` despite not being Python-equal. onix's [`SetMemberKey::Tuple`]
///   stays positional, matching `tuple.__eq__` exactly, which this crate
///   considers the honest answer rather than a nuance to chase — see
///   `tests/golden/README.md`'s "Set iteration order" §5.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SetMemberKey {
    /// A bare `bool`, `int` or `float`, in its own type's bucket.
    Number(ItemKey),
    /// `None` or a `str`, and every number *nested* inside a container,
    /// where Python's `==` does collapse `1`, `1.0` and `True`.
    Scalar(ScalarKey),
    /// A `datetime` or `date`, at any nesting depth — see this type's own
    /// doc for why it is not a plain [`Self::Scalar`].
    Calendar(ItemKey),
    /// A tuple, positionally (Python's tuple equality is order-sensitive).
    Tuple(Vec<SetMemberKey>),
    /// A frozenset, by membership (Python's frozenset equality is not
    /// order-sensitive).
    FrozenSet(BTreeSet<SetMemberKey>),
    /// A `list`, which Python cannot hash. Reachable through a `list`
    /// subclass that defines `__hash__`, and keyed structurally so it is at
    /// least deterministic — in its own variant, because a list and a set
    /// holding the same members are not Python-equal either.
    UnhashableList(Vec<SetMemberKey>),
    /// A `set`, likewise unhashable and likewise in its own variant.
    UnhashableSet(Vec<SetMemberKey>),
    /// A `dict`, likewise.
    UnhashableDict(BTreeMap<String, SetMemberKey>),
}

/// The members of `a` that no member of `b` shares an identity with, and
/// the same the other way round — the whole of `_diff_set`'s comparison,
/// and of the distance mirror that counts what it would report.
///
/// Each member's identity is computed exactly once. Shared by
/// [`crate::diff::set_diff`] and
/// [`crate::ignore_order::count_set_diff_leaves`] so the two can never drift
/// on what "the same member" means.
pub(crate) fn set_difference<'a>(
    a: &'a [Value],
    b: &'a [Value],
) -> (Vec<&'a Value>, Vec<&'a Value>) {
    let a_keys: Vec<SetMemberKey> = a.iter().map(set_member_key).collect();
    let b_keys: Vec<SetMemberKey> = b.iter().map(set_member_key).collect();
    let a_lookup: BTreeSet<&SetMemberKey> = a_keys.iter().collect();
    let b_lookup: BTreeSet<&SetMemberKey> = b_keys.iter().collect();

    let only_in = |items: &'a [Value], keys: &[SetMemberKey], other: &BTreeSet<&SetMemberKey>| {
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

/// The identity of one set member — see [`SetMemberKey`].
pub(crate) fn set_member_key(value: &Value) -> SetMemberKey {
    match value {
        Value::Bool(b) => SetMemberKey::Number(ItemKey::Bool(*b)),
        Value::Number(n) => SetMemberKey::Number(number_key(n)),
        other => python_identity(other),
    }
}

/// [`set_member_key`]'s walk: Python's own `==` all the way down, with no
/// type-wrapping (a number nested inside a container is compared the way
/// Python compares it, so `(1,)` and `(1.0,)` are one identity) — except a
/// calendar value, which keys by instant instead of Python's stricter `==`;
/// see [`SetMemberKey::Calendar`]'s doc.
///
/// Iterative (an explicit heap work-stack, no native recursion), because
/// this runs from [`crate::value::SetItems::new`] — at *construction* time,
/// before any of the engine's depth guards have seen the value — so a
/// natively recursive version would be an unguarded overflow sink on
/// adversarially nested input.
///
/// The walk pushes each node before its children, so `order` holds parents
/// before children and each child's subtree is contiguous; assembling from
/// the end therefore always finds a node's own children as the last entries
/// of `built`, in their original order.
fn python_identity(root: &Value) -> SetMemberKey {
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

    let mut built: Vec<SetMemberKey> = Vec::with_capacity(order.len());
    for value in order.iter().rev() {
        let mut children = |count: usize| built.split_off(built.len() - count);

        let key = match value {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Str(_) => {
                SetMemberKey::Scalar(
                    python_scalar_key(value).expect("python_scalar_key covers every scalar"),
                )
            }
            // Not `python_scalar_key`: `_diff_set` hashes through `DeepHash`,
            // whose `_prep_datetime` normalizes to UTC — see
            // [`SetMemberKey::Calendar`]'s doc.
            Value::DateTime(dt) => SetMemberKey::Calendar(ItemKey::DateTime(dt.instant())),
            Value::Date(date) => SetMemberKey::Calendar(ItemKey::Date(date.ordinal())),
            Value::Tuple(items) => SetMemberKey::Tuple(children(items.len())),
            Value::FrozenSet(items) => {
                SetMemberKey::FrozenSet(children(items.len()).into_iter().collect())
            }
            Value::Array(items) => SetMemberKey::UnhashableList(children(items.len())),
            Value::Set(items) => SetMemberKey::UnhashableSet(children(items.len())),
            Value::Object(map) => SetMemberKey::UnhashableDict(
                map.keys()
                    .map(str::to_owned)
                    .zip(children(map.len()))
                    .collect(),
            ),
        };
        built.push(key);
    }

    built
        .pop()
        .expect("the walk pushes at least the root's own key")
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
/// makes it inherit its digest (see `super::memo`'s "Tuple digests" section
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
