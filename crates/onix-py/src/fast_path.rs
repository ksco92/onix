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
use crate::guard::{check_max_depth_ceiling, run_on_worker};

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
///   (default [`DEFAULT_MAX_DEPTH`], 512).
#[pyfunction]
#[pyo3(signature = (a, b, ignore_order=false, max_depth=None))]
pub(crate) fn diff_json(
    py: Python<'_>,
    a: &str,
    b: &str,
    ignore_order: bool,
    max_depth: Option<usize>,
) -> PyResult<String> {
    let max_depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
    check_max_depth_ceiling(max_depth)?;
    // `serde_json::from_str` caps its own recursion at ~128 levels, so a
    // parsed value is never nested past that — comfortably under
    // `MAX_DEPTH_CEILING`, so the diff below cannot exceed the ceiling
    // whatever `max_depth` is. That parser cap is defense in depth here, not
    // the safety guarantee: the diff still runs on the sized worker thread
    // (like `DeepDiff`), so this path stays safe even if a future parser
    // change lifted that cap.
    let a_value = parse_json(a, "a")?;
    let b_value = parse_json(b, "b")?;
    let opts = DiffOptions {
        max_depth,
        ignore_order,
    };
    // The diff and the serialization of its (potentially deep) report are
    // both natively recursive, so both run on the stack-sized worker; the
    // parsed inputs and the report are all dropped there too.
    let outcome = run_on_worker(py, move || {
        let report = onix_core::diff_with_options(&a_value, &b_value, &opts)
            .map_err(DiffJsonError::Depth)?;
        serde_json::to_string(&report.to_json_value())
            .map_err(|error| DiffJsonError::Serialize(error.to_string()))
    })?;

    match outcome {
        Ok(json) => Ok(json),
        Err(DiffJsonError::Depth(error)) => Err(map_diff_error(&error)),
        Err(DiffJsonError::Serialize(message)) => Err(PyValueError::new_err(message)),
    }
}

/// A `diff_json` failure in a `Send` form, so the diff worker thread (which
/// runs with the GIL released and cannot construct a `PyErr`) can hand its
/// failure back to be turned into a Python exception on the calling thread.
enum DiffJsonError {
    Depth(onix_core::Error),
    Serialize(String),
}

fn parse_json(text: &str, argument_name: &str) -> PyResult<serde_json::Value> {
    serde_json::from_str(text).map_err(|error| {
        PyValueError::new_err(format!(
            "failed to parse argument {argument_name:?} as JSON: {error}"
        ))
    })
}
