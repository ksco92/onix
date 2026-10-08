//! End-to-end tests driving the compiled `onix` binary as a subprocess.

use std::process::{Command, Output};

#[path = "../src/test_support.rs"]
mod test_support;
use test_support::write_temp_file;

/// Runs the built `onix` binary with `args`, returning its raw [`Output`].
fn run_onix(args: &[&str]) -> Output {
    run_onix_with_env(args, &[])
}

/// Like [`run_onix`], with extra environment variables.
fn run_onix_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_onix"))
        .args(args)
        .envs(env.iter().copied())
        .output()
        .expect("failed to execute onix binary")
}

fn stdout_str(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout was not valid UTF-8")
}

fn stderr_str(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr was not valid UTF-8")
}

#[test]
fn diff_with_differences_prints_exact_stdout_json_and_exits_zero() {
    let a = write_temp_file("a.json", r#"{"x": 1, "y": 2}"#);
    let b = write_temp_file("b.json", r#"{"x": 1, "y": 3, "z": 4}"#);

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_str(&output).trim_end(),
        r#"{"dictionary_item_added":{"root['z']":4},"values_changed":{"root['y']":{"new_value":3,"old_value":2}}}"#
    );
    assert!(stderr_str(&output).is_empty());
}

#[test]
fn multiline_string_change_carries_the_unified_diff_field() {
    let a = write_temp_file("ml_a.json", r#""a\nb""#);
    let b = write_temp_file("ml_b.json", r#""c\nd""#);

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_str(&output).trim_end(),
        r#"{"values_changed":{"root":{"diff":"--- \n+++ \n@@ -1,2 +1,2 @@\n-a\n-b\n+c\n+d","new_value":"c\nd","old_value":"a\nb"}}}"#
    );
    assert!(stderr_str(&output).is_empty());
}

#[test]
fn diff_with_no_differences_prints_empty_object_and_exits_zero() {
    let a = write_temp_file("same_a.json", r#"{"a": [1, 2, 3]}"#);
    let b = write_temp_file("same_b.json", r#"{"a": [1, 2, 3]}"#);

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_str(&output).trim_end(), "{}");
    assert!(stderr_str(&output).is_empty());
}

#[test]
fn max_depth_flag_trips_max_depth_exceeded_and_exits_three() {
    let a = write_temp_file("deep_a.json", r#"{"a": {"b": 1}}"#);
    let b = write_temp_file("deep_b.json", r#"{"a": {"b": 2}}"#);

    let output = run_onix(&[
        "diff",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--max-depth",
        "1",
    ]);

    assert_eq!(output.status.code(), Some(3));
    assert!(stdout_str(&output).is_empty());
    let stderr = stderr_str(&output);
    assert!(stderr.contains("root['a']['b']"));
    assert!(stderr.contains('1'));
}

fn nested_array(depth: usize, leaf: &str) -> String {
    "[".repeat(depth) + leaf + &"]".repeat(depth)
}

#[test]
fn json_nested_127_levels_parses_and_exits_zero() {
    let a = write_temp_file("nest127_a.json", &nested_array(127, "1"));
    let b = write_temp_file("nest127_b.json", &nested_array(127, "1"));

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn json_nested_128_levels_exits_two_with_recursion_limit() {
    let a = write_temp_file("nest128_a.json", &nested_array(128, "1"));
    let b = write_temp_file("nest128_b.json", &nested_array(128, "1"));

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr_str(&output);
    assert!(stderr.contains("failed to parse"));
    assert!(stderr.contains("recursion limit exceeded"));
}

#[test]
fn default_max_depth_is_used_when_no_flag_or_env_is_set() {
    // Removes ONIX_MAX_DEPTH so an exporting shell cannot change the default.
    let a = write_temp_file("default_depth_a.json", r#"{"a": {"b": 1}}"#);
    let b = write_temp_file("default_depth_b.json", r#"{"a": {"b": 2}}"#);

    let output = Command::new(env!("CARGO_BIN_EXE_onix"))
        .args(["diff", a.to_str().unwrap(), b.to_str().unwrap()])
        .env_remove("ONIX_MAX_DEPTH")
        .output()
        .expect("failed to execute onix binary");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_str(&output).trim_end(),
        r#"{"values_changed":{"root['a']['b']":{"new_value":2,"old_value":1}}}"#
    );
}

#[test]
fn onix_max_depth_env_var_is_respected_when_flag_absent() {
    let a = write_temp_file("env_a.json", r#"{"a": {"b": 1}}"#);
    let b = write_temp_file("env_b.json", r#"{"a": {"b": 2}}"#);

    let output = run_onix_with_env(
        &["diff", a.to_str().unwrap(), b.to_str().unwrap()],
        &[("ONIX_MAX_DEPTH", "1")],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(stderr_str(&output).contains("root['a']['b']"));
}

#[test]
fn onix_max_depth_env_var_is_overridden_by_explicit_flag() {
    let a = write_temp_file("env_override_a.json", r#"{"a": {"b": 1}}"#);
    let b = write_temp_file("env_override_b.json", r#"{"a": {"b": 2}}"#);

    let output = run_onix_with_env(
        &[
            "diff",
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            "--max-depth",
            "50",
        ],
        &[("ONIX_MAX_DEPTH", "1")],
    );

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_str(&output).trim_end(),
        r#"{"values_changed":{"root['a']['b']":{"new_value":2,"old_value":1}}}"#
    );
}

#[test]
fn timing_flag_emits_parse_and_diff_ns_json_on_stderr_with_clean_stdout() {
    let a = write_temp_file("timing_a.json", r#"{"x": 1}"#);
    let b = write_temp_file("timing_b.json", r#"{"x": 2}"#);

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap(), "--timing"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_str(&output).trim_end(),
        r#"{"values_changed":{"root['x']":{"new_value":2,"old_value":1}}}"#
    );

    let stderr = stderr_str(&output);
    let timing: serde_json::Value =
        serde_json::from_str(stderr.trim_end()).expect("stderr --timing line was not valid JSON");
    assert!(
        timing
            .get("parse_ns")
            .and_then(serde_json::Value::as_u64)
            .is_some()
    );
    assert!(
        timing
            .get("diff_ns")
            .and_then(serde_json::Value::as_u64)
            .is_some()
    );
}

#[test]
fn no_timing_flag_leaves_stderr_clean_on_success() {
    let a = write_temp_file("clean_a.json", "1");
    let b = write_temp_file("clean_b.json", "2");

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr_str(&output).is_empty());
}

#[test]
fn missing_subcommand_is_a_usage_error() {
    let output = run_onix(&[]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout_str(&output).is_empty());
    assert!(stderr_str(&output).contains("usage: onix diff"));
}

#[test]
fn unknown_flag_is_a_usage_error() {
    let a = write_temp_file("flag_a.json", "1");
    let b = write_temp_file("flag_b.json", "2");

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap(), "--bogus"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr_str(&output).contains("unknown flag: --bogus"));
}

#[test]
fn missing_input_file_exits_two() {
    let b = write_temp_file("exists.json", "{}");

    let output = run_onix(&[
        "diff",
        "/nonexistent/onix-integration-test-missing.json",
        b.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stdout_str(&output).is_empty());
    assert!(stderr_str(&output).contains("failed to read"));
}

#[test]
fn invalid_json_input_exits_two() {
    let a = write_temp_file("bad.json", "not json at all");
    let b = write_temp_file("bad_b.json", "{}");

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr_str(&output).contains("failed to parse"));
}

#[test]
fn a_tagged_object_is_diffed_as_the_ordinary_dict_it_is() {
    // `$tuple` and its siblings are a convention of the golden corpus's
    // fixture files (see tests/golden/README.md), never of the JSON the
    // product reads: the CLI must diff this as a one-key dict.
    let a = write_temp_file("tagged_a.json", r#"{"$tuple": [1]}"#);
    let b = write_temp_file("tagged_b.json", r#"{"$tuple": [2]}"#);

    let output = run_onix(&["diff", a.to_str().unwrap(), b.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout_str(&output).trim_end(),
        r#"{"values_changed":{"root['$tuple'][0]":{"new_value":2,"old_value":1}}}"#
    );
}
