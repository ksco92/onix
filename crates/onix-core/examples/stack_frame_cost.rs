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
//! cargo run --quiet -p onix-core --example stack_frame_cost -- --max-bytes-per-level N
//! ```
//!
//! With `--max-bytes-per-level`, the run exits 1 when any shape exceeds `N`.
//!
//! `-- --handoff` instead measures the fixed cost above the first engine
//! level: the smallest thread stack, to 1 KiB, on which an unequal `list` diff,
//! its `Report::to_value` and the report's JSON rendering succeed, at depths
//! 100 and 300. The line's `base` is the intercept of those two points, the
//! frames every `onix-py` worker pays at any `max_depth` (the sizing floor in
//! `crates/onix-py/src/guard.rs`); a value within a few KiB of zero means the
//! fixed cost is below the 1 KiB search step and the fit's noise.
//!
//! The worst case (largest bytes/level) is `pairing`, an `ignore_order` list
//! nested at every level beside two shared strings, in a debug build.

use std::process::Command;

use onix_core::diff::{DiffOptions, diff_with_options};
use serde_json::{Map, Value};

/// The fixed stack each probe thread is given while searching. Large enough
/// that the deepest non-overflowing input is in the thousands, so the
/// division has several significant figures.
const PROBE_STACK_BYTES: usize = 16 * 1024 * 1024;

/// The `pairing` shape's probe stack: its run time grows with the cube of
/// the depth, so it searches a stack that overflows in the hundreds.
const PAIRING_PROBE_STACK_BYTES: usize = 1024 * 1024;

fn probe_stack_bytes(shape: &str) -> usize {
    if shape == "pairing" {
        PAIRING_PROBE_STACK_BYTES
    } else {
        PROBE_STACK_BYTES
    }
}

fn build(shape: &str, depth: usize, leaf: i64) -> Value {
    let mut value = Value::from(leaf);
    for _ in 0..depth {
        if shape == "dict" {
            let mut map = Map::new();
            map.insert("k".to_owned(), value);
            value = Value::Object(map);
        } else if shape == "pairing" {
            value = Value::Array(vec![value, "s1".into(), "s2".into()]);
        } else {
            value = Value::Array(vec![value]);
        }
    }
    value
}

/// One probe: build two unequal `depth`-deep values, diff them, and exit
/// with a distinct status. Runs on a thread with `probe_stack_bytes` of
/// stack; if the recursion overflows, the process dies with a signal
/// instead of exiting cleanly, which is exactly the signal the parent reads.
fn run_probe(shape: &str, depth: usize) -> ! {
    let shape = shape.to_owned();
    let handle = std::thread::Builder::new()
        .stack_size(probe_stack_bytes(&shape))
        .spawn(move || {
            let a = build(&shape, depth, 1);
            let b = build(&shape, depth, 2);
            // `depth + 1` so the max_depth guard never trips before the
            // intended leaf finding at `depth`.
            // Temporary bridge: the engine consumes the compact
            // onix_core::Value; convert here (runs on the sized probe thread
            // alongside the diff it measures).
            let a = onix_core::Value::from(a);
            let b = onix_core::Value::from(b);
            let opts = DiffOptions {
                max_depth: depth + 1,
                ignore_order: shape == "pairing",
            };
            let report = diff_with_options(&a, &b, &opts).expect("depth budget covers the input");
            assert!(!report.is_empty(), "unequal inputs must produce a finding");
        })
        .expect("probe thread spawns");
    let survived = handle.join().is_ok();
    std::process::exit(i32::from(!survived));
}

/// One handoff probe: the minimal worker body (diff, `to_value`, JSON text)
/// on a thread with `stack` bytes, over a `depth`-deep `list`.
fn run_handoff_probe(stack: usize, depth: usize) -> ! {
    let handle = std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            let a = onix_core::Value::from(build("list", depth, 1));
            let b = onix_core::Value::from(build("list", depth, 2));
            let opts = DiffOptions {
                max_depth: depth + 1,
                ignore_order: false,
            };
            let report = diff_with_options(&a, &b, &opts).expect("depth budget covers the input");
            serde_json::to_string(&report.to_value().to_serde_json()).expect("report serializes");
        })
        .expect("probe thread spawns");
    std::process::exit(i32::from(handle.join().is_err()));
}

/// Binary-searches the smallest stack, in 1 KiB steps, on which the handoff
/// body survives at `depth`.
fn min_handoff_stack(exe: &str, depth: usize) -> usize {
    let survives = |stack: usize| {
        Command::new(exe)
            .args(["--probe-handoff", &stack.to_string(), &depth.to_string()])
            .status()
            .expect("child probe runs")
            .success()
    };
    let (mut low, mut high) = (16 * 1024_usize, 16 * 1024 * 1024_usize);
    while high - low > 1024 {
        let mid = usize::midpoint(low, high);
        if survives(mid) {
            high = mid;
        } else {
            low = mid;
        }
    }
    high
}

/// The fixed handoff cost: the intercept of the minimum stack at two depths,
/// which a thread's platform minimum stack would otherwise hide.
fn measure_handoff(exe: &str) -> isize {
    let (near, far) = (100_usize, 300_usize);
    let (s_near, s_far) = (min_handoff_stack(exe, near), min_handoff_stack(exe, far));
    let per_level = (s_far - s_near) / (far - near);
    let base = isize::try_from(s_near).expect("stack fits isize")
        - isize::try_from(near * per_level).expect("stack fits isize");
    println!(
        "handoff: min_ok_stack@{near}={s_near} @{far}={s_far} bytes_per_level={per_level} base={base}"
    );
    base
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
/// `probe_stack_bytes`, prints and returns the implied per-level cost.
fn measure(exe: &str, shape: &str) -> usize {
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
    let stack = probe_stack_bytes(shape);
    let bytes_per_level = stack / low;
    println!(
        "{shape:>7}: stack={stack:>8}  max_ok_depth={low:>6}  bytes_per_level={bytes_per_level}"
    );
    bytes_per_level
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--probe") {
        let shape = args.get(2).map_or("list", String::as_str);
        let depth = args.get(3).and_then(|d| d.parse().ok()).unwrap_or(0);
        run_probe(shape, depth);
    }

    if args.get(1).map(String::as_str) == Some("--probe-handoff") {
        let stack = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(0);
        let depth = args.get(3).and_then(|d| d.parse().ok()).unwrap_or(0);
        run_handoff_probe(stack, depth);
    }

    let exe = std::env::current_exe()
        .expect("current exe path")
        .to_string_lossy()
        .into_owned();
    if args.iter().any(|a| a == "--handoff") {
        measure_handoff(&exe);
        return;
    }
    println!("onix_core diff recursion stack cost");
    let max = args
        .iter()
        .position(|a| a == "--max-bytes-per-level")
        .map(|i| {
            args.get(i + 1)
                .and_then(|n| n.parse::<usize>().ok())
                .expect("--max-bytes-per-level takes a number")
        });
    for shape in ["list", "dict", "pairing"] {
        let bytes_per_level = measure(&exe, shape);
        if let Some(max) = max.filter(|&max| bytes_per_level > max) {
            eprintln!(
                "{shape}: bytes_per_level={bytes_per_level} exceeds --max-bytes-per-level={max}"
            );
            std::process::exit(1);
        }
    }
}
