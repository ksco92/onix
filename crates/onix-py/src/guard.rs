//! Native-stack-overflow hardening shared by both Python entry points
//! ([`crate::deepdiff::DeepDiff`] and [`crate::fast_path::diff_json`]).
//!
//! `onix_core`'s diff engine is natively recursive: [`onix_core::diff`]
//! walks the two value trees on the call stack, bounded by a `max_depth`
//! *budget counter* but not by any stack-safety mechanism. That counter
//! makes the default (`max_depth = 512`) safe on any normal thread stack,
//! but a caller is free to raise `max_depth` — that is the whole reason the
//! parameter is exposed — and a genuinely-unequal input nested just under a
//! raised bound makes the traversal recurse that many frames deep. Past a
//! few thousand levels that overflows an ordinary thread stack and aborts
//! the whole interpreter with an uncatchable `SIGSEGV`, which no amount of
//! Python `try`/`except` can recover.
//!
//! Two mechanisms here make that structurally impossible, so no input and
//! no `max_depth` reachable from Python can crash the process:
//!
//! 1. A hard ceiling ([`MAX_DEPTH_CEILING`]) on the `max_depth` a caller may
//!    request. Anything above it is rejected up front with a catchable
//!    `ValueError` ([`check_max_depth_ceiling`]); the default is far below
//!    it and is unaffected.
//! 2. The diff itself (and every recursive operation on its potentially
//!    deep result — serialization and `Drop` of the report `Value`) runs on
//!    a dedicated worker thread whose stack is sized so that the recursive
//!    engine, at the ceiling depth, cannot overflow it ([`run_on_worker`]).
//!    The GIL is released while that worker runs.
//!
//! # Where the stack size comes from
//!
//! [`WORKER_STACK_BYTES`] is derived from an empirical measurement, not a
//! guess. Diffing two genuinely-unequal values nested `D` levels deep, on a
//! thread with a known stack size, and binary-searching the largest `D`
//! that does not overflow gives the per-level native stack cost of the
//! recursive engine for the worst-case shape (nested lists, whose frames
//! are larger than nested dicts'):
//!
//! | build profile | bytes / level (measured) |
//! | --- | --- |
//! | release | ~0.9 KiB |
//! | debug (`cargo test` builds this) | ~3.5 KiB |
//!
//! [`PER_LEVEL_STACK_BYTES`] rounds the worst case (debug) up to 4 KiB, and
//! [`WORKER_STACK_BYTES`] multiplies that by the ceiling and a further
//! [`STACK_SAFETY_MARGIN`] of 4. The result was verified to complete a
//! debug diff of both nested lists and nested dicts at the full ceiling
//! depth, with headroom to spare well beyond it.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;

/// The largest `max_depth` a Python caller may request through either entry
/// point. A value above this is rejected with a catchable `ValueError`
/// rather than risking a native stack overflow the interpreter cannot
/// catch — see this module's doc for why. The default `max_depth`
/// ([`onix_core::DEFAULT_MAX_DEPTH`], 512) is far below it and is
/// unaffected; this ceiling only ever rejects an explicitly, unusually high
/// caller-supplied value (real JSON is essentially never nested even into
/// the hundreds).
pub(crate) const MAX_DEPTH_CEILING: usize = 20_000;

/// Worst-case native stack, in bytes, one level of the recursive diff
/// engine costs — the measured debug figure (~3.5 KiB/level, the profile
/// `cargo test` builds) rounded up to 4 KiB. See this module's doc for how
/// it was measured.
const PER_LEVEL_STACK_BYTES: usize = 4_096;

/// Extra multiplier over the bare `ceiling * per-level` figure, so the
/// worker stack is comfortably larger than the deepest recursion the
/// ceiling permits — never a hair's breadth from overflow.
const STACK_SAFETY_MARGIN: usize = 4;

/// The diff worker thread's stack size: enough for the recursive engine to
/// run at [`MAX_DEPTH_CEILING`] with a [`STACK_SAFETY_MARGIN`]-fold margin.
/// At the constants above this is `20_000 * 4096 * 4` = 327,680,000 bytes
/// (~312 MiB) of *virtual* stack — reserved, not committed, so only the
/// pages a given diff actually touches cost real memory.
const WORKER_STACK_BYTES: usize = MAX_DEPTH_CEILING * PER_LEVEL_STACK_BYTES * STACK_SAFETY_MARGIN;

/// Depth up to which the report `Value` can be serialized or dropped
/// directly on the calling (Python) thread without risking a native stack
/// overflow. The calling thread's stack is out of this crate's control and
/// may be as small as a few megabytes — a deep dict `Value`'s natively
/// recursive `Drop` overflows a 16 MiB stack in a debug build well before
/// the ceiling — so anything nested deeper than this is handled on the
/// sized worker instead ([`report_needs_worker`]). Chosen very
/// conservatively: real diff reports are nested only a handful of levels,
/// so this branch practically always stays on the calling thread with zero
/// extra thread overhead.
const SAFE_CALLING_THREAD_DEPTH: usize = 1_000;

/// Rejects a caller-supplied `max_depth` above [`MAX_DEPTH_CEILING`] with a
/// catchable `ValueError` whose message names the ceiling.
///
/// # Errors
///
/// `ValueError` if `max_depth > MAX_DEPTH_CEILING`.
pub(crate) fn check_max_depth_ceiling(max_depth: usize) -> PyResult<()> {
    if max_depth > MAX_DEPTH_CEILING {
        return Err(PyValueError::new_err(format!(
            "max_depth {max_depth} exceeds deepdiff_rs's ceiling of {MAX_DEPTH_CEILING}; \
             diffing values nested that deep cannot be done without risking a native stack \
             overflow that would crash the interpreter, so it is refused up front. Reduce \
             max_depth to at most {MAX_DEPTH_CEILING}."
        )));
    }
    Ok(())
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
/// normal operation — `onix_core` is panic-free on the reachable paths —
/// but is surfaced as a catchable exception rather than aborting.
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

/// Whether `report` is nested deeply enough that serializing or dropping it
/// on the calling thread could overflow that thread's stack, so those
/// operations should be routed to the sized worker instead. See
/// [`SAFE_CALLING_THREAD_DEPTH`].
#[must_use]
pub(crate) fn report_needs_worker(report: &Value) -> bool {
    exceeds_depth(report, SAFE_CALLING_THREAD_DEPTH)
}

/// Returns `true` if `value` is nested strictly deeper than `limit` levels,
/// treating `value` as its own root (depth `0`). Iterative (an explicit
/// heap work-stack, no native recursion), so it is itself safe on any stack
/// and exits as soon as one node past `limit` is seen without visiting the
/// rest.
fn exceeds_depth(value: &Value, limit: usize) -> bool {
    let mut stack: Vec<(&Value, usize)> = vec![(value, 0)];

    while let Some((node, depth)) = stack.pop() {
        if depth > limit {
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

/// Drops `value` safely regardless of how deeply it is nested: on the sized
/// worker thread if it is deep enough that `serde_json::Value`'s natively
/// recursive `Drop` could overflow the calling thread's stack
/// ([`report_needs_worker`]), inline otherwise. Used from
/// [`crate::deepdiff::DeepDiff`]'s `Drop`, which has no `Python` token, so
/// this spawns a plain owned thread (the value is moved into it) rather than
/// releasing the GIL.
pub(crate) fn drop_report(value: Value) {
    if !report_needs_worker(&value) {
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
