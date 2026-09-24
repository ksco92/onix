//! The fast path: `diff_json(a, b, ignore_order=False, max_depth=None)`.
//! Parses, diffs and serializes back to JSON entirely in Rust, with no
//! Python-object traversal, unlike [`crate::deepdiff::DeepDiff`].

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::guard::{diff_to_value, is_deep, resolve_options, serialize_value};

/// Diffs two JSON documents and returns a `DeepDiff`-compatible JSON report
/// string (`verbose_level=2` shape).
///
/// # Errors
///
/// - `ValueError` if `a` or `b` fails to parse as JSON, or if `max_depth`
///   exceeds `deepdiff_rs.MAX_DEPTH_CEILING` (see [`crate::guard`]).
/// - `deepdiff_rs.MaxDepthError` if diffing would recurse past `max_depth`.
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
    // Stack safety: diffing, see `guard`'s doc; parsing/dropping, see `onix_core::value`'s.
    let a_value = parse_json(a, "a")?;
    let b_value = parse_json(b, "b")?;
    let report_value = diff_to_value(py, &a_value, &b_value, opts, &mut |_| None, false)?;
    // `false`: parsed JSON text can never hold a lone surrogate escape.
    serialize_value(py, &report_value, is_deep(&report_value), false)
}

fn parse_json(text: &str, argument_name: &str) -> PyResult<onix_core::Value> {
    serde_json::from_str::<onix_core::Value>(text).map_err(|error| {
        PyValueError::new_err(format!(
            "failed to parse argument {argument_name:?} as JSON: {error}"
        ))
    })
}
