//! The fast path: `diff_json(a, b, ignore_order=False, max_depth=None)`.
//!
//! Parses both inputs, diffs, and serializes the result back to a JSON
//! string entirely in Rust — no Python-object traversal at all, unlike
//! [`crate::deepdiff::DeepDiff`]. Use this when the caller already has (or
//! is happy to produce) JSON text rather than live Python objects.

use onix_core::{DEFAULT_MAX_DEPTH, DiffOptions};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::errors::map_diff_error;

/// Diffs two JSON documents and returns a `DeepDiff`-compatible JSON report
/// string (`verbose_level=2` shape) — see [`crate::deepdiff::DeepDiff`] for
/// the equivalent live-Python-object entry point.
///
/// # Errors
///
/// - `ValueError` if `a` or `b` fails to parse as JSON.
/// - `deepdiff_rs.MaxDepthError` if diffing would recurse past `max_depth`
///   (default [`DEFAULT_MAX_DEPTH`], 512).
#[pyfunction]
#[pyo3(signature = (a, b, ignore_order=false, max_depth=None))]
pub(crate) fn diff_json(
    a: &str,
    b: &str,
    ignore_order: bool,
    max_depth: Option<usize>,
) -> PyResult<String> {
    let a_value = parse_json(a, "a")?;
    let b_value = parse_json(b, "b")?;
    let opts = DiffOptions {
        max_depth: max_depth.unwrap_or(DEFAULT_MAX_DEPTH),
        ignore_order,
    };
    let report = onix_core::diff_with_options(&a_value, &b_value, &opts)
        .map_err(|error| map_diff_error(&error))?;

    serde_json::to_string(&report.to_json_value())
        .map_err(|error| PyValueError::new_err(error.to_string()))
}

fn parse_json(text: &str, argument_name: &str) -> PyResult<serde_json::Value> {
    serde_json::from_str(text).map_err(|error| {
        PyValueError::new_err(format!(
            "failed to parse argument {argument_name:?} as JSON: {error}"
        ))
    })
}
