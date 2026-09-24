//! Python-visible exception types this crate raises, plus the shared
//! mapping from an [`onix_core::Error`] to one of them.

use pyo3::create_exception;
use pyo3::exceptions::PyValueError;

create_exception!(
    deepdiff_rs,
    MaxDepthError,
    PyValueError,
    "Raised when diffing two values would need to recurse past the configured `max_depth` \
     (the same guard `onix_core` enforces natively, surfaced here as a catchable Python \
     exception instead of a native crash). A `ValueError` subclass, so callers that only \
     catch `ValueError` still catch this."
);

/// Maps an [`onix_core::Error`] to the [`pyo3::PyErr`] a Python caller sees.
pub(crate) fn map_diff_error(error: &onix_core::Error) -> pyo3::PyErr {
    match error {
        onix_core::Error::MaxDepthExceeded { .. } => MaxDepthError::new_err(error.to_string()),
        onix_core::Error::DateTimeOutOfRange { .. } => PyValueError::new_err(error.to_string()),
    }
}
