//! Converts a live Python object graph into a [`serde_json::Value`] once,
//! up front — the [`crate::deepdiff::DeepDiff`] class's "drop-in" layer diffs the
//! converted value model natively; it never touches Python objects again
//! after conversion.
//!
//! # Supported types (documented MVP scope)
//!
//! | Python | `Value` | Notes |
//! | --- | --- | --- |
//! | `None` | `Null` | |
//! | `bool` | `Bool` | checked before `int` — `bool` is a Python `int` subclass |
//! | `int` | `Number` | must fit in `i64` or `u64`; see below |
//! | `float` | `Number` | must be finite; see below |
//! | `str` | `String` | |
//! | `dict` (`str` keys only) | `Object` | |
//! | `list` | `Array` | |
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
//! - Any other unrecognized type (`tuple`, `set`, `frozenset`, dates,
//!   custom objects, …) raises [`PyTypeError`] naming the type and the
//!   exact path it was found at (e.g. `"unsupported type for diffing:
//!   tuple at root['a'][2]"`) — none of these have a lossless JSON
//!   representation, and real `DeepDiff`'s support for some of them
//!   (tuples, sets, dates) is explicitly out of scope for this MVP.
//!
//! # Depth guard, and why this walk is iterative
//!
//! This conversion mirrors the Python object graph's own shape — a naive
//! implementation would walk it via native recursion, exactly the
//! stack-overflow class `onix_core`'s own diff engine eliminates for the
//! *diff* itself (see `onix_core::diff`'s module doc: an explicit
//! heap-allocated work-stack, no native recursion, so nesting depth can
//! never overflow the call stack). [`to_value`] uses the identical
//! technique: an explicit `Vec`-backed stack of in-progress
//! list/dict frames, walked in a single loop, so peak *native* stack usage
//! is `O(1)` regardless of how deeply the input is nested — only the heap
//! (`stack`/`path` below) grows with depth, which is an ordinary,
//! catchable allocation concern, not an uncatchable process abort. This
//! matters independently of the `max_depth` budget below: without it, a
//! sufficiently deep (but otherwise valid) input crashes the whole Python
//! interpreter (`SIGSEGV`) before the depth check even has a chance to
//! fire, however small `max_depth` is set to, once native stack space
//! itself runs out.
//!
//! On top of that native-stack safety, [`to_value`] separately takes the
//! same `max_depth` budget the diff itself will use and raises
//! [`crate::errors::MaxDepthError`] once conversion would recurse past it
//! — independent of, and running strictly *before*,
//! `onix_core::diff_with_options`'s own recursion-depth guard on the
//! *diff*. This uses the identical depth-counting convention as
//! `onix_core` (the root value is depth `0`; stepping into a dict value or
//! list element adds one). This is intentionally a little stricter than
//! `onix_core::diff_with_max_depth`'s own guarantee that two *equal*
//! inputs of any depth always diff cleanly — equality can't be known yet
//! at conversion time, before either side is even a `Value`.
use onix_core::path::{PathSegment, render_path};
use pyo3::conversion::IntoPyObjectExt;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::iter::{BoundDictIterator, BoundListIterator};
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};
use serde_json::{Map, Number, Value};

use crate::errors::MaxDepthError;

/// One in-progress container on [`to_value`]'s explicit work-stack: either
/// a list or a dict whose *n*th child has been dispatched for conversion
/// and whose remaining children (plus everything converted so far) are
/// parked here until that child's result comes back.
enum Frame<'py> {
    List {
        remaining: BoundListIterator<'py>,
        built: Vec<Value>,
        next_index: usize,
        child_depth: usize,
    },
    Dict {
        remaining: BoundDictIterator<'py>,
        built: Map<String, Value>,
        current_key: String,
        child_depth: usize,
    },
}

/// What happens when converting a single object: either it produced a
/// finished [`Value`] outright (a scalar, or an empty list/dict), or it's a
/// non-empty container — [`to_value`]'s loop pushes a [`Frame`] and
/// descends into the returned first child.
enum Step<'py> {
    Done(Value),
    List {
        iter: BoundListIterator<'py>,
        first: Bound<'py, PyAny>,
    },
    Dict {
        iter: BoundDictIterator<'py>,
        first_key: String,
        first_value: Bound<'py, PyAny>,
    },
}

/// Classifies a single Python object: everything [`to_value`]'s loop does
/// per node except the `max_depth` check (needs the loop's own `depth`
/// counter) and attaching the result to the work-stack (needs the loop's
/// own `path`/`stack`) — kept separate so `to_value` itself stays within a
/// reasonable function length.
///
/// `path` is the path to `current` itself (used verbatim for an
/// unsupported-type error, and — when `current` is a dict — also passed
/// through to [`next_dict_entry`] for a bad-key error).
fn classify<'py>(current: &Bound<'py, PyAny>, path: &[PathSegment]) -> PyResult<Step<'py>> {
    if current.is_none() {
        return Ok(Step::Done(Value::Null));
    }

    // `bool` is a Python `int` subclass, so this check must precede the
    // `PyInt` one below or every bool would be misread as an int.
    if let Ok(b) = current.cast::<PyBool>() {
        return Ok(Step::Done(Value::Bool(b.is_true())));
    }

    if let Ok(i) = current.cast::<PyInt>() {
        return Ok(Step::Done(int_to_value(i)?));
    }

    if let Ok(f) = current.cast::<PyFloat>() {
        return Ok(Step::Done(float_to_value(f.value())?));
    }

    if let Ok(s) = current.cast::<PyString>() {
        return Ok(Step::Done(Value::String(s.to_string())));
    }

    if let Ok(list) = current.cast::<PyList>() {
        let mut iter = list.iter();

        return Ok(match iter.next() {
            None => Step::Done(Value::Array(Vec::new())),
            Some(first) => Step::List { iter, first },
        });
    }

    if let Ok(dict) = current.cast::<PyDict>() {
        let mut iter = dict.iter();

        return Ok(match next_dict_entry(&mut iter, path)? {
            None => Step::Done(Value::Object(Map::new())),
            Some((first_key, first_value)) => Step::Dict {
                iter,
                first_key,
                first_value,
            },
        });
    }

    Err(unsupported_type_error(current, path))
}

/// What [`advance_frame`] returns: either the frame needs its next child
/// converted before it can finish, or it's fully built.
enum Advance<'py> {
    NeedsChild {
        pending: (Bound<'py, PyAny>, usize),
        frame: Frame<'py>,
    },
    Done(Value),
}

/// Attaches a just-finished child `value` into `frame` and figures out
/// what happens next: either `frame` has another child to convert
/// (`Advance::NeedsChild`, with `path` extended for it), or `frame` is
/// fully built (`Advance::Done`). `path` must already have had the
/// finished child's own segment popped by the caller — see [`to_value`].
fn advance_frame<'py>(
    frame: Frame<'py>,
    value: Value,
    path: &mut Vec<PathSegment>,
) -> PyResult<Advance<'py>> {
    match frame {
        Frame::List {
            mut remaining,
            mut built,
            next_index,
            child_depth,
        } => {
            built.push(value);

            Ok(match remaining.next() {
                Some(next_item) => {
                    path.push(PathSegment::Index(next_index));
                    Advance::NeedsChild {
                        pending: (next_item, child_depth),
                        frame: Frame::List {
                            remaining,
                            built,
                            next_index: next_index + 1,
                            child_depth,
                        },
                    }
                }
                None => Advance::Done(Value::Array(built)),
            })
        }
        Frame::Dict {
            mut remaining,
            mut built,
            current_key,
            child_depth,
        } => {
            built.insert(current_key, value);

            Ok(match next_dict_entry(&mut remaining, path)? {
                Some((key, next_value)) => {
                    path.push(PathSegment::Key(key.clone()));
                    Advance::NeedsChild {
                        pending: (next_value, child_depth),
                        frame: Frame::Dict {
                            remaining,
                            built,
                            current_key: key,
                            child_depth,
                        },
                    }
                }
                None => Advance::Done(Value::Object(built)),
            })
        }
    }
}

/// Converts a Python object into a [`Value`], recursing at most
/// `max_depth` levels deep — see the module doc for the full conversion
/// table and why this walk uses an explicit stack instead of native
/// recursion.
///
/// # Errors
///
/// Returns a Python `ValueError`/[`MaxDepthError`] or `TypeError` per the
/// module doc's conversion table.
pub(crate) fn to_value(obj: &Bound<'_, PyAny>, max_depth: usize) -> PyResult<Value> {
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut path: Vec<PathSegment> = Vec::new();
    let mut pending: Option<(Bound<'_, PyAny>, usize)> = Some((obj.clone(), 0));
    let mut finished: Option<Value> = None;

    loop {
        if let Some((current, depth)) = pending.take() {
            if depth > max_depth {
                return Err(max_depth_error(max_depth, &path));
            }

            match classify(&current, &path)? {
                Step::Done(value) => finished = Some(value),
                Step::List { iter, first } => {
                    let child_depth = depth + 1;
                    path.push(PathSegment::Index(0));
                    stack.push(Frame::List {
                        remaining: iter,
                        built: Vec::new(),
                        next_index: 1,
                        child_depth,
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
                    stack.push(Frame::Dict {
                        remaining: iter,
                        built: Map::new(),
                        current_key: first_key,
                        child_depth,
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

                match advance_frame(frame, value, &mut path)? {
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

/// Pulls the next `(key, value)` pair out of a dict iterator, validating
/// that the key is a `str` — shared by [`to_value`]'s initial descent into
/// a dict and its `Frame::Dict` advance step, so the validation (and its
/// error message) is written exactly once.
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

fn int_to_value(i: &Bound<'_, PyInt>) -> PyResult<Value> {
    if let Ok(v) = i.extract::<i64>() {
        return Ok(Value::Number(Number::from(v)));
    }

    if let Ok(v) = i.extract::<u64>() {
        return Ok(Value::Number(Number::from(v)));
    }

    Err(PyValueError::new_err(
        "integer is out of range for onix's internal value model (must fit in i64 or u64); \
         arbitrary-precision integers are not supported in this MVP, unlike real DeepDiff",
    ))
}

fn float_to_value(f: f64) -> PyResult<Value> {
    Number::from_f64(f).map(Value::Number).ok_or_else(|| {
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
        "unsupported type for diffing: {} at {}; only None/bool/int/float/str/dict[str, ...]/list \
         are supported in this MVP (tuples, sets, dates, and custom objects are not)",
        type_name(obj),
        render_path(path),
    ))
}

fn type_name(obj: &Bound<'_, PyAny>) -> String {
    obj.get_type()
        .name()
        .map_or_else(|_| "<unknown type>".to_string(), |name| name.to_string())
}

/// One in-progress container on [`value_to_pyobject`]'s explicit
/// work-stack — the same technique as [`Frame`]/[`to_value`], applied in
/// the opposite direction (`Value` -> Python object) so this direction is
/// equally immune to the native-stack-overflow class described in the
/// module doc, on a `Value` tree deep enough to matter.
enum RenderFrame<'py, 'v> {
    Array {
        remaining: std::slice::Iter<'v, Value>,
        built: Vec<Py<PyAny>>,
    },
    Object {
        remaining: serde_json::map::Iter<'v>,
        built: Bound<'py, PyDict>,
        current_key: String,
    },
}

/// Converts a rendered [`Value`] (a [`crate::deepdiff::DeepDiff`] report, or one of
/// its nested values) back into a native Python object — the parsed form
/// [`crate::deepdiff::DeepDiff::to_dict`] returns. Unlike [`to_value`], this never
/// fails: every [`Value`] variant has a lossless Python equivalent. It does
/// not separately enforce `max_depth` — [`Value`] trees this crate ever
/// renders are already bounded by `to_value`'s own guard on the way in, or
/// by `onix_core`'s own `max_depth` contract on a diff report's output —
/// but it still walks via an explicit stack (see [`RenderFrame`]), not
/// native recursion, so a `Value` tree nested close to a caller-raised
/// `max_depth` can never overflow the native stack converting it back,
/// independent of that bound holding.
pub(crate) fn value_to_pyobject(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    let mut stack: Vec<RenderFrame<'_, '_>> = Vec::new();
    let mut pending: Option<&Value> = Some(value);
    let mut finished: Option<Py<PyAny>> = None;

    loop {
        if let Some(current) = pending.take() {
            finished = Some(match current {
                Value::Null => py.None(),
                Value::Bool(b) => b.into_py_any(py)?,
                Value::Number(n) => number_to_pyobject(py, n)?,
                Value::String(s) => s.into_py_any(py)?,
                Value::Array(items) => {
                    let mut iter = items.iter();

                    match iter.next() {
                        None => Vec::<Py<PyAny>>::new().into_py_any(py)?,
                        Some(first) => {
                            stack.push(RenderFrame::Array {
                                remaining: iter,
                                built: Vec::new(),
                            });
                            pending = Some(first);
                            continue;
                        }
                    }
                }
                Value::Object(map) => {
                    let mut iter = map.iter();

                    match iter.next() {
                        None => PyDict::new(py).into_py_any(py)?,
                        Some((key, first_value)) => {
                            stack.push(RenderFrame::Object {
                                remaining: iter,
                                built: PyDict::new(py),
                                current_key: key.clone(),
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
            Some(RenderFrame::Array {
                mut remaining,
                mut built,
            }) => {
                built.push(rendered);

                match remaining.next() {
                    Some(next_item) => {
                        pending = Some(next_item);
                        stack.push(RenderFrame::Array { remaining, built });
                    }
                    None => finished = Some(built.into_py_any(py)?),
                }
            }
            Some(RenderFrame::Object {
                mut remaining,
                built,
                current_key,
            }) => {
                built.set_item(&current_key, rendered)?;

                match remaining.next() {
                    Some((key, next_value)) => {
                        pending = Some(next_value);
                        stack.push(RenderFrame::Object {
                            remaining,
                            built,
                            current_key: key.clone(),
                        });
                    }
                    None => finished = Some(built.into_py_any(py)?),
                }
            }
        }
    }
}

fn number_to_pyobject(py: Python<'_>, n: &Number) -> PyResult<Py<PyAny>> {
    if let Some(v) = n.as_i64() {
        return v.into_py_any(py);
    }

    if let Some(v) = n.as_u64() {
        return v.into_py_any(py);
    }

    // Every Number this crate ever constructs is either an integer that
    // fit in i64/u64 (see int_to_value, both handled above) or came
    // through Number::from_f64 (see float_to_value) — so reaching here
    // always means the f64 case.
    n.as_f64()
        .expect("serde_json::Number is always i64, u64, or f64")
        .into_py_any(py)
}
