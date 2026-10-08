//! A compact, JSON-shaped value model: a memory-frugal stand-in for
//! [`serde_json::Value`] with byte-identical rendering.
//!
//! * Objects are an exactly-sized slice sorted like [`serde_json`]'s `BTreeMap`; `str` keys are
//!   interned per session.
//! * Numbers keep the `i64`/`u64`/`f64` distinction (see [`Number`]) plus an arbitrary-precision
//!   arm.
//! * [`Deserialize`] streams straight into this type, with no [`serde_json::Value`] tree.
//!
//! See `docs/design/value-model.md` for stack safety and subclass identity.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

use crate::path::{PathSegment, entry_path_segment};

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde::de::{Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::datetime::{Date, DateTime, Time, TimeDelta, times_equal};

/// A Python `str`'s content: UTF-8, or WTF-8 when it holds a lone surrogate.
///
/// Byte order equals code-point order across both variants, so comparison is plain byte
/// comparison. Only the Python bindings produce [`Str::Wtf8`].
#[derive(Debug, Clone)]
pub enum Str {
    /// Valid UTF-8.
    Utf8(Box<str>),
    /// WTF-8 bytes holding at least one lone surrogate.
    Wtf8(Box<[u8]>),
}

impl Str {
    /// This string's content as WTF-8 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Str::Utf8(s) => s.as_bytes(),
            Str::Wtf8(b) => b,
        }
    }

    /// This string as a `&str`, or `None` for a [`Str::Wtf8`].
    #[must_use]
    pub fn as_utf8(&self) -> Option<&str> {
        match self {
            Str::Utf8(s) => Some(s),
            Str::Wtf8(_) => None,
        }
    }

    /// Whether this string has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    /// Walks this string one code point at a time.
    #[must_use]
    pub fn chars(&self) -> Wtf8Chars<'_> {
        Wtf8Chars::new(self.as_bytes())
    }
}

/// One code point read back out of [`Str`]/[`Key`] content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wtf8Char {
    /// An ordinary, UTF-8-encodable code point.
    Scalar(char),
    /// A lone surrogate code point (always in `0xD800..=0xDFFF`).
    Surrogate(u16),
}

/// [`Str::chars`]'s iterator over WTF-8 code points.
pub struct Wtf8Chars<'a> {
    remaining: &'a [u8],
}

impl<'a> Wtf8Chars<'a> {
    /// Decodes `bytes`, which must be valid WTF-8.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Wtf8Chars { remaining: bytes }
    }
}

/// Byte width of the sequence starting with `first_byte`, which sits at a character boundary.
fn utf8_sequence_width(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte & 0xE0 == 0xC0 {
        2
    } else if first_byte & 0xF0 == 0xE0 {
        3
    } else {
        4
    }
}

/// Test-only: bytes `Wtf8Chars::next` validates, counted per thread.
#[cfg(test)]
pub(crate) mod wtf8_decode_stats {
    use std::cell::Cell;

    thread_local! {
        static STATS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    }

    /// Adds `bytes` to the total and the running max.
    pub(crate) fn record(bytes: usize) {
        STATS.with(|c| {
            let (total, max) = c.get();
            c.set((total + bytes, max.max(bytes)));
        });
    }

    /// Returns `(total bytes, largest single call)` and resets both.
    pub(crate) fn take() -> (usize, usize) {
        STATS.with(|c| c.replace((0, 0)))
    }
}

impl Iterator for Wtf8Chars<'_> {
    type Item = Wtf8Char;

    fn next(&mut self) -> Option<Wtf8Char> {
        let &first_byte = self.remaining.first()?;
        let width = utf8_sequence_width(first_byte);
        let candidate = &self.remaining[..width];
        #[cfg(test)]
        wtf8_decode_stats::record(candidate.len());

        let (first, rest) = if let Ok(valid) = std::str::from_utf8(candidate) {
            let c = valid
                .chars()
                .next()
                .expect("a `width`-byte lead sequence always decodes to exactly one char");
            (Wtf8Char::Scalar(c), &self.remaining[c.len_utf8()..])
        } else {
            // Only the three-byte surrogate encoding (lead byte `0xED`) fails validation here.
            let code_point = (u32::from(candidate[0] & 0x0F) << 12)
                | (u32::from(candidate[1] & 0x3F) << 6)
                | u32::from(candidate[2] & 0x3F);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a surrogate code point (0xD800..=0xDFFF by construction) always fits \
                          in u16"
            )]
            (Wtf8Char::Surrogate(code_point as u16), &self.remaining[3..])
        };

        self.remaining = rest;
        Some(first)
    }
}

/// Byte-based `PartialEq`/`Eq`/`Ord`/`PartialOrd`/`Hash` for [`Str`] and [`Key`].
macro_rules! impl_wtf8_bytes_ord {
    ($ty:ty) => {
        impl PartialEq for $ty {
            fn eq(&self, other: &Self) -> bool {
                self.as_bytes() == other.as_bytes()
            }
        }

        impl Eq for $ty {}

        impl Ord for $ty {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.as_bytes().cmp(other.as_bytes())
            }
        }

        impl PartialOrd for $ty {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Hash for $ty {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.as_bytes().hash(state);
            }
        }
    };
}

impl_wtf8_bytes_ord!(Str);

impl From<&str> for Str {
    fn from(value: &str) -> Self {
        Str::Utf8(Box::from(value))
    }
}

impl From<String> for Str {
    fn from(value: String) -> Self {
        Str::Utf8(value.into_boxed_str())
    }
}

impl From<Box<str>> for Str {
    fn from(value: Box<str>) -> Self {
        Str::Utf8(value)
    }
}

impl fmt::Display for Str {
    /// Lossy for [`Str::Wtf8`] (each surrogate becomes `U+FFFD`); not for byte-exact output.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Str::Utf8(s) => f.write_str(s),
            Str::Wtf8(b) => f.write_str(&String::from_utf8_lossy(b)),
        }
    }
}

/// A compact JSON value: the memory-frugal counterpart of [`serde_json::Value`].
///
/// Tuple, set, frozenset and the four calendar variants are types JSON cannot express; only a
/// caller holding Python objects produces them. The size is at most 40 bytes (pinned by
/// `value_is_compact`).
#[derive(Debug, Clone)]
pub enum Value {
    /// JSON `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number, keeping the int/float distinction.
    Number(Number),
    /// A string — see [`Str`].
    Str(Str),
    /// A Python `datetime.datetime` — see [`DateTime`].
    DateTime(Typed<DateTime>),
    /// A Python `datetime.date` — see [`Date`].
    Date(Typed<Date>),
    /// A Python `datetime.time` — see [`Time`].
    Time(Typed<Time>),
    /// A Python `datetime.timedelta` — see [`TimeDelta`].
    TimeDelta(Typed<TimeDelta>),
    /// An array of exactly-sized items.
    Array(Typed<Box<[Value]>>),
    /// A Python tuple, a distinct type from [`Value::Array`] (a `type_changes` finding).
    Tuple(Typed<Box<[Value]>>),
    /// A Python `set`, held in canonical order — see [`SetItems`].
    Set(SetItems),
    /// A Python `frozenset`, a distinct type from [`Value::Set`].
    FrozenSet(SetItems),
    /// An object with key-sorted entries — see [`Object`].
    Object(Object),
}

/// One [`Object`] key: a `str` or any other key `DeepDiff` accepts. No `Hash` impl;
/// `crate::ignore_order::hash`'s `object_key_item_key` hashes a key's content instead.
#[derive(Debug, Clone)]
pub enum ObjectKey {
    /// A `str` key — see [`Key`].
    Str(Key),
    /// `None`, `bool`, `int`, `float`, `datetime`, `date`, or a `tuple` of those. `onix-py`'s
    /// conversion enforces the set; this type does not.
    Other(Box<Value>),
}

impl ObjectKey {
    /// This key's `str` content, or `None` for [`ObjectKey::Other`] or a [`Key::Wtf8`].
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ObjectKey::Str(s) => s.as_utf8(),
            ObjectKey::Other(_) => None,
        }
    }
}

/// Structural equality, consistent with [`Ord`]; a `Str` never equals an `Other`.
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

/// Every `Str` key sorts before every `Other` key; `Str`s by content, `Other`s by
/// `canonical_cmp`. A storage order only: `crate::diff::object` holds the matching rule.
impl Ord for ObjectKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        object_key_cmp(self, other)
    }
}

/// [`ObjectKey`]'s comparison, shared by [`PartialEq`] and [`Ord`] so they cannot drift.
fn object_key_cmp(a: &ObjectKey, b: &ObjectKey) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (a, b) {
        (ObjectKey::Str(x), ObjectKey::Str(y)) => x.cmp(y),
        (ObjectKey::Str(_), ObjectKey::Other(_)) => Ordering::Less,
        (ObjectKey::Other(_), ObjectKey::Str(_)) => Ordering::Greater,
        (ObjectKey::Other(x), ObjectKey::Other(y)) => canonical_cmp(x, y),
    }
}

/// Wraps a value with its source Python class name, `None` for the exact base type.
/// [`PartialEq`] compares only the wrapped value; `docs/design/value-model.md`'s "Subclasses"
/// section says where the name is checked instead.
#[derive(Debug, Clone)]
pub struct Typed<T> {
    inner: T,
    class_name: Option<Arc<str>>,
}

impl<T> Typed<T> {
    /// Wraps `inner` with no subclass name.
    #[must_use]
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            class_name: None,
        }
    }

    /// Wraps `inner` with an explicit subclass name.
    #[must_use]
    pub fn with_class_name(inner: T, class_name: Option<Arc<str>>) -> Self {
        Self { inner, class_name }
    }

    /// The subclass name, or `None` for the exact base type.
    #[must_use]
    pub fn class_name(&self) -> Option<&str> {
        self.class_name.as_deref()
    }

    /// Unwraps, discarding the class name.
    pub(crate) fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: Copy> Typed<T> {
    /// A copy of the wrapped value, discarding the class name.
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

/// Lets `Box::new([a, b, c])` build a `Typed<Box<[Value]>>`.
impl<const N: usize> From<Box<[Value; N]>> for Typed<Box<[Value]>> {
    fn from(items: Box<[Value; N]>) -> Self {
        let items: Box<[Value]> = items;
        Self::new(items)
    }
}

/// Ignores `class_name`; see [`Typed`].
impl<T: PartialEq> PartialEq for Typed<T> {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

/// The subclass name `value` carries, or `None` (always `None` for `Null`, `Bool`, `Number`,
/// `Str`).
#[must_use]
pub(crate) fn class_name(value: &Value) -> Option<&str> {
    match value {
        Value::DateTime(t) => t.class_name(),
        Value::Date(t) => t.class_name(),
        Value::Time(t) => t.class_name(),
        Value::TimeDelta(t) => t.class_name(),
        Value::Array(t) | Value::Tuple(t) => t.class_name(),
        Value::Set(items) | Value::FrozenSet(items) => items.type_name(),
        Value::Object(map) => map.type_name(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Str(_) => None,
    }
}

/// Whether `a` and `b` are the same Python class: [`Object`]s by class identity and kind
/// ([`Object::same_class`]), every other variant by subclass `__name__` alone.
#[must_use]
pub(crate) fn same_class(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x.same_class(y),
        _ => class_name(a) == class_name(b),
    }
}

/// Iterative structural equality; see `docs/design/value-model.md`'s "Stack safety" section.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        structural_eq(self, other)
    }
}

impl Value {
    /// Renders back into an equivalent [`serde_json::Value`]: sorted object keys, exact number
    /// kinds. Tuples and sets render as arrays; a lone surrogate renders lossily and a
    /// non-finite float as `null`.
    #[must_use]
    pub fn to_serde_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Number(n) => n
                .to_serde_number()
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Value::Str(s) => serde_json::Value::String(s.to_string()),
            Value::DateTime(value) => serde_json::Value::String(value.isoformat()),
            Value::Date(value) => serde_json::Value::String(value.isoformat()),
            Value::Time(value) => serde_json::Value::String(value.isoformat()),
            Value::TimeDelta(value) => serde_json::Value::String(value.python_str()),
            Value::Array(items) | Value::Tuple(items) => {
                serde_json::Value::Array(items.iter().map(Value::to_serde_json).collect())
            }
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

/// Renders an [`ObjectKey`] as the JSON string key [`Value::to_serde_json`] and `onix-py`'s
/// writer embed. `Other` keys follow `json.dumps` (a non-finite float as `NaN`/`Infinity`);
/// `datetime`, `date` and `tuple` keys render as their path text, see `tests/golden/README.md`'s
/// "Nested non-`str` dict key in `to_json()`" section.
// Each `expect` below proves an invariant `Number` guarantees.
#[allow(clippy::missing_panics_doc)]
#[must_use]
pub fn object_key_json_string(key: &ObjectKey) -> String {
    match key {
        ObjectKey::Str(s) => s.to_lossy_string(),
        ObjectKey::Other(value) => match value.as_ref() {
            Value::Null => "null".to_string(),
            Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            Value::Number(n) if n.is_f64() => {
                let f = n
                    .as_f64()
                    .expect("Number::is_f64 guarantees as_f64 succeeds");
                if f.is_finite() {
                    crate::path::python_float_repr(f)
                } else if f.is_nan() {
                    "NaN".to_string()
                } else if f.is_sign_positive() {
                    "Infinity".to_string()
                } else {
                    "-Infinity".to_string()
                }
            }
            Value::Number(n) => n
                .as_i64()
                .map(|i| i.to_string())
                .or_else(|| n.as_u64().map(|u| u.to_string()))
                .or_else(|| n.as_big().map(ToString::to_string))
                .expect("a non-float Number is an i64, a u64, or an arbitrary-precision integer"),
            other => crate::path::python_repr(other),
        },
    }
}

/// Whether `value`, or any [`Object`] key inside it, holds a [`Str::Wtf8`] or [`Key::Wtf8`].
/// Iterative; see `docs/design/value-model.md`'s "Stack safety" section.
#[must_use]
pub fn contains_wtf8(value: &Value) -> bool {
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            Value::Str(Str::Wtf8(_)) => return true,
            Value::Str(Str::Utf8(_))
            | Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::DateTime(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::TimeDelta(_) => {}
            Value::Array(items) | Value::Tuple(items) => stack.extend(items.iter()),
            Value::Set(items) | Value::FrozenSet(items) => stack.extend(items.iter()),
            Value::Object(obj) => {
                for (key, value) in obj {
                    match key {
                        ObjectKey::Str(Key::Wtf8(_)) => return true,
                        ObjectKey::Str(Key::Utf8(_)) => {}
                        ObjectKey::Other(other) => stack.push(other),
                    }
                    stack.push(value);
                }
            }
        }
    }
    false
}

/// Writes the escaped content of a JSON string (no quotes) for WTF-8 `bytes`, with a lone
/// surrogate as a single-backslash `\uXXXX` escape. Public for `onix-py`'s `guard` module.
pub fn write_json_str_content(bytes: &[u8], out: &mut String) {
    let mut run = String::new();
    for c in Wtf8Chars::new(bytes) {
        match c {
            Wtf8Char::Scalar(c) => run.push(c),
            Wtf8Char::Surrogate(code_point) => {
                if !run.is_empty() {
                    push_escaped_run(&run, out);
                    run.clear();
                }
                let _ = write!(out, "\\u{code_point:04x}");
            }
        }
    }
    if !run.is_empty() {
        push_escaped_run(&run, out);
    }
}

/// Escapes `run` through `serde_json`, stripping the quotes it adds.
fn push_escaped_run(run: &str, out: &mut String) {
    let quoted =
        serde_json::to_string(run).expect("a &str always serializes to a JSON string literal");
    out.push_str(&quoted[1..quoted.len() - 1]);
}

impl From<serde_json::Value> for Value {
    /// Converts into a compact [`Value`], interning object keys across the whole tree.
    fn from(value: serde_json::Value) -> Self {
        let mut interner = Interner::new();
        from_serde(value, &mut interner)
    }
}

/// Converts one node, threading a single [`Interner`] across the tree.
fn from_serde(value: serde_json::Value, interner: &mut Interner) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => Value::Number(Number::from_serde(&n)),
        serde_json::Value::String(s) => Value::Str(Str::Utf8(s.into_boxed_str())),
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
                .map(|(key, value)| {
                    (
                        ObjectKey::Str(Key::Utf8(interner.intern(&key))),
                        from_serde(value, interner),
                    )
                })
                .collect();
            Value::Object(Object::from_pairs(pairs))
        }
    }
}

/// Iterative destructor: `O(1)` native stack, see `docs/design/value-model.md`'s "Stack safety"
/// section.
impl Drop for Value {
    fn drop(&mut self) {
        let mut stack: Vec<Value> = Vec::new();
        take_children(self, &mut stack);
        while let Some(mut node) = stack.pop() {
            take_children(&mut node, &mut stack);
            // Its children were taken, so this drop does not recurse.
        }
    }
}

/// Moves `value`'s direct children onto `stack`, leaving empty containers.
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
        | Value::Date(_)
        | Value::Time(_)
        | Value::TimeDelta(_) => {}
    }
}

/// Iterative structural equality backing `Value`'s [`PartialEq`]: same-variant only (an array
/// never equals a tuple), `Number` variant-sensitive (`1` is not `1.0`), sets in stored order.
fn structural_eq(a: &Value, b: &Value) -> bool {
    let mut stack: Vec<(&Value, &Value)> = vec![(a, b)];
    while let Some((a, b)) = stack.pop() {
        // A subclass-vs-base pair is a `type_changes` finding even when every field matches.
        if !same_class(a, b) {
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
                // By instant, a naive value read as UTC.
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
                // A naive time never equals an aware one.
                if !times_equal(x.value(), y.value()) {
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
                if (x.kind() == ObjectKind::Failed || y.kind() == ObjectKind::Failed)
                    && !x.same_instance(y)
                {
                    return false;
                }
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

/// A number preserving [`serde_json`]'s `u64`/`i64`/`f64` split, plus an arbitrary-precision arm
/// for a Python `int` beyond them, so `1` and `1.0` render differently. A float is non-finite
/// only when built through [`Number::from_f64`].
#[derive(Debug, Clone, PartialEq)]
pub struct Number {
    repr: NumberRepr,
}

/// Mirrors [`serde_json`]'s internal `N`, plus a boxed `Big` that keeps the enum pointer-sized.
#[derive(Debug, Clone, PartialEq)]
enum NumberRepr {
    /// A non-negative integer.
    PosInt(u64),
    /// A negative integer.
    NegInt(i64),
    /// A float.
    Float(f64),
    /// An integer outside `i64::MIN..=u64::MAX`.
    Big(Box<BigInt>),
}

impl Number {
    /// Builds a number from a `u64` (stored as a non-negative integer).
    #[must_use]
    pub fn from_u64(value: u64) -> Self {
        Self {
            repr: NumberRepr::PosInt(value),
        }
    }

    /// Builds a number from an `i64`; a non-negative value is stored as `PosInt`, as
    /// [`serde_json`] does.
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        match u64::try_from(value) {
            Ok(non_negative) => Self::from_u64(non_negative),
            Err(_) => Self {
                repr: NumberRepr::NegInt(value),
            },
        }
    }

    /// Builds a number from an `f64`, finite or not, storing the bits as given.
    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        Self {
            repr: NumberRepr::Float(value),
        }
    }

    /// Builds a number from an arbitrary-precision integer, narrowing to the `u64`/`i64` arms
    /// when it fits.
    #[must_use]
    pub fn from_bigint(value: BigInt) -> Self {
        if let Some(u) = value.to_u64() {
            return Self::from_u64(u);
        }
        if let Some(i) = value.to_i64() {
            return Self::from_i64(i);
        }
        Self {
            repr: NumberRepr::Big(Box::new(value)),
        }
    }

    /// Whether this number is a float.
    #[must_use]
    pub fn is_f64(&self) -> bool {
        matches!(self.repr, NumberRepr::Float(_))
    }

    /// This number as an `i64` if it fits, else `None`.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match &self.repr {
            NumberRepr::PosInt(u) => i64::try_from(*u).ok(),
            NumberRepr::NegInt(i) => Some(*i),
            NumberRepr::Float(_) | NumberRepr::Big(_) => None,
        }
    }

    /// This number as a `u64` if it is a non-negative integer that fits, else `None`.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match &self.repr {
            NumberRepr::PosInt(u) => Some(*u),
            NumberRepr::NegInt(_) | NumberRepr::Float(_) | NumberRepr::Big(_) => None,
        }
    }

    /// This number as an `f64` (always `Some`); a large integer may lose precision.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "mirrors serde_json::Number::as_f64, which likewise converts \
                  large integers to the nearest f64"
    )]
    pub fn as_f64(&self) -> Option<f64> {
        Some(match &self.repr {
            NumberRepr::PosInt(u) => *u as f64,
            NumberRepr::NegInt(i) => *i as f64,
            NumberRepr::Float(f) => *f,
            NumberRepr::Big(b) => b.to_f64().unwrap_or(f64::INFINITY),
        })
    }

    /// This integer as an `i128`, or `None` for a float or a `Big` beyond `i128`.
    #[must_use]
    pub(crate) fn as_i128(&self) -> Option<i128> {
        match &self.repr {
            NumberRepr::PosInt(u) => Some(i128::from(*u)),
            NumberRepr::NegInt(i) => Some(i128::from(*i)),
            NumberRepr::Big(b) => b.to_i128(),
            NumberRepr::Float(_) => None,
        }
    }

    /// The arbitrary-precision payload, or `None` for a fast-arm integer or a float.
    #[must_use]
    pub fn as_big(&self) -> Option<&BigInt> {
        match &self.repr {
            NumberRepr::Big(b) => Some(b),
            NumberRepr::PosInt(_) | NumberRepr::NegInt(_) | NumberRepr::Float(_) => None,
        }
    }

    /// This integer's exact value as a [`BigInt`]; [`Number::integer_cmp`]'s slow path.
    fn to_bigint(&self) -> BigInt {
        self.as_big().cloned().unwrap_or_else(|| {
            BigInt::from(
                self.as_i128()
                    .expect("a non-Big integer fits i128; integer_cmp never passes a float"),
            )
        })
    }

    /// Orders two integers by value across every representation; callers pass no float.
    #[must_use]
    pub(crate) fn integer_cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.as_i128(), other.as_i128()) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => self.to_bigint().cmp(&other.to_bigint()),
        }
    }

    /// Classifies a [`serde_json::Number`], preserving which kind it chose.
    fn from_serde(number: &serde_json::Number) -> Self {
        if let Some(u) = number.as_u64() {
            Self::from_u64(u)
        } else if let Some(i) = number.as_i64() {
            Self::from_i64(i)
        } else {
            let f = number
                .as_f64()
                .expect("a serde_json Number that is neither u64 nor i64 is a finite f64");
            Self {
                repr: NumberRepr::Float(f),
            }
        }
    }

    /// The exact [`serde_json::Number`], or `None` for a non-finite float. A `Big` becomes its
    /// nearest `f64`; its byte-exact digits go through `onix-py`'s JSON writer.
    fn to_serde_number(&self) -> Option<serde_json::Number> {
        match &self.repr {
            NumberRepr::PosInt(u) => Some(serde_json::Number::from(*u)),
            NumberRepr::NegInt(i) => Some(serde_json::Number::from(*i)),
            NumberRepr::Float(f) => serde_json::Number::from_f64(*f),
            NumberRepr::Big(_) => self.as_f64().and_then(serde_json::Number::from_f64),
        }
    }
}

/// A `set`'s or `frozenset`'s members: duplicate-free, in canonical set order, so no rendering
/// depends on Python's hash order (`tests/golden/README.md`, "Set iteration order").
///
/// The order is `None`, `bool`, `int`, `float`, `str`, `tuple`, `frozenset`, `list`, `set`,
/// `dict`, `datetime`, `date`, `time`, `timedelta`, each kind by value.
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
    /// Ascending in [`canonical_cmp`] order, no two structurally equal.
    items: Box<[Value]>,
    /// The subclass name, or `None` for the exact base type.
    type_name: Option<Arc<str>>,
}

impl SetItems {
    /// Sorts into canonical order and drops structurally equal members; `-0.0` and `0.0` fold
    /// and bit-identical `NaN`s collapse (`tests/golden/README.md`, "Non-finite floats").
    /// Costs one `O(n log n)` sort.
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

    /// Attaches a `set`/`frozenset` subclass name (`None` for the exact base type).
    #[must_use]
    pub fn with_type_name(mut self, type_name: Option<Arc<str>>) -> Self {
        self.type_name = type_name;
        self
    }

    /// The subclass name, or `None` for the exact base type.
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

/// The canonical set order (see [`SetItems`]) as a structural comparison. Iterative: a set is
/// built during conversion, before any depth guard runs (`docs/design/value-model.md`, "Stack
/// safety"). Pending comparisons pop in lexicographic order; the first non-`Equal` wins.
fn canonical_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    /// The kind's place in the order.
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

    /// One comparison still owed: two values, two keys, or a container's length tie-break.
    enum Work<'a> {
        Values(&'a Value, &'a Value),
        Keys(&'a ObjectKey, &'a ObjectKey),
        Lengths(usize, usize),
    }

    /// Schedules the elements in order, with the length tie-break last.
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
                    // By instant, then offset, so same-instant datetimes order deterministically.
                    (Value::DateTime(x), Value::DateTime(y)) => x
                        .instant()
                        .cmp(&y.instant())
                        .then_with(|| x.utc_offset_seconds().cmp(&y.utc_offset_seconds())),
                    (Value::Date(x), Value::Date(y)) => x.ordinal().cmp(&y.ordinal()),
                    // Naive before aware, then `times_equal`'s instant, then the raw offset
                    // (Python-equal aware times can differ in offset).
                    (Value::Time(x), Value::Time(y)) => x
                        .utc_offset_seconds()
                        .is_some()
                        .cmp(&y.utc_offset_seconds().is_some())
                        .then_with(|| x.sort_instant().cmp(&y.sort_instant()))
                        .then_with(|| x.utc_offset_seconds().cmp(&y.utc_offset_seconds())),
                    (Value::TimeDelta(x), Value::TimeDelta(y)) => x.value().cmp(&y.value()),
                    (Value::Array(x), Value::Array(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
                        push_slices(&mut stack, x, y);
                        Ordering::Equal
                    }
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
                    // Only `Null` against `Null`.
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

/// Maps `-0.0` to `0.0`; every other float, any `NaN` included, is returned bit-identical
/// (adding `0.0` would quiet a signaling `NaN`, so `NaN` skips it).
pub(crate) fn fold_signed_zero(f: f64) -> f64 {
    if f.is_nan() { f } else { f + 0.0 }
}

/// [`canonical_cmp`]'s number case: floats by [`fold_signed_zero`] and [`f64::total_cmp`],
/// integers by value.
fn number_cmp(a: &Number, b: &Number) -> std::cmp::Ordering {
    if a.is_f64() {
        let af = fold_signed_zero(a.as_f64().unwrap_or_default());
        let bf = fold_signed_zero(b.as_f64().unwrap_or_default());
        return af.total_cmp(&bf);
    }

    a.integer_cmp(b)
}

/// An [`Object`]'s key: an interned `Arc<str>`, or WTF-8 bytes for a key with a lone surrogate.
///
/// Read content through [`Key::as_bytes`] or the variant, never a lossy conversion: keys that
/// differ only in which surrogate they hold must stay distinct.
#[derive(Debug, Clone)]
pub enum Key {
    /// An interned, valid-UTF-8 key.
    Utf8(Arc<str>),
    /// A key containing a lone surrogate code point, as WTF-8 bytes.
    Wtf8(Box<[u8]>),
}

impl Key {
    /// This key's content as WTF-8 bytes — see [`Str::as_bytes`].
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Key::Utf8(s) => s.as_bytes(),
            Key::Wtf8(b) => b,
        }
    }

    /// Walks this key one code point at a time; see [`Str::chars`].
    #[must_use]
    pub fn chars(&self) -> Wtf8Chars<'_> {
        Wtf8Chars::new(self.as_bytes())
    }

    /// This key as a real `&str`, or `None` for a [`Key::Wtf8`] — see
    /// [`Str::as_utf8`].
    #[must_use]
    pub fn as_utf8(&self) -> Option<&str> {
        match self {
            Key::Utf8(s) => Some(s),
            Key::Wtf8(_) => None,
        }
    }
}

impl_wtf8_bytes_ord!(Key);

impl From<&Key> for Str {
    fn from(key: &Key) -> Self {
        match key {
            Key::Utf8(s) => Str::Utf8(Box::from(s.as_ref())),
            Key::Wtf8(b) => Str::Wtf8(b.clone()),
        }
    }
}

impl Key {
    /// Lossy rendering (each surrogate becomes `U+FFFD`); for [`Value::to_serde_json`] only.
    fn to_lossy_string(&self) -> String {
        match self {
            Key::Utf8(s) => s.to_string(),
            Key::Wtf8(b) => String::from_utf8_lossy(b).into_owned(),
        }
    }
}

/// What an [`Object`]'s entries represent. A `dict` entry renders as a subscript
/// (`root['key']`), a custom object's attribute as a dotted access (`root.attr`), and the two
/// never hash-match or pair under `ignore_order`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    /// A Python `dict` (or a `dict` subclass), rendered with subscript paths.
    Dict,
    /// A custom object diffed by its attributes, rendered with dotted paths.
    CustomObject,
    /// A value onix cannot diff, held by the identity of its Python object: it
    /// has no entries and equals only a token for the same object.
    Opaque,
    /// An object already on the conversion path above this position; the diff reports nothing
    /// where it is on the first side, as `DeepDiff`'s `parents_ids` does.
    Cycle,
    /// A custom object whose attributes could not be read, holding its instance `__dict__`
    /// entries; equal only to the same object.
    Failed,
}

impl ObjectKind {
    /// The kind walked in its place: a failed object walks as a custom object.
    fn walked(self) -> ObjectKind {
        match self {
            ObjectKind::Failed => ObjectKind::CustomObject,
            kind => kind,
        }
    }
}

/// A JSON object: key-sorted, exactly-sized entries with binary-search lookup and
/// ascending-key iteration. Non-`str` keys are an additive case ([`ObjectKey::Other`]) so the
/// `str` path keeps its allocation-free lookup. A custom object's attributes reuse the storage,
/// told apart by [`Object::kind`].
#[derive(Debug, Clone)]
pub struct Object {
    /// Strictly ascending by [`ObjectKey`]'s `Ord` ([`Object::from_pairs`] enforces it), so
    /// every `Str` entry precedes every `Other` one.
    entries: Box<[(ObjectKey, Value)]>,
    /// `None` for a plain `dict`; boxed so [`Value`] stays within its size cap
    /// (`value_is_compact`).
    class: Option<Box<ObjectClass>>,
}

/// A non-plain-`dict` [`Object`]'s class name, identity and kind.
#[derive(Debug, Clone)]
struct ObjectClass {
    /// The class `__name__`, for rendering only: two classes can share one.
    name: Arc<str>,
    /// Decides whether two objects share a class: the type object's address, which the caller
    /// keeps alive for the diff (the value's own address for an [`ObjectKind::Opaque`] token).
    identity: Arc<str>,
    kind: ObjectKind,
    lengths: ObjectLengths,
    /// Sorted names of the entries read from the class, which a whole-object render omits.
    class_attributes: Box<[Arc<str>]>,
    /// The converted object's address, held alive for the diff; equal addresses are the
    /// identical object, which `DeepDiff` never walks.
    instance: Option<usize>,
}

/// The lengths `ignore_order`'s distance reads off a custom object beyond its attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectLengths {
    /// `len(obj.__dict__)`, `0` without one: `_get_item_length` of the object.
    pub dict_len: usize,
    /// What `DeepHash` counts for the `__dict__` entries that are not
    /// attributes.
    pub hidden_count: usize,
    /// `_get_item_length(type(obj))`: `1` for a class, the sum of its
    /// members' `__dict__` lengths for an `Enum` class, which is iterable.
    pub type_len: usize,
}

impl Default for ObjectLengths {
    fn default() -> Self {
        Self {
            dict_len: 0,
            hidden_count: 0,
            type_len: 1,
        }
    }
}

impl Object {
    /// Builds an object from `(key, value)` pairs sorted by [`ObjectKey`]'s order; a duplicate
    /// key keeps the last value, as [`serde_json`] does.
    pub(crate) fn from_pairs(mut pairs: Vec<(ObjectKey, Value)>) -> Self {
        // Stable, so the loop below keeps the last occurrence of a duplicate.
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
            class: None,
        }
    }

    /// Attaches a `dict` subclass's `(name, identity)`; `None` is the exact base `dict`.
    #[must_use]
    pub fn with_dict_class(mut self, class: Option<(Arc<str>, Arc<str>)>) -> Self {
        self.class = class.map(|(name, identity)| {
            Box::new(ObjectClass {
                name,
                identity,
                kind: ObjectKind::Dict,
                lengths: ObjectLengths::default(),
                class_attributes: Box::default(),
                instance: None,
            })
        });
        self
    }

    /// Marks these entries as `kind`'s, under the given class parts.
    #[must_use]
    pub fn into_class(
        mut self,
        kind: ObjectKind,
        name: Arc<str>,
        identity: Arc<str>,
        lengths: ObjectLengths,
        mut class_attributes: Vec<Arc<str>>,
        instance: Option<usize>,
    ) -> Self {
        class_attributes.sort_unstable();
        self.class = Some(Box::new(ObjectClass {
            name,
            identity,
            kind,
            lengths,
            class_attributes: class_attributes.into_boxed_slice(),
            instance,
        }));
        self
    }

    /// The address of the Python object a custom object was converted from.
    #[must_use]
    pub fn instance(&self) -> Option<usize> {
        self.class.as_ref().and_then(|class| class.instance)
    }

    /// Whether this is an [`ObjectKind::Cycle`] token.
    #[must_use]
    pub fn is_cycle(&self) -> bool {
        self.kind() == ObjectKind::Cycle
    }

    /// Whether `self` and `other` stand for the identical Python object.
    #[must_use]
    pub fn same_instance(&self, other: &Object) -> bool {
        match (self.class.as_ref(), other.class.as_ref()) {
            (Some(a), Some(b)) if a.instance.is_some() => a.instance == b.instance,
            (Some(a), Some(b)) if a.kind == b.kind && a.kind != ObjectKind::CustomObject => {
                a.kind != ObjectKind::Dict && a.identity == b.identity
            }
            _ => false,
        }
    }

    /// Whether `key` names an entry read from the class rather than the instance.
    #[must_use]
    pub fn is_class_attribute(&self, key: &ObjectKey) -> bool {
        self.class.as_ref().is_some_and(|class| {
            key.as_str().is_some_and(|name| {
                class
                    .class_attributes
                    .binary_search_by(|a| a.as_ref().cmp(name))
                    .is_ok()
            })
        })
    }

    /// The class `__name__` for rendering, or `None` for a plain `dict`; identity is
    /// [`Object::same_class`].
    #[must_use]
    pub fn type_name(&self) -> Option<&str> {
        self.class.as_ref().map(|class| class.name.as_ref())
    }

    /// Whether `self` and `other` are the same Python class: equal class identity and kind
    /// (not name), as `DeepDiff` compares `type()` objects. Two plain `dicts` are.
    #[must_use]
    pub fn same_class(&self, other: &Object) -> bool {
        match (self.class.as_ref(), other.class.as_ref()) {
            (None, None) => true,
            (Some(a), Some(b)) => a.kind.walked() == b.kind.walked() && a.identity == b.identity,
            _ => false,
        }
    }

    /// The opaque token a walk reports for an [`ObjectKind::Failed`] object,
    /// `None` for anything else.
    #[must_use]
    pub fn failure_token(&self) -> Option<Value> {
        let class = self
            .class
            .as_ref()
            .filter(|class| class.kind == ObjectKind::Failed)?;
        let identity = format!("{:x}", class.instance?);
        Some(Builder::new().opaque(class.name.clone(), Arc::from(identity)))
    }

    /// Whether these entries are a `dict`'s items or a custom object's attributes.
    #[must_use]
    pub fn kind(&self) -> ObjectKind {
        self.class
            .as_ref()
            .map_or(ObjectKind::Dict, |class| class.kind)
    }

    /// Whether these entries are a custom object's attributes.
    #[must_use]
    pub fn is_custom_object(&self) -> bool {
        matches!(self.kind(), ObjectKind::CustomObject)
    }

    /// The identity of an [`ObjectKind::Opaque`] token, `None` otherwise.
    #[must_use]
    pub fn opaque_identity(&self) -> Option<&str> {
        self.class
            .as_ref()
            .filter(|class| class.kind == ObjectKind::Opaque)
            .map(|class| class.identity.as_ref())
    }

    /// The identity of an opaque or [`ObjectKind::Cycle`] token.
    #[must_use]
    pub fn token_identity(&self) -> Option<&str> {
        self.opaque_identity().or_else(|| {
            self.class
                .as_ref()
                .filter(|class| class.kind == ObjectKind::Cycle)
                .map(|class| class.identity.as_ref())
        })
    }

    /// The custom object's [`ObjectLengths`], the default for anything else.
    #[must_use]
    pub fn lengths(&self) -> ObjectLengths {
        self.class
            .as_ref()
            .map_or_else(ObjectLengths::default, |class| class.lengths)
    }

    /// The value for `key`, `O(log n)`.
    #[must_use]
    pub fn get(&self, key: &ObjectKey) -> Option<&Value> {
        self.entries
            .binary_search_by(|(entry_key, _)| entry_key.cmp(key))
            .ok()
            .map(|index| &self.entries[index].1)
    }

    /// Whether the object contains `key`, `O(log n)`.
    #[must_use]
    pub fn contains_key(&self, key: &ObjectKey) -> bool {
        self.entries
            .binary_search_by(|(entry_key, _)| entry_key.cmp(key))
            .is_ok()
    }

    /// [`Object::get`] for a plain `&str`, with no [`ObjectKey`] allocated.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&Value> {
        self.entries
            .binary_search_by(|(entry_key, _)| match entry_key {
                ObjectKey::Str(s) => s.as_bytes().cmp(key.as_bytes()),
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

    /// Whether the object has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether any key is an [`ObjectKey::Other`], `O(1)`: those sort last.
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

/// Iterator over an [`Object`]'s entries in ascending key order.
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

/// A per-session interner sharing one `Arc<str>` per distinct UTF-8 key; dropped once the
/// [`Value`] is built.
#[derive(Debug, Default)]
struct Interner {
    seen: HashSet<Arc<str>>,
}

impl Interner {
    /// Creates an empty interner.
    fn new() -> Self {
        Self::default()
    }

    /// A shared `Arc<str>` for `key`, allocated on first sight.
    fn intern(&mut self, key: &str) -> Arc<str> {
        if let Some(existing) = self.seen.get(key) {
            return Arc::clone(existing);
        }
        let shared: Arc<str> = Arc::from(key);
        self.seen.insert(Arc::clone(&shared));
        shared
    }

    /// An interned [`Key`] for [`Str::Utf8`]; a [`Str::Wtf8`] key is never interned
    /// (`docs/design/value-conversion.md`, "Key interning").
    fn intern_key(&mut self, key: Str) -> Key {
        match key {
            Str::Utf8(s) => Key::Utf8(self.intern(&s)),
            Str::Wtf8(b) => Key::Wtf8(b),
        }
    }
}

/// Builds [`Value`]s, interning object keys across one construction session so a key repeated
/// across objects shares one `Arc<str>`; route every object through [`Builder::object`].
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

    /// Builds an object [`Value`] from `entries` in sorted key order; a duplicate key keeps the
    /// last value.
    #[must_use]
    pub fn object<K: Into<Str>>(&mut self, entries: Vec<(K, Value)>) -> Value {
        let pairs = entries
            .into_iter()
            .map(|(key, value)| (ObjectKey::Str(self.interner.intern_key(key.into())), value))
            .collect();
        Value::Object(Object::from_pairs(pairs))
    }

    /// Interns `key` against this builder's session.
    #[must_use]
    pub fn intern(&mut self, key: &str) -> Arc<str> {
        self.interner.intern(key)
    }

    /// [`Builder::intern`] for a [`Str`]; a lone-surrogate key passes through un-interned.
    #[must_use]
    pub fn intern_key(&mut self, key: Str) -> Key {
        self.interner.intern_key(key)
    }

    /// [`Builder::object`] for entries that may carry any [`ObjectKey`].
    #[must_use]
    pub fn object_with_keys(&mut self, entries: Vec<(ObjectKey, Value)>) -> Value {
        Value::Object(Object::from_pairs(entries))
    }

    /// [`Builder::object_with_keys`] plus a `dict` subclass's `(name, identity)`; see
    /// `docs/design/value-model.md`, "Subclasses".
    #[must_use]
    pub fn object_with_keys_and_class(
        &mut self,
        entries: Vec<(ObjectKey, Value)>,
        class: Option<(Arc<str>, Arc<str>)>,
    ) -> Value {
        Value::Object(Object::from_pairs(entries).with_dict_class(class))
    }

    /// Builds a custom object [`Value`] from its attribute `entries` and class parts.
    #[must_use]
    pub fn custom_object(
        &mut self,
        entries: Vec<(ObjectKey, Value)>,
        name: Arc<str>,
        identity: Arc<str>,
        lengths: ObjectLengths,
        class_attributes: Vec<Arc<str>>,
        instance: Option<usize>,
    ) -> Value {
        Value::Object(Object::from_pairs(entries).into_class(
            ObjectKind::CustomObject,
            name,
            identity,
            lengths,
            class_attributes,
            instance,
        ))
    }

    /// Builds an [`ObjectKind::Opaque`] token for a `name` value identified by `identity`.
    #[must_use]
    pub fn opaque(&mut self, name: Arc<str>, identity: Arc<str>) -> Value {
        Value::Object(Object::from_pairs(Vec::new()).into_class(
            ObjectKind::Opaque,
            name,
            identity,
            ObjectLengths::default(),
            Vec::new(),
            None,
        ))
    }

    /// Builds an [`ObjectKind::Failed`] object from its instance `__dict__` `entries`.
    #[must_use]
    pub fn failed_object(
        &mut self,
        entries: Vec<(ObjectKey, Value)>,
        name: Arc<str>,
        identity: Arc<str>,
        instance: usize,
    ) -> Value {
        Value::Object(Object::from_pairs(entries).into_class(
            ObjectKind::Failed,
            name,
            identity,
            ObjectLengths::default(),
            Vec::new(),
            Some(instance),
        ))
    }

    /// Builds an [`ObjectKind::Cycle`] token for a `name` object identified by `identity`.
    #[must_use]
    pub fn cycle(&mut self, name: Arc<str>, identity: Arc<str>) -> Value {
        Value::Object(Object::from_pairs(Vec::new()).into_class(
            ObjectKind::Cycle,
            name,
            identity,
            ObjectLengths::default(),
            Vec::new(),
            None,
        ))
    }
}

/// A copy of `value` as a report shows it, with every custom object's class attributes left
/// out at any depth. Recurses natively; a caller runs a deep value on a sized stack.
///
/// # Errors
///
/// Returns every opaque token left in the render, in render order.
pub fn rendered(value: &Value) -> Result<Value, Vec<Unrendered>> {
    let mut unrendered = Vec::new();
    let rendered = rendered_at(value, &mut Vec::new(), &mut unrendered);
    if unrendered.is_empty() {
        Ok(rendered)
    } else {
        Err(unrendered)
    }
}

/// An opaque token a report would have to show.
#[derive(Debug, PartialEq, Eq)]
pub struct Unrendered {
    /// The token's path below the rendered value.
    pub path: Vec<PathSegment>,
    /// The type name of the value the token stands for.
    pub type_name: String,
    /// The token's identity.
    pub identity: String,
}

fn rendered_at(
    value: &Value,
    path: &mut Vec<PathSegment>,
    unrendered: &mut Vec<Unrendered>,
) -> Value {
    let (items, tuple) = match value {
        Value::Object(map) => {
            if let Some(identity) = map.opaque_identity() {
                unrendered.push(Unrendered {
                    path: path.clone(),
                    type_name: map.type_name().unwrap_or_default().to_string(),
                    identity: identity.to_string(),
                });
                return value.clone();
            }
            let mut kept = Vec::with_capacity(map.entries.len());
            for (key, child) in map {
                if map.is_class_attribute(key) {
                    continue;
                }
                path.push(entry_path_segment(map.kind(), key));
                kept.push((key.clone(), rendered_at(child, path, unrendered)));
                path.pop();
            }
            return Value::Object(Object {
                entries: kept.into_boxed_slice(),
                class: map.class.clone(),
            });
        }
        Value::Array(items) => (items, false),
        Value::Tuple(items) => (items, true),
        other => return other.clone(),
    };
    let mut kept = Vec::with_capacity(items.inner.len());
    for (index, child) in items.inner.iter().enumerate() {
        path.push(PathSegment::Index(index));
        kept.push(rendered_at(child, path, unrendered));
        path.pop();
    }
    let items = Typed::with_class_name(kept.into_boxed_slice(), items.class_name.clone());
    if tuple {
        Value::Tuple(items)
    } else {
        Value::Array(items)
    }
}

impl<'de> Deserialize<'de> for Value {
    /// Streams a [`Value`] from any [`Deserializer`] with no [`serde_json::Value`] tree,
    /// interning keys across the parse.
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

/// A [`DeserializeSeed`] carrying the session [`Interner`] through nested containers.
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

/// The [`Visitor`] mapping each input token onto a [`Value`], as [`serde_json::Value`]'s
/// does (non-finite floats become `Null`).
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
        Ok(Value::Null)
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::Str(Str::Utf8(Box::from(value))))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::Str(Str::Utf8(value.into_boxed_str())))
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
        let mut pairs: Vec<(ObjectKey, Value)> = Vec::new();
        while let Some(key) = map.next_key::<Cow<'_, str>>()? {
            let interned = ObjectKey::Str(Key::Utf8(self.interner.intern(&key)));
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
