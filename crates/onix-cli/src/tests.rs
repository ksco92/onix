use super::args::{DiffArgs, USAGE, parse_args, parse_diff_args, parse_max_depth_env_value};
use super::run::{
    EXIT_IO_OR_PARSE_ERROR, EXIT_MAX_DEPTH_EXCEEDED, EXIT_USAGE_ERROR, read_json_file, run,
};
use super::test_support::write_temp_file;

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}

// --- parse_args / parse_diff_args -----------------------------------

#[test]
fn parse_args_rejects_missing_subcommand() {
    assert_eq!(parse_args(&[]), Err("missing subcommand".to_string()));
}

#[test]
fn parse_args_rejects_unknown_subcommand() {
    assert_eq!(
        parse_args(&s(&["bogus"])),
        Err("unknown subcommand: bogus".to_string())
    );
}

#[test]
fn parse_diff_args_accepts_two_positionals() {
    assert_eq!(
        parse_args(&s(&["diff", "a.json", "b.json"])),
        Ok(DiffArgs {
            a_path: "a.json".to_string(),
            b_path: "b.json".to_string(),
            max_depth: None,
            ignore_order: false,
            timing: false,
        })
    );
}

#[test]
fn parse_diff_args_rejects_zero_positionals() {
    assert_eq!(
        parse_diff_args(&[]),
        Err("expected 2 positional arguments (a.json b.json), got 0".to_string())
    );
}

#[test]
fn parse_diff_args_rejects_one_positional() {
    assert_eq!(
        parse_diff_args(&s(&["a.json"])),
        Err("expected 2 positional arguments (a.json b.json), got 1".to_string())
    );
}

#[test]
fn parse_diff_args_rejects_three_positionals() {
    assert_eq!(
        parse_diff_args(&s(&["a.json", "b.json", "c.json"])),
        Err("expected 2 positional arguments (a.json b.json), got 3".to_string())
    );
}

#[test]
fn parse_diff_args_rejects_unknown_flag() {
    assert_eq!(
        parse_diff_args(&s(&["a.json", "b.json", "--bogus"])),
        Err("unknown flag: --bogus".to_string())
    );
}

#[test]
fn parse_diff_args_rejects_max_depth_missing_value() {
    assert_eq!(
        parse_diff_args(&s(&["a.json", "b.json", "--max-depth"])),
        Err("--max-depth requires a value".to_string())
    );
}

#[test]
fn parse_diff_args_rejects_max_depth_non_numeric_value() {
    assert_eq!(
        parse_diff_args(&s(&["a.json", "b.json", "--max-depth", "nope"])),
        Err("invalid --max-depth value: nope".to_string())
    );
}

#[test]
fn parse_diff_args_accepts_max_depth_and_timing_in_any_position() {
    assert_eq!(
        parse_diff_args(&s(&["--timing", "a.json", "--max-depth", "7", "b.json"])),
        Ok(DiffArgs {
            a_path: "a.json".to_string(),
            b_path: "b.json".to_string(),
            max_depth: Some(7),
            ignore_order: false,
            timing: true,
        })
    );
}

#[test]
fn parse_diff_args_accepts_ignore_order_flag() {
    assert_eq!(
        parse_diff_args(&s(&["a.json", "b.json", "--ignore-order"])),
        Ok(DiffArgs {
            a_path: "a.json".to_string(),
            b_path: "b.json".to_string(),
            max_depth: None,
            ignore_order: true,
            timing: false,
        })
    );
}

// --- read_json_file ---------------------------------------------------

#[test]
fn read_json_file_parses_valid_json() {
    let path = write_temp_file("valid.json", r#"{"a": 1}"#);
    assert_eq!(
        read_json_file(path.to_str().unwrap()),
        Ok((
            r#"{"a": 1}"#.to_string(),
            onix_core::Value::from(serde_json::json!({"a": 1})),
        ))
    );
}

#[test]
fn read_json_file_reports_missing_file() {
    let message = read_json_file("/nonexistent/path/onix-does-not-exist.json").unwrap_err();
    assert!(message.contains("failed to read"));
}

#[test]
fn read_json_file_reports_invalid_json() {
    let path = write_temp_file("invalid.json", "not json");
    let message = read_json_file(path.to_str().unwrap()).unwrap_err();
    assert!(message.contains("failed to parse"));
}

// --- run: end-to-end over in-memory buffers ---------------------------

fn run_and_capture(args: &[String]) -> (u8, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run(args, &mut stdout, &mut stderr);
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[test]
fn run_usage_error_on_no_args() {
    let (code, stdout, stderr) = run_and_capture(&[]);
    assert_eq!(code, EXIT_USAGE_ERROR);
    assert!(stdout.is_empty());
    assert!(stderr.contains("missing subcommand"));
    assert!(stderr.contains(USAGE));
}

#[test]
fn run_diff_with_differences_prints_exact_report_and_exits_zero() {
    let a = write_temp_file("a.json", r#"{"x": 1, "y": 2}"#);
    let b = write_temp_file("b.json", r#"{"x": 1, "y": 3, "z": 4}"#);
    let (code, stdout, stderr) =
        run_and_capture(&s(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]));
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim_end(),
        r#"{"dictionary_item_added":{"root['z']":4},"values_changed":{"root['y']":{"new_value":3,"old_value":2}}}"#
    );
    assert!(stderr.is_empty());
}

#[test]
fn run_diff_ignore_order_flag_reports_a_pure_shuffle_as_empty() {
    let a = write_temp_file("shuffle_a.json", "[1, 2, 3]");
    let b = write_temp_file("shuffle_b.json", "[3, 2, 1]");
    let (code, stdout, stderr) = run_and_capture(&s(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--ignore-order",
    ]));
    assert_eq!(code, 0);
    assert_eq!(stdout.trim_end(), "{}");
    assert!(stderr.is_empty());
}

#[test]
fn run_diff_without_ignore_order_flag_reports_the_shuffle_as_index_aligned_changes() {
    // Same input as the test above, without --ignore-order: the default
    // ordered path compares index by index instead, so the shuffle is
    // NOT empty — proves the flag is actually wired to DiffOptions, not
    // a no-op.
    let a = write_temp_file("shuffle_ordered_a.json", "[1, 2, 3]");
    let b = write_temp_file("shuffle_ordered_b.json", "[3, 2, 1]");
    let (code, stdout, _stderr) =
        run_and_capture(&s(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]));
    assert_eq!(code, 0);
    assert_ne!(stdout.trim_end(), "{}");
}

#[test]
fn run_diff_uses_real_default_max_depth_when_no_flag_or_env_is_set() {
    // Regression test for a cargo-mutants survivor: mutating
    // resolve_default_max_depth to always return 1 passed every other
    // test in this file, because none of them needed more than
    // max_depth == 1 to succeed (a change at path depth 1 fits under
    // that bound too). A change at path depth 2 needs max_depth >= 2,
    // which onix_core::DEFAULT_MAX_DEPTH (512) comfortably clears but
    // the hardcoded-1 mutant does not — so this fails loudly (exit 3
    // instead of 0) under that mutant. Relies on the ambient test
    // environment not exporting ONIX_MAX_DEPTH, same as every other
    // test here that never sets or clears it.
    let a = write_temp_file("default_depth_a.json", r#"{"a": {"b": 1}}"#);
    let b = write_temp_file("default_depth_b.json", r#"{"a": {"b": 2}}"#);
    let (code, stdout, stderr) =
        run_and_capture(&s(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]));
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim_end(),
        r#"{"values_changed":{"root['a']['b']":{"new_value":2,"old_value":1}}}"#
    );
    assert!(stderr.is_empty());
}

#[test]
fn run_diff_with_no_differences_prints_empty_object_and_exits_zero() {
    let a = write_temp_file("same_a.json", r#"{"x": 1}"#);
    let b = write_temp_file("same_b.json", r#"{"x": 1}"#);
    let (code, stdout, stderr) =
        run_and_capture(&s(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]));
    assert_eq!(code, 0);
    assert_eq!(stdout.trim_end(), "{}");
    assert!(stderr.is_empty());
}

#[test]
fn run_diff_missing_file_exits_two() {
    let b = write_temp_file("exists.json", "{}");
    let (code, stdout, stderr) = run_and_capture(&s(&[
        "diff",
        "/nonexistent/onix-missing.json",
        b.to_str().unwrap(),
    ]));
    assert_eq!(code, EXIT_IO_OR_PARSE_ERROR);
    assert!(stdout.is_empty());
    assert!(stderr.contains("failed to read"));
}

#[test]
fn run_diff_invalid_json_exits_two() {
    let a = write_temp_file("bad.json", "not json");
    let b = write_temp_file("bad_b.json", "{}");
    let (code, _stdout, stderr) =
        run_and_capture(&s(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]));
    assert_eq!(code, EXIT_IO_OR_PARSE_ERROR);
    assert!(stderr.contains("failed to parse"));
}

#[test]
fn run_diff_b_file_invalid_json_exits_two() {
    // Same as run_diff_invalid_json_exits_two, but with the *second*
    // (b) file the invalid one and a valid — exercises run()'s separate
    // error-handling arm for b_value specifically, not just a_value's.
    let a = write_temp_file("good_a.json", "{}");
    let b = write_temp_file("bad_only_b.json", "not json");
    let (code, stdout, stderr) =
        run_and_capture(&s(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]));
    assert_eq!(code, EXIT_IO_OR_PARSE_ERROR);
    assert!(stdout.is_empty());
    assert!(stderr.contains("failed to parse"));
}

#[test]
fn run_diff_max_depth_flag_trips_max_depth_exceeded() {
    let a = write_temp_file("deep_a.json", r#"{"a": {"b": 1}}"#);
    let b = write_temp_file("deep_b.json", r#"{"a": {"b": 2}}"#);
    let (code, stdout, stderr) = run_and_capture(&s(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--max-depth",
        "1",
    ]));
    assert_eq!(code, EXIT_MAX_DEPTH_EXCEEDED);
    assert!(stdout.is_empty());
    assert!(stderr.contains("root['a']['b']"));
}

#[test]
fn run_diff_timing_flag_emits_parse_and_diff_ns_on_stderr_and_clean_stdout_json() {
    let a = write_temp_file("timing_a.json", r#"{"x": 1}"#);
    let b = write_temp_file("timing_b.json", r#"{"x": 2}"#);
    let (code, stdout, stderr) = run_and_capture(&s(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--timing",
    ]));
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim_end(),
        r#"{"values_changed":{"root['x']":{"new_value":2,"old_value":1}}}"#
    );
    let timing: serde_json::Value = serde_json::from_str(stderr.trim_end()).unwrap();
    assert!(timing.get("parse_ns").is_some());
    assert!(timing.get("diff_ns").is_some());
}

// --- parse_max_depth_env_value ----------------------------------------
//
// The real end-to-end "ONIX_MAX_DEPTH is respected" behavior (reading
// the actual process environment) is covered by tests/cli.rs via a
// subprocess with the variable set on the Command builder — see
// parse_max_depth_env_value's doc for why that logic is split out and
// tested here in isolation instead of by mutating this test binary's own
// process environment.

#[test]
fn parse_max_depth_env_value_falls_back_to_default_when_unset() {
    assert_eq!(
        parse_max_depth_env_value(None),
        onix_core::DEFAULT_MAX_DEPTH
    );
}

#[test]
fn parse_max_depth_env_value_uses_parseable_value() {
    assert_eq!(parse_max_depth_env_value(Some("7")), 7);
}

#[test]
fn parse_max_depth_env_value_falls_back_to_default_when_unparseable() {
    assert_eq!(
        parse_max_depth_env_value(Some("not-a-number")),
        onix_core::DEFAULT_MAX_DEPTH
    );
}

#[test]
fn every_engine_error_maps_to_a_documented_exit_code() {
    // `DateTimeOutOfRange` cannot arise from JSON input (see
    // `exit_code_for`'s doc), so this is the only place its mapping is
    // exercised — and the `match` is what makes a future variant fail to
    // compile until it is mapped too.
    assert_eq!(
        super::run::exit_code_for(&onix_core::Error::MaxDepthExceeded {
            path: "root".to_string(),
            max_depth: 512,
        }),
        super::run::EXIT_MAX_DEPTH_EXCEEDED
    );
    assert_eq!(
        super::run::exit_code_for(&onix_core::Error::DateTimeOutOfRange {
            path: "root".to_string(),
        }),
        super::run::EXIT_IO_OR_PARSE_ERROR
    );
}
