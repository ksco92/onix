//! Measures the native stack cost, per nesting level, of the recursive walks
//! over an Arrow `DataType` that `crates/onix-arrow/src/schema.rs` guards with
//! [`onix_arrow::MAX_NESTING_DEPTH`] — the empirical basis for that bound.
//!
//! The bound protects several recursive operations on a deeply nested type:
//! `schema::normalized_type` (which rebuilds a normalized copy per level, like
//! `Clone`), the type's `Display` (used to render the report), and its own
//! `Clone`/`Drop`. This example exercises the public worst case — clone the
//! type, render it with `Display`, and drop the clone — on a thread with a
//! fixed stack size, and binary-searches the deepest input that does not
//! overflow. `bytes_per_level ≈ stack / max_ok_depth` is then the per-level
//! cost for this build profile and shape.
//!
//! Each probe runs in a child process (this same binary, re-invoked with
//! `--probe`) so an overflow is an exit status the parent can read, not a
//! signal that kills the measurement.
//!
//! Run (from the repository root):
//!
//! ```sh
//! cargo run --quiet -p onix-arrow --example type_stack_cost            # debug
//! cargo run --quiet --release -p onix-arrow --example type_stack_cost  # release
//! ```
//!
//! The worst case (largest bytes/level) is the debug build, which is the
//! profile `cargo test` uses and what `MAX_NESTING_DEPTH` sizes its margin
//! against.

use std::process::Command;
use std::sync::Arc;

use arrow_schema::{DataType, Field};

/// The fixed stack each probe thread is given while searching. Large enough
/// that the deepest non-overflowing input is in the thousands, so the division
/// has several significant figures.
const PROBE_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Builds a `DataType` nested `depth` levels deep, iteratively (no recursion),
/// so constructing the fixture never overflows the stack itself.
fn build(shape: &str, depth: usize) -> DataType {
    let mut ty = DataType::Int64;
    for _ in 0..depth {
        ty = match shape {
            "struct" => DataType::Struct(vec![Field::new("f", ty, true)].into()),
            "dict" => DataType::Dictionary(Box::new(DataType::Int32), Box::new(ty)),
            _ => DataType::List(Arc::new(Field::new("item", ty, true))),
        };
    }
    ty
}

/// One probe: build a `depth`-deep type and run the recursive operations the
/// bound protects (clone, `Display`, drop) on a thread with `PROBE_STACK_BYTES`
/// of stack. If the recursion overflows, the process dies with a signal, which
/// is the signal the parent reads.
fn run_probe(shape: &str, depth: usize) -> ! {
    let shape = shape.to_owned();
    let handle = std::thread::Builder::new()
        .stack_size(PROBE_STACK_BYTES)
        .spawn(move || {
            let ty = build(&shape, depth);
            let clone = ty.clone();
            let rendered = clone.to_string();
            assert!(!rendered.is_empty(), "Display produces output");
            drop(clone);
            drop(ty);
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
    println!("{shape:>6}: max_ok_depth={low:>6}  bytes_per_level={bytes_per_level}");
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
    println!("onix-arrow DataType recursion stack cost (probe stack {PROBE_STACK_BYTES} bytes)");
    measure(&exe, "struct");
    measure(&exe, "list");
    measure(&exe, "dict");
}
