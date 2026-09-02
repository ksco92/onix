//! The drop-in `DeepDiff` class: accepts live Python objects, converts them
//! to `onix_core`'s value model exactly once, diffs natively, and exposes
//! the result as `.to_json()`/`.to_dict()` — see this module's `DeepDiff`
//! doc for the full, documented MVP surface.

use onix_core::{DEFAULT_MAX_DEPTH, DiffOptions};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::Value;

use crate::convert::{to_value, value_to_pyobject};
use crate::errors::map_diff_error;
use crate::guard::{check_max_depth_ceiling, drop_report, report_needs_worker, run_on_worker};

/// A drop-in subset of `deepdiff.DeepDiff`.
///
/// ```text
/// from deepdiff_rs import DeepDiff
///
/// diff = DeepDiff({"a": 1}, {"a": 2})
/// if diff:
///     print(diff.to_json())
/// ```
///
/// # Constructor
///
/// `DeepDiff(t1, t2, ignore_order=False, max_depth=None)`:
///
/// - `t1`/`t2`: any of `None`, `bool`, `int`, `float`, `str`, `dict` (`str`
///   keys), or `list`, arbitrarily nested. Converted to `onix_core`'s value
///   model exactly once, up front — see `crate::convert`'s module doc for
///   the full conversion table and every unsupported-type error this can
///   raise (`TypeError` for an unsupported type, `ValueError` for an
///   out-of-range int or a non-finite float).
/// - `ignore_order`: mirrors `DeepDiff(..., ignore_order=True)`.
/// - `max_depth`: caller-chosen recursion-depth bound; defaults to
///   `onix_core::DEFAULT_MAX_DEPTH` (512) when omitted. Exceeding it —
///   during either the Python-object conversion above or the diff itself —
///   raises `deepdiff_rs.MaxDepthError` (a `ValueError` subclass), never a
///   native crash. `max_depth` itself may not exceed
///   `deepdiff_rs.MAX_DEPTH_CEILING` (see `crate::guard`): a larger value is
///   rejected up front with a plain `ValueError`, because the recursive diff
///   engine cannot safely run past that depth. The diff runs on a
///   stack-sized worker thread (GIL released) so that no in-range
///   `max_depth`, however high, can overflow the native stack — see
///   `crate::guard`'s module doc.
///
/// This intentionally does not attempt `deepdiff.DeepDiff`'s full option
/// surface (`exclude_paths`, `significant_digits`, custom operators,
/// `verbose_level`, …) — this is the documented MVP surface, matched at
/// `verbose_level=2` (the level `onix_core`'s report shape always
/// corresponds to).
#[pyclass(module = "deepdiff_rs")]
pub(crate) struct DeepDiff {
    report_value: Value,
    /// Whether `report_value` is nested deeply enough that serializing or
    /// dropping it must happen on the sized worker thread rather than the
    /// calling thread — computed once here so `to_json`/`Drop` need not
    /// re-walk the report. See `crate::guard::report_needs_worker`.
    needs_worker: bool,
}

#[pymethods]
impl DeepDiff {
    #[new]
    #[pyo3(signature = (t1, t2, ignore_order=false, max_depth=None))]
    fn new(
        py: Python<'_>,
        t1: &Bound<'_, PyAny>,
        t2: &Bound<'_, PyAny>,
        ignore_order: bool,
        max_depth: Option<usize>,
    ) -> PyResult<Self> {
        let max_depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
        check_max_depth_ceiling(max_depth)?;
        // Conversion is iterative (no native recursion — see `crate::convert`)
        // and needs the GIL to read the live Python objects, so it stays on
        // the calling thread.
        let a = to_value(t1, max_depth)?;
        let b = to_value(t2, max_depth)?;
        let opts = DiffOptions {
            max_depth,
            ignore_order,
        };
        // The diff is natively recursive, so it runs on the stack-sized
        // worker (GIL released); `a`, `b`, and the intermediate `Report`
        // are all owned by the closure and dropped there, on that large
        // stack, never on the calling thread.
        let report_value = run_on_worker(py, move || {
            onix_core::diff_with_options(&a, &b, &opts).map(|report| report.to_json_value())
        })?
        .map_err(|error| map_diff_error(&error))?;
        let needs_worker = report_needs_worker(&report_value);

        Ok(Self {
            report_value,
            needs_worker,
        })
    }

    /// Byte-compatible with real `DeepDiff(...).to_json()` at
    /// `verbose_level=2` — the whole point of this crate.
    ///
    /// `serde_json`'s serialization of a `Value` is itself natively
    /// recursive, so a report deep enough to matter is serialized on the
    /// sized worker thread; a shallow one (the overwhelmingly common case)
    /// serializes inline.
    fn to_json(&self, py: Python<'_>) -> PyResult<String> {
        let serialized = if self.needs_worker {
            run_on_worker(py, || serde_json::to_string(&self.report_value))?
        } else {
            serde_json::to_string(&self.report_value)
        };
        serialized.map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// The parsed form of [`Self::to_json`] — a native Python `dict`.
    /// Conversion back to Python objects is iterative (see
    /// `crate::convert::value_to_pyobject`), so it is safe on the calling
    /// thread at any depth.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        value_to_pyobject(py, &self.report_value)
    }

    /// `if diff:` truthiness — falsy exactly when `t1`/`t2` had no
    /// differences (an empty report).
    fn __bool__(&self) -> bool {
        !is_empty_report(&self.report_value)
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("DeepDiff({})", self.to_json(py)?))
    }
}

impl Drop for DeepDiff {
    fn drop(&mut self) {
        // `serde_json::Value`'s derived `Drop` is natively recursive, so a
        // report nested near the ceiling would overflow the calling
        // thread's stack if dropped here. Hand it to `drop_report`, which
        // drops a deep report on the sized worker thread and a shallow one
        // inline. `needs_worker` is already known, so shallow reports pay no
        // walk here.
        if self.needs_worker {
            drop_report(std::mem::replace(&mut self.report_value, Value::Null));
        }
    }
}

/// A [`DeepDiff`] report renders to `{}` (an empty JSON object) via
/// [`onix_core::Report::to_json_value`] when there are no findings — see
/// that function's own doc.
fn is_empty_report(value: &Value) -> bool {
    matches!(value, Value::Object(map) if map.is_empty())
}
