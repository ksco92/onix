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
//! | `str` | `Str` | |
//! | `dict` (`str` keys only) | `Object` | keys interned across the whole walk |
//! | `list` | `Array` | |
//! | `tuple` | `Tuple` | exactly `tuple`; every subclass is rejected, see below |
//!
//! Every other type raises a Python exception instead of converting:
//!
//! - An `int` outside `i64::MIN..=u64::MAX` raises [`PyValueError`]:
//!   arbitrary-precision integers are not supported in this MVP (real
//!   `DeepDiff` supports them natively).
//! - A `NaN` or infinite `float` raises [`PyValueError`] (JSON has no
//!   representation for either).
//! - A `dict` key that is not a `str` raises [`PyTypeError`] naming the
//!   key's type and the path to the dict containing it.
//! - Any other unrecognized type (`set`, `frozenset`, dates, custom
//!   objects, …) raises [`PyTypeError`] naming the type and the exact path
//!   it was found at (e.g. `"unsupported type for diffing: set at
//!   root['a'][2]"`).
//!
//! A `tuple` converts to [`onix_core::Value::Tuple`], which the engine
//! diffs positionally exactly like a list while still reporting a
//! tuple-vs-list pairing as a `type_changes` — matching `DeepDiff`.
//!
//! A `tuple` **subclass** is not converted, and raises [`PyTypeError`] naming
//! the class like any other unsupported type. `DeepDiff` reports a value's
//! `type(obj).__name__`, so a subclass never compares as a plain `tuple`
//! there: `DeepDiff(Pair((1, 2)), (1, 2))` is a `type_changes` from `Pair` to
//! `tuple`, where converting the subclass as a tuple would report no
//! difference at all. A `namedtuple` diverges further still — `DeepDiff`
//! walks its *fields* (`deephash.py`'s `_prep_tuple`), reporting `root.y`
//! rather than `root[1]` — and is refused by the same rule, needing no check
//! of its own.
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
//! input is nested. Because the build is iterative and the compact
//! [`onix_core::Value`]'s own `Drop` is iterative too, conversion — and the
//! teardown of a partially built tree on any error path — is stack-safe on
//! *any* thread at *any* depth, without a sized worker: only the natively
//! recursive diff engine still needs one (see [`crate::guard`]).
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
use onix_core::path::{PathSegment, render_path};
use onix_core::value::{Builder, Entries};
use onix_core::{Number as CNumber, Value as CValue};
use pyo3::conversion::IntoPyObjectExt;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::iter::{BoundDictIterator, BoundListIterator, BoundTupleIterator};
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

use crate::errors::MaxDepthError;

/// A Python sequence being walked: a `list` or a `tuple`. The two differ
/// only in their iterator type and in which [`CValue`] the finished items
/// become, so every other step of the walk treats them identically — the
/// same way the diff engine does.
enum SeqIter<'py> {
    List(BoundListIterator<'py>),
    Tuple(BoundTupleIterator<'py>),
}

impl<'py> SeqIter<'py> {
    fn next(&mut self) -> Option<Bound<'py, PyAny>> {
        match self {
            SeqIter::List(iter) => iter.next(),
            SeqIter::Tuple(iter) => iter.next(),
        }
    }

    /// How many elements are still to come (both iterators are
    /// `ExactSizeIterator`), so a frame can pre-size its buffer.
    fn len(&self) -> usize {
        match self {
            SeqIter::List(iter) => iter.len(),
            SeqIter::Tuple(iter) => iter.len(),
        }
    }

    /// Wraps this sequence's finished items in the matching value shape.
    fn build(&self, items: Vec<CValue>) -> CValue {
        let items = items.into_boxed_slice();
        match self {
            SeqIter::List(_) => CValue::Array(items),
            SeqIter::Tuple(_) => CValue::Tuple(items),
        }
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
    },
    Dict {
        remaining: BoundDictIterator<'py>,
        built: Vec<(String, CValue)>,
        current_key: String,
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
    },
    Dict {
        iter: BoundDictIterator<'py>,
        first_key: String,
        first_value: Bound<'py, PyAny>,
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
fn classify<'py>(
    current: &Bound<'py, PyAny>,
    path: &[PathSegment],
    builder: &mut Builder,
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
        return Ok(Step::Done(CValue::Str(s.to_string().into_boxed_str())));
    }

    if let Ok(list) = current.cast::<PyList>() {
        return Ok(seq_step(SeqIter::List(list.iter())));
    }

    // Exact, unlike the casts above: a tuple subclass is reported under its
    // own type name by `DeepDiff` and so is refused here (see the module
    // doc), which the fall-through to `unsupported_type_error` below does.
    if let Ok(tuple) = current.cast_exact::<PyTuple>() {
        return Ok(seq_step(SeqIter::Tuple(tuple.iter())));
    }

    if let Ok(dict) = current.cast::<PyDict>() {
        let mut iter = dict.iter();

        return Ok(match next_dict_entry(&mut iter, path)? {
            None => Step::Done(builder.object(Vec::new())),
            Some((first_key, first_value)) => Step::Dict {
                iter,
                first_key,
                first_value,
            },
        });
    }

    Err(unsupported_type_error(current, path))
}

/// Starts one sequence: an empty one is finished outright, a non-empty one
/// hands its first element back for conversion with the rest parked in the
/// returned iterator.
fn seq_step(mut iter: SeqIter<'_>) -> Step<'_> {
    match iter.next() {
        None => Step::Done(iter.build(Vec::new())),
        Some(first) => Step::Seq { iter, first },
    }
}

/// What [`advance_frame`] returns: either the frame needs its next child
/// converted before it can finish, or it's fully built.
enum Advance<'py> {
    NeedsChild {
        pending: (Bound<'py, PyAny>, usize),
        frame: Frame<'py>,
    },
    Done(CValue),
}

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
        } => {
            built.push(value);

            Ok(match remaining.next() {
                Some(next_item) => {
                    // The just-finished child was appended above, so the next
                    // child's index is the new length, and its depth is the
                    // path length once its segment is pushed.
                    path.push(PathSegment::Index(built.len()));
                    Advance::NeedsChild {
                        pending: (next_item, path.len()),
                        frame: Frame::Seq { remaining, built },
                    }
                }
                None => Advance::Done(remaining.build(built)),
            })
        }
        Frame::Dict {
            mut remaining,
            mut built,
            current_key,
        } => {
            built.push((current_key, value));

            match next_dict_entry(&mut remaining, path)? {
                Some((key, next_value)) => {
                    path.push(PathSegment::Key(key.clone()));
                    Ok(Advance::NeedsChild {
                        // The child's depth is the path length once its key
                        // segment is pushed above.
                        pending: (next_value, path.len()),
                        frame: Frame::Dict {
                            remaining,
                            built,
                            current_key: key,
                        },
                    })
                }
                None => Ok(Advance::Done(builder.object(built))),
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
    let mut pending: Option<(Bound<'_, PyAny>, usize)> = Some((obj.clone(), 0));
    let mut finished: Option<CValue> = None;

    // On any error break, `stack` (and its parked, possibly deep entries)
    // drops here at function return. Every `CValue` has an iterative `Drop`,
    // so that teardown is stack-safe on the calling thread at any depth — the
    // conversion never needs a sized-worker drop path.
    loop {
        if let Some((current, depth)) = pending.take() {
            if depth > max_depth {
                return Err(max_depth_error(max_depth, &path));
            }

            match classify(&current, &path, &mut builder)? {
                Step::Done(value) => finished = Some(value),
                Step::Seq { iter, first } => {
                    let child_depth = depth + 1;
                    path.push(PathSegment::Index(0));
                    // `iter` has already yielded `first`, so the finished
                    // sequence will hold `iter.len() + 1` elements — pre-size
                    // for exactly that (both sequence iterators are
                    // `ExactSizeIterator`).
                    let capacity = iter.len().saturating_add(1);
                    stack.push(Frame::Seq {
                        remaining: iter,
                        built: Vec::with_capacity(capacity),
                    });
                    pending = Some((first, child_depth));
                    continue;
                }
                Step::Dict {
                    iter,
                    first_key,
                    first_value,
                } => {
                    let child_depth = depth + 1;
                    path.push(PathSegment::Key(first_key.clone()));
                    let capacity = iter.len().saturating_add(1);
                    stack.push(Frame::Dict {
                        remaining: iter,
                        built: Vec::with_capacity(capacity),
                        current_key: first_key,
                    });
                    pending = Some((first_value, child_depth));
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

    Ok(Some((key.to_string(), value)))
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
         None/bool/int/float/str/dict[str, ...]/list/tuple are supported in this MVP (sets, \
         dates, tuple subclasses including namedtuples, and custom objects are not)",
        type_name(obj),
        render_path(path),
    ))
}

fn type_name(obj: &Bound<'_, PyAny>) -> String {
    obj.get_type()
        .name()
        .map_or_else(|_| "<unknown type>".to_string(), |name| name.to_string())
}

/// Which Python sequence [`value_to_pyobject`] rebuilds a run of items into
/// — the report side of [`SeqIter`], where both shapes carry the same
/// `&[CValue]` and only the finished object differs.
#[derive(Clone, Copy)]
enum SeqKind {
    List,
    Tuple,
}

impl SeqKind {
    fn build(self, py: Python<'_>, items: Vec<Py<PyAny>>) -> PyResult<Py<PyAny>> {
        match self {
            SeqKind::List => items.into_py_any(py),
            SeqKind::Tuple => PyTuple::new(py, items)?.into_py_any(py),
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
            finished = Some(match current {
                CValue::Null => py.None(),
                CValue::Bool(b) => b.into_py_any(py)?,
                CValue::Number(n) => number_to_pyobject(py, n)?,
                CValue::Str(s) => s.as_ref().into_py_any(py)?,
                CValue::Array(items) | CValue::Tuple(items) => {
                    let kind = if matches!(current, CValue::Tuple(_)) {
                        SeqKind::Tuple
                    } else {
                        SeqKind::List
                    };
                    let mut iter = items.iter();

                    match iter.next() {
                        None => kind.build(py, Vec::new())?,
                        Some(first) => {
                            let capacity = iter.len().saturating_add(1);
                            stack.push(RenderFrame::Seq {
                                kind,
                                remaining: iter,
                                built: Vec::with_capacity(capacity),
                            });
                            pending = Some(first);
                            continue;
                        }
                    }
                }
                CValue::Object(map) => {
                    let mut iter = map.iter();

                    match iter.next() {
                        None => PyDict::new(py).into_py_any(py)?,
                        Some((key, first_value)) => {
                            stack.push(RenderFrame::Object {
                                remaining: iter,
                                built: PyDict::new(py),
                                current_key: key,
                            });
                            pending = Some(first_value);
                            continue;
                        }
                    }
                }
            });
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
