//! Measures the native stack cost, per nesting level, of `onix_core`'s
//! recursive diff traversal — the empirical basis for the worker-thread
//! stack size and the inline-diff depth threshold in the `onix-py` bindings
//! (`crates/onix-py/src/guard.rs`).
//!
//! It diffs two genuinely-unequal values nested `depth` levels deep on a
//! thread with a fixed stack size, and binary-searches the deepest input
//! that does not overflow that stack. `bytes_per_level ≈ stack / max_ok_depth`
//! is then the per-level cost for this build profile and value shape.
//!
//! Each probe runs in a child process (this same binary, re-invoked with
//! `--probe`) so that an overflow is an exit status the parent can read,
//! not a signal that kills the measurement itself.
//!
//! Run (from the repository root):
//!
//! ```sh
//! cargo run --quiet -p onix-core --example stack_frame_cost            # debug
//! cargo run --quiet --release -p onix-core --example stack_frame_cost  # release
//! ```
//!
//! The worst case (largest bytes/level) is nested lists in a debug build,
//! which is what the bindings size their margins against.

use std::process::Command;

use onix_core::diff::diff_with_max_depth;
use serde_json::{Map, Value};

/// The fixed stack each probe thread is given while searching. Large enough
/// that the deepest non-overflowing input is in the thousands, so the
/// division has several significant figures.
const PROBE_STACK_BYTES: usize = 16 * 1024 * 1024;

fn build(shape: &str, depth: usize, leaf: i64) -> Value {
    let mut value = Value::from(leaf);
    for _ in 0..depth {
        if shape == "dict" {
            let mut map = Map::new();
            map.insert("k".to_owned(), value);
            value = Value::Object(map);
        } else {
            value = Value::Array(vec![value]);
        }
    }
    value
}

/// One probe: build two unequal `depth`-deep values, diff them, and exit
/// with a distinct status. Runs on a thread with `PROBE_STACK_BYTES` of
/// stack; if the recursion overflows, the process dies with a signal
/// instead of exiting cleanly, which is exactly the signal the parent reads.
fn run_probe(shape: &str, depth: usize) -> ! {
    let shape = shape.to_owned();
    let handle = std::thread::Builder::new()
        .stack_size(PROBE_STACK_BYTES)
        .spawn(move || {
            let a = build(&shape, depth, 1);
            let b = build(&shape, depth, 2);
            // `depth + 1` so the max_depth guard never trips before the
            // intended leaf finding at `depth`.
            let report =
                diff_with_max_depth(&a, &b, depth + 1).expect("depth budget covers the input");
            assert!(!report.is_empty(), "unequal inputs must produce a finding");
        })
        .expect("probe thread spawns");
    let survived = handle.join().is_ok();
    std::process::exit(i32::from(!survived));
}

/// Returns whether a probe at `depth` for `shape` survived (exited cleanly).
fn probe_survives(exe: &str, shape: &str, depth: usize) -> bool {
    Command::new(exe)
        .args(["--probe", shape, &depth.to_string()])
        .status()
        .expect("child probe runs")
        .success()
}

/// Binary-searches the deepest input `shape` that does not overflow
/// `PROBE_STACK_BYTES`, and reports the implied per-level cost.
fn measure(exe: &str, shape: &str) {
    let mut low = 10_usize;
    let mut high = 100_000_usize;
    while probe_survives(exe, shape, high) {
        low = high;
        high *= 2;
    }
    while high - low > 4 {
        let mid = usize::midpoint(low, high);
        if probe_survives(exe, shape, mid) {
            low = mid;
        } else {
            high = mid;
        }
    }
    let bytes_per_level = PROBE_STACK_BYTES / low;
    println!("{shape:>4}: max_ok_depth={low:>6}  bytes_per_level={bytes_per_level}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--probe") {
        let shape = args.get(2).map_or("list", String::as_str);
        let depth = args.get(3).and_then(|d| d.parse().ok()).unwrap_or(0);
        run_probe(shape, depth);
    }

    let exe = std::env::current_exe()
        .expect("current exe path")
        .to_string_lossy()
        .into_owned();
    println!("onix_core diff recursion stack cost (probe stack {PROBE_STACK_BYTES} bytes)");
    measure(&exe, "list");
    measure(&exe, "dict");
}
