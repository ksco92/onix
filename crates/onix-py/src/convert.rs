//! Converts a live Python object graph into an [`onix_core::Value`] once, up
//! front — the [`crate::deepdiff::DeepDiff`] class's "drop-in" layer diffs the
//! converted value model natively; it never touches Python objects again
//! after conversion. The compact value model is built *directly*: there is no
//! intermediate `serde_json::Value` tree, so the two input trees only ever
//! exist in the memory-frugal representation.
//!
//! # Supported types (documented MVP scope)
//!
//! | Python | `Value` | Notes |
//! | --- | --- | --- |
//! | `None` | `Null` | |
//! | `bool` | `Bool` | checked before `int` — `bool` is a Python `int` subclass |
//! | `int` | `Number` | any magnitude — `i64`/`u64` fast path, arbitrary precision beyond it |
//! | `float` | `Number` | `NaN`/`Infinity`/`-Infinity` included |
//! | `str` | `Str` | UTF-8 the fast, common way; a lone surrogate code point survives too, see below |
//! | `dict` (keys below), or a subclass | `Object` | a `str` key (including a surrogate one) interned across the whole walk |
//! | `list`, or a subclass | `Array` | |
//! | `tuple`, or a subclass (including a `namedtuple`) | `Tuple` | diffed positionally even for a `namedtuple`, see below |
//! | `set`, or a subclass | `Set` | members restricted, see below |
//! | `frozenset`, or a subclass | `FrozenSet` | members restricted, see below |
//! | `datetime.datetime`, or a subclass (e.g. pandas `Timestamp`) | `DateTime` | naive or any `tzinfo`, see below |
//! | `datetime.date`, or a subclass | `Date` | |
//! | `datetime.time`, or a subclass | `Time` | naive or any `tzinfo`, see below |
//! | `datetime.timedelta`, or a subclass | `TimeDelta` | |
//! | any other object | `Object` (custom) | diffed by its attributes, see below |
//!
//! A subclass instance converts and compares exactly like the base type —
//! see the "Subclasses" section below — except as a `set`/`frozenset`
//! *member*, where only the exact `tuple`/`frozenset`/`datetime`/`date`/
//! `time`/`timedelta` type is accepted (a `list`/`dict`/`set` subclass, or a
//! `tuple`/`frozenset` subclass including a `namedtuple`, reaching a set
//! member is refused the same way any other unsupported type is).
//!
//! An `int` of any magnitude converts: a value in `i64::MIN..=u64::MAX` takes
//! the compact fast arm, and a larger one keeps its exact arbitrary-precision
//! value (read through `int`'s own unbound `to_bytes`, never a subclass's own
//! methods — see [`exact_big_int`]), matching real `DeepDiff`, which compares
//! Python `int`s natively.
//!
//! Every other type raises a Python exception instead of converting:
//!
//! - A `dict` key may be `str` (including one holding a lone surrogate code
//!   point — see below), `None`, `bool`, `int`, `float`, `datetime`,
//!   `date`, or a `tuple` of those (never a nested `tuple`), or a
//!   `tuple`/`datetime`/`date` subclass (including a `namedtuple`) — unlike
//!   a `tuple` *value*, which keeps the exactness rule, since `DeepDiff`'s
//!   own key matching is plain Python `==`/`hash` and never consults
//!   `type(obj)`: a key subclass instance classifies as its exact base type
//!   with no class name tracked (see [`ObjectKey`], which has no class-name
//!   field). A key of any other type — including `time`/`timedelta`,
//!   a custom object, or a `tuple` that nests another `tuple` — raises
//!   [`PyTypeError`] naming the key's type and the path to the dict
//!   containing it. Path rendering for a non-`str` key follows `DeepDiff`'s
//!   own rule ([`onix_core::path::dict_key_repr`]): `repr()` for every kind
//!   but `tuple`, which instead splits into one bracket group per element
//!   (`root[1][2]`, never `root[(1, 2)]`). Two keys that are Python-equal
//!   but not the same type (`1`/`1.0`/`True`) are matched as *one* key
//!   between two dicts being diffed — real Python `dict`/`set` semantics —
//!   even though this crate's own `Value` keeps them structurally distinct
//!   everywhere else; see `crate::ignore_order::match_dict_keys`'s doc in
//!   `onix-core`.
//! - A `tzinfo` whose `utcoffset()` is not a whole number of seconds raises
//!   [`PyValueError`]: the value model carries an offset in seconds. Applies
//!   equally to a `datetime` and a `time`.
//! - A `set`/`frozenset` member that is not one of the types this MVP allows
//!   a set to hold (`None`, `bool`, `int`, `float`, `str`, `tuple`,
//!   `frozenset`, `datetime`, `date`, `time`, `timedelta`, or a
//!   `datetime`/`date`/`time`/`timedelta` subclass) raises [`PyTypeError`]
//!   naming the member's type and its path. A plain `list` or `dict` cannot
//!   reach a set member at all — Python itself refuses `{[1]}` with
//!   `TypeError: unhashable type: 'list'` — but a `list`/`dict`/`set`
//!   subclass that defines `__hash__` can, and real `DeepDiff` would report
//!   it under that subclass's own name; a `tuple`/`frozenset` subclass
//!   (including a `namedtuple`) has no such obstacle at all — so all of
//!   these stay refused here, including nested inside an otherwise-allowed
//!   container: `{(datetime(2024, 1, 1),)}` converts, but
//!   `{(HashableList([1]),)}` does not, for a `list` subclass `HashableList`
//!   defining `__hash__`.
//! - Any other type is diffed as a **custom object**, by its attributes,
//!   matching `DeepDiff`'s own `_diff_obj` fallback (see [`object_attributes`]
//!   for the exact enumeration and `tests/golden/README.md`'s "Custom objects"
//!   section for the enumerated divergences). This is the fallback for every
//!   value that reached none of the arms above — a user-defined class, and
//!   also a builtin `DeepDiff` sends to a handler this MVP lacks (`bytes`, an
//!   arbitrary `Iterable`, an `Enum`), which is therefore diffed as an object
//!   here rather than the way `DeepDiff` handles it. A custom object cannot
//!   reach a `set`/`frozenset` member (it is refused there like any other
//!   unsupported member type).
//!
//! # Subclasses
//!
//! This conversion checks the *exact* type first, falling through to a
//! second, non-exact `isinstance`-style cast that additionally records
//! `type(obj).__name__` for a subclass — see [`onix_core::value`]'s
//! "Subclasses" section for how that name flows through the rest of the
//! value model and diff engine. A
//! `namedtuple` is accepted as an ordinary `tuple` subclass and diffed
//! **positionally** (`root[0][1]`), not by field (`root[0].y`) the way real
//! `DeepDiff` does — a documented divergence (see `tests/golden/README.md`),
//! not an approximation of the field-walking shape. A subclass instance of
//! any type this conversion carries a class name for also cannot round-trip
//! through [`crate::deepdiff::DeepDiff::to_dict`] as itself: it renders back
//! as the plain base type its fields describe, the same simplification the
//! `zoneinfo`/`pytz` round trip below already documents.
//!
//! # Datetimes and dates
//!
//! A `datetime` converts with its wall-clock fields and, when it is aware,
//! the *fixed* offset its `tzinfo.utcoffset()` reports at that moment. A
//! `zoneinfo`/`pytz` zone therefore round-trips through
//! [`crate::deepdiff::DeepDiff::to_dict`] as a plain
//! `datetime.timezone(timedelta(...))` carrying the same offset, not as the
//! original zone object — which changes nothing about the diff, since
//! `DeepDiff` compares datetimes by instant and reports a `values_changed`
//! pair normalized to UTC regardless.
//!
//! The exact-type cast runs first: `datetime` is itself a `date` subclass,
//! so an inexact check in either direction would misread one as the other,
//! and checking `datetime` (both exact and subclass) before `date` is what
//! keeps a `datetime`/`Timestamp` from ever being misclassified as a `date`.
//!
//! A `time` (or a subclass, the same exact-then-subclass cast as `datetime`)
//! converts the same way a `datetime` does (wall-clock fields plus the fixed
//! offset in force); unlike `datetime`, real `DeepDiff` never normalizes a
//! `time` at report time, so onix reports it raw everywhere (see
//! [`onix_core::datetime`]'s module doc for the exact, confirmed comparison
//! and hashing rules — genuinely different from `datetime`'s). A
//! `timedelta` (or a subclass) converts to its exact
//! `(days, seconds, microseconds)`.
//!
//! A `tuple` converts to [`onix_core::Value::Tuple`], which the engine
//! diffs positionally exactly like a list while still reporting a
//! tuple-vs-list pairing as a `type_changes` — matching `DeepDiff`.
//!
//! A `set`/`frozenset` converts to [`onix_core::Value::Set`]/
//! [`onix_core::Value::FrozenSet`]. Its members are compared, and rendered,
//! without reference to the order they were iterated in — see
//! [`onix_core::value::SetItems`], and `tests/golden/README.md`'s "Set
//! iteration order" section for where that leaves `DeepDiff` behind.
//!
//! # Lone surrogate code points
//!
//! A Python `str` can legally hold an unpaired surrogate code point (e.g.
//! `"\udc80"`), the one code point UTF-8 cannot encode. [`pystring_to_cstr`]
//! reads every `str` through [`Bound::to_cow`] first — a zero-copy borrow
//! that succeeds for the overwhelming common case and costs nothing beyond
//! it — and only on that borrow's failure falls back to
//! `str.encode('utf-8', 'surrogatepass')`, the `CPython` idiom that yields
//! [WTF-8](https://simonsapin.github.io/wtf-8/) bytes: valid UTF-8 with each
//! surrogate direct-encoded in the three-byte form strict UTF-8 forbids for
//! that range. [`onix_core::value::Str`] stores exactly that split, so
//! equality and ordering both follow Python code-point comparison, and
//! [`wtf8_to_pyobject`] reverses the encoding (`bytes.decode('utf-8',
//! 'surrogatepass')`) when rendering a report value or a dict key back to a
//! live Python object. See `tests/golden/README.md` for the small,
//! documented set of nuances this leaves relative to real `DeepDiff` (all in
//! `to_dict()`'s structural key/path rendering, never in a reported value).
//!
//! # Key interning
//!
//! An object key that is plain UTF-8 (the overwhelming common case) is
//! interned across the whole conversion via a single
//! [`onix_core::value::Builder`] threaded through the walk: record-shaped
//! data repeats a handful of keys across tens of thousands of objects, so
//! each distinct key costs a single shared allocation rather than one per
//! occurrence. A key holding a lone surrogate is never interned — see
//! `onix_core::value::Key`'s own doc for why that shape isn't worth sharing.
//!
//! # Depth guard, and why this walk is iterative
//!
//! This conversion mirrors the Python object graph's own shape. A naive
//! implementation would walk it via native recursion, exactly the
//! stack-overflow class `onix_core`'s own diff engine eliminates for the
//! *diff* itself. [`to_value`] uses the identical technique, an explicit
//! `Vec`-backed stack of in-progress list/dict frames walked in a single
//! loop, so peak *native* stack usage is `O(1)` regardless of how deeply the
//! input is nested. The same has to hold for anything `onix_core` runs while
//! a value is being built — a set sorts its members into canonical order at
//! construction, and that comparison is iterative for exactly this reason
//! (see `onix_core::value`'s "Stack safety" section). Because every step of
//! the build is iterative and the compact [`onix_core::Value`]'s own `Drop`
//! is iterative too, conversion — and the teardown of a partially built tree
//! on any error path — is stack-safe on *any* thread at *any* depth, without
//! a sized worker: only the natively recursive diff engine still needs one
//! (see [`crate::guard`]).
//!
//! On top of that native-stack safety, [`to_value`] separately takes the
//! same `max_depth` budget the diff itself will use and raises
//! [`crate::errors::MaxDepthError`] once conversion would recurse past it.
//! That check runs strictly *before* `onix_core::diff_with_options`'s own
//! guard, using the identical depth-counting convention (the root value is
//! depth `0`; stepping into a dict value or list element adds one). It is
//! intentionally a little stricter than `onix_core::diff_with_max_depth`'s
//! guarantee that two *equal* inputs of any depth always diff cleanly,
//! because equality can't be known yet at conversion time.
use std::sync::Arc;

use num_bigint::BigInt;
use onix_core::datetime::{
    Date as CDate, DateTime as CDateTime, Time as CTime, TimeDelta as CTimeDelta,
};
use onix_core::path::{
    PathSegment, attribute_path_segment, object_key_path_segment as key_path_segment, render_path,
};
use onix_core::value::{
    Builder, Entries, Key as CKey, ObjectKey, ObjectKind, SetItems, Str as CStr, Typed,
};
use onix_core::{Number as CNumber, Value as CValue};
use pyo3::conversion::IntoPyObjectExt;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::iter::{
    BoundDictIterator, BoundFrozenSetIterator, BoundListIterator, BoundSetIterator,
    BoundTupleIterator,
};
use pyo3::types::{
    IntoPyDict, PyBool, PyBytes, PyDate, PyDateTime, PyDelta, PyDict, PyFloat, PyFrozenSet, PyInt,
    PyList, PySet, PyString, PyTime, PyTuple, PyType, PyTzInfo,
};

use crate::errors::MaxDepthError;

/// A Python sequence being walked: a `list` or a `tuple`. The two differ
/// only in their iterator type and in which [`CValue`] the finished items
/// become, so every other step of the walk treats them identically — the
/// same way the diff engine does.
enum SeqIter<'py> {
    List(BoundListIterator<'py>),
    Tuple(BoundTupleIterator<'py>),
    Set(BoundSetIterator<'py>),
    FrozenSet(BoundFrozenSetIterator<'py>),
}

impl<'py> SeqIter<'py> {
    fn next(&mut self) -> Option<Bound<'py, PyAny>> {
        match self {
            SeqIter::List(iter) => iter.next(),
            SeqIter::Tuple(iter) => iter.next(),
            SeqIter::Set(iter) => iter.next(),
            SeqIter::FrozenSet(iter) => iter.next(),
        }
    }

    /// How many elements are still to come (both iterators are
    /// `ExactSizeIterator`), so a frame can pre-size its buffer.
    fn len(&self) -> usize {
        match self {
            SeqIter::List(iter) => iter.len(),
            SeqIter::Tuple(iter) => iter.len(),
            SeqIter::Set(iter) => iter.len(),
            SeqIter::FrozenSet(iter) => iter.len(),
        }
    }

    /// Wraps this sequence's finished items in the matching value shape,
    /// attaching `class_name` (`None` for the exact base type) — see the
    /// module doc's "Subclasses" section.
    fn build(&self, items: Vec<CValue>, class_name: Option<Arc<str>>) -> CValue {
        match self {
            SeqIter::List(_) => {
                CValue::Array(Typed::with_class_name(items.into_boxed_slice(), class_name))
            }
            SeqIter::Tuple(_) => {
                CValue::Tuple(Typed::with_class_name(items.into_boxed_slice(), class_name))
            }
            SeqIter::Set(_) => CValue::Set(SetItems::new(items).with_type_name(class_name)),
            SeqIter::FrozenSet(_) => {
                CValue::FrozenSet(SetItems::new(items).with_type_name(class_name))
            }
        }
    }

    /// Whether this sequence's elements are set members — the ones that get
    /// a [`child_segment`] placeholder instead of an index.
    fn holds_set_members(&self) -> bool {
        matches!(self, SeqIter::Set(_) | SeqIter::FrozenSet(_))
    }
}

/// One in-progress container on [`to_value`]'s explicit work-stack: either a
/// sequence or a dict whose *n*th child has been dispatched for conversion
/// and whose remaining children (plus everything converted so far) are parked
/// here until that child's result comes back.
///
/// The next child's index (for a sequence) and every child's depth are
/// derivable at the one place they are read, in [`advance_frame`] — the next
/// index is `built.len()` once the finished child has been pushed, and the
/// child depth is `path.len()` once its path segment has been pushed.
enum Frame<'py> {
    Seq {
        remaining: SeqIter<'py>,
        built: Vec<CValue>,
        /// Whether this sequence's elements are inside a set member, which
        /// restricts the types [`classify`] accepts for them. Transitive:
        /// true for a set's own members, and for the elements of any
        /// container nested inside one (see the module doc).
        restricted: bool,
        /// The subclass name this sequence's own container carries (`None`
        /// for the exact base type) — see the module doc's "Subclasses"
        /// section. Unrelated to `restricted`, which is about the
        /// *elements*', not this container's own, type.
        class_name: Option<Arc<str>>,
    },
    Dict {
        remaining: BoundDictIterator<'py>,
        built: Vec<(ObjectKey, CValue)>,
        current_key: ObjectKey,
        /// See [`Frame::Seq::class_name`].
        class_name: Option<Arc<str>>,
        /// Whether the entries being collected are a `dict`'s items or a
        /// custom object's attributes — decides which `Value` this frame
        /// builds (see [`finish_object`]).
        kind: ObjectKind,
    },
}

/// What happens when converting a single object: either it produced a
/// finished [`CValue`] outright (a scalar, or an empty sequence/dict), or
/// it's a non-empty container — [`to_value`]'s loop pushes a [`Frame`] and
/// descends into the returned first child.
enum Step<'py> {
    Done(CValue),
    Seq {
        iter: SeqIter<'py>,
        first: Bound<'py, PyAny>,
        /// See [`Frame::Seq::class_name`].
        class_name: Option<Arc<str>>,
    },
    Dict {
        iter: BoundDictIterator<'py>,
        first_key: ObjectKey,
        first_value: Bound<'py, PyAny>,
        /// See [`Frame::Seq::class_name`].
        class_name: Option<Arc<str>>,
        /// See [`Frame::Dict::kind`].
        kind: ObjectKind,
    },
}

/// Tries every temporal type [`classify`] accepts (`datetime`, `date`,
/// `time`, `timedelta`, exact or a subclass) — split out to keep `classify`
/// itself under the line-count limit. `None` when `current` is none of
/// these, so `classify` falls through to its remaining checks.
///
/// `datetime` before `date`: see the module doc — every `datetime` is also a
/// `date` at the C level, so checking `date` first would swallow every
/// `datetime` too. Each type's exact-type branch runs first so the common
/// case pays only one cast; a subclass (pandas' `Timestamp` is the common
/// one) falls through to the second, non-exact branch and carries its own
/// class name — see the module doc's "Subclasses" section. All four convert
/// the same way whether or not they sit inside a set member.
fn classify_temporal(current: &Bound<'_, PyAny>, path: &[PathSegment]) -> PyResult<Option<CValue>> {
    if current.cast_exact::<PyDateTime>().is_ok() {
        return Ok(Some(datetime_to_value(current, path, None)?));
    }
    if current.cast::<PyDateTime>().is_ok() {
        return Ok(Some(datetime_to_value(
            current,
            path,
            Some(class_name(current)),
        )?));
    }

    if current.cast_exact::<PyDate>().is_ok() {
        return Ok(Some(CValue::Date(Typed::new(date_fields(current, path)?))));
    }
    if current.cast::<PyDate>().is_ok() {
        return Ok(Some(CValue::Date(Typed::with_class_name(
            date_fields(current, path)?,
            Some(class_name(current)),
        ))));
    }

    if current.cast_exact::<PyTime>().is_ok() {
        return Ok(Some(time_to_value(current, path, None)?));
    }
    if current.cast::<PyTime>().is_ok() {
        return Ok(Some(time_to_value(
            current,
            path,
            Some(class_name(current)),
        )?));
    }

    if current.cast_exact::<PyDelta>().is_ok() {
        return Ok(Some(timedelta_to_value(current, path, None)?));
    }
    if current.cast::<PyDelta>().is_ok() {
        return Ok(Some(timedelta_to_value(
            current,
            path,
            Some(class_name(current)),
        )?));
    }

    Ok(None)
}

/// Classifies a single Python object: everything [`to_value`]'s loop does per
/// node except the `max_depth` check (needs the loop's own `depth` counter)
/// and attaching the result to the work-stack (needs the loop's own
/// `path`/`stack`).
///
/// `path` is the path to `current` itself (used verbatim for an
/// unsupported-type error, and — when `current` is a dict — also passed
/// through to [`next_dict_entry`] for a bad-key error). `builder` builds the
/// one container this can finish outright, an empty dict.
///
/// `set_member` restricts the accepted types to the ones this MVP allows
/// inside a set: a `list` or `dict` reaching a set member (only possible
/// through a subclass defining `__hash__`, since a plain `list`/`dict` is
/// unhashable and Python itself refuses to build the set) is refused with
/// the same error any other unsupported type gets. The flag is *transitive*
/// — it is set for a set's own members and for everything nested inside one
/// — so `{(HashableList([1]),)}` is refused for its nested `list` subclass
/// the same way `{HashableList([1])}` would be. A `datetime`/`date` is
/// accepted either way: [`onix_core::path::set_item_repr`] defines how one
/// renders as a set item, top-level or nested.
///
/// A `tuple`/`frozenset` **subclass** — including a `namedtuple`, a `tuple`
/// subclass — reaching a set member is refused the same way a `list`/`dict`
/// subclass is: only the *exact* base type is accepted there (see the
/// module doc's "Subclasses" section for why this member-position
/// restriction is unaffected by the general subclass support this function
/// otherwise adds). A `datetime`/`date` subclass has no such restriction —
/// it converts identically whether or not it sits inside a set member,
/// exactly like the base type already does.
fn classify<'py>(
    current: &Bound<'py, PyAny>,
    path: &[PathSegment],
    builder: &mut Builder,
    set_member: bool,
) -> PyResult<Step<'py>> {
    if current.is_none() {
        return Ok(Step::Done(CValue::Null));
    }

    // `bool` is a Python `int` subclass, so this check must precede the
    // `PyInt` one below or every bool would be misread as an int.
    if let Ok(b) = current.cast::<PyBool>() {
        return Ok(Step::Done(CValue::Bool(b.is_true())));
    }

    if let Ok(i) = current.cast::<PyInt>() {
        return Ok(Step::Done(int_to_value(i, path)?));
    }

    if let Ok(f) = current.cast::<PyFloat>() {
        return Ok(Step::Done(float_to_value(f.value())));
    }

    if let Ok(s) = current.cast::<PyString>() {
        return Ok(Step::Done(CValue::Str(pystring_to_cstr(s)?)));
    }

    if let Some(value) = classify_temporal(current, path)? {
        return Ok(Step::Done(value));
    }

    if !set_member && let Ok(list) = current.cast_exact::<PyList>() {
        return Ok(seq_step(SeqIter::List(list.iter()), None));
    }
    if !set_member && let Ok(list) = current.cast::<PyList>() {
        return Ok(seq_step(
            SeqIter::List(list.iter()),
            Some(class_name(current)),
        ));
    }

    if let Ok(tuple) = current.cast_exact::<PyTuple>() {
        return Ok(seq_step(SeqIter::Tuple(tuple.iter()), None));
    }
    // Non-exact, unlike the branch above: a `tuple` subclass — including a
    // `namedtuple` — carries its own class name and compares as a plain
    // `tuple` otherwise (see the module doc's "Subclasses" section), except
    // as a set member, where only the exact type is accepted (see this
    // function's own doc).
    if !set_member && let Ok(tuple) = current.cast::<PyTuple>() {
        return Ok(seq_step(
            SeqIter::Tuple(tuple.iter()),
            Some(class_name(current)),
        ));
    }

    if let Ok(set) = current.cast_exact::<PySet>() {
        return Ok(seq_step(SeqIter::Set(set.iter()), None));
    }
    // Non-exact: a `set` subclass, refused as a set member like `tuple`
    // above (a plain `set` is itself unhashable and so can never actually
    // reach here as a member; a hashable subclass could, and is refused the
    // same way for consistency).
    if !set_member && let Ok(set) = current.cast::<PySet>() {
        return Ok(seq_step(
            SeqIter::Set(set.iter()),
            Some(class_name(current)),
        ));
    }

    if let Ok(frozen) = current.cast_exact::<PyFrozenSet>() {
        return Ok(seq_step(SeqIter::FrozenSet(frozen.iter()), None));
    }
    if !set_member && let Ok(frozen) = current.cast::<PyFrozenSet>() {
        return Ok(seq_step(
            SeqIter::FrozenSet(frozen.iter()),
            Some(class_name(current)),
        ));
    }

    if !set_member && let Ok(dict) = current.cast::<PyDict>() {
        let class_name = current
            .cast_exact::<PyDict>()
            .is_err()
            .then(|| class_name(current));
        let mut iter = dict.iter();

        return Ok(match next_dict_entry(&mut iter, path, builder)? {
            None => Step::Done(builder.object_with_keys_and_type_name(Vec::new(), class_name)),
            Some((first_key, first_value)) => Step::Dict {
                iter,
                first_key,
                first_value,
                class_name,
                kind: ObjectKind::Dict,
            },
        });
    }

    // A custom object: diffed by its attributes, matching `DeepDiff`'s
    // `_diff_obj` (see [`object_attributes`]). Never a set member — a set
    // member is restricted to the hashable base types (see this function's
    // own doc); a custom object reaching one is refused like any other
    // unsupported member type.
    if !set_member {
        let class = class_name(current);
        let attrs = object_attributes(current, path)?;
        let mut iter = attrs.iter();
        return Ok(match next_dict_entry(&mut iter, path, builder)? {
            None => Step::Done(builder.custom_object(Vec::new(), class)),
            Some((first_key, first_value)) => Step::Dict {
                iter,
                first_key,
                first_value,
                class_name: Some(class),
                kind: ObjectKind::CustomObject,
            },
        });
    }

    Err(unhashable_member_error(current, path))
}

/// The attributes `DeepDiff`'s `_diff_obj` (diff.py, `deepdiff==9.1.0`) sees
/// for a custom object, as a Python `dict` this walk then iterates like any
/// other dict — so a custom object reuses the whole `Frame::Dict` machinery,
/// differing only in the `Value` it finally builds and the `.attr` path
/// segments it renders (see [`classify`]).
///
/// Matches `_diff_obj`'s own per-object strategy exactly (the pair check
/// `all('__dict__' in dir(t))` reduces to this per-object test because
/// `diff_at` only ever diffs two instances of the *same* class — a class
/// mismatch is a `type_changes` that never enumerates attributes):
///
/// - `'__dict__' in dir(obj)`: `helper.detailed__dict__` — the instance
///   `__dict__` plus every non-callable, non-dunder name `dir(obj)` adds
///   (class attributes and `@property` values, read through `getattr` exactly
///   as `DeepDiff` does), dropping dunder (`__x`) keys but keeping a single
///   underscore (`_x`) and a name-mangled (`_Cls__x`) one.
/// - else `'__slots__' in dir(obj)`: `_diff_obj._dict_from_slots` — the slot
///   values gathered up the MRO, un-mangling each slot name to read it.
/// - else: `getmembers(obj)` keeping the non-callable members.
///
/// A final pass drops any attribute whose name starts with `__`, matching
/// `_diff_dict`'s own `print_as_attribute` key filter (diff.py) — so a slot
/// or member named `__x` never reaches the diff, exactly as in `DeepDiff`.
///
/// `getattr` here runs user code for a `@property`, precisely as `DeepDiff`
/// does; a property that raises is skipped (its name simply does not appear),
/// keeping conversion total rather than propagating a mid-walk failure.
fn object_attributes<'py>(
    obj: &Bound<'py, PyAny>,
    _path: &[PathSegment],
) -> PyResult<Bound<'py, PyDict>> {
    let dir = obj.dir()?;
    let result = if dir_contains(&dir, "__dict__")? {
        detailed_dict(obj, &dir)?
    } else if dir_contains(&dir, "__slots__")? {
        slots_dict(obj)?
    } else {
        getmembers_noncallable(obj, &dir)?
    };

    // `_diff_dict`'s `print_as_attribute` filter: no dunder-named attribute
    // is ever diffed. `detailed__dict__` already excludes them, but the slots
    // and getmembers strategies can still surface one.
    let dunder: Vec<Bound<'py, PyAny>> = result
        .keys()
        .iter()
        .filter(|key| {
            key.extract::<String>()
                .is_ok_and(|name| name.starts_with("__"))
        })
        .collect();
    for key in dunder {
        result.del_item(&key)?;
    }
    Ok(result)
}

/// Whether `dir` (the result of `dir(obj)`) contains `name`.
fn dir_contains(dir: &Bound<'_, PyList>, name: &str) -> PyResult<bool> {
    dir.contains(name)
}

/// `helper.detailed__dict__(obj)` (diff.py) — see [`object_attributes`].
fn detailed_dict<'py>(
    obj: &Bound<'py, PyAny>,
    dir: &Bound<'py, PyList>,
) -> PyResult<Bound<'py, PyDict>> {
    let py = obj.py();
    let result = PyDict::new(py);
    let private_prefix = format!("_{}__", type_name(obj));

    let instance_dict = obj.getattr("__dict__")?;
    let instance_dict = instance_dict.cast::<PyDict>()?;
    for (key, value) in instance_dict.iter() {
        if let Ok(name) = key.extract::<String>()
            && name.starts_with("__")
            && !name.starts_with(&private_prefix)
        {
            continue;
        }
        result.set_item(key, value)?;
    }

    for name in dir.iter() {
        let Ok(text) = name.extract::<String>() else {
            continue;
        };
        if text.starts_with("__") || text.starts_with(&private_prefix) {
            continue;
        }
        if result.contains(&name)? {
            continue;
        }
        // `getattr` runs a `@property`'s getter — user code, exactly as
        // `DeepDiff` does; one that raises just leaves its name out.
        let Ok(value) = obj.getattr(&*text) else {
            continue;
        };
        if value.is_callable() {
            continue;
        }
        result.set_item(name, value)?;
    }

    Ok(result)
}

/// `_diff_obj._dict_from_slots(obj)` (diff.py) — see [`object_attributes`].
fn slots_dict<'py>(obj: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = obj.py();
    let result = PyDict::new(py);
    let type_name = type_name(obj);
    let mro = obj.get_type().getattr("__mro__")?;

    for base in mro.try_iter()? {
        let base = base?;
        let Ok(slots) = base.getattr("__slots__") else {
            continue;
        };
        let names: Vec<String> = if let Ok(single) = slots.extract::<String>() {
            vec![single]
        } else {
            slots
                .try_iter()?
                .map(|item| item?.extract::<String>())
                .collect::<PyResult<_>>()?
        };
        for name in names {
            // Un-mangle a dunder slot to read it, but key the result by the
            // slot's own name (`_dict_from_slots` returns `{i: getattr(...)}`).
            let attr = if name.starts_with("__") && name != "__weakref__" {
                format!("_{type_name}{name}")
            } else {
                name.clone()
            };
            if obj.hasattr(&*attr)? {
                result.set_item(&name, obj.getattr(&*attr)?)?;
            }
        }
    }

    Ok(result)
}

/// `{k: v for k, v in getmembers(obj) if not callable(v)}` (diff.py's
/// `_diff_obj` fallback) — see [`object_attributes`]. `getmembers` reads
/// every `dir(obj)` name through `getattr`, skipping any that raises.
fn getmembers_noncallable<'py>(
    obj: &Bound<'py, PyAny>,
    dir: &Bound<'py, PyList>,
) -> PyResult<Bound<'py, PyDict>> {
    let py = obj.py();
    let result = PyDict::new(py);
    for name in dir.iter() {
        let Ok(text) = name.extract::<String>() else {
            continue;
        };
        let Ok(value) = obj.getattr(&*text) else {
            continue;
        };
        if value.is_callable() {
            continue;
        }
        result.set_item(name, value)?;
    }
    Ok(result)
}

/// Starts one sequence: an empty one is finished outright, a non-empty one
/// hands its first element back for conversion with the rest parked in the
/// returned iterator. `class_name` is the subclass name the finished
/// container carries (`None` for the exact base type) — see the module
/// doc's "Subclasses" section.
fn seq_step(mut iter: SeqIter<'_>, class_name: Option<Arc<str>>) -> Step<'_> {
    match iter.next() {
        None => Step::Done(iter.build(Vec::new(), class_name)),
        Some(first) => Step::Seq {
            iter,
            first,
            class_name,
        },
    }
}

/// What [`advance_frame`] returns: either the frame needs its next child
/// converted before it can finish, or it's fully built.
enum Advance<'py> {
    NeedsChild {
        pending: Pending<'py>,
        frame: Frame<'py>,
    },
    Done(CValue),
}

/// The next object [`to_value`]'s loop must convert: the object itself, the
/// depth it sits at, and whether it is a set member (which restricts the
/// types [`classify`] accepts for it).
type Pending<'py> = (Bound<'py, PyAny>, usize, bool);

/// Attaches a just-finished child `value` into `frame` and figures out what
/// happens next: either `frame` has another child to convert
/// (`Advance::NeedsChild`, with `path` extended for it), or `frame` is fully
/// built (`Advance::Done`). `path` must already have had the finished child's
/// own segment popped by the caller — see [`to_value`].
///
/// On a bad dict key mid-frame the error just propagates: `built` (its
/// completed entries, possibly including a deep subtree) drops here
/// naturally, and the compact [`CValue`]'s iterative `Drop` cannot overflow
/// the calling thread — no worker hand-off is needed, unlike the old
/// `serde_json::Value` path.
fn advance_frame<'py>(
    frame: Frame<'py>,
    value: CValue,
    path: &mut Vec<PathSegment>,
    builder: &mut Builder,
) -> PyResult<Advance<'py>> {
    match frame {
        Frame::Seq {
            mut remaining,
            mut built,
            restricted,
            class_name,
        } => {
            built.push(value);

            Ok(match remaining.next() {
                Some(next_item) => {
                    // The just-finished child was appended above, so the next
                    // child's index is the new length, and its depth is the
                    // path length once its segment is pushed.
                    path.push(child_segment(remaining.holds_set_members(), built.len()));
                    Advance::NeedsChild {
                        pending: (next_item, path.len(), restricted),
                        frame: Frame::Seq {
                            remaining,
                            built,
                            restricted,
                            class_name,
                        },
                    }
                }
                None => Advance::Done(remaining.build(built, class_name)),
            })
        }
        Frame::Dict {
            mut remaining,
            mut built,
            current_key,
            class_name,
            kind,
        } => {
            built.push((current_key, value));

            match next_dict_entry(&mut remaining, path, builder)? {
                Some((key, next_value)) => {
                    path.push(entry_path_segment(kind, &key));
                    Ok(Advance::NeedsChild {
                        // The child's depth is the path length once its key
                        // segment is pushed above.
                        pending: (next_value, path.len(), false),
                        frame: Frame::Dict {
                            remaining,
                            built,
                            current_key: key,
                            class_name,
                            kind,
                        },
                    })
                }
                None => Ok(Advance::Done(finish_object(
                    builder, built, class_name, kind,
                ))),
            }
        }
    }
}

/// The path segment one `dict` entry or object attribute contributes, chosen
/// by [`ObjectKind`]: a subscript for a `dict`, a dotted attribute for a
/// custom object. Mirrors `onix_core`'s own `object_diff` segment choice, so
/// a conversion-error path names an attribute the same way a diff finding
/// does.
fn entry_path_segment(kind: ObjectKind, key: &ObjectKey) -> PathSegment {
    match kind {
        ObjectKind::Dict => key_path_segment(key),
        ObjectKind::CustomObject => attribute_path_segment(key),
    }
}

/// Builds the finished [`CValue`] for a `Frame::Dict`, as the kind selects: a
/// `dict` (carrying its optional subclass name) or a custom object (carrying
/// its class name).
fn finish_object(
    builder: &mut Builder,
    built: Vec<(ObjectKey, CValue)>,
    class_name: Option<Arc<str>>,
    kind: ObjectKind,
) -> CValue {
    match kind {
        ObjectKind::Dict => builder.object_with_keys_and_type_name(built, class_name),
        ObjectKind::CustomObject => builder.custom_object(
            built,
            class_name.expect("a custom object always carries its class name"),
        ),
    }
}

/// Converts a Python object into an [`onix_core::Value`], recursing at most
/// `max_depth` levels deep — see the module doc for the full conversion table
/// and why this walk uses an explicit stack instead of native recursion.
///
/// The second return value is whether this walk ever built a `Str::Wtf8`/
/// `Key`-with-a-surrogate (see the module doc's "Lone surrogate code
/// points" section) — a byproduct of this walk's own string handling, not a
/// second pass over the result: [`crate::deepdiff::DeepDiff::new`] ORs the
/// two sides' flags together and caches the result so
/// [`crate::guard::serialize_value`] can skip
/// [`onix_core::value::contains_wtf8`]'s own tree walk for the overwhelming
/// common case (no surrogate anywhere), which otherwise doubles that
/// function's cost on every `to_json()` call regardless of whether this
/// feature is in use.
///
/// # Errors
///
/// Returns a Python `ValueError`/[`MaxDepthError`] or `TypeError` per the
/// module doc's conversion table.
pub(crate) fn to_value(obj: &Bound<'_, PyAny>, max_depth: usize) -> PyResult<(CValue, bool)> {
    let mut builder = Builder::new();
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut path: Vec<PathSegment> = Vec::new();
    let mut pending: Option<Pending<'_>> = Some((obj.clone(), 0, false));
    let mut finished: Option<CValue> = None;
    let mut saw_wtf8 = false;

    // On any error break, `stack` (and its parked, possibly deep entries)
    // drops here at function return. Every `CValue` has an iterative `Drop`,
    // so that teardown is stack-safe on the calling thread at any depth — the
    // conversion never needs a sized-worker drop path.
    loop {
        if let Some((current, depth, set_member)) = pending.take() {
            if depth > max_depth {
                return Err(max_depth_error(max_depth, &path));
            }

            match classify(&current, &path, &mut builder, set_member)? {
                Step::Done(value) => {
                    if matches!(&value, CValue::Str(CStr::Wtf8(_))) {
                        saw_wtf8 = true;
                    }
                    finished = Some(value);
                }
                Step::Seq {
                    iter,
                    first,
                    class_name,
                } => {
                    let child_depth = depth + 1;
                    // Transitive: a set's members are restricted, and so is
                    // everything inside a container that is itself restricted.
                    let restricted = set_member || iter.holds_set_members();
                    path.push(child_segment(iter.holds_set_members(), 0));
                    // `iter` has already yielded `first`, so the finished
                    // sequence will hold `iter.len() + 1` elements — pre-size
                    // for exactly that (both sequence iterators are
                    // `ExactSizeIterator`).
                    let capacity = iter.len().saturating_add(1);
                    stack.push(Frame::Seq {
                        remaining: iter,
                        built: Vec::with_capacity(capacity),
                        restricted,
                        class_name,
                    });
                    pending = Some((first, child_depth, restricted));
                    continue;
                }
                Step::Dict {
                    iter,
                    first_key,
                    first_value,
                    class_name,
                    kind,
                } => {
                    let child_depth = depth + 1;
                    if matches!(first_key, ObjectKey::Str(CKey::Wtf8(_))) {
                        saw_wtf8 = true;
                    }
                    path.push(entry_path_segment(kind, &first_key));
                    let capacity = iter.len().saturating_add(1);
                    stack.push(Frame::Dict {
                        remaining: iter,
                        built: Vec::with_capacity(capacity),
                        current_key: first_key,
                        class_name,
                        kind,
                    });
                    pending = Some((first_value, child_depth, false));
                    continue;
                }
            }
        }

        let value = finished.take().expect(
            "loop invariant: every iteration either sets `pending` (and `continue`s) or `finished`",
        );

        match stack.pop() {
            None => return Ok((value, saw_wtf8)),
            Some(frame) => {
                path.pop();

                match advance_frame(frame, value, &mut path, &mut builder)? {
                    Advance::NeedsChild {
                        pending: next_pending,
                        frame,
                    } => {
                        if let Frame::Dict { current_key, .. } = &frame
                            && matches!(current_key, ObjectKey::Str(CKey::Wtf8(_)))
                        {
                            saw_wtf8 = true;
                        }
                        stack.push(frame);
                        pending = Some(next_pending);
                    }
                    Advance::Done(v) => finished = Some(v),
                }
            }
        }
    }
}

/// The path segment for one sequence element.
///
/// A set member has no subscript at all — `DeepDiff` names one only by its
/// *rendered value*, which an object that fails to convert never gets — so
/// reporting a positional index there would be inventing a path the tool
/// cannot resolve (`root[0][2]` where `root[0]` is a set). This placeholder
/// keeps the depth count honest and reports as
/// `root['a'][<set member>]`, including for a failure further inside the
/// member (`root['a'][<set member>][1]`).
fn child_segment(set_member: bool, index: usize) -> PathSegment {
    if set_member {
        PathSegment::SetItem("<set member>".to_string())
    } else {
        PathSegment::Index(index)
    }
}

/// Pulls the next `(key, value)` pair out of a dict iterator, classifying
/// and validating the key — shared by [`to_value`]'s initial descent into a
/// dict and its `Frame::Dict` advance step, so the validation (and its error
/// message) is written exactly once.
///
/// `dict_path` is the path to the *dict itself* (not the entry) — that is
/// deliberately what a bad key's error reports, since a key that fails
/// classification has no path segment of its own to report. `builder`
/// interns a `str` key exactly as every other object key in this walk is
/// interned; a non-`str` key needs no interning (see the module doc's
/// key-type table).
fn next_dict_entry<'py>(
    iter: &mut BoundDictIterator<'py>,
    dict_path: &[PathSegment],
    builder: &mut Builder,
) -> PyResult<Option<(ObjectKey, Bound<'py, PyAny>)>> {
    let Some((key, value)) = iter.next() else {
        return Ok(None);
    };

    let key = classify_dict_key(&key, dict_path, builder)?;

    Ok(Some((key, value)))
}

/// Classifies one Python dict key into an [`ObjectKey`] — the `str` case
/// (interned, as always — including a lone-surrogate one, see
/// [`pystring_to_cstr`]) plus every other key `DeepDiff` also accepts:
/// `None`, `bool`, `int`, `float`, `datetime`, `date`, or a `tuple` of those
/// (never a nested `tuple` — see the module doc's key-type table and
/// [`classify_key_scalar`], which this delegates every non-`tuple` case to),
/// or a subclass of `tuple`/`datetime`/`date` (including a `namedtuple`).
/// `DeepDiff`'s own dict-key matching is plain Python `==`/`hash`, which
/// never consults `type(obj)`, so — unlike a *value*, which carries its
/// class name into a `type_changes` entry (see the module doc's
/// "Subclasses" section) — a key subclass instance is classified as its
/// exact base type with no name tracked at all: [`ObjectKey`] has no
/// class-name field. A subclass key matches by its base type's *value*;
/// an overridden `__eq__`/`__hash__` is not consulted (that is custom-object
/// territory, out of this MVP's scope), so a key subclass whose equality
/// or hash disagrees with its base type's is a documented nuance, not a
/// bug — see `tests/golden/README.md`'s subclass section.
fn classify_dict_key(
    key: &Bound<'_, PyAny>,
    dict_path: &[PathSegment],
    builder: &mut Builder,
) -> PyResult<ObjectKey> {
    if let Ok(s) = key.cast::<PyString>() {
        let s = pystring_to_cstr(s)?;
        return Ok(ObjectKey::Str(builder.intern_key(s)));
    }

    // Non-exact (`cast`, not `cast_exact`): a `tuple` subclass key,
    // including a `namedtuple`, classifies the same way its base type does
    // — see this function's own doc for why no class name is tracked.
    if let Ok(tuple) = key.cast::<PyTuple>() {
        let mut items = Vec::with_capacity(tuple.len());
        for item in tuple.iter() {
            items.push(classify_key_scalar(&item, dict_path)?);
        }
        return Ok(ObjectKey::Other(Box::new(CValue::Tuple(
            items.into_boxed_slice().into(),
        ))));
    }

    Ok(ObjectKey::Other(Box::new(classify_key_scalar(
        key, dict_path,
    )?)))
}

/// Classifies one Python object as a dict-key **scalar**: every key type
/// [`classify_dict_key`] accepts except `str` (interned separately, at the
/// top level only) and `tuple` (split out there, since a tuple key may not
/// itself nest one — see the module doc). Shared between a bare key and
/// each element of a `tuple` key.
fn classify_key_scalar(obj: &Bound<'_, PyAny>, dict_path: &[PathSegment]) -> PyResult<CValue> {
    if obj.is_none() {
        return Ok(CValue::Null);
    }

    // `bool` before `int`: see the module doc.
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(CValue::Bool(b.is_true()));
    }

    if let Ok(i) = obj.cast::<PyInt>() {
        return int_to_value(i, dict_path);
    }

    if let Ok(f) = obj.cast::<PyFloat>() {
        return Ok(float_to_value(f.value()));
    }

    if let Ok(s) = obj.cast::<PyString>() {
        return Ok(CValue::Str(pystring_to_cstr(s)?));
    }

    // Non-exact, and `datetime` before `date` (every `datetime` is also a
    // `date` at the C level — see the module doc): a `datetime`/`date`
    // subclass key classifies as its base type with no class name tracked,
    // see [`classify_dict_key`]'s own doc for why.
    if obj.cast::<PyDateTime>().is_ok() {
        return datetime_to_value(obj, dict_path, None);
    }

    if obj.cast::<PyDate>().is_ok() {
        return Ok(CValue::Date(date_fields(obj, dict_path)?.into()));
    }

    Err(PyTypeError::new_err(format!(
        "unsupported type for a dict key: {} at {}; a dict key must be \
         None/bool/int/float/str/datetime/date, a tuple of those, or a \
         tuple/datetime/date subclass (including a namedtuple)",
        type_name(obj),
        render_path(dict_path),
    )))
}

/// Converts a Python `str` into the crate's compact [`CStr`]: the fast,
/// zero-copy UTF-8 path for the overwhelming common case, falling back only
/// when the string contains a lone (unpaired) surrogate code point — legal
/// in Python, not encodable as UTF-8 — to `str.encode('utf-8',
/// 'surrogatepass')`, the `CPython` idiom for round-tripping exactly that
/// content: WTF-8 bytes (see [`CStr`]'s own doc), with each surrogate in
/// the same three-byte form that encoding produces. Shared by a scalar
/// `str` value and a dict key, the two places a Python `str` enters the
/// value model.
fn pystring_to_cstr(s: &Bound<'_, PyString>) -> PyResult<CStr> {
    if let Ok(cow) = s.to_cow() {
        return Ok(CStr::Utf8(cow.into_owned().into_boxed_str()));
    }

    let bytes: Vec<u8> = s
        .call_method1("encode", ("utf-8", "surrogatepass"))?
        .extract()?;
    Ok(CStr::Wtf8(bytes.into_boxed_slice()))
}

/// [`pystring_to_cstr`]'s inverse: rebuilds a Python `str` from WTF-8 bytes
/// (a [`CStr`] or a [`CKey`]'s content — either offers `.as_bytes()`). The
/// fast path is the overwhelming common case, valid UTF-8, built directly;
/// only a `Str::Wtf8`/`Key::Wtf8` (bytes that fail `str::from_utf8`, holding
/// a lone surrogate) takes the slower `bytes.decode('utf-8',
/// 'surrogatepass')` round trip, the exact `CPython` idiom that reverses
/// `pystring_to_cstr`'s `encode`.
fn wtf8_to_pyobject(py: Python<'_>, bytes: &[u8]) -> PyResult<Py<PyAny>> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.into_py_any(py);
    }

    PyBytes::new(py, bytes)
        .call_method1("decode", ("utf-8", "surrogatepass"))?
        .into_py_any(py)
}

/// Reads a `date`'s (or a `datetime`'s) `year`/`month`/`day` attributes.
///
/// Attributes rather than `PyO3`'s `PyDateAccess` trait: that trait wraps the
/// `PyDateTime_GET_*` C macros, which the limited API this extension builds
/// against (`abi3-py39`) does not expose, so it is not compiled at all under
/// that feature.
fn date_fields(obj: &Bound<'_, PyAny>, path: &[PathSegment]) -> PyResult<CDate> {
    let year: i32 = obj.getattr("year")?.extract()?;
    let month: u8 = obj.getattr("month")?.extract()?;
    let day: u8 = obj.getattr("day")?.extract()?;

    CDate::new(year, month, day).ok_or_else(|| out_of_range_error("date", path))
}

/// Converts a `datetime.datetime` (exact or a subclass) — see
/// [`date_fields`] for why the fields are read as attributes. `class_name`
/// is the subclass name to attach (`None` for the exact base type) — see
/// the module doc's "Subclasses" section.
fn datetime_to_value(
    obj: &Bound<'_, PyAny>,
    path: &[PathSegment],
    class_name: Option<Arc<str>>,
) -> PyResult<CValue> {
    let date = date_fields(obj, path)?;
    let hour: u8 = obj.getattr("hour")?.extract()?;
    let minute: u8 = obj.getattr("minute")?.extract()?;
    let second: u8 = obj.getattr("second")?.extract()?;
    let microsecond: u32 = obj.getattr("microsecond")?.extract()?;
    let offset = utc_offset_seconds(obj, "datetime", path)?;

    CDateTime::new(date, hour, minute, second, microsecond, offset)
        .map(|dt| CValue::DateTime(Typed::with_class_name(dt, class_name)))
        .ok_or_else(|| out_of_range_error("datetime", path))
}

/// Converts a `datetime.time` (exact or a subclass) — the same field-reading
/// pattern as [`datetime_to_value`], minus the date. `class_name` is the
/// subclass name to attach (`None` for the exact base type).
fn time_to_value(
    obj: &Bound<'_, PyAny>,
    path: &[PathSegment],
    class_name: Option<Arc<str>>,
) -> PyResult<CValue> {
    let hour: u8 = obj.getattr("hour")?.extract()?;
    let minute: u8 = obj.getattr("minute")?.extract()?;
    let second: u8 = obj.getattr("second")?.extract()?;
    let microsecond: u32 = obj.getattr("microsecond")?.extract()?;
    let offset = utc_offset_seconds(obj, "time", path)?;

    CTime::new(hour, minute, second, microsecond, offset)
        .map(|t| CValue::Time(Typed::with_class_name(t, class_name)))
        .ok_or_else(|| out_of_range_error("time", path))
}

/// Converts a `datetime.timedelta` (exact or a subclass), reading its own
/// already-normalized `days`/`seconds`/`microseconds` attributes — the same
/// three fields [`utc_offset_seconds`] reads off the `timedelta` a
/// `utcoffset()` call returns. `class_name` is the subclass name to attach
/// (`None` for the exact base type).
fn timedelta_to_value(
    obj: &Bound<'_, PyAny>,
    path: &[PathSegment],
    class_name: Option<Arc<str>>,
) -> PyResult<CValue> {
    let days: i64 = obj.getattr("days")?.extract()?;
    let seconds: i64 = obj.getattr("seconds")?.extract()?;
    let microseconds: i64 = obj.getattr("microseconds")?.extract()?;

    CTimeDelta::new(days, seconds, microseconds)
        .map(|td| CValue::TimeDelta(Typed::with_class_name(td, class_name)))
        .ok_or_else(|| out_of_range_error("timedelta", path))
}

/// The datetime's/time's fixed UTC offset in whole seconds, or `None` when
/// it is naive.
///
/// Asks the object itself (`utcoffset()`) rather than inspecting `tzinfo`,
/// so any `tzinfo` implementation — `timezone`, `zoneinfo`, `pytz` — reports
/// the offset in force at *this* moment, which is what
/// `datetime_normalize`'s own `astimezone` would use. `type_name` names the
/// caller's own type (`"datetime"`/`"time"`) for the two error messages.
fn utc_offset_seconds(
    obj: &Bound<'_, PyAny>,
    type_name: &str,
    path: &[PathSegment],
) -> PyResult<Option<i32>> {
    let offset = obj.call_method0("utcoffset")?;

    if offset.is_none() {
        return Ok(None);
    }

    let days: i64 = offset.getattr("days")?.extract()?;
    let seconds: i64 = offset.getattr("seconds")?.extract()?;
    let microseconds: i64 = offset.getattr("microseconds")?.extract()?;

    if microseconds != 0 {
        return Err(PyValueError::new_err(format!(
            "a tzinfo whose utcoffset() is not a whole number of seconds is not supported \
             (at {}); onix stores a {type_name}'s UTC offset in seconds",
            render_path(path),
        )));
    }

    i32::try_from(days * 86_400 + seconds)
        .map(Some)
        .map_err(|_| out_of_range_error(type_name, path))
}

/// A `date`/`datetime`/`time`/`timedelta` whose fields the compact value
/// model rejects. Python itself enforces every one of those bounds on a real
/// object of any of the four types, so this is reachable only through a
/// custom `tzinfo` returning an out-of-range offset (`datetime`/`time`) —
/// never for `date`/`timedelta`, whose own constructors already enforce
/// every bound this crate's own `new` re-checks.
fn out_of_range_error(type_name: &str, path: &[PathSegment]) -> PyErr {
    PyValueError::new_err(format!(
        "{type_name} at {} is out of range for onix's internal value model",
        render_path(path),
    ))
}

fn int_to_value(i: &Bound<'_, PyInt>, path: &[PathSegment]) -> PyResult<CValue> {
    if let Ok(v) = i.extract::<i64>() {
        return Ok(CValue::Number(CNumber::from_i64(v)));
    }

    if let Ok(v) = i.extract::<u64>() {
        return Ok(CValue::Number(CNumber::from_u64(v)));
    }

    // Beyond i64/u64: read the exact value (see `exact_big_int`).
    // `CNumber::from_bigint` narrows it back to a fast arm if it turns out to
    // fit one, so only a genuinely large integer keeps the arbitrary-precision
    // representation.
    let big = exact_big_int(i).map_err(|err| {
        PyValueError::new_err(format!(
            "could not read a large integer at {}: {err}",
            render_path(path),
        ))
    })?;

    Ok(CValue::Number(CNumber::from_bigint(big)))
}

/// Reads a Python `int`'s exact value as a [`BigInt`] through `int`'s own
/// **unbound** `bit_length`/`to_bytes`, never the object's own methods.
///
/// The fast `i64`/`u64` path above reads the value straight from `PyLong`'s
/// storage; a subclass instance keeps that true value there, but can override
/// `__str__`/`__index__`/`to_bytes` to report a *different* one, so reading a
/// large value through any of those (as an earlier `str(int)` version did)
/// would let a subclass control what onix compares — a false match, a
/// fabricated value, or a flipped sign. Calling the base `int` type's own
/// slots on the instance bypasses every override and reads the same value the
/// fast path and `DeepDiff` do. It also sidesteps `CPython`'s `int`->`str`
/// digit cap (`sys.set_int_max_str_digits`), so an integer of any length
/// converts.
fn exact_big_int(i: &Bound<'_, PyInt>) -> PyResult<BigInt> {
    let (int_type, kwargs) = int_type_and_signed_kwargs(i.py())?;
    let bit_length: usize = int_type.getattr("bit_length")?.call1((i,))?.extract()?;
    // One extra byte so the two's-complement sign bit always has room.
    let byte_len = bit_length / 8 + 1;
    let bytes: Vec<u8> = int_type
        .getattr("to_bytes")?
        .call((i, byte_len, "little"), Some(&kwargs))?
        .extract()?;

    Ok(BigInt::from_signed_bytes_le(&bytes))
}

/// The base `int` type object and a `{"signed": True}` kwargs dict — the shared
/// pieces of the byte-based big-int read ([`exact_big_int`]) and write
/// ([`number_to_pyobject`]), which both call `int.to_bytes`/`int.from_bytes`
/// **unbound** on the base type so a subclass override cannot intercept them.
fn int_type_and_signed_kwargs(py: Python<'_>) -> PyResult<(Bound<'_, PyType>, Bound<'_, PyDict>)> {
    Ok((py.get_type::<PyInt>(), [("signed", true)].into_py_dict(py)?))
}

/// Every `float` converts, including `NaN`/`Infinity`/`-Infinity`: unlike
/// JSON, a Python `float` has no finiteness restriction, and
/// [`CNumber::from_f64`] stores whichever bits it is given.
fn float_to_value(f: f64) -> CValue {
    CValue::Number(CNumber::from_f64(f))
}

fn max_depth_error(max_depth: usize, path: &[PathSegment]) -> PyErr {
    MaxDepthError::new_err(format!(
        "python object nesting exceeds the configured max_depth ({max_depth}) while converting \
         to onix's internal value model, at {}",
        render_path(path),
    ))
}

/// The error for an object that reached a set member, or anything nested
/// inside one, but is not a type this MVP allows there — see [`classify`]'s
/// `set_member` parameter.
fn unhashable_member_error(obj: &Bound<'_, PyAny>, path: &[PathSegment]) -> PyErr {
    PyTypeError::new_err(format!(
        "unsupported type for a set member: {} at {}; a set member must be \
         None/bool/int/float/str/tuple/frozenset/datetime/date/time/timedelta, or a \
         datetime/date/time/timedelta subclass (a tuple/frozenset subclass, including a \
         namedtuple, is not accepted as a set member)",
        type_name(obj),
        render_path(path),
    ))
}

fn type_name(obj: &Bound<'_, PyAny>) -> String {
    obj.get_type()
        .name()
        .map_or_else(|_| "<unknown type>".to_string(), |name| name.to_string())
}

/// `type_name`, as the `Arc<str>` [`Typed`]/[`SetItems`]/`onix_core::value::Object`
/// carry for a subclass instance — see the module doc's "Subclasses" section.
fn class_name(obj: &Bound<'_, PyAny>) -> Arc<str> {
    Arc::from(type_name(obj))
}

/// Which Python sequence [`value_to_pyobject`] rebuilds a run of items into
/// — the report side of [`SeqIter`], where both shapes carry the same
/// `&[CValue]` and only the finished object differs.
#[derive(Clone, Copy)]
enum SeqKind {
    List,
    Tuple,
    Set,
    FrozenSet,
}

impl SeqKind {
    fn build(self, py: Python<'_>, items: Vec<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match self {
            SeqKind::List => items.into_py_any(py),
            SeqKind::Tuple => PyTuple::new(py, items)?.into_py_any(py),
            // Every member of a `Value::Set`/`Value::FrozenSet` came through
            // `classify`'s transitive set-member restriction, which accepts
            // only hashable kinds all the way down (see the module doc), so
            // `PySet::new` cannot fail on one.
            SeqKind::Set => PySet::new(py, items)?.into_py_any(py),
            SeqKind::FrozenSet => PyFrozenSet::new(py, items)?.into_py_any(py),
        }
    }
}

/// One in-progress container on [`value_to_pyobject`]'s explicit work-stack —
/// the same technique as [`Frame`]/[`to_value`], applied in the opposite
/// direction (report `Value` -> Python object) so this direction is equally
/// immune to the native-stack-overflow class on a `Value` tree deep enough to
/// matter.
enum RenderFrame<'py, 'v> {
    Seq {
        kind: SeqKind,
        remaining: std::slice::Iter<'v, CValue>,
        built: Vec<Py<PyAny>>,
    },
    Object {
        remaining: Entries<'v>,
        built: Bound<'py, PyDict>,
        current_key: &'v ObjectKey,
    },
}

/// Converts a rendered report [`onix_core::Value`] (a
/// [`crate::deepdiff::DeepDiff`] report, or one of its nested values) into a
/// native Python object — the parsed form
/// [`crate::deepdiff::DeepDiff::to_dict`] returns.
///
/// The report is rendered as the crate's own value model
/// ([`onix_core::Report::to_value`]) rather than as JSON, which is what lets
/// this hand back a real `tuple` wherever the diff found one: JSON has no
/// tuple, so a report round-tripped through `serde_json` could only ever
/// produce the list `to_json()` shows. The only failure this can report is a
/// Python-side allocation failure building the objects themselves.
/// It walks via an explicit stack (see [`RenderFrame`]), not native
/// recursion, so a deep report can never overflow the native stack
/// converting it back.
pub(crate) fn value_to_pyobject(py: Python<'_>, value: &CValue) -> PyResult<Py<PyAny>> {
    let mut stack: Vec<RenderFrame<'_, '_>> = Vec::new();
    let mut pending: Option<&CValue> = Some(value);
    let mut finished: Option<Py<PyAny>> = None;

    loop {
        if let Some(current) = pending.take() {
            let step = match current {
                CValue::Null => RenderStep::Done(py.None()),
                CValue::Bool(b) => RenderStep::Done(b.into_py_any(py)?),
                CValue::Number(n) => RenderStep::Done(number_to_pyobject(py, n)?),
                CValue::Str(s) => RenderStep::Done(wtf8_to_pyobject(py, s.as_bytes())?),
                // Renders back as the plain base type, never the original
                // subclass instance (there is nothing left to reconstruct
                // one from once the value has passed through the compact
                // model) — the same simplification the module doc's
                // "Datetimes and dates" section already documents for a
                // `zoneinfo`/`pytz` `tzinfo`.
                CValue::DateTime(value) => {
                    RenderStep::Done(datetime_to_pyobject(py, value.value())?)
                }
                CValue::Date(value) => RenderStep::Done(date_to_pyobject(py, value.value())?),
                CValue::Time(value) => RenderStep::Done(time_to_pyobject(py, value.value())?),
                CValue::TimeDelta(value) => {
                    RenderStep::Done(timedelta_to_pyobject(py, value.value())?)
                }
                CValue::Array(items) => {
                    start_sequence(py, SeqKind::List, items, &mut stack, &mut pending)?
                }
                CValue::Tuple(items) => {
                    start_sequence(py, SeqKind::Tuple, items, &mut stack, &mut pending)?
                }
                CValue::Set(items) => {
                    start_sequence(py, SeqKind::Set, items, &mut stack, &mut pending)?
                }
                CValue::FrozenSet(items) => {
                    start_sequence(py, SeqKind::FrozenSet, items, &mut stack, &mut pending)?
                }
                CValue::Object(map) => {
                    let mut iter = map.iter();

                    match iter.next() {
                        None => RenderStep::Done(PyDict::new(py).into_py_any(py)?),
                        Some((key, first_value)) => {
                            stack.push(RenderFrame::Object {
                                remaining: iter,
                                built: PyDict::new(py),
                                current_key: key,
                            });
                            pending = Some(first_value);
                            RenderStep::Descend
                        }
                    }
                }
            };

            match step {
                RenderStep::Descend => continue,
                RenderStep::Done(value) => finished = Some(value),
            }
        }

        let rendered = finished.take().expect(
            "loop invariant: every iteration either sets `pending` (and `continue`s) or `finished`",
        );

        match stack.pop() {
            None => return Ok(rendered),
            Some(RenderFrame::Seq {
                kind,
                mut remaining,
                mut built,
            }) => {
                built.push(rendered);

                match remaining.next() {
                    Some(next_item) => {
                        pending = Some(next_item);
                        stack.push(RenderFrame::Seq {
                            kind,
                            remaining,
                            built,
                        });
                    }
                    None => finished = Some(kind.build(py, built)?),
                }
            }
            Some(RenderFrame::Object {
                mut remaining,
                built,
                current_key,
            }) => {
                built.set_item(object_key_to_pyobject(py, current_key)?, rendered)?;

                match remaining.next() {
                    Some((key, next_value)) => {
                        pending = Some(next_value);
                        stack.push(RenderFrame::Object {
                            remaining,
                            built,
                            current_key: key,
                        });
                    }
                    None => finished = Some(built.into_py_any(py)?),
                }
            }
        }
    }
}

/// Rebuilds one [`ObjectKey`] as the Python object [`value_to_pyobject`]
/// hands back as a dict key: a `str` key via [`wtf8_to_pyobject`] (WTF-8-aware,
/// so a lone surrogate in the key round-trips exactly), any other key by
/// recursively rendering its wrapped [`CValue`] the same way any other
/// value in the tree renders. That recursive call is a bounded native-stack
/// use, not the deep-nesting hazard [`value_to_pyobject`]'s own iterative
/// design exists to close: a dict key's `Value` is at most a `tuple` of
/// scalars (`onix-py`'s conversion enforces this — see the module doc), so
/// the recursion is at most two levels deep regardless of how deep the
/// containing report is.
fn object_key_to_pyobject(py: Python<'_>, key: &ObjectKey) -> PyResult<Py<PyAny>> {
    match key {
        ObjectKey::Str(s) => wtf8_to_pyobject(py, s.as_bytes()),
        ObjectKey::Other(value) => value_to_pyobject(py, value),
    }
}

/// Rebuilds a `datetime.date`.
fn date_to_pyobject(py: Python<'_>, value: CDate) -> PyResult<Py<PyAny>> {
    PyDate::new(py, value.year(), value.month(), value.day())?.into_py_any(py)
}

/// Rebuilds a `datetime.datetime`, aware values carrying a fixed-offset
/// `datetime.timezone` (a zero offset is Python's own `timezone.utc`
/// singleton) — see the module doc's note on the `zoneinfo` round trip.
fn datetime_to_pyobject(py: Python<'_>, value: CDateTime) -> PyResult<Py<PyAny>> {
    let tzinfo = value
        .utc_offset_seconds()
        .map(|offset| PyTzInfo::fixed_offset(py, PyDelta::new(py, 0, offset, 0, true)?))
        .transpose()?;
    let date = value.date();

    PyDateTime::new(
        py,
        date.year(),
        date.month(),
        date.day(),
        value.hour(),
        value.minute(),
        value.second(),
        value.microsecond(),
        tzinfo.as_ref(),
    )?
    .into_py_any(py)
}

/// Rebuilds a `datetime.time`, aware values carrying a fixed-offset
/// `datetime.timezone` — [`datetime_to_pyobject`]'s twin minus the date.
fn time_to_pyobject(py: Python<'_>, value: CTime) -> PyResult<Py<PyAny>> {
    let tzinfo = value
        .utc_offset_seconds()
        .map(|offset| PyTzInfo::fixed_offset(py, PyDelta::new(py, 0, offset, 0, true)?))
        .transpose()?;

    PyTime::new(
        py,
        value.hour(),
        value.minute(),
        value.second(),
        value.microsecond(),
        tzinfo.as_ref(),
    )?
    .into_py_any(py)
}

/// Rebuilds a `datetime.timedelta` from its own normalized
/// `(days, seconds, microseconds)`.
fn timedelta_to_pyobject(py: Python<'_>, value: CTimeDelta) -> PyResult<Py<PyAny>> {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "onix_core::datetime::TimeDelta's own days/seconds/microseconds are already \
                  bounded to Python's own timedelta range, which fits i32"
    )]
    PyDelta::new(
        py,
        value.days() as i32,
        value.seconds() as i32,
        value.microseconds() as i32,
        true,
    )?
    .into_py_any(py)
}

/// One step of [`value_to_pyobject`]'s loop: a value finished outright, or
/// a container whose first child was just made pending.
enum RenderStep {
    Done(Py<PyAny>),
    Descend,
}

/// Starts one sequence-shaped value: an empty one is finished outright, a
/// non-empty one parks a [`RenderFrame::Seq`] and makes its first item
/// pending. Shared by all four sequence shapes, which differ only in the
/// Python object [`SeqKind::build`] finally produces.
fn start_sequence<'py, 'v>(
    py: Python<'py>,
    kind: SeqKind,
    items: &'v [CValue],
    stack: &mut Vec<RenderFrame<'py, 'v>>,
    pending: &mut Option<&'v CValue>,
) -> PyResult<RenderStep> {
    let mut remaining = items.iter();

    let Some(first) = remaining.next() else {
        return Ok(RenderStep::Done(kind.build(py, Vec::new())?));
    };

    let capacity = remaining.len().saturating_add(1);
    stack.push(RenderFrame::Seq {
        kind,
        remaining,
        built: Vec::with_capacity(capacity),
    });
    *pending = Some(first);
    Ok(RenderStep::Descend)
}

fn number_to_pyobject(py: Python<'_>, n: &CNumber) -> PyResult<Py<PyAny>> {
    if !n.is_f64() {
        if let Some(v) = n.as_i64() {
            return v.into_py_any(py);
        }

        if let Some(v) = n.as_u64() {
            return v.into_py_any(py);
        }

        // A non-float that fits neither is an arbitrary-precision integer;
        // rebuild a Python `int` from its exact two's-complement bytes via
        // `int.from_bytes`, the inverse of `exact_big_int`'s read. Bytes, not
        // decimal text, so this never trips `CPython`'s `int`<->`str` digit
        // cap that a several-thousand-digit integer would otherwise hit.
        if let Some(big) = n.as_big() {
            let (int_type, kwargs) = int_type_and_signed_kwargs(py)?;
            let bytes = big.to_signed_bytes_le();
            return int_type
                .getattr("from_bytes")?
                .call((PyBytes::new(py, &bytes), "little"), Some(&kwargs))?
                .into_py_any(py);
        }
    }

    n.as_f64()
        .expect("a non-integer Number is always an f64")
        .into_py_any(py)
}
