//! `onix` binary: a thin CLI over `onix-core`.
//!
//! The only subcommand is `onix diff <a.json> <b.json> [--max-depth N]
//! [--ignore-order] [--timing]` — see [`run::run`]'s doc for the full
//! argument, output, and exit-code contract.

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
