//! Native-stack-overflow hardening shared by both Python entry points
//! ([`crate::deepdiff::DeepDiff`] and [`crate::fast_path::diff_json`]).
//!
//! `onix_core`'s diff engine is natively recursive: it walks the two value
//! trees on the call stack, bounded by a `max_depth` budget counter but not
//! by any stack-safety mechanism. A caller is free to raise `max_depth` (that
//! is the whole reason the parameter is exposed), and a genuinely-unequal
//! input nested just under a raised bound makes the traversal recurse that
//! many frames deep. Past a few thousand levels that overflows an ordinary
//! thread stack and aborts the whole interpreter with an uncatchable
//! `SIGSEGV`, which no Python `try`/`except` can recover.
//!
//! Three mechanisms here make that impossible, so no input and no `max_depth`
//! reachable from Python can crash the process:
//!
//! 1. A hard ceiling ([`MAX_DEPTH_CEILING`]) on the `max_depth` a caller may
//!    request; anything above it is rejected up front with a catchable
//!    `ValueError` ([`resolve_options`]).
//! 2. A diff whose inputs are nested deeper than [`MAX_INLINE_DEPTH`] runs on
//!    a dedicated worker thread whose stack is sized so the recursive engine
//!    cannot overflow it even at the ceiling ([`diff_to_value`]), with the
//!    GIL released while it runs. Shallow diffs run inline on the calling
//!    thread to avoid the fixed cost of spawning a thread.
//! 3. Every recursive operation on the (potentially deep) result — its JSON
//!    serialization ([`serialize_value`]) and its `Drop` ([`drop_value_safely`])
//!    — is likewise routed to the sized worker when the result is deep, and
//!    the deep input values are dropped on the worker that diffed them.
//!
//! # Where the sizes come from
//!
//! Both depth constants are derived from the recursive engine's per-level
//! native stack cost, measured (not guessed) by a committed, runnable
//! example — see [`PER_LEVEL_STACK_BYTES`], which records the figure, the
//! example, and how to reproduce it. The two thresholds size their margins
//! against that one constant.

use onix_core::{DEFAULT_MAX_DEPTH, DiffOptions};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;

use crate::errors::map_diff_error;

/// The largest `max_depth` a Python caller may request through either entry
/// point. A value above this is rejected with a catchable `ValueError`
/// rather than risking a native stack overflow the interpreter cannot catch.
///
/// The default `max_depth` ([`onix_core::DEFAULT_MAX_DEPTH`], 512) is far
/// below this ceiling and is unaffected; the ceiling only ever rejects an
/// explicitly, unusually high caller-supplied value. Real-world JSON is
/// essentially never nested even into the hundreds, so 512 already covers
/// legitimate inputs with a wide margin, and this ceiling exists purely to
/// bound the adversarial worst case.
pub(crate) const MAX_DEPTH_CEILING: usize = 20_000;

/// Worst-case native stack, in bytes, one level of the recursive diff engine
/// costs — the single source of that figure for both the worker stack and the
/// inline-vs-worker threshold. Measured by
/// `crates/onix-core/examples/stack_frame_cost.rs` (`cargo run -p onix-core
/// --example stack_frame_cost`, and `--release`), which binary-searches the
/// deepest genuinely-unequal input that does not overflow a fixed stack: the
/// worst case is nested lists in a debug build (the profile `cargo test`
/// uses) at roughly 3.5 KiB/level, release roughly 0.9 KiB/level. Rounded up
/// to 4 KiB here.
const PER_LEVEL_STACK_BYTES: usize = 4_096;

/// Extra multiplier over the bare `ceiling * per-level` figure, so the worker
/// stack is comfortably larger than the deepest recursion the ceiling
/// permits.
const STACK_SAFETY_MARGIN: usize = 4;

/// The diff worker thread's stack size: enough for the recursive engine to
/// run at [`MAX_DEPTH_CEILING`] with a [`STACK_SAFETY_MARGIN`]-fold margin.
/// This is reserved virtual address space, committed lazily by the OS, so
/// only the pages a given diff actually touches cost real memory.
const WORKER_STACK_BYTES: usize = MAX_DEPTH_CEILING * PER_LEVEL_STACK_BYTES * STACK_SAFETY_MARGIN;

/// Depth up to which the recursive operations (the diff itself, plus
/// serializing or dropping its result) may run directly on the calling
/// thread; anything deeper is routed to the sized worker.
///
/// The calling thread's stack is out of this crate's control. Python's main
/// thread stack is large, but worker threads created with
/// `threading.stack_size()` — as web servers and async executors routinely
/// do — can be as small as 512 KiB. At [`PER_LEVEL_STACK_BYTES`] (4 KiB) per
/// level, a 512 KiB stack holds on the order of 128 levels of diff recursion
/// (512 KiB / 4 KiB) before overflowing, and somewhat fewer in practice
/// because thread bootstrap consumes some of it. This threshold, 32, sits
/// well below that, leaving room for that overhead and for the report's own
/// serialize/drop recursion. Real inputs are almost always far shallower than
/// this, so the inline path handles the overwhelming majority of diffs with
/// no thread-spawn overhead.
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

/// Diffs `a` and `b` and renders the report to a [`Value`], choosing where
/// the natively-recursive diff runs: inline on the calling thread when both
/// inputs are shallow (no thread-spawn cost), or on the sized worker thread
/// (GIL released) when either is nested past [`MAX_INLINE_DEPTH`]. In the
/// worker case `a` and `b` are moved in and dropped there, on the large
/// stack; in the inline case they are shallow, so dropping them here cannot
/// overflow.
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
            // Temporary bridge: the core diff consumes the compact
            // `onix_core::Value`, so convert here. The (natively recursive)
            // `From` conversion of a deep input runs on the sized worker
            // stack, alongside the diff itself. This conversion goes away once
            // the binding pipeline builds the compact value directly.
            let a = onix_core::Value::from(a);
            let b = onix_core::Value::from(b);
            onix_core::diff_with_options(&a, &b, &opts).map(|report| report.to_json_value())
        })?
        .map_err(|error| map_diff_error(&error))
    } else {
        // Temporary bridge (inline, shallow-input path): both inputs are
        // known shallow here (the `is_deep` guard above), so converting to the
        // compact `onix_core::Value` cannot overflow the calling thread. This
        // conversion goes away once the bindings build the compact value
        // directly.
        let a = onix_core::Value::from(a);
        let b = onix_core::Value::from(b);
        onix_core::diff_with_options(&a, &b, &opts)
            .map(|report| report.to_json_value())
            .map_err(|error| map_diff_error(&error))
    }
}

/// Serializes `value` to a JSON string, on the sized worker thread when
/// `deep` (the caller's [`is_deep`] verdict for `value`) is set, because
/// `serde_json`'s natively recursive serialization could then overflow the
/// calling thread; inline otherwise. The caller passes the verdict in so it
/// is computed once per value rather than re-walked here.
///
/// # Errors
///
/// `ValueError` if serialization fails (which, for a `Value`, does not happen
/// in practice) or the worker thread cannot be run.
pub(crate) fn serialize_value(py: Python<'_>, value: &Value, deep: bool) -> PyResult<String> {
    let serialized = if deep {
        run_on_worker(py, || serde_json::to_string(value))?
    } else {
        serde_json::to_string(value)
    };
    serialized.map_err(|error| PyValueError::new_err(error.to_string()))
}

/// Drops `value` safely regardless of nesting: on the sized worker thread
/// when `deep` (the caller's [`is_deep`] verdict for `value`) is set, because
/// `serde_json::Value`'s natively recursive `Drop` could then overflow the
/// calling thread's stack; inline otherwise. Callers with no `Python` token
/// (e.g. [`crate::deepdiff::DeepDiff`]'s `Drop`, and the bindings' error
/// paths) use this to hand a possibly-deep value off for destruction; it
/// spawns a plain owned thread (the value is moved into it) rather than
/// releasing the GIL. The caller passes the verdict in so it is computed once
/// per value rather than re-walked here.
pub(crate) fn drop_value_safely(value: Value, deep: bool) {
    if !deep {
        // Shallow: dropping here cannot overflow, and skips all thread
        // overhead — the overwhelmingly common case.
        return;
    }

    // If the spawn fails (resource exhaustion), the closure — and with it
    // `value` — is dropped inline by the failed spawn. That reintroduces the
    // overflow risk, but only in the extreme corner where the OS cannot
    // create a thread at all; nothing better is possible there.
    if let Ok(handle) = std::thread::Builder::new()
        .stack_size(WORKER_STACK_BYTES)
        .name("deepdiff-rs-drop".to_string())
        .spawn(move || drop(value))
    {
        let _ = handle.join();
    }
}

/// Runs `f` on a dedicated worker thread whose stack is large enough for the
/// recursive diff engine (and any recursive operation on its result) to run
/// at [`MAX_DEPTH_CEILING`] without overflowing, releasing the GIL while it
/// runs.
///
/// `f` must own or borrow only data that outlives the call; the worker is a
/// scoped thread, joined before this function returns, so a borrow of
/// `&self` data (e.g. the stored report `Value`) is fine.
///
/// # Errors
///
/// `RuntimeError` if the worker thread cannot be spawned (resource
/// exhaustion) or panics (an internal bug). The panic case cannot fire in
/// normal operation — `onix_core` is panic-free on the reachable paths — but
/// is surfaced as a catchable exception rather than aborting.
pub(crate) fn run_on_worker<F, T>(py: Python<'_>, f: F) -> PyResult<T>
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    // Construct no `PyErr` inside `detach` (the GIL is released there): the
    // closure returns a plain `Send` outcome, mapped to a `PyErr` afterwards
    // on the calling thread. `detach` is pyo3's GIL-release primitive
    // (formerly `allow_threads`).
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

/// Whether `value` is nested deeper than [`MAX_INLINE_DEPTH`], i.e. deep
/// enough that a recursive operation on it (diffing it, serializing it, or
/// dropping it) must run on the sized worker rather than the calling thread.
///
/// Iterative (explicit work-stack, no native recursion), so it is itself
/// safe to run on any input depth on the calling thread — which is exactly
/// why it operates on the still-`serde_json::Value` binding input rather than
/// converting to the compact `onix_core::Value` first: that conversion is
/// natively recursive and must run on the sized worker for a deep value, so
/// the routing decision has to come first.
///
/// Temporary bridge: this operates on the still-`serde_json::Value` binding
/// input and reimplements the depth walk locally, because
/// [`onix_core::exceeds_depth`] now takes the compact `onix_core::Value` and
/// converting to it here would itself recurse on the calling thread. Once the
/// binding pipeline builds the compact value directly, this reverts to
/// delegating to [`onix_core::exceeds_depth`].
#[must_use]
pub(crate) fn is_deep(value: &Value) -> bool {
    let mut stack: Vec<(&Value, usize)> = vec![(value, 0)];
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_INLINE_DEPTH {
            return true;
        }
        match node {
            Value::Array(items) => stack.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(map) => stack.extend(map.values().map(|item| (item, depth + 1))),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    false
}
