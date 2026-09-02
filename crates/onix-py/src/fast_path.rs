//! The fast path: `diff_json(a, b, ignore_order=False, max_depth=None)`.
//!
//! Parses both inputs, diffs, and serializes the result back to a JSON
//! string entirely in Rust — no Python-object traversal at all, unlike
//! [`crate::deepdiff::DeepDiff`]. Use this when the caller already has (or
//! is happy to produce) JSON text rather than live Python objects.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::guard::{diff_to_value, drop_value_safely, resolve_options, serialize_value};

/// Diffs two JSON documents and returns a `DeepDiff`-compatible JSON report
/// string (`verbose_level=2` shape) — see [`crate::deepdiff::DeepDiff`] for
/// the equivalent live-Python-object entry point.
///
/// # Errors
///
/// - `ValueError` if `a` or `b` fails to parse as JSON.
/// - `ValueError` if `max_depth` exceeds `deepdiff_rs.MAX_DEPTH_CEILING`
///   (see [`crate::guard`]).
/// - `deepdiff_rs.MaxDepthError` if diffing would recurse past `max_depth`
///   (default `onix_core::DEFAULT_MAX_DEPTH`, 512).
#[pyfunction]
#[pyo3(signature = (a, b, ignore_order=false, max_depth=None))]
pub(crate) fn diff_json(
    py: Python<'_>,
    a: &str,
    b: &str,
    ignore_order: bool,
    max_depth: Option<usize>,
) -> PyResult<String> {
    let opts = resolve_options(max_depth, ignore_order)?;
    // `serde_json::from_str` caps its own recursion at ~128 levels, so a
    // parsed value is never nested past that — comfortably under
    // `MAX_DEPTH_CEILING`. That parser cap is defense in depth, not the safety
    // guarantee: `diff_to_value`/`serialize_value` route anything past the
    // inline depth threshold onto the sized worker thread regardless, so this
    // path stays safe even if a future parser change lifted that cap.
    let a_value = parse_json(a, "a")?;
    let b_value = parse_json(b, "b")?;
    // Runs the diff inline or on the sized worker depending on input depth,
    // dropping the parsed inputs wherever it ran.
    let report_value = diff_to_value(py, a_value, b_value, opts)?;
    // Serialize (on the worker if the report is deep), then drop the report
    // through the same safe path — its recursive `Drop` must not run inline on
    // a small caller stack.
    let json = serialize_value(py, &report_value);
    drop_value_safely(report_value);
    json
}

fn parse_json(text: &str, argument_name: &str) -> PyResult<serde_json::Value> {
    serde_json::from_str(text).map_err(|error| {
        PyValueError::new_err(format!(
            "failed to parse argument {argument_name:?} as JSON: {error}"
        ))
    })
}
