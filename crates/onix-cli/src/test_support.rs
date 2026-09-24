//! Test-only support shared between `src/tests.rs` and `tests/cli.rs` via
//! `#[path = "..."] mod test_support;` (`onix-cli` has no lib target).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Keeps temp file names unique across parallel tests within one test binary.
static TEMP_FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Writes `contents` to a fresh, uniquely-named file under the OS temp
/// directory and returns its path.
pub(crate) fn write_temp_file(name: &str, contents: &str) -> PathBuf {
    let n = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("onix-cli-test-{}-{n}-{name}", std::process::id()));
    std::fs::write(&path, contents).expect("failed to write test fixture file");
    path
}
