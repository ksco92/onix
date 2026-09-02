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
//!   key's type.
//! - Any other unrecognized type (`tuple`, `set`, `frozenset`, dates,
//!   custom objects, …) raises [`PyTypeError`] naming the type — none of
//!   these have a lossless JSON representation, and real `DeepDiff`'s
//!   support for some of them (tuples, sets, dates) is explicitly out of
//!   scope for this MVP.
//!
//! # Depth guard
//!
//! This conversion is itself a native-recursion tree walk (mirroring the
//! Python object graph's own shape) — independent of, and running strictly
//! *before*, [`onix_core`]'s own recursion-depth guard on the *diff*. An
//! adversarially deep Python list/dict would overflow the native stack
//! while being *converted*, before `onix_core::diff_with_options` ever
//! runs. [`to_value`] therefore takes the same `max_depth` budget the diff
//! itself will use and raises [`crate::errors::MaxDepthError`] once
//! conversion would recurse past it, using the identical depth-counting
//! convention as `onix_core` (the root value is depth `0`; stepping into a
//! dict value or list element adds one). This is intentionally a little
//! stricter than `onix_core::diff_with_max_depth`'s own guarantee that two
//! *equal* inputs of any depth always diff cleanly — equality can't be
//! known yet at conversion time, before either side is even a `Value`.
use pyo3::conversion::IntoPyObjectExt;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};
use serde_json::{Map, Number, Value};

use crate::errors::MaxDepthError;

/// Converts a Python object into a [`Value`], recursing at most
/// `max_depth` levels deep — see the module doc for the full conversion
/// table and the depth-guard rationale.
///
/// # Errors
///
/// Returns a Python `ValueError`/[`MaxDepthError`] or `TypeError` per the
/// module doc's conversion table.
pub(crate) fn to_value(obj: &Bound<'_, PyAny>, max_depth: usize) -> PyResult<Value> {
    to_value_at_depth(obj, max_depth, 0)
}

fn to_value_at_depth(obj: &Bound<'_, PyAny>, max_depth: usize, depth: usize) -> PyResult<Value> {
    if depth > max_depth {
        return Err(MaxDepthError::new_err(format!(
            "python object nesting exceeds the configured max_depth ({max_depth}) while \
             converting to onix's internal value model"
        )));
    }

    if obj.is_none() {
        return Ok(Value::Null);
    }

    // `bool` is a Python `int` subclass, so this check must precede the
    // `PyInt` one below or every bool would be misread as an int.
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }

    if let Ok(i) = obj.cast::<PyInt>() {
        return int_to_value(i);
    }

    if let Ok(f) = obj.cast::<PyFloat>() {
        return float_to_value(f.value());
    }

    if let Ok(s) = obj.cast::<PyString>() {
        return Ok(Value::String(s.to_string()));
    }

    if let Ok(dict) = obj.cast::<PyDict>() {
        return dict_to_value(dict, max_depth, depth);
    }

    if let Ok(list) = obj.cast::<PyList>() {
        return list_to_value(list, max_depth, depth);
    }

    Err(unsupported_type_error(obj))
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

fn dict_to_value(dict: &Bound<'_, PyDict>, max_depth: usize, depth: usize) -> PyResult<Value> {
    let mut map = Map::with_capacity(dict.len());

    for (key, value) in dict.iter() {
        let key = key.cast::<PyString>().map_err(|_| {
            PyTypeError::new_err(format!(
                "dict keys must be str, got key of type {}; non-str dict keys are not \
                 supported in this MVP",
                type_name(&key)
            ))
        })?;
        map.insert(
            key.to_string(),
            to_value_at_depth(&value, max_depth, depth + 1)?,
        );
    }

    Ok(Value::Object(map))
}

fn list_to_value(list: &Bound<'_, PyList>, max_depth: usize, depth: usize) -> PyResult<Value> {
    let mut items = Vec::with_capacity(list.len());

    for item in list.iter() {
        items.push(to_value_at_depth(&item, max_depth, depth + 1)?);
    }

    Ok(Value::Array(items))
}

fn unsupported_type_error(obj: &Bound<'_, PyAny>) -> PyErr {
    PyTypeError::new_err(format!(
        "unsupported type for diffing: {}; only None/bool/int/float/str/dict[str, ...]/list \
         are supported in this MVP (tuples, sets, dates, and custom objects are not)",
        type_name(obj)
    ))
}

fn type_name(obj: &Bound<'_, PyAny>) -> String {
    obj.get_type()
        .name()
        .map_or_else(|_| "<unknown type>".to_string(), |name| name.to_string())
}

/// Converts a rendered [`Value`] (a [`crate::deepdiff::DeepDiff`] report, or one of
/// its nested values) back into a native Python object — the parsed form
/// [`crate::deepdiff::DeepDiff::to_dict`] returns. Unlike [`to_value`], this never
/// fails: every [`Value`] variant has a lossless Python equivalent, and
/// [`Value`] trees built by this crate are already bounded by the same
/// `max_depth` guard [`to_value`] enforces on the way in.
pub(crate) fn value_to_pyobject(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    match value {
        Value::Null => Ok(py.None()),
        Value::Bool(b) => b.into_py_any(py),
        Value::Number(n) => number_to_pyobject(py, n),
        Value::String(s) => s.into_py_any(py),
        Value::Array(items) => items
            .iter()
            .map(|item| value_to_pyobject(py, item))
            .collect::<PyResult<Vec<_>>>()?
            .into_py_any(py),
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (key, value) in map {
                dict.set_item(key, value_to_pyobject(py, value)?)?;
            }
            dict.into_py_any(py)
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
