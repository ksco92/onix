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
//!
//! # Subclasses
//!
//! [`Value::DateTime`]/[`Value::Date`]/[`Value::Array`]/[`Value::Tuple`]
//! wrap their payload in [`Typed`], and [`SetItems`]/[`Object`] carry an
//! equivalent `type_name` field, so a Python subclass instance keeps the
//! source class name it needs to report a `type_changes` finding, while
//! comparing, hashing, and rendering exactly like its base type everywhere
//! else — every matching identity in the crate (`SetItems` dedup,
//! `crate::lcs`'s scalar-list matching, `crate::ignore_order`'s hashing)
//! is unaffected, since none of them read the class name. `diff_at`
//! (`crate::diff`) is the one place that does, gating its per-pair dispatch
//! on it before recursing into any of those six variants.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::ops::Deref;
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
/// Five variants are the ones JSON itself cannot express:
/// [`Value::Tuple`], [`Value::Set`] and [`Value::FrozenSet`] — the Python
/// `tuple`, `set` and `frozenset` — and [`Value::DateTime`] and
/// [`Value::Date`]. Each is a *different type* from every other and from
/// `list` (a `tuple`-vs-`list` or `set`-vs-`frozenset` pairing is a
/// `type_changes` finding, and neither pair ever hash-matches under
/// `ignore_order`), which is exactly why each gets its own variant: the type
/// distinction is structural, so mixing two of them can only ever be a
/// compile error or a `type_changes`, never a silent equality. The three
/// container kinds render to a JSON array in [`Value::to_serde_json`],
/// matching what `DeepDiff`'s own `to_json()` shows.
///
/// The two calendar types (see [`mod@crate::datetime`]) are kept as
/// *structured* values rather than pre-rendered ISO strings because
/// `DeepDiff` renders the same datetime two different ways depending on
/// where it lands in a report (UTC-normalized in `values_changed`, raw
/// everywhere else) and because [`crate::Report::to_value`] must hand a real
/// `datetime` object back to a caller holding Python objects — neither is
/// possible once the value has collapsed to a string.
///
/// Neither [`From`]`<`[`serde_json::Value`]`>` nor [`Deserialize`] can
/// produce any of the five (JSON has no literal for them): they enter the
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
    /// doc for why it is a variant rather than a pre-rendered string. Wrapped
    /// in [`Typed`] so a `datetime` subclass (e.g. pandas `Timestamp`)
    /// carries its own class name — see this module's "Subclass type names"
    /// section.
    DateTime(Typed<DateTime>),
    /// A Python `datetime.date` — see [`Date`]. See [`Value::DateTime`]'s
    /// doc for the [`Typed`] wrapper.
    Date(Typed<Date>),
    /// An array, stored as an exactly-sized `Box<[Value]>` — wrapped in
    /// [`Typed`] for a `list` subclass, see this module's "Subclass type
    /// names" section.
    Array(Typed<Box<[Value]>>),
    /// A Python tuple, stored exactly like [`Value::Array`] but kept as a
    /// distinct variant — see this type's own doc for why. Also carries a
    /// [`Typed`] class name for a `tuple` subclass, including a
    /// `namedtuple` — see this module's "Subclass type names" section for
    /// how a `namedtuple` is diffed.
    Tuple(Typed<Box<[Value]>>),
    /// A Python `set`, stored as canonically ordered [`SetItems`], which
    /// carries its own optional class name for a `set` subclass — see this
    /// module's "Subclass type names" section.
    Set(SetItems),
    /// A Python `frozenset`, stored exactly like [`Value::Set`] but kept as
    /// a distinct variant — see this type's own doc for why.
    FrozenSet(SetItems),
    /// An object: key-sorted, exactly-sized entries (see [`Object`]), which
    /// carries its own optional class name for a `dict` subclass — see this
    /// module's "Subclass type names" section.
    Object(Object),
}

/// Wraps a value with the source Python class name, when it differs from
/// the base type this [`Value`] variant represents (`None` for the exact
/// base type). [`PartialEq`] compares only the wrapped value, ignoring the
/// class name — see the [module documentation](self)'s "Subclasses"
/// section, and `diff_at`'s class-name gate (`crate::diff`) for where the
/// name is checked instead.
#[derive(Debug, Clone)]
pub struct Typed<T> {
    inner: T,
    class_name: Option<Arc<str>>,
}

impl<T> Typed<T> {
    /// Wraps `inner` with no subclass name (the exact base type).
    #[must_use]
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            class_name: None,
        }
    }

    /// Wraps `inner` with an explicit subclass name (`None` for the exact
    /// base type, matching [`Typed::new`]).
    #[must_use]
    pub fn with_class_name(inner: T, class_name: Option<Arc<str>>) -> Self {
        Self { inner, class_name }
    }

    /// The subclass name this value carries, or `None` for the exact base
    /// type.
    #[must_use]
    pub fn class_name(&self) -> Option<&str> {
        self.class_name.as_deref()
    }

    /// Unwraps into the inner value, discarding the class name.
    pub(crate) fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: Copy> Typed<T> {
    /// A copy of the wrapped value, discarding the class name — for the
    /// small `Copy` payloads ([`DateTime`], [`Date`]) that call sites need
    /// to move out of a `Typed<T>` reference.
    #[must_use]
    pub fn value(&self) -> T {
        self.inner
    }
}

impl<T> Deref for Typed<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> From<T> for Typed<T> {
    fn from(inner: T) -> Self {
        Self::new(inner)
    }
}

/// A fixed-size boxed array (`Box::new([a, b, c])`, the common test-literal
/// shape for [`Value::Array`]/[`Value::Tuple`]) unsize-coerces to
/// `Box<[Value]>` on assignment, so it can build a [`Typed<Box<[Value]>>`]
/// the same way a `Vec<Value>`'s `.into_boxed_slice()` does.
impl<const N: usize> From<Box<[Value; N]>> for Typed<Box<[Value]>> {
    fn from(items: Box<[Value; N]>) -> Self {
        let items: Box<[Value]> = items;
        Self::new(items)
    }
}

/// Content-only equality: deliberately ignores `class_name` — see
/// [`Typed`]'s own doc for why matching identity is class-agnostic
/// throughout the crate.
impl<T: PartialEq> PartialEq for Typed<T> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

/// The subclass name `value` carries, or `None` for the exact base type —
/// `None` for every [`Value`] variant that cannot carry one at all (`Null`,
/// `Bool`, `Number`, `Str`). The one place every one of [`Typed`]'s and
/// [`SetItems`]'/[`Object`]'s `class_name`/`type_name` accessors is read
/// together, so `diff_at`'s (`crate::diff`'s recursive dispatch core)
/// type-change gate and [`Value`]'s own structural equality (below) share
/// one definition.
#[must_use]
pub(crate) fn class_name(value: &Value) -> Option<&str> {
    match value {
        Value::DateTime(t) => t.class_name(),
        Value::Date(t) => t.class_name(),
        Value::Array(t) | Value::Tuple(t) => t.class_name(),
        Value::Set(items) | Value::FrozenSet(items) => items.type_name(),
        Value::Object(map) => map.type_name(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Str(_) => None,
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
            Value::Number(n) => serde_json::Value::Number(n.to_serde_number()),
            Value::Str(s) => serde_json::Value::String(s.as_ref().to_owned()),
            Value::DateTime(value) => serde_json::Value::String(value.isoformat()),
            Value::Date(value) => serde_json::Value::String(value.isoformat()),
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
        serde_json::Value::Array(items) => Value::Array(Typed::new(
            items
                .into_iter()
                .map(|item| from_serde(item, interner))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )),
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
            let taken = std::mem::replace(items, Typed::new(Box::default()));
            stack.extend(taken.into_inner().into_vec());
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
        | Value::Date(_) => {}
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
        // `DeepDiff` reports a subclass-vs-base pair as a `type_changes`
        // finding even when every field matches (see [`Typed`]'s doc), so
        // two otherwise-identical values with different class names are not
        // structurally equal — checked once here rather than per-arm below,
        // since it applies identically to all six variants that can carry a
        // class name and is a no-op (`None == None`) for the rest.
        if class_name(a) != class_name(b) {
            return false;
        }

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
    /// The `set`/`frozenset` subclass name this value came from, or `None`
    /// for the exact base type — see [`Typed`]'s "Subclass type names" doc
    /// section (this field is that same concept, plain rather than wrapped,
    /// since [`SetItems::new`] already has its own constructor function to
    /// hide it behind).
    type_name: Option<Arc<str>>,
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
                type_name: None,
            };
        }

        items.sort_by(canonical_cmp);
        items.dedup_by(|a, b| canonical_cmp(a, b).is_eq());

        Self {
            items: items.into_boxed_slice(),
            type_name: None,
        }
    }

    /// Attaches a `set`/`frozenset` subclass name (`None` for the exact base
    /// type), for a caller (`onix-py`'s converter) that already has a
    /// built [`SetItems`] and knows which concrete class it came from.
    #[must_use]
    pub fn with_type_name(mut self, type_name: Option<Arc<str>>) -> Self {
        self.type_name = type_name;
        self
    }

    /// The subclass name this set carries, or `None` for the exact base
    /// type.
    #[must_use]
    pub fn type_name(&self) -> Option<&str> {
        self.type_name.as_deref()
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
        }
    }

    /// One comparison still owed: two values, two dict keys, or the length
    /// tie-break a container falls back on once its elements all matched.
    enum Work<'a> {
        Values(&'a Value, &'a Value),
        Keys(&'a str, &'a str),
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

/// Maps `-0.0` to `+0.0` and leaves every other float unchanged.
///
/// `-0.0` and `+0.0` are one value: Python's `==` and `hash` agree on it (a
/// `set` can hold only one), and so does [`Number`]'s own [`PartialEq`]
/// (IEEE `==`). Every place this crate orders or hashes a float folds the
/// sign away first with this function, so all of them agree with each other
/// and with that equality — `canonical_cmp`'s [`number_cmp`], and
/// `crate::ignore_order::hash`'s `number_key` and `keyed`.
pub(crate) fn fold_signed_zero(f: f64) -> f64 {
    f + 0.0
}

/// [`canonical_cmp`]'s number case, for two numbers of the same kind (an
/// int and a float are already ranked apart). Orders by [`fold_signed_zero`]
/// of each float, so this agrees with [`Number`]'s own [`PartialEq`].
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
    /// The `dict` subclass name this value came from, or `None` for the
    /// exact base type — see [`SetItems`]'s own `type_name` field doc; the
    /// same reasoning applies here.
    type_name: Option<Arc<str>>,
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
            type_name: None,
        }
    }

    /// Attaches a `dict` subclass name (`None` for the exact base type), for
    /// a caller (`onix-py`'s converter) that already has a built [`Object`]
    /// and knows which concrete class it came from.
    #[must_use]
    pub fn with_type_name(mut self, type_name: Option<Arc<str>>) -> Self {
        self.type_name = type_name;
        self
    }

    /// The subclass name this object carries, or `None` for the exact base
    /// type.
    #[must_use]
    pub fn type_name(&self) -> Option<&str> {
        self.type_name.as_deref()
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

impl DoubleEndedIterator for Entries<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner
            .next_back()
            .map(|(key, value)| (key.as_ref(), value))
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
        Value::Object(self.build_object(entries))
    }

    /// [`Builder::object`], additionally attaching a `dict` subclass name
    /// (`None` for the exact base type) — the entry point `onix-py`'s
    /// converter uses for a `dict` subclass, see [`Object::with_type_name`]
    /// and [`Typed`]'s "Subclass type names" doc section. A separate method
    /// rather than an added parameter on [`Builder::object`] because every
    /// other caller of that method never has a subclass name to attach.
    #[must_use]
    pub fn object_with_type_name(
        &mut self,
        entries: Vec<(String, Value)>,
        type_name: Option<Arc<str>>,
    ) -> Value {
        Value::Object(self.build_object(entries).with_type_name(type_name))
    }

    /// The shared body of [`Builder::object`]/[`Builder::object_with_type_name`]:
    /// interns each key against this builder's session and sorts into the
    /// canonical ascending key-string order, without yet deciding whether
    /// the result carries a subclass name.
    fn build_object(&mut self, entries: Vec<(String, Value)>) -> Object {
        let pairs = entries
            .into_iter()
            .map(|(key, value)| (self.interner.intern(&key), value))
            .collect();
        Object::from_pairs(pairs)
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
        Ok(Value::Array(items.into_boxed_slice().into()))
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
