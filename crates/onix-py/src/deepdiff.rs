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
///   native crash.
///
/// This intentionally does not attempt `deepdiff.DeepDiff`'s full option
/// surface (`exclude_paths`, `significant_digits`, custom operators,
/// `verbose_level`, …) — this is the documented MVP surface, matched at
/// `verbose_level=2` (the level `onix_core`'s report shape always
/// corresponds to).
#[pyclass(module = "deepdiff_rs")]
pub(crate) struct DeepDiff {
    report_value: Value,
}

#[pymethods]
impl DeepDiff {
    #[new]
    #[pyo3(signature = (t1, t2, ignore_order=false, max_depth=None))]
    fn new(
        t1: &Bound<'_, PyAny>,
        t2: &Bound<'_, PyAny>,
        ignore_order: bool,
        max_depth: Option<usize>,
    ) -> PyResult<Self> {
        let max_depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
        let a = to_value(t1, max_depth)?;
        let b = to_value(t2, max_depth)?;
        let opts = DiffOptions {
            max_depth,
            ignore_order,
        };
        let report =
            onix_core::diff_with_options(&a, &b, &opts).map_err(|error| map_diff_error(&error))?;

        Ok(Self {
            report_value: report.to_json_value(),
        })
    }

    /// Byte-compatible with real `DeepDiff(...).to_json()` at
    /// `verbose_level=2` — the whole point of this crate.
    fn to_json(&self) -> PyResult<String> {
        serde_json::to_string(&self.report_value)
            .map_err(|error| PyValueError::new_err(error.to_string()))
    }

    /// The parsed form of [`Self::to_json`] — a native Python `dict`.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        value_to_pyobject(py, &self.report_value)
    }

    /// `if diff:` truthiness — falsy exactly when `t1`/`t2` had no
    /// differences (an empty report).
    fn __bool__(&self) -> bool {
        !is_empty_report(&self.report_value)
    }

    fn __repr__(&self) -> PyResult<String> {
        Ok(format!("DeepDiff({})", self.to_json()?))
    }
}

/// A [`DeepDiff`] report renders to `{}` (an empty JSON object) via
/// [`onix_core::Report::to_json_value`] when there are no findings — see
/// that function's own doc.
fn is_empty_report(value: &Value) -> bool {
    matches!(value, Value::Object(map) if map.is_empty())
}
