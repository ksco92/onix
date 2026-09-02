//! The fast path: `diff_json(a, b, ignore_order=False, max_depth=None)`.
//!
//! Parses both inputs, diffs, and serializes the result back to a JSON
//! string entirely in Rust — no Python-object traversal at all, unlike
//! [`crate::deepdiff::DeepDiff`]. Use this when the caller already has (or
//! is happy to produce) JSON text rather than live Python objects.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::guard::{diff_to_value, drop_value_safely, is_deep, resolve_options, serialize_value};

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
    // parsed value is never nested past that, which keeps parsing itself off
    // the native-stack-overflow path. Independently of that cap, every
    // deep-value operation below (the diff, the report's serialization and
    // drop, and the drop of `a_value` on a `b` parse error) is routed onto the
    // sized worker whenever the value is past the inline depth threshold, so
    // those stay safe on their own.
    let a_value = parse_json(a, "a")?;
    let b_value = match parse_json(b, "b") {
        Ok(value) => value,
        Err(error) => {
            // `a` already parsed to a value that could (if the parser cap ever
            // changed) be deep; don't drop it inline on this error return.
            let a_is_deep = is_deep(&a_value);
            drop_value_safely(a_value, a_is_deep);
            return Err(error);
        }
    };
    // Runs the diff inline or on the sized worker depending on input depth,
    // dropping the parsed inputs wherever it ran.
    let report_value = diff_to_value(py, a_value, b_value, opts)?;
    let report_is_deep = is_deep(&report_value);
    // Serialize (on the worker if the report is deep), then drop the report
    // through the same safe path — its recursive `Drop` must not run inline on
    // a small caller stack.
    let json = serialize_value(py, &report_value, report_is_deep);
    drop_value_safely(report_value, report_is_deep);
    json
}

fn parse_json(text: &str, argument_name: &str) -> PyResult<serde_json::Value> {
    serde_json::from_str(text).map_err(|error| {
        PyValueError::new_err(format!(
            "failed to parse argument {argument_name:?} as JSON: {error}"
        ))
    })
}
