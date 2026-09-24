//! Native-stack-overflow hardening shared by both Python entry points
//! ([`crate::deepdiff::DeepDiff`] and [`crate::fast_path::diff_json`]).
//!
//! `onix_core`'s diff engine is natively recursive and can overflow the thread stack on deeply
//! nested input, aborting the interpreter with an uncatchable `SIGSEGV`. A hard ceiling on
//! `max_depth` ([`MAX_DEPTH_CEILING`], [`resolve_options`]) plus a sized worker thread for
//! diffing and serializing inputs nested past [`MAX_INLINE_DEPTH`] ([`diff_to_value`],
//! [`serialize_value`]) prevent that. `crate::convert`'s walk from Python objects runs on the
//! calling thread instead, so it must itself be iterative.

use onix_core::{DEFAULT_MAX_DEPTH, DiffOptions, Value};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use crate::errors::map_diff_error;

/// The largest `max_depth` a Python caller may request through either entry
/// point. A value above this is rejected with a catchable `ValueError`
/// rather than risking a native stack overflow the interpreter cannot catch.
pub(crate) const MAX_DEPTH_CEILING: usize = 20_000;

/// Worst-case native stack, in bytes, one level of the recursive diff engine
/// costs, measured by `crates/onix-core/examples/stack_frame_cost.rs` and
/// rounded up from its debug-build worst case (~3.5 KiB/level).
const PER_LEVEL_STACK_BYTES: usize = 4_096;

/// Extra multiplier over the bare `ceiling * per-level` figure, so the worker
/// stack is comfortably larger than the deepest recursion the ceiling
/// permits.
const STACK_SAFETY_MARGIN: usize = 4;

/// The diff worker thread's stack size: reserved virtual address space,
/// committed lazily, sized for [`MAX_DEPTH_CEILING`] with a
/// [`STACK_SAFETY_MARGIN`]-fold margin.
const WORKER_STACK_BYTES: usize = MAX_DEPTH_CEILING * PER_LEVEL_STACK_BYTES * STACK_SAFETY_MARGIN;

/// Depth up to which the recursive operations (the diff itself, plus
/// serializing or dropping its result) may run directly on the calling
/// thread; anything deeper is routed to the sized worker. Sized for thread
/// stacks of 512 KiB and up, the common server-executor size; a thread
/// configured near Python's 32 KiB minimum can still overflow on inline work.
const MAX_INLINE_DEPTH: usize = 32;

/// Resolves the two Python-supplied diff parameters into a [`DiffOptions`],
/// applying the default `max_depth` and enforcing [`MAX_DEPTH_CEILING`].
/// Shared by both entry points so the defaulting and the ceiling check live
/// in exactly one place.
///
/// # Errors
///
/// `ValueError` (naming the ceiling) if `max_depth` exceeds
/// [`MAX_DEPTH_CEILING`].
pub(crate) fn resolve_options(
    max_depth: Option<usize>,
    ignore_order: bool,
) -> PyResult<DiffOptions> {
    let max_depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
    if max_depth > MAX_DEPTH_CEILING {
        return Err(PyValueError::new_err(format!(
            "max_depth {max_depth} exceeds deepdiff_rs's ceiling of {MAX_DEPTH_CEILING}; \
             diffing values nested that deep cannot be done without risking a native stack \
             overflow that would crash the interpreter, so it is refused up front. Reduce \
             max_depth to at most {MAX_DEPTH_CEILING}."
        )));
    }
    Ok(DiffOptions {
        max_depth,
        ignore_order,
    })
}

/// Diffs `a` and `b` and renders the report to a [`Value`] (see
/// [`onix_core::Report::to_value`]), running the natively-recursive diff on
/// the sized worker thread when either input is nested past
/// [`MAX_INLINE_DEPTH`], inline otherwise.
///
/// # Errors
///
/// `deepdiff_rs.MaxDepthError` if the diff would exceed `opts.max_depth`.
pub(crate) fn diff_to_value(
    py: Python<'_>,
    a: Value,
    b: Value,
    opts: DiffOptions,
) -> PyResult<Value> {
    if is_deep(&a) || is_deep(&b) {
        run_on_worker(py, move || {
            onix_core::diff_with_options(&a, &b, &opts).map(|report| report.to_value())
        })?
        .map_err(|error| map_diff_error(&error))
    } else {
        onix_core::diff_with_options(&a, &b, &opts)
            .map(|report| report.to_value())
            .map_err(|error| map_diff_error(&error))
    }
}

/// Serializes `value` to a JSON string, on the sized worker thread when
/// `deep` is set (rendering is natively recursive too), inline otherwise.
/// `deep` and `may_have_wtf8` are the caller's own precomputed verdicts (see
/// [`is_deep`]) so this never re-walks `value` to answer either question.
///
/// # Errors
///
/// `RuntimeError` if the worker thread cannot be run (see
/// [`run_on_worker`]) — serialization itself cannot fail (see
/// [`to_json_string`]'s doc).
pub(crate) fn serialize_value(
    py: Python<'_>,
    value: &Value,
    deep: bool,
    may_have_wtf8: bool,
) -> PyResult<String> {
    Ok(if deep {
        run_on_worker(py, || to_json_string(value, may_have_wtf8))?
    } else {
        to_json_string(value, may_have_wtf8)
    })
}

/// Renders one compact [`Value`] to JSON text, matching real `DeepDiff`'s
/// `to_json()` for a non-finite float and a lone surrogate code point
/// (neither of which `serde_json`'s own path renders correctly); falls back
/// to the fast `serde_json` path when neither is present anywhere in `value`.
fn to_json_string(value: &Value, may_have_wtf8: bool) -> String {
    if !may_have_wtf8 && !needs_written_number(value) {
        return serde_json::to_string(&value.to_serde_json())
            .expect("a compact Value's to_serde_json() output always serializes");
    }
    let mut out = String::new();
    write_json(value, &mut out);
    out
}

/// Returns `true` if `value` contains a non-finite float or an
/// arbitrary-precision integer, either of which [`Value::to_serde_json`]
/// cannot render exactly and forces the hand-written [`write_json`] path.
fn needs_written_number(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_big().is_some() || n.as_f64().is_some_and(|f| !f.is_finite()),
        Value::Array(items) | Value::Tuple(items) => items.iter().any(needs_written_number),
        Value::Set(items) | Value::FrozenSet(items) => items.iter().any(needs_written_number),
        Value::Object(obj) => obj.values().any(needs_written_number),
        Value::Null
        | Value::Bool(_)
        | Value::Str(_)
        | Value::DateTime(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::TimeDelta(_) => false,
    }
}

/// [`to_json_string`]'s slow path: writes every node's JSON text by hand,
/// WTF-8-aware, touching each node once.
fn write_json(value: &Value, out: &mut String) {
    match value {
        Value::Number(n) => {
            if let Some(big) = n.as_big() {
                out.push_str(&big.to_string());
            } else {
                let f = n
                    .as_f64()
                    .expect("a non-Big Number is an i64, a u64, or an f64");
                if f.is_finite() {
                    out.push_str(
                        &serde_json::to_string(&value.to_serde_json())
                            .expect("a finite Number always serializes"),
                    );
                } else if f.is_nan() {
                    out.push_str("NaN");
                } else if f.is_sign_positive() {
                    out.push_str("Infinity");
                } else {
                    out.push_str("-Infinity");
                }
            }
        }
        Value::Str(s) => {
            out.push('"');
            onix_core::value::write_json_str_content(s.as_bytes(), out);
            out.push('"');
        }
        Value::Array(items) | Value::Tuple(items) => write_json_seq(items.iter(), out),
        Value::Set(items) | Value::FrozenSet(items) => write_json_seq(items.iter(), out),
        Value::Object(obj) => {
            out.push('{');
            for (index, (key, child)) in obj.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json_object_key(key, out);
                out.push(':');
                write_json(child, out);
            }
            out.push('}');
        }
        Value::Null
        | Value::Bool(_)
        | Value::DateTime(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::TimeDelta(_) => {
            out.push_str(
                &serde_json::to_string(&value.to_serde_json())
                    .expect("a Null/Bool/DateTime/Date/Time/TimeDelta always serializes"),
            );
        }
    }
}

/// Writes one [`onix_core::value::ObjectKey`] as a JSON string literal,
/// WTF-8-aware for a `str` key, the same way [`write_json`] renders a `Str`.
fn write_json_object_key(key: &onix_core::value::ObjectKey, out: &mut String) {
    match key {
        onix_core::value::ObjectKey::Str(s) => {
            out.push('"');
            onix_core::value::write_json_str_content(s.as_bytes(), out);
            out.push('"');
        }
        onix_core::value::ObjectKey::Other(_) => {
            out.push_str(
                &serde_json::to_string(&onix_core::value::object_key_json_string(key))
                    .expect("a String always serializes to a JSON string literal"),
            );
        }
    }
}

/// [`write_json`]'s array/tuple/set/frozenset case: every one of `Value`'s
/// sequence-shaped variants renders as a JSON array (matching real
/// `DeepDiff`'s `to_json()`; see `Value::to_serde_json`'s own doc).
fn write_json_seq<'a>(items: impl Iterator<Item = &'a Value>, out: &mut String) {
    out.push('[');
    for (index, item) in items.enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_json(item, out);
    }
    out.push(']');
}

/// Runs `f` on a dedicated worker thread sized to run the recursive diff
/// engine at [`MAX_DEPTH_CEILING`] without overflowing, GIL released. `f`
/// may borrow non-`'static` data because the worker is joined before this
/// function returns.
///
/// # Errors
///
/// `RuntimeError` if the worker thread cannot be spawned or panics.
pub(crate) fn run_on_worker<F, T>(py: Python<'_>, f: F) -> PyResult<T>
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    // `f` returns a plain Send outcome, mapped to a PyErr after `detach`
    // returns, since a PyErr cannot be built while the GIL is released.
    let outcome: Result<T, WorkerFailure> = py.detach(|| {
        std::thread::scope(|scope| {
            match std::thread::Builder::new()
                .stack_size(WORKER_STACK_BYTES)
                .name("deepdiff-rs-diff".to_string())
                .spawn_scoped(scope, f)
            {
                Ok(handle) => handle.join().map_err(|_| WorkerFailure::Panicked),
                Err(error) => Err(WorkerFailure::SpawnFailed(error.to_string())),
            }
        })
    });

    outcome.map_err(|failure| match failure {
        WorkerFailure::SpawnFailed(message) => PyRuntimeError::new_err(format!(
            "deepdiff_rs could not spawn its diff worker thread: {message}"
        )),
        WorkerFailure::Panicked => PyRuntimeError::new_err(
            "deepdiff_rs's diff worker thread panicked; this is an internal bug, please report it",
        ),
    })
}

/// A worker-thread failure, in a `Send` form so it can cross out of
/// [`run_on_worker`]'s GIL-released region before becoming a `PyErr`.
enum WorkerFailure {
    SpawnFailed(String),
    Panicked,
}

/// Whether `value` is nested past [`MAX_INLINE_DEPTH`] and must run its
/// recursive work on the sized worker. Delegates to
/// [`onix_core::exceeds_depth`], itself iterative and safe at any depth.
#[must_use]
pub(crate) fn is_deep(value: &Value) -> bool {
    onix_core::exceeds_depth(value, MAX_INLINE_DEPTH)
}
