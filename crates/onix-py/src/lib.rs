//! `PyO3` bindings for `onix_core`, published to `PyPI` as `deepdiff-rs`
//! (Python import name `deepdiff_rs`).
//!
//! # Architecture
//!
//! Three independent entry points, each documented in full on its own
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
//! - `arrow::diff_tables` — diffs two Arrow tables (from pyarrow, polars,
//!   or `DuckDB`) through the Arrow C Data Interface, using the `onix_arrow`
//!   crate. It is a separate value world from the two above: it never
//!   builds an `onix_core::Value`, so it does not touch `convert`,
//!   `errors::MaxDepthError`, or the `guard` machinery below — it compares
//!   Arrow schemas directly. Its own module doc covers its error mapping.
//!
//! `errors` holds the Python-visible exception type (`errors::MaxDepthError`)
//! the first two entry points raise instead of ever letting
//! `onix_core::Error::MaxDepthExceeded` — or a native stack overflow on
//! adversarially deep input — escape as anything else. `guard` holds the
//! shared native-stack-overflow hardening (the `max_depth` ceiling and the
//! sized diff worker thread) those two entry points route their diff through.
mod arrow;
mod convert;
mod deepdiff;
mod errors;
mod fast_path;
mod guard;

use pyo3::prelude::*;

#[pymodule]
fn deepdiff_rs(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<deepdiff::DeepDiff>()?;
    m.add_class::<arrow::TableDiff>()?;
    m.add_class::<arrow::ArrowTable>()?;
    m.add_function(wrap_pyfunction!(arrow::diff_tables, m)?)?;
    m.add_function(wrap_pyfunction!(fast_path::diff_json, m)?)?;
    m.add("MaxDepthError", py.get_type::<errors::MaxDepthError>())?;
    m.add("MAX_DEPTH_CEILING", guard::MAX_DEPTH_CEILING)?;
    Ok(())
}
