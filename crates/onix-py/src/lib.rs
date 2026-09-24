//! `PyO3` bindings for `onix_core`, published to `PyPI` as `deepdiff-rs`
//! (Python import name `deepdiff_rs`).
//!
//! Three entry points — `deepdiff::DeepDiff`, `fast_path::diff_json`, and
//! `arrow::diff_tables` — each documented on its own item; `errors` and
//! `guard` hold the exception type and the stack-overflow hardening they
//! share.
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
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
