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
//!   [`i64::MAX`] must survive as an integer. A Python `int` beyond that
//!   range keeps its exact value in a fourth, arbitrary-precision arm.
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
//! Every [`Value`] variant that can carry a subclass name wraps its payload
//! in [`Typed`], except [`SetItems`]/[`Object`], which carry an equivalent
//! `type_name` field instead — see `same_class`'s doc (`crate::diff::dispatch`)
//! for the exact list, so this one stays in sync with it. Either way, a
//! Python subclass instance keeps the source class name it needs to report
//! a `type_changes` finding, while comparing, hashing, and rendering
//! exactly like its base type everywhere else — every matching identity in
//! the crate (`SetItems` dedup, `crate::lcs`'s scalar-list matching,
//! `crate::ignore_order`'s hashing) is unaffected, since none of them read
//! the class name. `diff_at` (`crate::diff`) is the one place that does,
//! checking it before recursing into any of those variants.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use serde::de::{Deserialize, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::datetime::{Date, DateTime, Time, TimeDelta, times_equal};

/// A Python `str`'s content: valid UTF-8 for the overwhelming common case
/// (identical cost to the plain `Box<str>` this replaces), or WTF-8 bytes
/// when the string holds at least one lone (unpaired) surrogate code point
/// (e.g. `"\udc80"`) — legal in Python, but the one code point UTF-8 cannot
/// encode. Rust's `str`/`String` can never hold one either way (their
/// whole-type invariant is UTF-8 validity, which structurally excludes a
/// surrogate code point), so the rare case needs its own representation.
///
/// [WTF-8](https://simonsapin.github.io/wtf-8/) extends UTF-8 by
/// direct-encoding each surrogate code point in the three-byte form
/// strict UTF-8 forbids for that range; every other code point encodes
/// exactly as UTF-8 already does. This keeps the property this crate
/// depends on throughout — that byte-lexicographic order equals code-point
/// order — for both variants and across them, so every comparison below is
/// plain byte comparison, and a [`Str::Utf8`] and a [`Str::Wtf8`] compare
/// correctly against each other with no conversion.
///
/// Only [`Str::Utf8`] can be produced from JSON: [`serde_json`]'s own
/// parser rejects a lone surrogate escape outright, so the streaming
/// [`Deserialize`] impl below and [`From`]`<`[`serde_json::Value`]`>` never
/// construct a [`Str::Wtf8`]. It exists only for the Python bindings, which
/// can read one directly from a live `str` object.
#[derive(Debug, Clone)]
pub enum Str {
    /// The common case: valid UTF-8, stored exactly as before.
    Utf8(Box<str>),
    /// WTF-8 bytes; holds at least one lone surrogate code point (a
    /// constructor that hands this variant bytes with no surrogate in them
    /// at all should have used [`Str::Utf8`] instead — a documented
    /// invariant, not one anything here enforces or depends on for safety).
    Wtf8(Box<[u8]>),
}

impl Str {
    /// This string's content as WTF-8 bytes — valid UTF-8 bytes for
    /// [`Str::Utf8`], since valid UTF-8 is already valid WTF-8.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Str::Utf8(s) => s.as_bytes(),
            Str::Wtf8(b) => b,
        }
    }

    /// This string as a real `&str`, or `None` for a [`Str::Wtf8`] (which by
    /// construction is never valid UTF-8).
    #[must_use]
    pub fn as_utf8(&self) -> Option<&str> {
        match self {
            Str::Utf8(s) => Some(s),
            Str::Wtf8(_) => None,
        }
    }

    /// Whether this string has no content, by byte length — a
    /// [`Str::Wtf8`] is never empty (it holds at least one surrogate's three
    /// bytes), so this only ever answers `true` for [`Str::Utf8`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    /// Walks this string's content one code point at a time, each either a
    /// real Unicode scalar value or a lone surrogate — the primitive every
    /// surrogate-aware renderer (`crate::path`'s dict-key and `repr()`
    /// rendering, the Python bindings' byte-exact JSON writer) builds on, so
    /// the WTF-8 decoding rule is written exactly once.
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

/// [`Str::chars`]'s iterator: decodes WTF-8 bytes one code point at a time.
///
/// Reads only the *next* sequence, never re-validating bytes already
/// consumed or bytes still ahead: the leading byte's high bits alone give
/// the UTF-8 sequence width (1-4, see this module's private
/// `utf8_sequence_width`), so each
/// call slices at most that many bytes and asks [`str::from_utf8`] to
/// validate only that bounded slice, not `self.remaining` as a whole —
/// validating the whole remaining slice on every call would make every
/// walk over the string quadratic (`O(n)` per call, `O(n²)` total); this
/// way is `O(1)` per call and `O(n)` total. A validation failure on that
/// bounded slice can, by [`Str`]'s own invariant (every byte sequence here
/// is WTF-8), only be the three-byte sequence WTF-8 uses to direct-encode a
/// surrogate — the same three-byte pattern strict UTF-8 would use for a
/// scalar in `0x0800..=0xFFFF`, just with a surrogate's code point in that
/// range instead (both share the `0xED` leading byte, hence the shared
/// width), so it decodes with the identical bit-packing UTF-8 itself uses
/// for a three-byte sequence.
pub struct Wtf8Chars<'a> {
    remaining: &'a [u8],
}

impl<'a> Wtf8Chars<'a> {
    /// Builds a decoder over `bytes`, which must already be valid WTF-8 —
    /// see [`Str`]'s doc. `bytes` need not come from a [`Str`]/[`Key`]
    /// directly; a caller that already has `.as_bytes()` from either can
    /// use this without an intermediate allocation.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Wtf8Chars { remaining: bytes }
    }
}

/// The number of bytes a UTF-8 (or WTF-8 surrogate) sequence starting with
/// `first_byte` occupies, read from its high bits alone — never a look at
/// any later byte, which is what keeps [`Wtf8Chars::next`] to a single
/// bounded slice per call. `first_byte` is always a valid lead byte here
/// (an ASCII byte, or `0xC0..=0xF7`'s continuation-count patterns): every
/// call site holds a byte at a WTF-8 character boundary, by [`Str`]'s own
/// invariant.
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

/// Test-only instrumentation proving [`Wtf8Chars::next`] validates `O(n)`
/// bytes total over a full decode, not `O(n^2)`: a counter, incremented
/// with the size of every slice `next` hands to `str::from_utf8`. A counter
/// rather than a wall-clock measurement, so the check is exact and
/// noise-free instead of tolerating a fudge factor for scheduler jitter —
/// the same reason `crate::ignore_order::memo`'s recomputation count
/// replaced a timing assertion (issue #37). Thread-local, not a shared
/// `static`: the test harness runs each `#[test]` on its own thread by
/// default, and a process-global counter would let an unrelated test's
/// concurrent decode inflate this one's count, reintroducing exactly the
/// kind of nondeterminism a counter is meant to avoid.
#[cfg(test)]
pub(crate) mod wtf8_decode_stats {
    use std::cell::Cell;

    thread_local! {
        static STATS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    }

    /// Adds `bytes` to the running total and folds it into the running max.
    pub(crate) fn record(bytes: usize) {
        STATS.with(|c| {
            let (total, max) = c.get();
            c.set((total + bytes, max.max(bytes)));
        });
    }

    /// Reads `(total bytes, largest single call)` on this thread and resets
    /// both to zero in the same step, so a caller need not remember to
    /// clear residue from an earlier measurement separately.
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
            // Invalid, on a slice that is exactly one WTF-8 character wide:
            // by this type's own invariant, the only way that happens is
            // the three-byte surrogate encoding WTF-8 adds on top of UTF-8
            // (`width` is 3 here — see `utf8_sequence_width`'s doc — since
            // only the `0xED` lead byte both a valid 3-byte scalar and a
            // surrogate share).
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

/// Generates the byte-based `PartialEq`/`Eq`/`Ord`/`PartialOrd`/`Hash`
/// impls shared by [`Str`] and [`Key`]: both compare, order, and hash
/// purely by `as_bytes()`, which is byte equality/order — code-point
/// order for both variants, and correct across a `Utf8`/`Wtf8` mix — see
/// each type's own doc.
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
    /// Only meaningful for [`Str::Utf8`] — a [`Str::Wtf8`] has no valid
    /// textual form, so it renders via Rust's own lossy UTF-8 replacement
    /// (each surrogate becomes `U+FFFD`). Nothing on the byte-exact output
    /// path (`crate::guard`'s JSON writer in the Python bindings) uses this;
    /// it exists for debug/test contexts that need an inspectable string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Str::Utf8(s) => f.write_str(s),
            Str::Wtf8(b) => f.write_str(&String::from_utf8_lossy(b)),
        }
    }
}

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
    /// A string — see [`Str`] for the UTF-8/WTF-8 split.
    Str(Str),
    /// A Python `datetime.datetime` — see [`DateTime`], and this type's own
    /// doc for why it is a variant rather than a pre-rendered string. Wrapped
    /// in [`Typed`] so a `datetime` subclass (e.g. pandas `Timestamp`)
    /// carries its own class name — see this module's "Subclasses" section.
    DateTime(Typed<DateTime>),
    /// A Python `datetime.date` — see [`Date`]. See [`Value::DateTime`]'s
    /// doc for the [`Typed`] wrapper.
    Date(Typed<Date>),
    /// A Python `datetime.time` — see [`Time`]. See [`Value::DateTime`]'s
    /// doc for the [`Typed`] wrapper.
    Time(Typed<Time>),
    /// A Python `datetime.timedelta` — see [`TimeDelta`]. See
    /// [`Value::DateTime`]'s doc for the [`Typed`] wrapper.
    TimeDelta(Typed<TimeDelta>),
    /// An array, stored as an exactly-sized `Box<[Value]>` — wrapped in
    /// [`Typed`] for a `list` subclass, see this module's "Subclasses"
    /// section.
    Array(Typed<Box<[Value]>>),
    /// A Python tuple, stored exactly like [`Value::Array`] but kept as a
    /// distinct variant — see this type's own doc for why. Also carries a
    /// [`Typed`] class name for a `tuple` subclass, including a
    /// `namedtuple` — see this module's "Subclasses" section for
    /// how a `namedtuple` is diffed.
    Tuple(Typed<Box<[Value]>>),
    /// A Python `set`, stored as canonically ordered [`SetItems`], which
    /// carries its own optional class name for a `set` subclass — see this
    /// module's "Subclasses" section.
    Set(SetItems),
    /// A Python `frozenset`, stored exactly like [`Value::Set`] but kept as
    /// a distinct variant — see this type's own doc for why.
    FrozenSet(SetItems),
    /// An object: key-sorted, exactly-sized entries (see [`Object`]), which
    /// carries its own optional class name for a `dict` subclass — see this
    /// module's "Subclasses" section.
    Object(Object),
}

/// One [`Object`] key: the `str` fast path this crate has always had
/// ([`ObjectKey::Str`]), or any other key `DeepDiff` also accepts
/// ([`ObjectKey::Other`]) — see `onix-py`'s conversion table for the exact
/// set. No `Hash` impl; see `crate::ignore_order::hash`'s
/// `object_key_item_key` for how a content hash of one is computed instead.
#[derive(Debug, Clone)]
pub enum ObjectKey {
    /// A `str` key — see [`Key`] for the interned-common-case/lone-surrogate
    /// split.
    Str(Key),
    /// Any other key `DeepDiff` accepts: `None`, `bool`, `int`, `float`,
    /// `datetime`, `date`, or a `tuple` of those — never itself a `str`
    /// (that always takes the [`ObjectKey::Str`] arm) and never a container
    /// other than that restricted `tuple` (`onix-py`'s conversion layer is
    /// the boundary that enforces this; this type does not).
    Other(Box<Value>),
}

impl ObjectKey {
    /// This key's `str` content, or `None` for [`ObjectKey::Other`] *or* a
    /// [`Key::Wtf8`] (a lone surrogate code point, which has no valid `&str`
    /// form — see [`Key::as_utf8`]) — the convenience every call site
    /// expecting a plain `str` key needs, correctly falling through to the
    /// "not a match" case for a surrogate key too.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ObjectKey::Str(s) => s.as_utf8(),
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

/// Wraps a value with the source Python class name, when it differs from
/// the base type this [`Value`] variant represents (`None` for the exact
/// base type). [`PartialEq`] compares only the wrapped value, ignoring the
/// class name — see the [module documentation](self)'s "Subclasses"
/// section, and `diff_at`'s class-name check (`crate::diff`) for where the
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
/// type-change check and [`Value`]'s own structural equality (below) share
/// one definition.
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

/// Whether `a` and `b` are the same Python class, the single identity check
/// `diff_at` (`crate::diff::dispatch`) and [`Value`]'s own structural equality
/// both use so they cannot drift. Two [`Object`]s compare by qualified
/// identity *and* kind (see [`Object::same_class`]) — a `dict` subclass and a
/// custom object sharing a `__name__`, or two same-named classes from
/// different modules, are different classes. Every other variant compares by
/// the rendered [`class_name`], which is all its (base-type-plus-subclass-name)
/// identity has ever needed.
#[must_use]
pub(crate) fn same_class(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x.same_class(y),
        _ => class_name(a) == class_name(b),
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
            // `serde_json::Value::String` cannot hold a `Str::Wtf8` (a
            // lone surrogate has no valid Rust `String` representation);
            // this method's callers can only ever construct one from JSON
            // (see `Str`'s doc), which never carries a surrogate, so this
            // lossy fallback is unreachable in practice — the Python
            // bindings' byte-exact `to_json()` renders directly from
            // `Value` instead of through this method (`crate::guard`).
            Value::Str(s) => serde_json::Value::String(s.to_string()),
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
                    // Lossy for an `ObjectKey::Str(Key::Wtf8(_))` — see this
                    // method's own doc above for why that is accepted here.
                    map.insert(object_key_json_string(key), value.to_serde_json());
                }
                serde_json::Value::Object(map)
            }
        }
    }
}

/// Renders one [`ObjectKey`] as the JSON string key
/// [`Value::to_serde_json`] embeds it under — also reused by `onix-py`'s
/// hand-written non-finite-float/lone-surrogate JSON writer, so a report can
/// carry a non-`str` key regardless of which of `to_json()`'s two rendering
/// paths it takes.
///
/// A `Str` key renders as its own text, unchanged (the only case a JSON
/// object can ever hold in the first place) — lossily for a
/// [`Key::Wtf8`] (a lone surrogate code point) exactly as
/// [`Value::to_serde_json`]'s own doc explains for a string *value*; the
/// byte-exact rendering for that case is [`write_json_str_content`], which
/// `onix-py`'s writer calls directly instead of going through this
/// function. An `Other` key mirrors Python's `json.dumps`, which
/// stringifies a non-`str` dict key rather than rejecting it — `bool` to
/// `"true"`/`"false"`, `None` to `"null"`, `int` to its decimal text, and a
/// finite `float` through the identical shortest-round-trip `repr()`
/// [`crate::path::python_repr`] uses for a float *value* — so a report
/// embedding one of these four kinds as a nested key matches real
/// `DeepDiff`'s own `to_json()` byte-for-byte. A non-finite `float` key
/// renders as the bare token text a *value* of the same bits would get,
/// rather than reproducing a real `DeepDiff` bug that garbles it to `None`
/// — see `tests/golden/README.md`'s "Known `DeepDiff` quirks" section.
///
/// A `datetime`, `date`, or `tuple` key has no such rule to match: Python's
/// `json.dumps` (and so `DeepDiff.to_json()`) *raises* `TypeError` rather
/// than serializing one — confirmed against real `deepdiff==9.1.0` — so per
/// this crate's compatibility policy (crash → pick the simpler,
/// deterministic behavior, and document it) this renders the same
/// [`crate::path::python_repr`] text the key would get as a *top-level*
/// path segment, which is at least useful output instead of a hard failure.
/// See `tests/golden/README.md`'s "Known `DeepDiff` quirks" section.
// `Number`'s three-way i64/u64/f64 representation makes each `expect` below
// prove an invariant `Number` itself guarantees (mirrors `path::number_repr`,
// which cannot show this lint at all since it stayed `pub(crate)`).
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

/// Whether `value` — or, transitively, any [`Object`] key inside it, down
/// through an [`ObjectKey::Other`] tuple's own elements — holds a
/// [`Str::Wtf8`]/[`Key::Wtf8`] (a lone surrogate code point). Iterative (see
/// the [module documentation](self)'s "Stack safety" section): a heap
/// work-stack, so an adversarially deep report cannot overflow the native
/// stack checking this.
///
/// `onix-py`'s `to_json()` writer does not call this directly (it takes a
/// caller-tracked `may_have_wtf8` byproduct instead, computed once during
/// Python-object conversion, to avoid a second whole-tree walk here on top
/// of that one); this is the ground truth that byproduct approximates, kept
/// public for tests and for any caller without such a byproduct to hand.
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

/// Writes the escaped *content* of a JSON string (no surrounding quotes) for
/// `bytes` — WTF-8, per [`Str`]'s doc. Groups consecutive real Unicode
/// scalars into runs and hands each run to `serde_json` for escaping
/// (guaranteed identical to what [`Value::to_serde_json`] plus
/// `serde_json::to_string` already produces for that content — see this
/// module's private `push_escaped_run`), splicing in a lone surrogate's own
/// single-backslash `\uXXXX` escape between runs, exactly where real
/// `DeepDiff`'s `json.dumps` places it. Public so `onix-py`'s hand-written
/// `to_json()` writer (`crate::guard`, a separate crate) can render a
/// `Str::Wtf8`/`Key::Wtf8` byte-exactly without duplicating this escaping
/// rule.
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

/// Escapes `run` (a real `&str`, guaranteed non-empty) exactly the way
/// `serde_json` already does, by asking it to serialize `run` directly and
/// stripping the surrounding quotes it adds — reusing `serde_json`'s own
/// escaper rather than reimplementing it, so this can never drift from
/// `Value::to_serde_json`'s (unconditionally-correct) output for the same
/// content.
fn push_escaped_run(run: &str, out: &mut String) {
    let quoted =
        serde_json::to_string(run).expect("a &str always serializes to a JSON string literal");
    out.push_str(&quoted[1..quoted.len() - 1]);
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
        // `DeepDiff` reports a subclass-vs-base pair as a `type_changes`
        // finding even when every field matches (see [`Typed`]'s doc), so
        // two values that are not the same class are not structurally equal —
        // checked once here rather than per-arm below through the one shared
        // [`same_class`] definition `diff_at` also uses (an [`Object`] compares
        // by qualified identity plus kind, every other variant by render name),
        // a no-op (both `None`) for a variant that carries no class at all.
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

/// A number preserving [`serde_json`]'s exact three-way representation for
/// the values that fit — a non-negative integer (`u64`), a negative integer
/// (`i64`), or a float (`f64`) — plus a fourth arm for a Python `int` whose
/// magnitude exceeds `i64`/`u64`. A float built from
/// JSON (via `Number::from_serde` or the streaming [`Deserialize`]) is
/// always finite — JSON itself has no `NaN`/`Infinity` literal — but
/// [`Number::from_f64`] is not limited to that boundary: it also builds the
/// [`Number`] a Python `float` converts to, and Python's `float` can be
/// non-finite, so a stored float need not round-trip through
/// [`serde_json::Number`] ([`Value::to_serde_json`] falls back to `null` for
/// one that can't, the same collapse the streaming parse path already used
/// for a non-finite value arriving some other way).
///
/// See the [module documentation](self) for why this int/float distinction
/// is load-bearing for byte-compatible output.
#[derive(Debug, Clone, PartialEq)]
pub struct Number {
    repr: NumberRepr,
}

/// A non-negative integer, a negative integer, a float, or an
/// arbitrary-precision integer. The first three mirror [`serde_json`]'s
/// internal `N` enum so classification and reconstruction match exactly;
/// [`NumberRepr::Big`] is boxed so this arm keeps the enum pointer-sized (see
/// [`Number::from_bigint`] for the one-representation-per-value invariant).
#[derive(Debug, Clone, PartialEq)]
enum NumberRepr {
    /// A non-negative integer (covers the whole `u64` range, including
    /// values above [`i64::MAX`]).
    PosInt(u64),
    /// A negative integer.
    NegInt(i64),
    /// A float — finite, or (via [`Number::from_f64`] only; a
    /// `serde_json::Number` is always finite) `NaN`/`Infinity`/`-Infinity`.
    Float(f64),
    /// An arbitrary-precision integer outside `i64::MIN..=u64::MAX` — a
    /// Python `int` too large for the fast arms above.
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

    /// Builds a number from an arbitrary-precision integer, narrowing to the
    /// `u64`/`i64` fast arms when it fits so every integer value keeps a
    /// single canonical representation — the entry point for a Python `int`
    /// (`crate::convert::int_to_value` in `onix-py`) of any magnitude.
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

    /// Returns `true` if this number was parsed/stored as a float.
    #[must_use]
    pub fn is_f64(&self) -> bool {
        matches!(self.repr, NumberRepr::Float(_))
    }

    /// Returns this number as an `i64` if it fits, else `None` (floats,
    /// `u64` values above [`i64::MAX`], and arbitrary-precision integers
    /// return `None`). Mirrors [`serde_json::Number::as_i64`].
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match &self.repr {
            NumberRepr::PosInt(u) => i64::try_from(*u).ok(),
            NumberRepr::NegInt(i) => Some(*i),
            NumberRepr::Float(_) | NumberRepr::Big(_) => None,
        }
    }

    /// Returns this number as a `u64` if it is a non-negative integer that
    /// fits, else `None`. Mirrors [`serde_json::Number::as_u64`].
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match &self.repr {
            NumberRepr::PosInt(u) => Some(*u),
            NumberRepr::NegInt(_) | NumberRepr::Float(_) | NumberRepr::Big(_) => None,
        }
    }

    /// Returns this number as an `f64` (always `Some`, matching
    /// [`serde_json::Number::as_f64`]; integer values are converted, which
    /// may lose precision for magnitudes beyond `2^53` and saturate to an
    /// infinity beyond `f64::MAX`, matching Python's own `float(int)`).
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
            // num-bigint's `ToPrimitive::to_f64` is total — it saturates to an
            // infinity beyond `f64::MAX`, never `None` — so the default is
            // unreachable.
            NumberRepr::Big(b) => b.to_f64().unwrap_or(f64::INFINITY),
        })
    }

    /// This integer's value as an `i128` when it fits, else `None` (a float,
    /// or an integer whose magnitude exceeds `i128`). Every `u64`/`i64` value
    /// fits, so this is `Some` for every non-`Big` integer.
    #[must_use]
    pub(crate) fn as_i128(&self) -> Option<i128> {
        match &self.repr {
            NumberRepr::PosInt(u) => Some(i128::from(*u)),
            NumberRepr::NegInt(i) => Some(i128::from(*i)),
            NumberRepr::Big(b) => b.to_i128(),
            NumberRepr::Float(_) => None,
        }
    }

    /// The arbitrary-precision payload, or `None` for a value that fits a
    /// fast arm (a `u64`/`i64` integer or a float) — the accessor the Python
    /// bindings and the byte-exact JSON writer read a big integer's exact
    /// digits through.
    #[must_use]
    pub fn as_big(&self) -> Option<&BigInt> {
        match &self.repr {
            NumberRepr::Big(b) => Some(b),
            NumberRepr::PosInt(_) | NumberRepr::NegInt(_) | NumberRepr::Float(_) => None,
        }
    }

    /// This integer's exact value as a [`BigInt`] — a `Big`'s payload, or a
    /// fast-arm integer's `i128` value. [`Number::integer_cmp`]'s slow path.
    fn to_bigint(&self) -> BigInt {
        self.as_big().cloned().unwrap_or_else(|| {
            BigInt::from(
                self.as_i128()
                    .expect("a non-Big integer fits i128; integer_cmp never passes a float"),
            )
        })
    }

    /// Orders two integers by value across every representation. The
    /// `i128` fast path covers every pair that does not involve a `Big`
    /// beyond `i128` (so `u64::MAX` and `-1` order correctly without
    /// allocating); only a genuinely huge operand falls back to a [`BigInt`]
    /// comparison. Callers establish that both numbers are integers, never a
    /// float.
    #[must_use]
    pub(crate) fn integer_cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.as_i128(), other.as_i128()) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => self.to_bigint().cmp(&other.to_bigint()),
        }
    }

    /// Classifies a [`serde_json::Number`] into the compact representation,
    /// preserving exactly which of the three kinds [`serde_json`] chose so
    /// reconstruction is byte-identical. `serde_json`'s own parser never
    /// yields an integer beyond `u64`/`i64` (it renders one as an `f64`
    /// instead), so this never produces a [`NumberRepr::Big`] — that arm is
    /// reached only from the Python-object boundary.
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
    ///
    /// A [`NumberRepr::Big`] has no exact [`serde_json::Number`] form either
    /// (`serde_json`'s number type, absent the `arbitrary_precision` feature,
    /// tops out at `u64`/`i64`/`f64`), so it renders as its nearest `f64` —
    /// the identical value `serde_json` would itself parse the same digits
    /// back into. The byte-exact digits survive through the Python bindings'
    /// own hand-written JSON writer (`crate::guard` in `onix-py`) and
    /// [`Value::to_serde_json`]'s callers that need them; this
    /// `serde_json::Value` bridge is only the CLI/report path, where an
    /// integer beyond `u64` cannot enter from JSON text in the first place.
    fn to_serde_number(&self) -> Option<serde_json::Number> {
        match &self.repr {
            NumberRepr::PosInt(u) => Some(serde_json::Number::from(*u)),
            NumberRepr::NegInt(i) => Some(serde_json::Number::from(*i)),
            NumberRepr::Float(f) => serde_json::Number::from_f64(*f),
            NumberRepr::Big(_) => self.as_f64().and_then(serde_json::Number::from_f64),
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
    /// for the exact base type — see the module documentation's "Subclasses"
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
                    (Value::TimeDelta(x), Value::TimeDelta(y)) => x.value().cmp(&y.value()),
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
/// of each float via [`f64::total_cmp`], so this agrees with [`Number`]'s
/// own [`PartialEq`] on every non-`NaN` pair; see [`SetItems::new`]'s doc
/// for the one place it is deliberately coarser (two bit-identical `NaN`s).
fn number_cmp(a: &Number, b: &Number) -> std::cmp::Ordering {
    if a.is_f64() {
        let af = fold_signed_zero(a.as_f64().unwrap_or_default());
        let bf = fold_signed_zero(b.as_f64().unwrap_or_default());
        return af.total_cmp(&bf);
    }

    // Both are integers (an int and a float rank apart, so this arm never
    // mixes them): compare by value across every representation, including a
    // `u64` above `i64::MAX` and an arbitrary-precision `Big`.
    a.integer_cmp(b)
}

/// An [`Object`]'s key: an interned `Arc<str>` for the common (valid UTF-8)
/// case, or, for a key containing a lone surrogate code point, WTF-8 bytes
/// held in their own,
/// un-interned allocation. Interning shares one allocation across the
/// handful of keys a record-shaped payload repeats thousands of times (see
/// the [module documentation](self)); a surrogate-bearing key is never that
/// shape in practice, so it costs its own small allocation instead of
/// complicating the interner for a case that would not benefit from it.
///
/// See [`Str`] for why byte comparison alone orders and compares both
/// variants correctly, including against each other. Every caller that
/// needs a key's content — rendering, hashing, dict-vs-dict comparison —
/// reads it through [`Key::as_bytes`] or matches the variant directly,
/// never through a lossy conversion: two structurally different keys
/// (differing only in which surrogate they hold) must never collapse to
/// the same representation anywhere in this crate — the exact hazard
/// [`Str`]'s own doc explains for values applies identically to keys used
/// as dict-vs-dict identity.
#[derive(Debug, Clone)]
pub enum Key {
    /// The common case: an interned, valid-UTF-8 key.
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

    /// Walks this key's content one code point at a time — see
    /// [`Str::chars`].
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
    /// Renders this key to an owned `String`, lossily for a [`Key::Wtf8`]
    /// (Rust's own UTF-8 replacement: each surrogate becomes `U+FFFD`).
    ///
    /// Not a `Display`/`ToString` impl: naming it explicitly
    /// keeps every call site announcing that it accepts the lossy,
    /// collision-capable fallback — see [`Key`]'s own doc for why that is
    /// unsafe for anything that decides dict-vs-dict identity or byte-exact
    /// output. [`Value::to_serde_json`] is this crate's only caller, which
    /// is itself already documented as the non-byte-exact rendering.
    fn to_lossy_string(&self) -> String {
        match self {
            Key::Utf8(s) => s.to_string(),
            Key::Wtf8(b) => String::from_utf8_lossy(b).into_owned(),
        }
    }
}

/// What an [`Object`]'s entries represent: a Python `dict` (mapping) or a
/// custom object's attributes.
///
/// Both share [`Object`]'s key-sorted storage — a custom object's attributes
/// are `str`-keyed entries exactly like a `dict`'s `str` keys — but they
/// render two different ways, matching `DeepDiff`: a `dict` entry is a
/// subscript (`root['key']`, `dictionary_item_added`/`removed`), a custom
/// object's attribute is a dotted access (`root.attr`,
/// `attribute_added`/`removed`). `crate::diff::object_diff` reads this to
/// choose the path segment and report category; `crate::ignore_order`'s
/// hashing and distance read it to keep a custom object from ever
/// hash-matching or pairing with a plain `dict` (`DeepDiff`'s own `DeepHash`
/// tags an object with its class name and a `dict` with the bare word
/// `dict`, so the two never share a bucket).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    /// A Python `dict` (or a `dict` subclass), rendered with subscript paths.
    Dict,
    /// A custom object diffed by its attributes, rendered with dotted paths.
    CustomObject,
}

/// A JSON object: key-sorted, exactly-sized entries backed by a single
/// `Box<[(ObjectKey, Value)]>`, with binary-search lookup
/// ([`get`](Object::get)/[`contains_key`](Object::contains_key)) and
/// ascending-key iteration.
///
/// See the [module documentation](self) for why entries are sorted and
/// `str` keys interned (byte-identical rendering and small-map footprint),
/// and [`ObjectKey`]'s own doc for why every other key kind is a second,
/// additive case rather than a change to that representation. A custom
/// object's attributes reuse this same storage, distinguished only by
/// [`Object::kind`] — see [`ObjectKind`].
#[derive(Debug, Clone)]
pub struct Object {
    /// Key-sorted, duplicate-free entries. Invariant: strictly ascending by
    /// [`ObjectKey`]'s own [`Ord`] (enforced by [`Object::from_pairs`]) —
    /// every [`ObjectKey::Str`] entry before every [`ObjectKey::Other`] one,
    /// so [`Object::has_non_str_keys`] can check the last entry alone.
    entries: Box<[(ObjectKey, Value)]>,
    /// The subclass name and kind, or `None` for a plain `dict` (the
    /// overwhelming common case, so it costs one null pointer, not an inline
    /// name-plus-kind). Boxed rather than an inline
    /// `Option<Arc<str>>`-plus-kind so [`Value`] stays within its
    /// frame-budget size cap (`value_is_compact`): a plain `dict` pays a
    /// single pointer here, and only a `dict` subclass or a custom object —
    /// both rare — pays the one small heap allocation. See the `ObjectClass` struct.
    class: Option<Box<ObjectClass>>,
}

/// A non-plain-`dict` [`Object`]'s class: the class name, a qualified identity,
/// and whether the entries are a `dict`'s items or a custom object's
/// attributes. Held behind [`Object::class`]'s `Box` so a plain `dict` carries
/// none of it.
#[derive(Debug, Clone)]
struct ObjectClass {
    /// The Python class *name* (`__name__`): a `dict` subclass's name, or a
    /// custom object's class — the name `DeepDiff` renders in `old_type`/
    /// `new_type`. Used only for *rendering*, never for identity: two
    /// different classes can share a `__name__`.
    name: Arc<str>,
    /// The class's qualified *identity* (`__module__` + `__qualname__`), used
    /// to decide whether two objects are the same class — `DeepDiff` compares
    /// the actual `type` objects (`type(t1) != type(t2)` -> `type_changes`),
    /// so two same-named classes from different modules must not compare equal.
    /// A finer proxy than `name`; see [`Object::same_class`].
    identity: Arc<str>,
    /// Whether these entries are a `dict`'s items or a custom object's
    /// attributes.
    kind: ObjectKind,
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
            class: None,
        }
    }

    /// Attaches a `dict` subclass's `name` and qualified `identity` (`None`
    /// for the exact base `dict`), for a caller (`onix-py`'s converter) that
    /// already has a built [`Object`] and knows which concrete class it came
    /// from. See the `ObjectClass` struct for the name-versus-identity split.
    #[must_use]
    pub fn with_dict_class(mut self, class: Option<(Arc<str>, Arc<str>)>) -> Self {
        self.class = class.map(|(name, identity)| {
            Box::new(ObjectClass {
                name,
                identity,
                kind: ObjectKind::Dict,
            })
        });
        self
    }

    /// Marks these entries as a custom object's attributes (rather than a
    /// `dict`'s items) under class `name` and qualified `identity` — see
    /// [`ObjectKind`] and the `ObjectClass` struct. Both are always present for a custom
    /// object, unlike a `dict` subclass's optional name.
    #[must_use]
    pub fn into_custom_object(mut self, name: Arc<str>, identity: Arc<str>) -> Self {
        self.class = Some(Box::new(ObjectClass {
            name,
            identity,
            kind: ObjectKind::CustomObject,
        }));
        self
    }

    /// The class *name* (`__name__`) this object carries, or `None` for the
    /// exact base type — the name for *rendering*, not identity (see
    /// [`Object::same_class`]).
    #[must_use]
    pub fn type_name(&self) -> Option<&str> {
        self.class.as_ref().map(|class| class.name.as_ref())
    }

    /// Whether `self` and `other` are the same Python class: same qualified
    /// identity *and* same kind. `DeepDiff` reports `type_changes` between two
    /// values whose `type()` objects are not identical, so a `dict` subclass
    /// and a custom object sharing a `__name__`, or two same-named classes from
    /// different modules, are *not* the same class here — matched by the
    /// `ObjectClass::identity` proxy, not the render name. Two plain `dict`s
    /// (no class) are the same class.
    #[must_use]
    pub fn same_class(&self, other: &Object) -> bool {
        match (self.class.as_ref(), other.class.as_ref()) {
            (None, None) => true,
            (Some(a), Some(b)) => a.kind == b.kind && a.identity == b.identity,
            _ => false,
        }
    }

    /// Whether these entries are a `dict`'s items or a custom object's
    /// attributes — see [`ObjectKind`]. A plain `dict` (no class) is
    /// [`ObjectKind::Dict`].
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

    /// Iterates `(key, value)` pairs in ascending key order, with the exact
    /// [`ObjectKey`] (never lossily rendered — see [`Key`]'s doc).
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

/// A per-session string interner sharing one `Arc<str>` per distinct
/// UTF-8 key.
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

    /// Converts `key` into an [`Object`] [`Key`]: an interned handle for the
    /// common [`Str::Utf8`] case (see [`Interner::intern`]), or an owned,
    /// un-interned allocation for the rare [`Str::Wtf8`] one — see [`Key`]'s
    /// doc for why the latter is never worth sharing.
    fn intern_key(&mut self, key: Str) -> Key {
        match key {
            Str::Utf8(s) => Key::Utf8(self.intern(&s)),
            Str::Wtf8(b) => Key::Wtf8(b),
        }
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

    /// Builds an object [`Value`] from `entries`, interning each
    /// [`Str::Utf8`] key against this builder's session (a rare
    /// [`Str::Wtf8`] key — one holding a lone surrogate — is never interned,
    /// see [`Key`]'s doc) and sorting into the canonical ascending key
    /// order. A duplicate key keeps the last value, matching [`From`] and
    /// [`Deserialize`]. Accepts anything convertible to [`Str`], so a plain
    /// `String` key (every call site that predates [`Str::Wtf8`]) keeps
    /// working unchanged.
    #[must_use]
    pub fn object<K: Into<Str>>(&mut self, entries: Vec<(K, Value)>) -> Value {
        let pairs = entries
            .into_iter()
            .map(|(key, value)| (ObjectKey::Str(self.interner.intern_key(key.into())), value))
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

    /// [`Builder::intern`]'s [`Str`]-aware twin: interns a plain `str` key
    /// exactly as that method does, or passes a key holding a lone
    /// surrogate code point through un-interned — see [`Key`]'s doc for why
    /// that rare shape is never worth sharing. For a caller building an
    /// [`ObjectKey::Str`] directly (for [`Builder::object_with_keys`])
    /// alongside a mix of other key kinds.
    #[must_use]
    pub fn intern_key(&mut self, key: Str) -> Key {
        self.interner.intern_key(key)
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

    /// [`Builder::object_with_keys`], additionally attaching a `dict`
    /// subclass's `(name, identity)` (`None` for the exact base `dict`) — the
    /// entry point `onix-py`'s converter uses for every `dict` subclass,
    /// whether or not its keys are all `str`. See [`Object::with_dict_class`]
    /// for the name-versus-identity split and the module documentation's
    /// "Subclasses" section.
    #[must_use]
    pub fn object_with_keys_and_class(
        &mut self,
        entries: Vec<(ObjectKey, Value)>,
        class: Option<(Arc<str>, Arc<str>)>,
    ) -> Value {
        Value::Object(Object::from_pairs(entries).with_dict_class(class))
    }

    /// Builds a custom object [`Value`] from its attribute `entries` (all
    /// `str`-keyed) under class `name` and qualified `identity` — the entry
    /// point `onix-py`'s converter uses for an instance of a user-defined
    /// class, diffed by its attributes rather than as a `dict`. See
    /// [`ObjectKind::CustomObject`] and [`Object::same_class`].
    #[must_use]
    pub fn custom_object(
        &mut self,
        entries: Vec<(ObjectKey, Value)>,
        name: Arc<str>,
        identity: Arc<str>,
    ) -> Value {
        Value::Object(Object::from_pairs(entries).into_custom_object(name, identity))
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
