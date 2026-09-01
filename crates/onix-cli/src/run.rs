//! Runs the parsed `diff` subcommand end to end: reads both input files,
//! calls into `onix_core`, and writes the report (plus, with `--timing`, a
//! parse/diff timing line) to the caller-supplied `stdout`/`stderr` — see
//! [`run`]'s own doc for the full output and exit-code contract.

use std::io::Write;
use std::time::Instant;

use onix_core::DiffOptions;
use serde_json::Value;

use super::args::{USAGE, parse_args, resolve_default_max_depth};

/// Exit code for a usage error (bad/missing arguments, unknown flag).
pub(crate) const EXIT_USAGE_ERROR: u8 = 1;
/// Exit code for an I/O error (missing file) or a JSON-parse error.
pub(crate) const EXIT_IO_OR_PARSE_ERROR: u8 = 2;
/// Exit code for [`onix_core::Error::MaxDepthExceeded`].
pub(crate) const EXIT_MAX_DEPTH_EXCEEDED: u8 = 3;

/// Reads `path` and parses it as JSON, returning both the raw text and the
/// parsed value (`--timing` re-parses the same in-memory text again to
/// measure parse cost in isolation — see [`run`]'s doc — so the raw text is
/// returned here rather than re-read from disk a second time), or a single
/// human-readable error message on either failure (both map to
/// [`EXIT_IO_OR_PARSE_ERROR`] via [`read_or_bail`] — see [`run`]'s
/// exit-code contract).
pub(crate) fn read_json_file(path: &str) -> Result<(String, Value), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    let value =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse {path} as JSON: {e}"))?;
    Ok((text, value))
}

/// Wraps [`read_json_file`] for [`run`]'s use: on failure, writes the error
/// to `stderr` and returns [`EXIT_IO_OR_PARSE_ERROR`] as an `Err`, so `run`'s
/// two call sites (for `a_path` and `b_path`) share one copy of this
/// "read-or-report-and-bail" behavior instead of two identical `match` arms.
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
///   `{}`). Compact rather than pretty-printed: this output is meant for
///   machine consumption (golden-file comparison, the M6 benchmark harness),
///   where a single deterministic line is easier to diff byte-for-byte than
///   a pretty-printed, indentation-sensitive one.
/// - **stderr** carries usage/error text, and — only when `--timing` is
///   passed — exactly one line of JSON shaped `{"parse_ns": N, "diff_ns":
///   N}` measuring, respectively, only the two [`serde_json::from_str`]
///   calls and only the [`onix_core::diff_with_options`] call. Without
///   `--timing`, stderr carries nothing on a successful run.
///
/// # Exit codes
///
/// - `0`: the diff was computed successfully. This holds whether or not the
///   report is empty — presence/absence of differences is carried in the
///   stdout JSON itself, not the exit code, so a benchmark or CI harness
///   scripting against the exit code alone cannot use it as a "differences
///   found" signal.
/// - `1`: a usage error (missing/unknown subcommand, missing/extra
///   positional arguments, an unknown flag, or a non-numeric `--max-depth`).
///   `stderr` gets the specific error plus [`USAGE`].
/// - `2`: an I/O error (e.g. a missing input file) or a JSON-parse error on
///   either input.
/// - `3`: [`onix_core::Error::MaxDepthExceeded`] — `stderr` gets the error's
///   `Display` text.
///
/// # Stack safety on adversarially deep input
///
/// `--max-depth`/`ONIX_MAX_DEPTH` can be set arbitrarily high with no upper
/// bound enforced here, but that is safe: `serde_json`'s own parser enforces
/// a default recursion limit of 128 levels of nesting (this crate does not
/// enable its `unbounded_depth` feature), so any input `read_json_file` can
/// successfully parse is already at most 128 levels deep — far under
/// [`onix_core::DEFAULT_MAX_DEPTH`] — regardless of how high the configured
/// bound is.
#[allow(clippy::missing_panics_doc)] // see the serde_json::to_string comment below
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
    let mut opts = DiffOptions::default();
    opts.max_depth = max_depth;
    opts.ignore_order = parsed.ignore_order;

    let diff_start = Instant::now();
    let result = onix_core::diff_with_options(&a_value, &b_value, &opts);
    let diff_ns = diff_start.elapsed().as_nanos();

    if parsed.timing {
        // Re-parses the same in-memory text read_or_bail already read
        // (rather than reading either file from disk a second time): a
        // second disk read would be both a TOCTOU hazard (the file could
        // change between reads) and would need a silent fallback for a
        // failure that "can't happen" here (the read already succeeded
        // once above). Measuring the exact same bytes is also more
        // representative of the parse cost the diff above actually paid.
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
            // A Report's to_json_value() is built entirely from data that
            // round-tripped through serde_json::Value already (the parsed
            // inputs above) plus our own path strings, so it can never
            // contain NaN/Infinity — the only way serde_json::to_string can
            // fail on a Value (every Value::Object key is always a String,
            // so the other failure mode serde_json documents does not apply
            // here). Safe by construction, not by luck.
            let serialized = serde_json::to_string(&value)
                .expect("a Report's JSON value is always serializable");
            let _ = writeln!(stdout, "{serialized}");
            0
        }
        Err(error @ onix_core::Error::MaxDepthExceeded { .. }) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_MAX_DEPTH_EXCEEDED
        }
    }
}
