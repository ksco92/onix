//! `onix` binary: a thin CLI over `onix-core`.
//!
//! The only subcommand is `onix diff <a.json> <b.json> [--max-depth N]
//! [--ignore-order] [--timing]` — see [`run::run`]'s doc for the full
//! argument, output, and exit-code contract. `main()` itself is a
//! two-statement shim over [`run::run`] so every branch of the actual logic
//! is covered by ordinary unit tests against real [`std::io::Write`]
//! buffers, with `tests/cli.rs` covering the same contract end-to-end
//! through the built binary.
//!
//! # Internal layout
//!
//! - `args` — argument parsing: `DiffArgs` and how to get one from
//!   `std::env::args()` (plus the `ONIX_MAX_DEPTH` fallback), no I/O; the
//!   only `onix_core` reference is the `DEFAULT_MAX_DEPTH` fallback
//!   constant.
//! - `run` — execution: reads both input files, calls into `onix_core`, and
//!   writes the report (plus, with `--timing`, a timing line) to
//!   `stdout`/`stderr` — see [`run::run`]'s own doc for the full contract.

mod args;
mod run;

#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::process::ExitCode;

use run::run;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(run(&args, &mut std::io::stdout(), &mut std::io::stderr()))
}
