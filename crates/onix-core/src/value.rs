//! A compact, JSON-shaped value model — a memory-frugal stand-in for
//! [`serde_json::Value`] with byte-identical rendering.
//!
//! [`serde_json::Value`] is convenient but heavy on the shapes this engine
//! diffs most: its object type is a `BTreeMap<String, Value>` whose leaf
//! node is a fixed ~640-byte, 11-slot allocation regardless of how few
//! entries it holds, so a tree dominated by small maps (`{"tag": "..."}`)
//! spends most of its footprint on empty slots, and repeats every key
//! `String` once per occurrence. [`Value`] replaces both costs:
//!
//! * **Objects** are an exactly-sized, key-sorted `Box<[(ObjectKey, Value)]>`
//!   — one heap block holding precisely the entries present, no spare slots.
//!   Sorted by key means lookups are a binary search and iteration is in the
//!   same order [`serde_json`]'s `BTreeMap` produces for a `str`-only
//!   object, so anything rendered from a [`Value`] stays byte-identical. See
//!   [`ObjectKey`] for the (additive) non-`str` key case.
//! * **`str` keys** are interned within one conversion/parse session (see
//!   `Interner`): the handful of distinct keys a real payload repeats
//!   thousands of times collapse to one `Arc<str>` each, shared by cheap
//!   refcount bumps.
//! * **Numbers** preserve [`serde_json`]'s exact three-way `i64`/`u64`/`f64`
//!   distinction (see [`Number`]), which is load-bearing for byte-compatible
//!   output: `1` and `1.0` must render differently, and a `u64` above
//!   [`i64::MAX`] must survive as an integer.
//!
//! Conversions in both directions ([`From`]`<`[`serde_json::Value`]`>` and
//! [`Value::to_serde_json`]) and a direct streaming
//! [`Deserialize`] (no transient [`serde_json::Value`] tree) let this type
//! sit at the parse boundary; the diff engine consumes it directly. See the
//! crate root's architecture map for how each caller produces a `Value`;
//! [`From`] is the path for one that already holds a [`serde_json::Value`].
//!
//! # Stack safety
//!
//! [`Value`] nests through `Box<[Value]>` (both [`Value::Array`] and
//! [`Value::Tuple`]), through [`SetItems`] (both [`Value::Set`] and
//! [`Value::FrozenSet`]) and through [`Object`]'s entries, so a naive derived `Drop` would
//! recurse natively — an uncatchable process abort on adversarially deep
//! input, the same latent sink [`serde_json::Value`]'s derived `Drop` has.
//! This type instead implements an **iterative `Drop`** (see the `impl Drop`
//! below) that hoists children onto a heap work-stack, so teardown uses
//! `O(1)` native stack regardless of nesting depth — strictly safer than
//! [`serde_json::Value`], not merely equal. Construction paths
//! ([`From`]/[`Value::to_serde_json`]) remain ordinary recursion, matching
//! [`serde_json`]'s own posture at those bounded API-boundary calls; the
//! streaming [`Deserialize`] path is bounded by
//! [`serde_json`]'s own parser recursion limit.
//!
//! Structural equality ([`PartialEq`]) is likewise iterative (an explicit
//! work-stack, the same posture as `Drop`), so deep comparison — which the
//! engine migration will run on attacker-shaped input — cannot overflow the
//! native stack either. So is the canonical set ordering `canonical_cmp`
//! that [`SetItems::new`] sorts with, and for a sharper reason: a set is
//! built during *conversion*, on whatever thread the caller is on, before
//! any depth guard has seen the value and with no sized worker underneath
//! it — a recursive comparator there was an uncatchable abort on a set of
//! two deep members. The derived [`Debug`] and [`Clone`] are deliberately
//! left recursive: `Debug` is debug/test-only, and the diff engine only ever
//! clones a value that has already passed its combined path-plus-value depth
//! guard (`crate::diff`'s internal `check_value_depth`), so clone recursion
//! is bounded by `max_depth` — the same guarded posture `serde_json::Value`'s
//! own recursive `Clone` had before the engine migrated onto this type. A
//! caller cloning an untrusted value outside that guard should reject
//! over-deep input up front with [`crate::exceeds_depth`].

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use serde::de::{Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::datetime::{Date, DateTime, Time, TimeDelta, times_equal};

/// A compact JSON value: the memory-frugal counterpart of
/// [`serde_json::Value`].
///
/// See the [module documentation](self) for the representation choices and
/// their rationale. Six of the variants mirror JSON's own shapes; objects
/// are held as an [`Object`] (a sorted, exactly-sized entry slice) and
/// numbers as a [`Number`] preserving the `i64`/`u64`/`f64` distinction.
///
/// Seven variants are the ones JSON itself cannot express:
/// [`Value::Tuple`], [`Value::Set`] and [`Value::FrozenSet`] — the Python
/// `tuple`, `set` and `frozenset` — and [`Value::DateTime`], [`Value::Date`],
/// [`Value::Time`] and [`Value::TimeDelta`]. Each is a *different type* from
/// every other and from `list` (a `tuple`-vs-`list` or `set`-vs-`frozenset`
/// pairing is a `type_changes` finding, and neither pair ever hash-matches
/// under `ignore_order`), which is exactly why each gets its own variant: the
/// type distinction is structural, so mixing two of them can only ever be a
/// compile error or a `type_changes`, never a silent equality. The three
/// container kinds render to a JSON array in [`Value::to_serde_json`],
/// matching what `DeepDiff`'s own `to_json()` shows.
///
/// The four calendar types (see [`mod@crate::datetime`]) are kept as
/// *structured* values rather than pre-rendered ISO strings because
/// `DeepDiff` renders the same datetime two different ways depending on
/// where it lands in a report (UTC-normalized in `values_changed`, raw
/// everywhere else) and because [`crate::Report::to_value`] must hand a real
/// `datetime` object back to a caller holding Python objects — neither is
/// possible once the value has collapsed to a string.
///
/// Neither [`From`]`<`[`serde_json::Value`]`>` nor [`Deserialize`] can
/// produce any of the seven (JSON has no literal for them): they enter the
/// model only from a caller holding real Python objects.
#[derive(Debug, Clone)]
pub enum Value {
    /// JSON `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number (see [`Number`] for the preserved int/float distinction).
    Number(Number),
    /// A string, stored as an exactly-sized `Box<str>` (no spare capacity).
    Str(Box<str>),
    /// A Python `datetime.datetime` — see [`DateTime`], and this type's own
    /// doc for why it is a variant rather than a pre-rendered string.
    DateTime(DateTime),
    /// A Python `datetime.date` — see [`Date`].
    Date(Date),
    /// A Python `datetime.time` — see [`Time`].
    Time(Time),
    /// A Python `datetime.timedelta` — see [`TimeDelta`].
    TimeDelta(TimeDelta),
    /// An array, stored as an exactly-sized `Box<[Value]>`.
    Array(Box<[Value]>),
    /// A Python tuple, stored exactly like [`Value::Array`] but kept as a
    /// distinct variant — see this type's own doc for why.
    Tuple(Box<[Value]>),
    /// A Python `set`, stored as canonically ordered [`SetItems`].
    Set(SetItems),
    /// A Python `frozenset`, stored exactly like [`Value::Set`] but kept as
    /// a distinct variant — see this type's own doc for why.
    FrozenSet(SetItems),
    /// An object: key-sorted, exactly-sized entries (see [`Object`]).
    Object(Object),
}

/// One [`Object`] key: the `str` fast path this crate has always had
/// ([`ObjectKey::Str`]), or any other key `DeepDiff` also accepts
/// ([`ObjectKey::Other`]) — see `onix-py`'s conversion table for the exact
/// set. No `Hash` impl; see `crate::ignore_order::hash`'s
/// `object_key_item_key` for how a content hash of one is computed instead.
#[derive(Debug, Clone)]
pub enum ObjectKey {
    /// A `str` key, interned the same way [`Object`] has always interned
    /// its keys.
    Str(Arc<str>),
    /// Any other key `DeepDiff` accepts: `None`, `bool`, `int`, `float`,
    /// `datetime`, `date`, or a `tuple` of those — never itself a `str`
    /// (that always takes the [`ObjectKey::Str`] arm) and never a container
    /// other than that restricted `tuple` (`onix-py`'s conversion layer is
    /// the boundary that enforces this; this type does not).
    Other(Box<Value>),
}

impl ObjectKey {
    /// This key's `str` content, or `None` for [`ObjectKey::Other`] — the
    /// convenience every call site that only ever handled `str` keys before
    /// this variant existed still needs.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ObjectKey::Str(s) => Some(s.as_ref()),
            ObjectKey::Other(_) => None,
        }
    }
}

/// Structural equality, consistent with [`ObjectKey`]'s [`Ord`]
/// (`object_key_cmp`): two `str` keys compare by content, two `Other` keys
/// by [`Value`]'s own structural equality, and a `Str` never equals an
/// `Other` (`DeepDiff` never treats a `str` key as equal to any other key
/// kind either).
impl PartialEq for ObjectKey {
    fn eq(&self, other: &Self) -> bool {
        object_key_cmp(self, other).is_eq()
    }
}

impl Eq for ObjectKey {}

impl PartialOrd for ObjectKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Total order backing [`Object`]'s own sort/binary-search: every `Str` key
/// sorts before every `Other` key (so an all-`str` object's entries land in
/// exactly the order they always have — this variant changes nothing about
/// it), `Str`-vs-`Str` by string content, and `Other`-vs-`Other` by
/// `canonical_cmp` (the same structural order [`SetItems`] sorts its
/// members with). This is an internal storage/lookup order, unrelated to
/// `DeepDiff`'s own (unreproducible) dict iteration order — see
/// `crate::diff::object` for the *matching* rule (Python `==`, which treats
/// `1`/`1.0`/`True` as one key) applied on top of this when two [`Object`]s
/// are diffed against each other.
impl Ord for ObjectKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        object_key_cmp(self, other)
    }
}

/// [`ObjectKey`]'s comparison, factored out so [`PartialEq`] and [`Ord`]
/// cannot drift (equality is exactly `Ordering::Equal`).
fn object_key_cmp(a: &ObjectKey, b: &ObjectKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (a, b) {
        (ObjectKey::Str(x), ObjectKey::Str(y)) => x.cmp(y),
        (ObjectKey::Str(_), ObjectKey::Other(_)) => Ordering::Less,
        (ObjectKey::Other(_), ObjectKey::Str(_)) => Ordering::Greater,
        (ObjectKey::Other(x), ObjectKey::Other(y)) => canonical_cmp(x, y),
    }
}

/// Delegates to the iterative `structural_eq`. The result is exactly what a
/// derived `PartialEq` produces, verified by a differential property test
/// against the derive before it was replaced. See the [module
/// documentation](self)'s "Stack safety" section for why equality is
/// iterative.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        structural_eq(self, other)
    }
}

impl Value {
    /// Renders this value back into an equivalent [`serde_json::Value`].
    ///
    /// Objects render with keys in sorted order (the order this type already
    /// stores them in, matching [`serde_json`]'s `BTreeMap` iteration), and
    /// numbers reconstruct their exact `i64`/`u64`/`f64` representation, so
    /// `to_serde_json().to_string()` is byte-identical to the string the
    /// original [`serde_json::Value`] would have produced.
    #[must_use]
    pub fn to_serde_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            // A non-finite float has no `serde_json::Number` form at all;
            // `null` is what the streaming parse path already renders for
            // one arriving that way (see `ValueVisitor::visit_f64`), and
            // this is unreachable from the CLI (JSON text cannot carry a
            // `NaN`/`Infinity` literal) or from the Python bindings' own
            // `to_json()` (`crate::guard::to_json_string` in `onix-py`
            // renders a report directly, without going through this method,
            // precisely so it can emit `NaN`/`Infinity` instead).
            Value::Number(n) => n
                .to_serde_number()
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Value::Str(s) => serde_json::Value::String(s.as_ref().to_owned()),
            Value::DateTime(value) => serde_json::Value::String(value.isoformat()),
            Value::Date(value) => serde_json::Value::String(value.isoformat()),
            Value::Time(value) => serde_json::Value::String(value.isoformat()),
            Value::TimeDelta(value) => serde_json::Value::String(value.python_str()),
            Value::Array(items) | Value::Tuple(items) => {
                serde_json::Value::Array(items.iter().map(Value::to_serde_json).collect())
            }
            // Already in canonical order: a set's source order is dropped at
            // construction, since it is not reproducible (see [`SetItems`]).
            Value::Set(items) | Value::FrozenSet(items) => {
                serde_json::Value::Array(items.iter().map(Value::to_serde_json).collect())
            }
            Value::Object(obj) => {
                let mut map = serde_json::Map::with_capacity(obj.len());
                for (key, value) in obj {
                    map.insert(object_key_json_string(key), value.to_serde_json());
                }
                serde_json::Value::Object(map)
            }
        }
    }
}

/// Renders one [`ObjectKey`] as the JSON string key
/// [`Value::to_serde_json`] embeds it under.
///
/// A `Str` key renders as its own text, unchanged (the only case a JSON
/// object can ever hold in the first place). An `Other` key mirrors Python's
/// `json.dumps`, which stringifies a non-`str` dict key rather than
/// rejecting it — `bool` to `"true"`/`"false"`, `None` to `"null"`, `int` to
/// its decimal text, and `float` through the identical shortest-round-trip
/// `repr()` [`crate::path::python_repr`] uses for a float *value* — so a
/// report embedding one of these four kinds as a nested key matches real
/// `DeepDiff`'s own `to_json()` byte-for-byte.
///
/// A `datetime`, `date`, or `tuple` key has no such rule to match: Python's
/// `json.dumps` (and so `DeepDiff.to_json()`) *raises* `TypeError` rather
/// than serializing one — confirmed against real `deepdiff==9.1.0` — so per
/// this crate's compatibility policy (crash → pick the simpler,
/// deterministic behavior, and document it) this renders the same
/// [`crate::path::python_repr`] text the key would get as a *top-level*
/// path segment, which is at least useful output instead of a hard failure.
/// See `tests/golden/README.md`'s "Known `DeepDiff` quirks" section.
fn object_key_json_string(key: &ObjectKey) -> String {
    match key {
        ObjectKey::Str(s) => s.to_string(),
        ObjectKey::Other(value) => match value.as_ref() {
            Value::Null => "null".to_string(),
            Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            Value::Number(n) if n.is_f64() => crate::path::python_float_repr(
                n.as_f64()
                    .expect("Number::is_f64 guarantees as_f64 succeeds"),
            ),
            Value::Number(n) => n
                .as_i64()
                .map(|i| i.to_string())
                .or_else(|| n.as_u64().map(|u| u.to_string()))
                .expect("a non-float Number always has an i64 or u64 representation"),
            other => crate::path::python_repr(other),
        },
    }
}

impl From<serde_json::Value> for Value {
    /// Converts an owned [`serde_json::Value`] into a compact [`Value`],
    /// interning object keys across the whole tree in one session so a key
    /// repeated at many places costs one `Arc<str>` rather than one `String`
    /// per occurrence.
    fn from(value: serde_json::Value) -> Self {
        let mut interner = Interner::new();
        from_serde(value, &mut interner)
    }
}

/// Recursively converts one [`serde_json::Value`] node, threading a single
/// [`Interner`] so keys are shared across the entire tree.
fn from_serde(value: serde_json::Value, interner: &mut Interner) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => Value::Number(Number::from_serde(&n)),
        serde_json::Value::String(s) => Value::Str(s.into_boxed_str()),
        serde_json::Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| from_serde(item, interner))
                .collect(),
        ),
        serde_json::Value::Object(map) => {
            let pairs = map
                .into_iter()
                .map(|(key, value)| {
                    (
                        ObjectKey::Str(interner.intern(&key)),
                        from_serde(value, interner),
                    )
                })
                .collect();
            Value::Object(Object::from_pairs(pairs))
        }
    }
}

/// Iterative destructor: hoists nested children onto a heap work-stack so no
/// single native-stack frame recurses into the next nesting level.
///
/// Each node has its children *taken* (replaced with empty containers)
/// before it is dropped, so when the emptied shell's own `Drop` runs it
/// finds nothing to recurse into — teardown of arbitrarily deep input uses
/// `O(1)` native stack and `O(nodes)` heap, rather than the `O(depth)`
/// native frames a derived recursive `Drop` (like [`serde_json::Value`]'s)
/// would need. See the [module documentation](self)'s "Stack safety" note.
impl Drop for Value {
    fn drop(&mut self) {
        let mut stack: Vec<Value> = Vec::new();
        take_children(self, &mut stack);
        while let Some(mut node) = stack.pop() {
            take_children(&mut node, &mut stack);
            // `node` drops here, but its children were just taken, so its
            // own `Drop` finds empty containers and does not recurse.
        }
    }
}

/// Moves `value`'s direct children onto `stack`, leaving `value` holding
/// empty containers (arrays and tuples alike). Scalars contribute nothing.
fn take_children(value: &mut Value, stack: &mut Vec<Value>) {
    match value {
        Value::Array(items) | Value::Tuple(items) => {
            let taken = std::mem::take(items);
            stack.extend(taken.into_vec());
        }
        Value::Set(items) | Value::FrozenSet(items) => {
            let taken = std::mem::take(&mut items.items);
            stack.extend(taken.into_vec());
        }
        Value::Object(obj) => {
            let taken = std::mem::take(&mut obj.entries);
            stack.extend(taken.into_vec().into_iter().map(|(_, value)| value));
        }
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

/// Iterative structural equality backing `Value`'s [`PartialEq`]. Semantics
/// match a derived `PartialEq`: same-variant structural equality — so an
/// array and a tuple holding identical items are never equal, matching
/// `DeepDiff`'s own `tuple`-vs-`list` type distinction — with
/// `Number`'s variant sensitivity intact (`PosInt(1)` is not equal to
/// `Float(1.0)`); objects compare over their sorted entries (equal key sets
/// and per-key values), and arrays over equal length and per-index values.
///
/// Sets compare like arrays, element-wise in stored order — which is
/// canonical (see [`SetItems`]), so two sets built from the same members in
/// any order do compare equal. See
/// the [module documentation](self)'s "Stack safety" section for why it is
/// iterative rather than recursive.
fn structural_eq(a: &Value, b: &Value) -> bool {
    let mut stack: Vec<(&Value, &Value)> = vec![(a, b)];
    while let Some((a, b)) = stack.pop() {
        match (a, b) {
            (Value::Null, Value::Null) => {}
            (Value::Bool(x), Value::Bool(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::Number(x), Value::Number(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::Str(x), Value::Str(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::DateTime(x), Value::DateTime(y)) => {
                // By instant, not by field: this backs the engine's own
                // "equal inputs report nothing" fast path, and `DeepDiff`
                // compares two datetimes by instant with a naive value read
                // as UTC (see `crate::datetime`).
                if x.instant() != y.instant() {
                    return false;
                }
            }
            (Value::Date(x), Value::Date(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::Time(x), Value::Time(y)) => {
                // `times_equal`, not the struct's own derived `==`: real
                // `_diff_time` never normalizes, so this is the exact rule a
                // naive value can never equal an aware one (see
                // `crate::datetime`'s module doc).
                if !times_equal(*x, *y) {
                    return false;
                }
            }
            (Value::TimeDelta(x), Value::TimeDelta(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::Array(x), Value::Array(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                stack.extend(x.iter().zip(y.iter()));
            }
            (Value::Set(x), Value::Set(y)) | (Value::FrozenSet(x), Value::FrozenSet(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                stack.extend(x.iter().zip(y.iter()));
            }
            (Value::Object(x), Value::Object(y)) => {
                if x.entries.len() != y.entries.len() {
                    return false;
                }
                for ((x_key, x_value), (y_key, y_value)) in x.entries.iter().zip(y.entries.iter()) {
                    if x_key != y_key {
                        return false;
                    }
                    stack.push((x_value, y_value));
                }
            }
            _ => return false,
        }
    }
    true
}

/// A JSON number preserving [`serde_json`]'s exact three-way representation:
/// a non-negative integer (`u64`), a negative integer (`i64`), or a float
/// (`f64`). A float built from JSON (via `Number::from_serde` or the
/// streaming [`Deserialize`]) is always finite — JSON itself has no
/// `NaN`/`Infinity` literal — but [`Number::from_f64`] is not limited to
/// that boundary: it also builds the [`Number`] a Python `float` converts
/// to, and Python's `float` can be non-finite, so a stored float need not
/// round-trip through [`serde_json::Number`] ([`Value::to_serde_json`]
/// falls back to `null` for one that can't, the same collapse the streaming
/// parse path already used for a non-finite value arriving some other way).
///
/// See the [module documentation](self) for why this int/float distinction
/// is load-bearing for byte-compatible output.
#[derive(Debug, Clone, PartialEq)]
pub struct Number {
    repr: NumberRepr,
}

/// The three concrete number representations, mirroring [`serde_json`]'s
/// internal `N` enum so classification and reconstruction match exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NumberRepr {
    /// A non-negative integer (covers the whole `u64` range, including
    /// values above [`i64::MAX`]).
    PosInt(u64),
    /// A negative integer.
    NegInt(i64),
    /// A float — finite, or (via [`Number::from_f64`] only; a
    /// `serde_json::Number` is always finite) `NaN`/`Infinity`/`-Infinity`.
    Float(f64),
}

impl Number {
    /// Builds a number from a `u64` (stored as a non-negative integer).
    #[must_use]
    pub fn from_u64(value: u64) -> Self {
        Self {
            repr: NumberRepr::PosInt(value),
        }
    }

    /// Builds a number from an `i64`, mirroring [`serde_json`]: a
    /// non-negative value is stored as a `u64` (`PosInt`), a negative one as
    /// an `i64` (`NegInt`), so both representations of the same integer
    /// value compare and render identically.
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        match u64::try_from(value) {
            Ok(non_negative) => Self::from_u64(non_negative),
            Err(_) => Self {
                repr: NumberRepr::NegInt(value),
            },
        }
    }

    /// Builds a number from an `f64`, finite or not.
    ///
    /// Unlike `Number::from_serde` (which mirrors
    /// [`serde_json::Number::from_f64`] and only ever sees a finite value,
    /// because a `serde_json::Number` is finite by construction), this
    /// constructor is also the Python-`float` boundary
    /// (`crate::convert::float_to_value` in `onix-py`), where a `NaN` or an
    /// infinity is an ordinary, legal input — so it always succeeds and
    /// stores exactly the bits it was given.
    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        Self {
            repr: NumberRepr::Float(value),
        }
    }

    /// Returns `true` if this number was parsed/stored as a float.
    #[must_use]
    pub fn is_f64(&self) -> bool {
        matches!(self.repr, NumberRepr::Float(_))
    }

    /// Returns this number as an `i64` if it fits, else `None` (floats and
    /// `u64` values above [`i64::MAX`] return `None`). Mirrors
    /// [`serde_json::Number::as_i64`].
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self.repr {
            NumberRepr::PosInt(u) => i64::try_from(u).ok(),
            NumberRepr::NegInt(i) => Some(i),
            NumberRepr::Float(_) => None,
        }
    }

    /// Returns this number as a `u64` if it is a non-negative integer, else
    /// `None`. Mirrors [`serde_json::Number::as_u64`].
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self.repr {
            NumberRepr::PosInt(u) => Some(u),
            NumberRepr::NegInt(_) | NumberRepr::Float(_) => None,
        }
    }

    /// Returns this number as an `f64` (always `Some`, matching
    /// [`serde_json::Number::as_f64`]; integer values are converted, which
    /// may lose precision for magnitudes beyond `2^53`).
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "mirrors serde_json::Number::as_f64, which likewise converts \
                  large integers to the nearest f64"
    )]
    pub fn as_f64(&self) -> Option<f64> {
        Some(match self.repr {
            NumberRepr::PosInt(u) => u as f64,
            NumberRepr::NegInt(i) => i as f64,
            NumberRepr::Float(f) => f,
        })
    }

    /// Classifies a [`serde_json::Number`] into the compact representation,
    /// preserving exactly which of the three kinds [`serde_json`] chose so
    /// reconstruction is byte-identical.
    fn from_serde(number: &serde_json::Number) -> Self {
        if let Some(u) = number.as_u64() {
            Self::from_u64(u)
        } else if let Some(i) = number.as_i64() {
            Self::from_i64(i)
        } else {
            // Neither a `u64` nor an `i64`, so by construction a finite
            // `f64` (a `serde_json::Number` is always one of the three).
            let f = number
                .as_f64()
                .expect("a serde_json Number that is neither u64 nor i64 is a finite f64");
            Self {
                repr: NumberRepr::Float(f),
            }
        }
    }

    /// Reconstructs the exact [`serde_json::Number`] this value came from, or
    /// `None` for a non-finite float — the one stored value JSON cannot
    /// represent at all (not even as an "impossible" `serde_json::Number`;
    /// [`serde_json::Number::from_f64`] itself rejects it). The caller
    /// ([`Value::to_serde_json`]) falls back to `null`, matching how the
    /// streaming parse path already collapses a non-finite value reaching it
    /// some other way (see [`ValueVisitor::visit_f64`]).
    fn to_serde_number(&self) -> Option<serde_json::Number> {
        match self.repr {
            NumberRepr::PosInt(u) => Some(serde_json::Number::from(u)),
            NumberRepr::NegInt(i) => Some(serde_json::Number::from(i)),
            NumberRepr::Float(f) => serde_json::Number::from_f64(f),
        }
    }
}

/// A Python `set`'s or `frozenset`'s members: duplicate-free, and held in
/// the crate's canonical set order.
///
/// A set's members reach `onix` in whatever order the source iterated them,
/// which for a real Python set is hash order — unreproducible from one
/// process to the next, and for `str` members dependent on
/// `PYTHONHASHSEED`. Nothing here depends on it: membership, hashing and
/// coercion all go through order-independent identities (`set_difference`'s
/// own doc, in `crate::ignore_order`, has the matching rule the set diff
/// compares members by), and the source order is dropped outright at
/// construction: [`SetItems::new`] stores the
/// members in the crate's **canonical set order** instead, so every
/// rendering of a set is canonical without sorting anything. Reproducing
/// `DeepDiff`'s own order-dependent answers is impossible, and matching them
/// is not worth being nondeterministic for. See `tests/golden/README.md`'s
/// "Set iteration order" section.
///
/// The order is: `None` first, then `bool`, `int`, `float`, `str`, `tuple`,
/// `frozenset`, `list`, `set`, `dict` and finally the two calendar kinds —
/// each kind after the last — and within a kind by value: booleans and
/// numbers numerically, strings by code point, datetimes by instant, dates
/// by ordinal, and every container element by element and then by length.
/// It is a purely structural comparison (the crate-private
/// `canonical_cmp`), so ordering a
/// set never renders its members.
///
/// A set has no duplicate members, so [`SetItems::new`] drops any member
/// equal to an earlier one.
///
/// # Examples
///
/// ```
/// use onix_core::{Number, Value};
/// use onix_core::value::SetItems;
///
/// let set = Value::Set(SetItems::new(vec![
///     Value::Number(Number::from_u64(2)),
///     Value::Number(Number::from_u64(1)),
/// ]));
/// assert_eq!(set.to_serde_json().to_string(), "[1,2]");
/// ```
#[derive(Debug, Clone, Default)]
pub struct SetItems {
    /// The members. Invariants, both established by [`SetItems::new`]: in
    /// ascending [`canonical_cmp`] order, and no two structurally equal.
    items: Box<[Value]>,
}

impl SetItems {
    /// Builds a set's members, sorting them into canonical set order and
    /// dropping any member equal to an earlier one.
    ///
    /// A real Python `set` cannot hold two equal members, but this
    /// constructor cannot assume it was handed one: this type is public, so
    /// a caller building a [`Value`] directly can still hand it two
    /// structurally equal members. Two equal members would render to the
    /// same path segment and so to the same *structural* report path, which
    /// [`crate::report::Report`] requires to be unique; dropping the later
    /// one is what a Python set would have done with the pair in the first
    /// place.
    ///
    /// Equality here is the structural one `canonical_cmp` decides, which
    /// is exactly what "renders to the same path segment" means. It is
    /// *finer* than the membership identity the diff itself compares by —
    /// `set_difference`'s own doc (in `crate::ignore_order`) has the exact,
    /// two-path matching rule: two members Python would call equal but that
    /// this crate can tell apart — `(1,)` and `(1.0,)` — are both kept here,
    /// and then reported as the two distinct items they are. No Python set
    /// can hold that particular pair, and the golden generator can never
    /// write one, so it is reachable only by building a [`Value`] directly.
    ///
    /// A naive and an aware `datetime` at one instant are the *opposite*
    /// case: `naive == aware` is `false` in Python, so `{naive, aware}` is a
    /// perfectly ordinary two-member set — `canonical_cmp` keeps both here
    /// too (it orders a `datetime` by instant, then by whether it is aware,
    /// so the two never compare equal) — even though the matching identity
    /// `set_difference` uses treats a same-instant naive/aware pair as one
    /// (again, see its doc for the exact rule), for comparing across two
    /// different sets. Storing every structurally
    /// distinct member and matching by a coarser identity are not in
    /// tension: this is the same split ordinary Rust `HashMap`/`HashSet`
    /// keys make between `Eq` and a custom-normalized lookup key. See
    /// `tests/golden/README.md`'s "Set iteration order" section for where
    /// this leaves `DeepDiff`'s own (hash-order-dependent) answer behind.
    ///
    /// `canonical_cmp`'s one deliberately *coarser* spot is a bare `-0.0`
    /// versus `0.0`: it folds them together (see `number_cmp`), so both
    /// dedup here exactly as a real Python `set` would (they hash and
    /// compare equal there too), instead of surviving as two members the
    /// way `(1,)`/`(1.0,)` do.
    ///
    /// A `NaN` member dedups too, but only against a bit-identical `NaN` —
    /// `canonical_cmp` never folds two differently-signed or -payloaded
    /// `NaN`s together, so it stays no coarser there than `PartialEq` (which
    /// never calls two `NaN`s equal at all). A real Python `set` can hold two
    /// members that are both, individually, `float('nan')` — `nan != nan`
    /// means they never dedup by value — so this is a real, if narrow,
    /// divergence: this crate's value model has no notion of the *object
    /// identity* Python's set falls back on, so a bit-identical pair of
    /// `NaN`s collapses to one canonical member here where two independently
    /// constructed Python `NaN` objects would not. See
    /// `tests/golden/README.md`'s "Non-finite floats" section.
    ///
    /// Comparing structurally rather than by identity is also what keeps
    /// this cheap: a comparison stops at the first difference, where
    /// building an identity always walks the whole member, which would make
    /// constructing a deeply nested set quadratic in its depth.
    ///
    /// Costs one `O(n log n)` sort of short-circuiting comparisons, and
    /// nothing at all below two members.
    #[must_use]
    pub fn new(mut items: Vec<Value>) -> Self {
        if items.len() < 2 {
            return Self {
                items: items.into_boxed_slice(),
            };
        }

        items.sort_by(canonical_cmp);
        items.dedup_by(|a, b| canonical_cmp(a, b).is_eq());

        Self {
            items: items.into_boxed_slice(),
        }
    }
}

impl std::ops::Deref for SetItems {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        &self.items
    }
}

impl<'a> IntoIterator for &'a SetItems {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

/// The crate's canonical set order, as a comparison — see [`SetItems`] for
/// the rule it implements, and why it is structural rather than based on
/// each member's rendered text (rendering a member to order it costs as much
/// as the member is big, which makes ordering a nested set quadratic in its
/// depth).
///
/// Iterative (an explicit heap work-stack, no native recursion), matching
/// [`Value`]'s [`PartialEq`] and `Drop`. It has to be: [`SetItems::new`]
/// sorts with it, and a set is built during *conversion*, which runs on the
/// caller's own thread: `onix-py`'s guard module hands the *diff* a
/// stack-sized worker thread, but conversion never gets one (see that
/// module's doc).
///
/// The stack holds the comparisons still owed, deepest-first, so a container
/// pushes its length tie-break underneath its elements and each element's
/// own sub-comparisons land on top: popping therefore visits exactly the
/// lexicographic order a recursive version would, and the first non-`Equal`
/// answer wins.
fn canonical_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    /// The kind's place in the documented order.
    fn rank(value: &Value) -> u8 {
        match value {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Number(n) if n.is_f64() => 3,
            Value::Number(_) => 2,
            Value::Str(_) => 4,
            Value::Tuple(_) => 5,
            Value::FrozenSet(_) => 6,
            Value::Array(_) => 7,
            Value::Set(_) => 8,
            Value::Object(_) => 9,
            Value::DateTime(_) => 10,
            Value::Date(_) => 11,
            Value::Time(_) => 12,
            Value::TimeDelta(_) => 13,
        }
    }

    /// One comparison still owed: two values, two dict keys, or the length
    /// tie-break a container falls back on once its elements all matched.
    enum Work<'a> {
        Values(&'a Value, &'a Value),
        Keys(&'a ObjectKey, &'a ObjectKey),
        Lengths(usize, usize),
    }

    /// Schedules `a` and `b`'s elements, in order, with their length
    /// tie-break last.
    fn push_slices<'a>(stack: &mut Vec<Work<'a>>, a: &'a [Value], b: &'a [Value]) {
        stack.push(Work::Lengths(a.len(), b.len()));
        for (a, b) in a.iter().zip(b.iter()).rev() {
            stack.push(Work::Values(a, b));
        }
    }

    let mut stack = vec![Work::Values(a, b)];

    while let Some(work) = stack.pop() {
        let ordering = match work {
            Work::Keys(a, b) => a.cmp(b),
            Work::Lengths(a, b) => a.cmp(&b),
            Work::Values(a, b) => {
                let ranking = rank(a).cmp(&rank(b));
                if ranking != Ordering::Equal {
                    return ranking;
                }

                match (a, b) {
                    (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
                    (Value::Number(x), Value::Number(y)) => number_cmp(x, y),
                    (Value::Str(x), Value::Str(y)) => x.cmp(y),
                    // By instant, then by whether the value is aware, so that
                    // two datetimes at one instant still order deterministically.
                    (Value::DateTime(x), Value::DateTime(y)) => x
                        .instant()
                        .cmp(&y.instant())
                        .then_with(|| x.utc_offset_seconds().cmp(&y.utc_offset_seconds())),
                    (Value::Date(x), Value::Date(y)) => x.ordinal().cmp(&y.ordinal()),
                    // Naive sorts before aware (an arbitrary but total
                    // split — `Time` has no cross-awareness instant the way
                    // `DateTime` does, since a naive value is never Python-
                    // equal to an aware one); within a group, by the same
                    // instant `times_equal` compares by, then by the raw
                    // offset as a final tie-break for two aware values that
                    // are Python-equal despite differing stored offsets —
                    // reachable only by building a `Value` directly, never a
                    // real Python set (see `SetItems::new`'s doc, and
                    // `DateTime`'s identical tie-break above).
                    (Value::Time(x), Value::Time(y)) => x
                        .utc_offset_seconds()
                        .is_some()
                        .cmp(&y.utc_offset_seconds().is_some())
                        .then_with(|| x.sort_instant().cmp(&y.sort_instant()))
                        .then_with(|| x.utc_offset_seconds().cmp(&y.utc_offset_seconds())),
                    (Value::TimeDelta(x), Value::TimeDelta(y)) => x.cmp(y),
                    (Value::Array(x), Value::Array(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
                        push_slices(&mut stack, x, y);
                        Ordering::Equal
                    }
                    // A set is stored in this very order, so its members
                    // compare element-wise like any other sequence.
                    (Value::Set(x), Value::Set(y)) | (Value::FrozenSet(x), Value::FrozenSet(y)) => {
                        push_slices(&mut stack, x, y);
                        Ordering::Equal
                    }
                    (Value::Object(x), Value::Object(y)) => {
                        stack.push(Work::Lengths(x.entries.len(), y.entries.len()));
                        for ((x_key, x_value), (y_key, y_value)) in
                            x.entries.iter().zip(y.entries.iter()).rev()
                        {
                            stack.push(Work::Values(x_value, y_value));
                            stack.push(Work::Keys(x_key, y_key));
                        }
                        Ordering::Equal
                    }
                    // Equal ranks with no arm above can only be `Null`
                    // against `Null`.
                    _ => Ordering::Equal,
                }
            }
        };

        if ordering != Ordering::Equal {
            return ordering;
        }
    }

    Ordering::Equal
}

/// Maps `-0.0` to `+0.0` and leaves every other float — including a `NaN` of
/// any sign or payload — unchanged.
///
/// `-0.0` and `+0.0` are one value: Python's `==` and `hash` agree on it (a
/// `set` can hold only one), and so does [`Number`]'s own [`PartialEq`]
/// (IEEE `==`). Every place this crate orders or hashes a float folds the
/// sign away first with this function, so all of them agree with each other
/// and with that equality — `canonical_cmp`'s [`number_cmp`], and
/// `crate::ignore_order::hash`'s `number_key` and `keyed`.
///
/// `NaN` is deliberately excluded from the `+ 0.0` fold rather than just
/// happening to pass through it unchanged: IEEE-754 addition does not
/// guarantee a NaN operand's own bits survive an arithmetic op — on this
/// crate's tier-1 targets it quiets a signaling NaN (flips its top mantissa
/// bit), which would make [`number_cmp`]'s [`f64::total_cmp`] (and
/// `crate::ignore_order::hash`'s bit-based keys) silently key two distinct
/// inputs on a value neither one actually is. Skipping the fold for any
/// `NaN` keeps this function the identity on every bit pattern it does not
/// explicitly normalize.
pub(crate) fn fold_signed_zero(f: f64) -> f64 {
    if f.is_nan() { f } else { f + 0.0 }
}

/// [`canonical_cmp`]'s number case, for two numbers of the same kind (an
/// int and a float are already ranked apart). Orders by [`fold_signed_zero`]
/// of each float, so this agrees with [`Number`]'s own [`PartialEq`] on every
/// non-`NaN` pair.
///
/// [`f64::total_cmp`] gives `NaN` a well-defined (if arbitrary) place in the
/// order, by raw bits, so two floats that are `NaN` compare `Equal` here
/// exactly when they share a bit pattern — coarser than [`Number`]'s
/// [`PartialEq`], which (matching Python's `nan != nan`) never calls two
/// `NaN`s equal. This is safe for [`SetItems::new`]'s dedup: it can only
/// ever *drop* two bit-identical `NaN`s into one canonical member, a
/// deterministic choice `tests/golden/README.md`'s "Non-finite floats"
/// section documents as a divergence from a real Python `set` (which keeps
/// two distinct-`NaN`-object members apart by identity, something this
/// crate's value model has no notion of).
fn number_cmp(a: &Number, b: &Number) -> std::cmp::Ordering {
    if a.is_f64() {
        let af = fold_signed_zero(a.as_f64().unwrap_or_default());
        let bf = fold_signed_zero(b.as_f64().unwrap_or_default());
        return af.total_cmp(&bf);
    }

    match (a.as_i64(), b.as_i64()) {
        (Some(x), Some(y)) => x.cmp(&y),
        // A `u64` above `i64::MAX` has no `i64` form, and is greater than
        // every value that does.
        (x, y) => x.is_some().cmp(&y.is_some()).reverse().then_with(|| {
            a.as_u64()
                .unwrap_or_default()
                .cmp(&b.as_u64().unwrap_or_default())
        }),
    }
}

/// A JSON object: key-sorted, exactly-sized entries backed by a single
/// `Box<[(ObjectKey, Value)]>`, with binary-search lookup
/// ([`get`](Object::get)/[`contains_key`](Object::contains_key)) and
/// ascending-key iteration.
///
/// See the [module documentation](self) for why entries are sorted and
/// `str` keys interned (byte-identical rendering and small-map footprint),
/// and [`ObjectKey`]'s own doc for why every other key kind is a second,
/// additive case rather than a change to that representation.
#[derive(Debug, Clone)]
pub struct Object {
    /// Key-sorted, duplicate-free entries. Invariant: strictly ascending by
    /// [`ObjectKey`]'s own [`Ord`] (enforced by [`Object::from_pairs`]) —
    /// every [`ObjectKey::Str`] entry before every [`ObjectKey::Other`] one,
    /// so [`Object::has_non_str_keys`] can check the last entry alone.
    entries: Box<[(ObjectKey, Value)]>,
}

impl Object {
    /// Builds an object from arbitrary `(key, value)` pairs: sorts them by
    /// [`ObjectKey`]'s own order and collapses duplicate keys keeping the
    /// last value seen (matching [`serde_json`], whose `BTreeMap` insert
    /// overwrites), so the stored entries satisfy the strictly-ascending
    /// invariant.
    pub(crate) fn from_pairs(mut pairs: Vec<(ObjectKey, Value)>) -> Self {
        // Stable sort keeps duplicate keys in their original order, so the
        // overwrite loop below retains the *last* occurrence's value.
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut entries: Vec<(ObjectKey, Value)> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            if let Some(last) = entries.last_mut()
                && last.0 == key
            {
                last.1 = value;
                continue;
            }
            entries.push((key, value));
        }
        Self {
            entries: entries.into_boxed_slice(),
        }
    }

    /// Returns the value for `key`, or `None` if the object has no such key.
    /// `O(log n)` binary search over the sorted entries.
    #[must_use]
    pub fn get(&self, key: &ObjectKey) -> Option<&Value> {
        self.entries
            .binary_search_by(|(entry_key, _)| entry_key.cmp(key))
            .ok()
            .map(|index| &self.entries[index].1)
    }

    /// Returns `true` if the object contains `key`. `O(log n)`.
    #[must_use]
    pub fn contains_key(&self, key: &ObjectKey) -> bool {
        self.entries
            .binary_search_by(|(entry_key, _)| entry_key.cmp(key))
            .is_ok()
    }

    /// [`Object::get`] for a plain `&str`, with no [`ObjectKey`] to
    /// construct: every [`ObjectKey::Str`] entry sorts before every
    /// [`ObjectKey::Other`] one (see [`ObjectKey`]'s `Ord`), so comparing an
    /// `Other` entry as "greater than any `str`" keeps the binary search
    /// correct without allocating — the same `O(log n)`, zero-allocation
    /// lookup this crate has always given a `str`-only object, now also
    /// available on one that mixes in a non-`str` key elsewhere.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&Value> {
        self.entries
            .binary_search_by(|(entry_key, _)| match entry_key {
                ObjectKey::Str(s) => s.as_ref().cmp(key),
                ObjectKey::Other(_) => std::cmp::Ordering::Greater,
            })
            .ok()
            .map(|index| &self.entries[index].1)
    }

    /// [`Object::contains_key`] for a plain `&str` — see [`Object::get_str`].
    #[must_use]
    pub fn contains_key_str(&self, key: &str) -> bool {
        self.get_str(key).is_some()
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the object has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns `true` if any key is an [`ObjectKey::Other`] — `O(1)`, since
    /// `Object::from_pairs`'s sort always puts every `Other` key after
    /// every `Str` one, so the last entry alone answers the question. Every
    /// dict-diffing call site that would otherwise pay for python-equality
    /// key matching (`crate::diff::object`, `crate::ignore_order::distance`)
    /// checks this first and takes an unchanged, allocation-free path when
    /// both sides answer `false`.
    #[must_use]
    pub fn has_non_str_keys(&self) -> bool {
        matches!(self.entries.last(), Some((ObjectKey::Other(_), _)))
    }

    /// Iterates `(key, value)` pairs in ascending key order.
    #[must_use]
    pub fn iter(&self) -> Entries<'_> {
        Entries {
            inner: self.entries.iter(),
        }
    }

    /// Iterates keys in ascending order.
    pub fn keys(&self) -> impl Iterator<Item = &ObjectKey> {
        self.entries.iter().map(|(key, _)| key)
    }

    /// Iterates values in ascending key order.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.iter().map(|(_, value)| value)
    }
}

impl<'a> IntoIterator for &'a Object {
    type Item = (&'a ObjectKey, &'a Value);
    type IntoIter = Entries<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over an [`Object`]'s `(key, value)` entries in ascending key
/// order, yielded by [`Object::iter`] and `&Object`'s [`IntoIterator`].
pub struct Entries<'a> {
    inner: std::slice::Iter<'a, (ObjectKey, Value)>,
}

impl<'a> Iterator for Entries<'a> {
    type Item = (&'a ObjectKey, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| (key, value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl DoubleEndedIterator for Entries<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back().map(|(key, value)| (key, value))
    }
}

impl ExactSizeIterator for Entries<'_> {}

/// A per-session string interner sharing one `Arc<str>` per distinct key.
///
/// A single [`Interner`] is threaded through one whole conversion or parse
/// (see [`from_serde`] and the [`Deserialize`] impl); it exists only during
/// construction, and the finished [`Value`] holds the shared handles while
/// the lookup table is dropped. See the [module documentation](self) for the
/// key-interning footprint rationale.
#[derive(Debug, Default)]
struct Interner {
    seen: HashSet<Arc<str>>,
}

impl Interner {
    /// Creates an empty interner.
    fn new() -> Self {
        Self::default()
    }

    /// Returns a shared `Arc<str>` for `key`, allocating one only the first
    /// time a given key string is seen this session.
    fn intern(&mut self, key: &str) -> Arc<str> {
        if let Some(existing) = self.seen.get(key) {
            return Arc::clone(existing);
        }
        let shared: Arc<str> = Arc::from(key);
        self.seen.insert(Arc::clone(&shared));
        shared
    }
}

/// Builds compact [`Value`]s while interning object keys across one
/// construction session.
///
/// A caller assembling a large tree from an external source — the Python
/// bindings walking a live object graph, say — threads one `Builder` through
/// the whole walk and routes every object through [`Builder::object`], so a
/// key repeated across many objects costs a single `Arc<str>` allocation
/// shared by reference count. This is the same interning [`From`] and
/// [`Deserialize`] perform internally, exposed for callers that build a
/// [`Value`] some other way (e.g. from Python objects rather than JSON).
///
/// # Examples
///
/// ```
/// use onix_core::Value;
/// use onix_core::value::Builder;
///
/// let mut builder = Builder::new();
/// let value = builder.object(vec![
///     ("b".to_owned(), Value::Bool(true)),
///     ("a".to_owned(), Value::Null),
/// ]);
/// // Rendered back out, keys are in canonical (sorted) order.
/// assert_eq!(value.to_serde_json().to_string(), r#"{"a":null,"b":true}"#);
/// ```
#[derive(Debug, Default)]
pub struct Builder {
    interner: Interner,
}

impl Builder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds an object [`Value`] from `entries`, interning each key against
    /// this builder's session and sorting into the canonical ascending
    /// key-string order. A duplicate key keeps the last value, matching
    /// [`From`] and [`Deserialize`].
    #[must_use]
    pub fn object(&mut self, entries: Vec<(String, Value)>) -> Value {
        let pairs = entries
            .into_iter()
            .map(|(key, value)| (ObjectKey::Str(self.interner.intern(&key)), value))
            .collect();
        Value::Object(Object::from_pairs(pairs))
    }

    /// Interns `key` against this builder's session, exactly as
    /// [`Builder::object`] does internally — exposed so a caller building an
    /// [`ObjectKey`] directly (for [`Builder::object_with_keys`], because the
    /// dict it is converting has a non-`str` key somewhere) still shares one
    /// `str` key's allocation across every object that repeats it.
    #[must_use]
    pub fn intern(&mut self, key: &str) -> Arc<str> {
        self.interner.intern(key)
    }

    /// Builds an object [`Value`] from `entries`, which may carry any
    /// [`ObjectKey`] — the general form of [`Builder::object`] for a caller
    /// that has already classified its keys (`onix-py`'s conversion, which
    /// must tell a `str` key needing [`Builder::intern`] apart from any other
    /// kind).
    #[must_use]
    pub fn object_with_keys(&mut self, entries: Vec<(ObjectKey, Value)>) -> Value {
        Value::Object(Object::from_pairs(entries))
    }
}

impl<'de> Deserialize<'de> for Value {
    /// Streams a [`Value`] directly from any [`Deserializer`] with no
    /// transient [`serde_json::Value`] tree, interning object keys across the
    /// whole parse in one session. Driven by [`serde_json`]'s own
    /// deserializer (e.g. via [`serde_json::from_str`]), this is the
    /// peak-memory path for parsing untrusted input.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut interner = Interner::new();
        ValueSeed {
            interner: &mut interner,
        }
        .deserialize(deserializer)
    }
}

/// A [`DeserializeSeed`] carrying the session [`Interner`] down through
/// nested containers, so every object key parsed anywhere in the tree is
/// interned against the same table.
struct ValueSeed<'i> {
    interner: &'i mut Interner,
}

impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor {
            interner: self.interner,
        })
    }
}

/// The [`Visitor`] that maps each self-describing input token onto a
/// [`Value`], mirroring [`serde_json::Value`]'s own visitor semantics
/// (including non-finite floats collapsing to `Null`) so parsing the same
/// input yields byte-identical output.
struct ValueVisitor<'i> {
    interner: &'i mut Interner,
}

impl<'de> Visitor<'de> for ValueVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any valid JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(Number::from_i64(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(Number::from_u64(value)))
    }

    fn visit_i128<E>(self, value: i128) -> Result<Value, E>
    where
        E: serde::de::Error,
    {
        i64::try_from(value)
            .map(Number::from_i64)
            .or_else(|_| u64::try_from(value).map(Number::from_u64))
            .map(Value::Number)
            .map_err(|_| E::custom("integer out of range for a JSON number"))
    }

    fn visit_u128<E>(self, value: u128) -> Result<Value, E>
    where
        E: serde::de::Error,
    {
        u64::try_from(value)
            .map(Number::from_u64)
            .map(Value::Number)
            .map_err(|_| E::custom("integer out of range for a JSON number"))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
        if value.is_finite() {
            return Ok(Value::Number(Number::from_f64(value)));
        }
        // Non-finite floats have no JSON representation; collapse to Null,
        // exactly as serde_json::Value's own visitor does. Unreachable
        // through serde_json's own parser (its grammar has no
        // `NaN`/`Infinity` literal), so this only matters for another
        // `Deserializer` implementation driving this same `Visitor`.
        Ok(Value::Null)
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::Str(Box::from(value)))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::Str(value.into_boxed_str()))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items: Vec<Value> = Vec::new();
        while let Some(item) = seq.next_element_seed(ValueSeed {
            interner: self.interner,
        })? {
            items.push(item);
        }
        Ok(Value::Array(items.into_boxed_slice()))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut pairs: Vec<(ObjectKey, Value)> = Vec::new();
        while let Some(key) = map.next_key::<Cow<'_, str>>()? {
            let interned = ObjectKey::Str(self.interner.intern(&key));
            let value = map.next_value_seed(ValueSeed {
                interner: self.interner,
            })?;
            pairs.push((interned, value));
        }
        Ok(Value::Object(Object::from_pairs(pairs)))
    }
}

#[cfg(test)]
#[path = "value_tests.rs"]
mod tests;
