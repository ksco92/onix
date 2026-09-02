//! The drop-in `DeepDiff` class: accepts live Python objects, converts them
//! to `onix_core`'s value model exactly once, diffs natively, and exposes
//! the result as `.to_json()`/`.to_dict()` — see this module's `DeepDiff`
//! doc for the full, documented MVP surface.

use pyo3::prelude::*;
use serde_json::Value;

use crate::convert::{to_value, value_to_pyobject};
use crate::guard::{diff_to_value, drop_value_safely, is_deep, resolve_options, serialize_value};

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
    /// Whether `report_value` is nested past the inline-depth threshold, and
    /// so must be serialized and dropped on the sized worker thread rather
    /// than the calling thread. Computed once in `new`, so `to_json` and
    /// `Drop` do not each re-walk the report. See `crate::guard::is_deep`.
    report_is_deep: bool,
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
        let opts = resolve_options(max_depth, ignore_order)?;
        // Conversion is iterative (no native recursion — see `crate::convert`)
        // and needs the GIL to read the live Python objects, so it stays on
        // the calling thread.
        let a = to_value(t1, opts.max_depth)?;
        // If converting `t2` fails after `a` is already a (possibly deep)
        // legal `Value`, the early return must not drop `a` inline: its
        // recursive `Drop` could overflow this calling thread. Route it
        // through the sized-worker drop path first.
        let b = match to_value(t2, opts.max_depth) {
            Ok(b) => b,
            Err(error) => {
                let a_is_deep = is_deep(&a);
                drop_value_safely(a, a_is_deep);
                return Err(error);
            }
        };
        // The diff is natively recursive: it runs inline when both inputs are
        // shallow, else on the stack-sized worker (GIL released), which also
        // drops `a`, `b`, and the intermediate report on its large stack.
        let report_value = diff_to_value(py, a, b, opts)?;
        let report_is_deep = is_deep(&report_value);

        Ok(Self {
            report_value,
            report_is_deep,
        })
    }

    /// Byte-compatible with real `DeepDiff(...).to_json()` at
    /// `verbose_level=2` — the whole point of this crate.
    ///
    /// `serde_json`'s serialization of a `Value` is itself natively
    /// recursive, so a report deep enough to matter is serialized on the
    /// sized worker thread; a shallow one (the overwhelmingly common case)
    /// serializes inline. See `crate::guard::serialize_value`.
    fn to_json(&self, py: Python<'_>) -> PyResult<String> {
        serialize_value(py, &self.report_value, self.report_is_deep)
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
        // report nested past the inline threshold would overflow the calling
        // thread's stack if dropped here. `drop_value_safely` drops a deep
        // report on the sized worker thread and a shallow one inline.
        drop_value_safely(
            std::mem::replace(&mut self.report_value, Value::Null),
            self.report_is_deep,
        );
    }
}

/// A [`DeepDiff`] report renders to `{}` (an empty JSON object) via
/// [`onix_core::Report::to_json_value`] when there are no findings — see
/// that function's own doc.
fn is_empty_report(value: &Value) -> bool {
    matches!(value, Value::Object(map) if map.is_empty())
}
