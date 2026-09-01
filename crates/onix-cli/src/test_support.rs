//! Test-only support shared between `src/tests.rs`'s unit tests and
//! `tests/cli.rs`'s integration tests.
//!
//! `onix-cli` is a binary-only crate (no library target), so the usual
//! "shared test helper lives in a lib" trick isn't available; instead both
//! test binaries pull this file in verbatim via `#[path = "..."] mod
//! test_support;` (see its two call sites) rather than each keeping its own
//! copy — the two copies had already drifted slightly (different temp-file
//! name prefixes) before this file existed.
//!
//! Never referenced from `main()`'s own non-test code path, so it compiles
//! into neither the release nor the debug `onix` binary — only into the two
//! `#[cfg(test)]`/integration test binaries that declare the `mod`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A process-wide counter so temp file names stay unique across every test
/// that calls [`write_temp_file`] within one test binary, even though
/// `cargo test` runs tests in parallel by default.
static TEMP_FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Writes `contents` to a fresh, uniquely-named file under the OS temp
/// directory and returns its path. Not cleaned up automatically:
/// `std::env::temp_dir()` is OS-managed scratch space, and leaving a handful
/// of tiny files behind after a test run is a fine trade against the
/// complexity of a drop-guard for this test-only helper.
pub(crate) fn write_temp_file(name: &str, contents: &str) -> PathBuf {
    let n = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("onix-cli-test-{}-{n}-{name}", std::process::id()));
    std::fs::write(&path, contents).expect("failed to write test fixture file");
    path
}
