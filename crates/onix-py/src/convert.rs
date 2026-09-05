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
//! | `int` | `Number` | must fit in `i64` or `u64`; see below |
//! | `float` | `Number` | must be finite; see below |
//! | `str` | `Str` | must be encodable as UTF-8; see below |
//! | `dict` (`str` keys only), or a subclass | `Object` | keys interned across the whole walk |
//! | `list`, or a subclass | `Array` | |
//! | `tuple`, or a subclass (including a `namedtuple`) | `Tuple` | diffed positionally even for a `namedtuple`, see below |
//! | `set`, or a subclass | `Set` | members restricted, see below |
//! | `frozenset`, or a subclass | `FrozenSet` | members restricted, see below |
//! | `datetime.datetime`, or a subclass (e.g. pandas `Timestamp`) | `DateTime` | naive or any `tzinfo`, see below |
//! | `datetime.date`, or a subclass | `Date` | |
//!
//! A subclass instance converts and compares exactly like the base type —
//! see the "Subclasses" section below — except as a `set`/`frozenset`
//! *member*, where only the exact `tuple`/`frozenset`/`datetime`/`date` type
//! is accepted (a `list`/`dict`/`set` subclass, or a `tuple`/`frozenset`
//! subclass including a `namedtuple`, reaching a set member is refused the
//! same way any other unsupported type is).
//!
//! Every other type raises a Python exception instead of converting:
//!
//! - An `int` outside `i64::MIN..=u64::MAX` raises [`PyValueError`]:
//!   arbitrary-precision integers are not supported in this MVP (real
//!   `DeepDiff` supports them natively).
//! - A `NaN` or infinite `float` raises [`PyValueError`] (JSON has no
//!   representation for either).
//! - A `str` containing a lone (unpaired) surrogate code point (e.g.
//!   `"\udc80"`) raises [`PyValueError`] naming the exact path: it has no
//!   UTF-8 encoding. See `tests/golden/README.md` for why this diverges from
//!   real `DeepDiff`.
//! - A `dict` key that is not a `str` raises [`PyTypeError`] naming the
//!   key's type and the path to the dict containing it; a `str` key with a
//!   lone surrogate raises [`PyValueError`] the same way, naming the dict's
//!   path (the key itself has no path segment of its own).
//! - A `tzinfo` whose `utcoffset()` is not a whole number of seconds raises
//!   [`PyValueError`]: the value model carries an offset in seconds.
//! - A `set`/`frozenset` member that is not one of the types this MVP allows
//!   a set to hold (`None`, `bool`, `int`, `float`, `str`, `tuple`,
//!   `frozenset`, `datetime`, `date`, or a `datetime`/`date` subclass)
//!   raises [`PyTypeError`] naming the member's type and its path. A plain
//!   `list` or `dict` cannot reach a set member at all — Python itself
//!   refuses `{[1]}` with `TypeError: unhashable type: 'list'` — but a
//!   `list`/`dict`/`set` subclass that defines `__hash__` can, and real
//!   `DeepDiff` would report it under that subclass's own name; a
//!   `tuple`/`frozenset` subclass (including a `namedtuple`) has no such
//!   obstacle at all — so all of these stay refused here, including nested
//!   inside an otherwise-allowed container: `{(datetime(2024, 1, 1),)}`
//!   converts, but `{(HashableList([1]),)}` does not, for a `list` subclass
//!   `HashableList` defining `__hash__`.
//! - Any other unrecognized type (`datetime.time`, `datetime.timedelta`,
//!   custom objects, …) raises [`PyTypeError`] naming the type and the exact
//!   path it was found at (e.g. `"unsupported type for diffing: complex at
//!   root['a'][2]"`).
//!
//! # Subclasses
//!
//! `DeepDiff` reports every value under `type(obj).__name__`, so a subclass
//! of a supported type — a `datetime`/`date` subclass (pandas' `Timestamp`
//! is the common one), a `list`/`tuple`/`set`/`frozenset`/`dict` subclass, or
//! a `namedtuple` (a `tuple` subclass with named fields) — is never a plain
//! instance of the base type there, even when every field matches: a
//! subclass-vs-base pair is a `type_changes` finding naming the two concrete
//! classes, confirmed against real `deepdiff==9.1.0`. This conversion checks
//! the *exact* type first (the common, and cheapest, case), falling through
//! to a second, non-exact `isinstance`-style cast that additionally records
//! `type(obj).__name__` for a subclass — see [`onix_core::value::Typed`]'s
//! doc (the "Subclass type names" section) for how that name flows through
//! the rest of the value model and diff engine, and why every other
//! matching identity in the crate (`SetItems` dedup, `crate::lcs`'s
//! scalar-list matching, `crate::ignore_order`'s hashing) needs no change at
//! all to stay class-agnostic, matching Python's own subclass-oblivious
//! `__eq__`/`__hash__` for these types.
//!
//! A `namedtuple` is accepted as an ordinary `tuple` subclass and diffed
//! **positionally** (`root[0][1]`, not `root[0].y`) — real `DeepDiff` instead
//! walks a `namedtuple`'s fields by name (`deephash.py`'s `_prep_tuple`),
//! producing `attribute_added`/`attribute_removed`/dotted-path findings this
//! crate has no machinery for. Reproducing that shape would need a second,
//! name-keyed diffing path threaded through the whole engine for one
//! single-source special case; this is a documented divergence (see
//! `tests/golden/README.md`), not an approximation of the field-walking
//! shape — the class name itself still carries through, so a namedtuple
//! type change against a plain tuple (or a different namedtuple type) still
//! names the concrete classes correctly.
//!
//! [`crate::deepdiff::DeepDiff::to_dict`] cannot reconstruct the original
//! subclass *instance* for a `datetime`/`date` subclass: once a value has
//! passed through the compact model there is no reference to the original
//! class left to rebuild one from, so it renders back as the plain base
//! type its fields describe — the same simplification already documented
//! below for the `zoneinfo`/`pytz` `tzinfo` round trip. `list`/`tuple`
//! subclasses/`namedtuple`s render back as a plain `list`/`tuple` for the
//! same reason (a `dict`/`set`/`frozenset` subclass renders back as its
//! base type too).
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
//! # Key interning
//!
//! Object keys are interned across the whole conversion via a single
//! [`onix_core::value::Builder`] threaded through the walk: record-shaped
//! data repeats a handful of keys across tens of thousands of objects, so
//! each distinct key costs a single shared allocation rather than one per
//! occurrence.
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

use onix_core::datetime::{Date as CDate, DateTime as CDateTime};
use onix_core::path::{PathSegment, render_path};
use onix_core::value::{Builder, Entries, SetItems, Typed};
use onix_core::{Number as CNumber, Value as CValue};
use pyo3::conversion::IntoPyObjectExt;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::iter::{
    BoundDictIterator, BoundFrozenSetIterator, BoundListIterator, BoundSetIterator,
    BoundTupleIterator,
};
use pyo3::types::{
    PyBool, PyDate, PyDateTime, PyDelta, PyDict, PyFloat, PyFrozenSet, PyInt, PyList, PySet,
    PyString, PyTuple, PyTzInfo,
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
        built: Vec<(String, CValue)>,
        current_key: String,
        /// See [`Frame::Seq::class_name`].
        class_name: Option<Arc<str>>,
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
        first_key: String,
        first_value: Bound<'py, PyAny>,
        /// See [`Frame::Seq::class_name`].
        class_name: Option<Arc<str>>,
    },
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
        return Ok(Step::Done(int_to_value(i)?));
    }

    if let Ok(f) = current.cast::<PyFloat>() {
        return Ok(Step::Done(float_to_value(f.value())?));
    }

    if let Ok(s) = current.cast::<PyString>() {
        let s = s.to_cow().map_err(|_| lone_surrogate_error(path, false))?;
        return Ok(Step::Done(CValue::Str(s.into_owned().into_boxed_str())));
    }

    // `datetime` before `date`: see the module doc — every `datetime` is
    // also a `date` at the C level, so checking `date` first would swallow
    // every `datetime` too. The exact-type branch runs first so the common
    // case pays only one cast; a subclass (pandas' `Timestamp` is the
    // common one) falls through to the second, non-exact branch and carries
    // its own class name — see the module doc's "Subclasses" section. Both
    // convert the same way whether or not they sit inside a set member.
    if current.cast_exact::<PyDateTime>().is_ok() {
        return Ok(Step::Done(datetime_to_value(current, path, None)?));
    }
    if current.cast::<PyDateTime>().is_ok() {
        return Ok(Step::Done(datetime_to_value(
            current,
            path,
            Some(class_name(current)),
        )?));
    }

    if current.cast_exact::<PyDate>().is_ok() {
        return Ok(Step::Done(CValue::Date(Typed::new(date_fields(
            current, path,
        )?))));
    }
    if current.cast::<PyDate>().is_ok() {
        return Ok(Step::Done(CValue::Date(Typed::with_class_name(
            date_fields(current, path)?,
            Some(class_name(current)),
        ))));
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

        return Ok(match next_dict_entry(&mut iter, path)? {
            None => Step::Done(builder.object_with_type_name(Vec::new(), class_name)),
            Some((first_key, first_value)) => Step::Dict {
                iter,
                first_key,
                first_value,
                class_name,
            },
        });
    }

    Err(if set_member {
        unhashable_member_error(current, path)
    } else {
        unsupported_type_error(current, path)
    })
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
        } => {
            built.push((current_key, value));

            match next_dict_entry(&mut remaining, path)? {
                Some((key, next_value)) => {
                    path.push(PathSegment::Key(key.clone()));
                    Ok(Advance::NeedsChild {
                        // The child's depth is the path length once its key
                        // segment is pushed above.
                        pending: (next_value, path.len(), false),
                        frame: Frame::Dict {
                            remaining,
                            built,
                            current_key: key,
                            class_name,
                        },
                    })
                }
                None => Ok(Advance::Done(
                    builder.object_with_type_name(built, class_name),
                )),
            }
        }
    }
}

/// Converts a Python object into an [`onix_core::Value`], recursing at most
/// `max_depth` levels deep — see the module doc for the full conversion table
/// and why this walk uses an explicit stack instead of native recursion.
///
/// # Errors
///
/// Returns a Python `ValueError`/[`MaxDepthError`] or `TypeError` per the
/// module doc's conversion table.
pub(crate) fn to_value(obj: &Bound<'_, PyAny>, max_depth: usize) -> PyResult<CValue> {
    let mut builder = Builder::new();
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut path: Vec<PathSegment> = Vec::new();
    let mut pending: Option<Pending<'_>> = Some((obj.clone(), 0, false));
    let mut finished: Option<CValue> = None;

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
                Step::Done(value) => finished = Some(value),
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
                } => {
                    let child_depth = depth + 1;
                    path.push(PathSegment::Key(first_key.clone()));
                    let capacity = iter.len().saturating_add(1);
                    stack.push(Frame::Dict {
                        remaining: iter,
                        built: Vec::with_capacity(capacity),
                        current_key: first_key,
                        class_name,
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
            None => return Ok(value),
            Some(frame) => {
                path.pop();

                match advance_frame(frame, value, &mut path, &mut builder)? {
                    Advance::NeedsChild {
                        pending: next_pending,
                        frame,
                    } => {
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

/// Pulls the next `(key, value)` pair out of a dict iterator, validating that
/// the key is a `str` — shared by [`to_value`]'s initial descent into a dict
/// and its `Frame::Dict` advance step, so the validation (and its error
/// message) is written exactly once.
///
/// `dict_path` is the path to the *dict itself* (not the entry) — that is
/// deliberately what a bad key's error reports, since a key that fails to
/// even parse as a `str` has no path segment of its own to report.
fn next_dict_entry<'py>(
    iter: &mut BoundDictIterator<'py>,
    dict_path: &[PathSegment],
) -> PyResult<Option<(String, Bound<'py, PyAny>)>> {
    let Some((key, value)) = iter.next() else {
        return Ok(None);
    };

    let key = key.cast::<PyString>().map_err(|_| {
        PyTypeError::new_err(format!(
            "dict keys must be str, got key of type {} at {}; non-str dict keys are not \
             supported in this MVP",
            type_name(&key),
            render_path(dict_path),
        ))
    })?;

    let key = key
        .to_cow()
        .map_err(|_| lone_surrogate_error(dict_path, true))?
        .into_owned();

    Ok(Some((key, value)))
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
    let offset = utc_offset_seconds(obj, path)?;

    CDateTime::new(date, hour, minute, second, microsecond, offset)
        .map(|dt| CValue::DateTime(Typed::with_class_name(dt, class_name)))
        .ok_or_else(|| out_of_range_error("datetime", path))
}

/// The datetime's fixed UTC offset in whole seconds, or `None` when it is
/// naive.
///
/// Asks the object itself (`utcoffset()`) rather than inspecting `tzinfo`,
/// so any `tzinfo` implementation — `timezone`, `zoneinfo`, `pytz` — reports
/// the offset in force at *this* moment, which is what
/// `datetime_normalize`'s own `astimezone` would use.
fn utc_offset_seconds(obj: &Bound<'_, PyAny>, path: &[PathSegment]) -> PyResult<Option<i32>> {
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
             (at {}); onix stores a datetime's UTC offset in seconds",
            render_path(path),
        )));
    }

    i32::try_from(days * 86_400 + seconds)
        .map(Some)
        .map_err(|_| out_of_range_error("datetime", path))
}

/// A `date`/`datetime` whose fields the compact value model rejects. Python
/// itself enforces every one of those bounds on a real `datetime` object, so
/// this is reachable only through a custom `tzinfo` returning an out-of-range
/// offset.
fn out_of_range_error(type_name: &str, path: &[PathSegment]) -> PyErr {
    PyValueError::new_err(format!(
        "{type_name} at {} is out of range for onix's internal value model",
        render_path(path),
    ))
}

/// `to_cow`'s own `UnicodeEncodeError` is discarded in favor of this, so the
/// message names the exact path the way every other conversion error in this
/// module does; see the module doc and `tests/golden/README.md` for why this
/// diverges from real `DeepDiff`.
///
/// `is_key` distinguishes the two call sites' wording: a dict key that fails
/// this check has no path segment of its own yet (like a non-`str` key, see
/// [`next_dict_entry`]), so `path` there is the path to the *dict*, not the
/// entry, and the message says so explicitly to avoid implying otherwise.
fn lone_surrogate_error(path: &[PathSegment], is_key: bool) -> PyErr {
    let subject = if is_key { "dict key" } else { "str" };
    let path = render_path(path);

    PyValueError::new_err(format!(
        "{subject} at {path} contains a lone (unpaired) surrogate code point, which has no \
         UTF-8 representation; onix's internal value model is UTF-8 and cannot represent it, \
         unlike Python's str"
    ))
}

fn int_to_value(i: &Bound<'_, PyInt>) -> PyResult<CValue> {
    if let Ok(v) = i.extract::<i64>() {
        return Ok(CValue::Number(CNumber::from_i64(v)));
    }

    if let Ok(v) = i.extract::<u64>() {
        return Ok(CValue::Number(CNumber::from_u64(v)));
    }

    Err(PyValueError::new_err(
        "integer is out of range for onix's internal value model (must fit in i64 or u64); \
         arbitrary-precision integers are not supported in this MVP, unlike real DeepDiff",
    ))
}

fn float_to_value(f: f64) -> PyResult<CValue> {
    CNumber::from_f64(f).map(CValue::Number).ok_or_else(|| {
        PyValueError::new_err(
            "NaN and infinite floats have no JSON representation and are not supported in this \
             MVP",
        )
    })
}

fn max_depth_error(max_depth: usize, path: &[PathSegment]) -> PyErr {
    MaxDepthError::new_err(format!(
        "python object nesting exceeds the configured max_depth ({max_depth}) while converting \
         to onix's internal value model, at {}",
        render_path(path),
    ))
}

fn unsupported_type_error(obj: &Bound<'_, PyAny>, path: &[PathSegment]) -> PyErr {
    PyTypeError::new_err(format!(
        "unsupported type for diffing: {} at {}; only \
         None/bool/int/float/str/dict[str, ...]/list/tuple/set/frozenset/datetime/date, and \
         subclasses of dict/list/tuple/set/frozenset/datetime/date (including namedtuples), are \
         supported in this MVP (time, timedelta, and custom objects are not)",
        type_name(obj),
        render_path(path),
    ))
}

/// The error for an object that reached a set member, or anything nested
/// inside one, but is not a type this MVP allows there — see [`classify`]'s
/// `set_member` parameter.
fn unhashable_member_error(obj: &Bound<'_, PyAny>, path: &[PathSegment]) -> PyErr {
    PyTypeError::new_err(format!(
        "unsupported type for a set member: {} at {}; a set member must be \
         None/bool/int/float/str/tuple/frozenset/datetime/date, or a datetime/date subclass \
         (a tuple/frozenset subclass, including a namedtuple, is not accepted as a set member)",
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
        current_key: &'v str,
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
                CValue::Str(s) => RenderStep::Done(s.as_ref().into_py_any(py)?),
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
                built.set_item(current_key, rendered)?;

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

        // A non-float that does not fit an i64 is by construction a u64
        // above i64::MAX.
        if let Some(v) = n.as_u64() {
            return v.into_py_any(py);
        }
    }

    n.as_f64()
        .expect("a Number is always an i64, a u64, or an f64")
        .into_py_any(py)
}
