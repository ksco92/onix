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
//! * **Objects** are an exactly-sized, key-sorted `Box<[(Arc<str>, Value)]>`
//!   — one heap block holding precisely the entries present, no spare slots.
//!   Sorted by key string means lookups are a binary search and iteration is
//!   in the same lexicographic order [`serde_json`]'s `BTreeMap` produces, so
//!   anything rendered from a [`Value`] stays byte-identical.
//! * **Keys** are interned within one conversion/parse session (see
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
//! [`Value::Tuple`]) and through [`Object`]'s entries, so a naive derived `Drop` would
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
//! native stack either. The derived [`Debug`] and [`Clone`] are deliberately
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

use crate::datetime::{Date, DateTime};

/// A compact JSON value: the memory-frugal counterpart of
/// [`serde_json::Value`].
///
/// See the [module documentation](self) for the representation choices and
/// their rationale. Six of the variants mirror JSON's own shapes; objects
/// are held as an [`Object`] (a sorted, exactly-sized entry slice) and
/// numbers as a [`Number`] preserving the `i64`/`u64`/`f64` distinction.
///
/// [`Value::Tuple`] is the seventh, and the only one JSON itself cannot
/// express: a Python `tuple`, which `DeepDiff` diffs positionally exactly
/// like a `list` but reports as a *different type* from one (`tuple` vs
/// `list` is a `type_changes` finding, and the two never hash-match under
/// `ignore_order`). It carries the same `Box<[Value]>` payload as
/// [`Value::Array`] and renders to the same JSON array in
/// [`Value::to_serde_json`]; the separate variant is what makes the
/// type distinction structural, so mixing the two can only ever be a
/// compile error or a `type_changes`, never a silent equality. Neither
/// [`From`]`<`[`serde_json::Value`]`>` nor [`Deserialize`] can produce one
/// (JSON has no tuple literal): a tuple enters the model only from a caller
/// holding real Python objects, through [`Value::Tuple`] directly.
///
/// [`Value::DateTime`] and [`Value::Date`] are the same story for the two
/// calendar types (see [`mod@crate::datetime`]). They are kept as *structured*
/// values rather than pre-rendered ISO strings because `DeepDiff` renders the
/// same datetime two different ways depending on where it lands in a report
/// (UTC-normalized in `values_changed`, raw everywhere else) and because
/// [`crate::Report::to_value`] must hand a real `datetime` object back to a
/// caller holding Python objects — neither is possible once the value has
/// collapsed to a string. JSON has no literal for either, so, like a tuple,
/// they enter the model only from such a caller.
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
    /// An array, stored as an exactly-sized `Box<[Value]>`.
    Array(Box<[Value]>),
    /// A Python tuple, stored exactly like [`Value::Array`] but kept as a
    /// distinct variant — see this type's own doc for why.
    Tuple(Box<[Value]>),
    /// An object: key-sorted, exactly-sized entries (see [`Object`]).
    Object(Object),
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
            Value::Number(n) => serde_json::Value::Number(n.to_serde_number()),
            Value::Str(s) => serde_json::Value::String(s.as_ref().to_owned()),
            Value::DateTime(value) => serde_json::Value::String(value.isoformat()),
            Value::Date(value) => serde_json::Value::String(value.isoformat()),
            Value::Array(items) | Value::Tuple(items) => {
                serde_json::Value::Array(items.iter().map(Value::to_serde_json).collect())
            }
            Value::Object(obj) => {
                let mut map = serde_json::Map::with_capacity(obj.len());
                for (key, value) in obj {
                    map.insert(key.to_owned(), value.to_serde_json());
                }
                serde_json::Value::Object(map)
            }
        }
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
                .map(|(key, value)| (interner.intern(&key), from_serde(value, interner)))
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
        Value::Object(obj) => {
            let taken = std::mem::take(&mut obj.entries);
            stack.extend(taken.into_vec().into_iter().map(|(_, value)| value));
        }
        Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Str(_)
        | Value::DateTime(_)
        | Value::Date(_) => {}
    }
}

/// Iterative structural equality backing `Value`'s [`PartialEq`]. Semantics
/// match a derived `PartialEq`: same-variant structural equality — so an
/// array and a tuple holding identical items are never equal, matching
/// `DeepDiff`'s own `tuple`-vs-`list` type distinction — with
/// `Number`'s variant sensitivity intact (`PosInt(1)` is not equal to
/// `Float(1.0)`); objects compare over their sorted entries (equal key sets
/// and per-key values), arrays over equal length and per-index values. See
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
            (Value::Array(x), Value::Array(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
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
/// (`f64`). Floats are always finite: the constructors reject non-finite
/// input, so every stored `f64` round-trips through [`serde_json::Number`].
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
    /// A finite float.
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

    /// Builds a number from an `f64`, returning `None` for non-finite input
    /// (`NaN`/`Infinity`), which JSON cannot represent — mirroring
    /// [`serde_json::Number::from_f64`].
    #[must_use]
    pub fn from_f64(value: f64) -> Option<Self> {
        value.is_finite().then_some(Self {
            repr: NumberRepr::Float(value),
        })
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

    /// Reconstructs the exact [`serde_json::Number`] this value came from.
    fn to_serde_number(&self) -> serde_json::Number {
        match self.repr {
            NumberRepr::PosInt(u) => serde_json::Number::from(u),
            NumberRepr::NegInt(i) => serde_json::Number::from(i),
            NumberRepr::Float(f) => serde_json::Number::from_f64(f)
                .expect("stored floats are finite by construction, so from_f64 succeeds"),
        }
    }
}

/// A JSON object: key-sorted, exactly-sized entries backed by a single
/// `Box<[(Arc<str>, Value)]>`, with binary-search lookup
/// ([`get`](Object::get)/[`contains_key`](Object::contains_key)) and
/// ascending-key iteration.
///
/// See the [module documentation](self) for why entries are sorted and keys
/// interned (byte-identical rendering and small-map footprint).
#[derive(Debug, Clone)]
pub struct Object {
    /// Key-sorted, duplicate-free entries. Invariant: strictly ascending by
    /// key string (enforced by [`Object::from_pairs`]).
    entries: Box<[(Arc<str>, Value)]>,
}

impl Object {
    /// Builds an object from arbitrary `(key, value)` pairs: sorts them by
    /// key string and collapses duplicate keys keeping the last value seen
    /// (matching [`serde_json`], whose `BTreeMap` insert overwrites), so the
    /// stored entries satisfy the strictly-ascending invariant.
    pub(crate) fn from_pairs(mut pairs: Vec<(Arc<str>, Value)>) -> Self {
        // Stable sort keeps duplicate keys in their original order, so the
        // overwrite loop below retains the *last* occurrence's value.
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut entries: Vec<(Arc<str>, Value)> = Vec::with_capacity(pairs.len());
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
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .binary_search_by(|(entry_key, _)| (**entry_key).cmp(key))
            .ok()
            .map(|index| &self.entries[index].1)
    }

    /// Returns `true` if the object contains `key`. `O(log n)`.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.entries
            .binary_search_by(|(entry_key, _)| (**entry_key).cmp(key))
            .is_ok()
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

    /// Iterates `(key, value)` pairs in ascending key order.
    #[must_use]
    pub fn iter(&self) -> Entries<'_> {
        Entries {
            inner: self.entries.iter(),
        }
    }

    /// Iterates keys in ascending order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(key, _)| key.as_ref())
    }

    /// Iterates values in ascending key order.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.iter().map(|(_, value)| value)
    }
}

impl<'a> IntoIterator for &'a Object {
    type Item = (&'a str, &'a Value);
    type IntoIter = Entries<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over an [`Object`]'s `(key, value)` entries in ascending key
/// order, yielded by [`Object::iter`] and `&Object`'s [`IntoIterator`].
pub struct Entries<'a> {
    inner: std::slice::Iter<'a, (Arc<str>, Value)>,
}

impl<'a> Iterator for Entries<'a> {
    type Item = (&'a str, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| (key.as_ref(), value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
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
            .map(|(key, value)| (self.interner.intern(&key), value))
            .collect();
        Value::Object(Object::from_pairs(pairs))
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
        // Non-finite floats have no JSON representation; collapse to Null,
        // exactly as serde_json::Value's own visitor does.
        Ok(Number::from_f64(value).map_or(Value::Null, Value::Number))
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
        let mut pairs: Vec<(Arc<str>, Value)> = Vec::new();
        while let Some(key) = map.next_key::<Cow<'_, str>>()? {
            let interned = self.interner.intern(&key);
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
