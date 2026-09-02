//! `PyO3` bindings for `onix_core`, published to `PyPI` as `deepdiff-rs`
//! (Python import name `deepdiff_rs`).
//!
//! # Architecture
//!
//! Two independent entry points, both documented in full on their own
//! items (all private — this crate is a `cdylib` consumed from Python, not
//! a Rust library, so its module tree has no public Rust API to link
//! against; see each module's own doc comment instead):
//!
//! - `deepdiff::DeepDiff` — the drop-in subset of `deepdiff.DeepDiff`:
//!   accepts live Python objects, converts them to `onix_core`'s value
//!   model exactly once (see `convert`'s module doc for the full
//!   conversion table), then diffs and renders natively.
//! - `fast_path::diff_json` — parses two JSON strings, diffs, and
//!   serializes the result, entirely in Rust with no Python-object
//!   traversal at all.
//!
//! `errors` holds the Python-visible exception type (`errors::MaxDepthError`)
//! both entry points raise instead of ever letting
//! `onix_core::Error::MaxDepthExceeded` — or a native stack overflow on
//! adversarially deep input — escape as anything else. `guard` holds the
//! shared native-stack-overflow hardening (the `max_depth` ceiling and the
//! sized diff worker thread) both entry points route their diff through.
mod convert;
mod deepdiff;
mod errors;
mod fast_path;
mod guard;

use pyo3::prelude::*;

#[pymodule]
fn deepdiff_rs(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<deepdiff::DeepDiff>()?;
    m.add_function(wrap_pyfunction!(fast_path::diff_json, m)?)?;
    m.add("MaxDepthError", py.get_type::<errors::MaxDepthError>())?;
    m.add("MAX_DEPTH_CEILING", guard::MAX_DEPTH_CEILING)?;
    Ok(())
}
