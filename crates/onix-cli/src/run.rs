//! Runs the parsed `diff` subcommand end to end: reads both input files,
//! calls into `onix_core`, and writes the report (plus, with `--timing`, a
//! parse/diff timing line) to the caller-supplied `stdout`/`stderr`.

use std::io::Write;
use std::time::Instant;

use onix_core::{DiffOptions, Value};

use super::args::{USAGE, parse_args, resolve_default_max_depth};

/// Exit code for a usage error (bad/missing arguments, unknown flag).
pub(crate) const EXIT_USAGE_ERROR: u8 = 1;
/// Exit code for an I/O error (missing file), a JSON-parse error, or an
/// input value the engine cannot compare (see [`exit_code_for`]).
pub(crate) const EXIT_IO_OR_PARSE_ERROR: u8 = 2;
/// Exit code for [`onix_core::Error::MaxDepthExceeded`].
pub(crate) const EXIT_MAX_DEPTH_EXCEEDED: u8 = 3;

/// Reads `path` and parses it as JSON, returning both the raw text (kept for
/// `--timing`'s re-parse) and the parsed value, or a human-readable error
/// message on either failure.
pub(crate) fn read_json_file(path: &str) -> Result<(String, Value), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    let value =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse {path} as JSON: {e}"))?;
    Ok((text, value))
}

/// Wraps [`read_json_file`] for [`run`]'s two call sites: on failure, writes
/// the error to `stderr` and returns [`EXIT_IO_OR_PARSE_ERROR`] as an `Err`.
fn read_or_bail(path: &str, stderr: &mut dyn Write) -> Result<(String, Value), u8> {
    read_json_file(path).map_err(|message| {
        let _ = writeln!(stderr, "error: {message}");
        EXIT_IO_OR_PARSE_ERROR
    })
}

/// Runs the CLI: parses `args` (the program name already stripped), performs
/// the `diff` subcommand, and writes its output to `stdout`/`stderr`.
///
/// # Output contract
///
/// - **stdout** carries only the diff report, as a single line of compact
///   JSON from [`onix_core::Report::to_json_value`] (an empty report prints
///   `{}`).
/// - **stderr** carries usage/error text, and — only with `--timing` — one
///   line `{"parse_ns": N, "diff_ns": N}` timing the two
///   [`serde_json::from_str`] calls and the [`onix_core::diff_with_options`]
///   call.
///
/// # Exit codes
///
/// - `0`: the diff was computed, whether or not the report is empty.
/// - `1`: a usage error; `stderr` gets the error plus [`USAGE`].
/// - `2`: an I/O error, a JSON-parse error on either input, or an input value
///   the engine cannot compare.
/// - `3`: [`onix_core::Error::MaxDepthExceeded`]; `stderr` gets its message.
#[allow(
    clippy::missing_panics_doc,
    reason = "see the serde_json::to_string comment below"
)]
pub(crate) fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            let _ = writeln!(stderr, "error: {message}");
            let _ = writeln!(stderr, "{USAGE}");
            return EXIT_USAGE_ERROR;
        }
    };

    let (a_text, a_value) = match read_or_bail(&parsed.a_path, stderr) {
        Ok(read) => read,
        Err(code) => return code,
    };
    let (b_text, b_value) = match read_or_bail(&parsed.b_path, stderr) {
        Ok(read) => read,
        Err(code) => return code,
    };

    let max_depth = parsed.max_depth.unwrap_or_else(resolve_default_max_depth);
    let opts = DiffOptions {
        max_depth,
        ignore_order: parsed.ignore_order,
    };

    let diff_start = Instant::now();
    let result = onix_core::diff_with_options(&a_value, &b_value, &opts);
    let diff_ns = diff_start.elapsed().as_nanos();

    if parsed.timing {
        let parse_start = Instant::now();
        let _: Result<Value, _> = serde_json::from_str(&a_text);
        let _: Result<Value, _> = serde_json::from_str(&b_text);
        let parse_ns = parse_start.elapsed().as_nanos();

        let timing = serde_json::json!({"parse_ns": parse_ns, "diff_ns": diff_ns});
        let _ = writeln!(stderr, "{timing}");
    }

    match result {
        Ok(report) => {
            let value = report.to_json_value();
            // Finite numbers and our own path strings: no NaN/Infinity.
            let serialized = serde_json::to_string(&value)
                .expect("a Report's JSON value is always serializable");
            let _ = writeln!(stdout, "{serialized}");
            0
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            exit_code_for(&error)
        }
    }
}

/// The exit code one engine error maps to.
pub(crate) fn exit_code_for(error: &onix_core::Error) -> u8 {
    match error {
        onix_core::Error::MaxDepthExceeded { .. } => EXIT_MAX_DEPTH_EXCEEDED,
        onix_core::Error::DateTimeOutOfRange { .. } => EXIT_IO_OR_PARSE_ERROR,
    }
}
