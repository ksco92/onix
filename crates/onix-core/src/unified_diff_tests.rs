//! Unit tests for the `_diff_str` port. Every expected string here was
//! captured from real `deepdiff==9.1.0` at `verbose_level=2` (its
//! `_diff_str` -> `difflib.unified_diff` output); the golden corpus and the
//! Python differential fuzz check the same behavior end-to-end and at scale
//! (including `difflib`'s autojunk heuristic, which needs 200+ lines).

use super::{format_range_unified, splitlines, str_diff_field};
use crate::value::Value;

fn s(text: &str) -> Value {
    Value::Str(text.into())
}

fn diff(t1: &str, t2: &str) -> Option<String> {
    str_diff_field(&s(t1), &s(t2))
}

#[test]
fn both_sides_multiline_replace() {
    assert_eq!(
        diff("a\nb", "c\nd").as_deref(),
        Some("--- \n+++ \n@@ -1,2 +1,2 @@\n-a\n-b\n+c\n+d"),
    );
}

#[test]
fn shared_context_line_is_kept() {
    assert_eq!(
        diff("line1\nline2", "line1\nline3").as_deref(),
        Some("--- \n+++ \n@@ -1,2 +1,2 @@\n line1\n-line2\n+line3"),
    );
}

#[test]
fn no_newline_either_side_has_no_diff() {
    assert_eq!(diff("ab", "cd"), None);
}

#[test]
fn newline_on_old_side_only_still_triggers() {
    assert_eq!(
        diff("a\nb", "cd").as_deref(),
        Some("--- \n+++ \n@@ -1,2 +1 @@\n-a\n-b\n+cd"),
    );
}

#[test]
fn newline_on_new_side_only_still_triggers() {
    assert_eq!(
        diff("ab", "c\nd").as_deref(),
        Some("--- \n+++ \n@@ -1 +1,2 @@\n-ab\n+c\n+d"),
    );
}

#[test]
fn crlf_is_one_boundary() {
    assert_eq!(
        diff("a\r\nb", "c\r\nd").as_deref(),
        Some("--- \n+++ \n@@ -1,2 +1,2 @@\n-a\n-b\n+c\n+d"),
    );
    assert_eq!(
        diff("a\r\nb", "a\r\nc").as_deref(),
        Some("--- \n+++ \n@@ -1,2 +1,2 @@\n a\n-b\n+c"),
    );
}

#[test]
fn leading_newline_becomes_a_blank_context_line() {
    assert_eq!(
        diff("\na\nb", "\nc\nd").as_deref(),
        Some("--- \n+++ \n@@ -1,3 +1,3 @@\n \n-a\n-b\n+c\n+d"),
    );
}

#[test]
fn trailing_newline_is_dropped_by_splitlines() {
    // `"a\nb\n"` and `"a\nb"` both split to `["a", "b"]`, so the diff is
    // identical to the no-trailing-newline case.
    assert_eq!(
        diff("a\nb\n", "c\nd\n").as_deref(),
        Some("--- \n+++ \n@@ -1,2 +1,2 @@\n-a\n-b\n+c\n+d"),
    );
}

#[test]
fn values_differing_only_by_a_trailing_newline_have_no_diff() {
    // The strings differ, so a `values_changed` is still emitted elsewhere,
    // but `splitlines()` collapses them to the same lines, so `unified_diff`
    // is empty and no `diff` field is attached.
    assert_eq!(diff("a\nb", "a\nb\n"), None);
}

#[test]
fn identical_multiline_strings_have_no_diff() {
    assert_eq!(diff("a\nb", "a\nb"), None);
}

#[test]
fn carriage_return_alone_does_not_trigger() {
    // The trigger is a literal `'\n'`; a `\r`-joined string has none, so
    // DeepDiff attaches no field even though `splitlines()` would split it.
    assert_eq!(diff("a\rb", "c\rd"), None);
}

#[test]
fn carriage_return_splits_once_a_newline_triggers() {
    // A `\n` anywhere triggers, and then `splitlines()` also breaks the `\r`.
    assert_eq!(
        diff("a\rb\nc", "x\ry\nz").as_deref(),
        Some("--- \n+++ \n@@ -1,3 +1,3 @@\n-a\n-b\n-c\n+x\n+y\n+z"),
    );
}

#[test]
fn far_apart_changes_produce_two_hunks() {
    let a: String = (0..20)
        .map(|i| format!("L{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut b_lines: Vec<String> = (0..20).map(|i| format!("L{i}")).collect();
    b_lines[1] = "CHANGED1".to_string();
    b_lines[15] = "CHANGED15".to_string();
    let b = b_lines.join("\n");
    assert_eq!(
        diff(&a, &b).as_deref(),
        Some(
            "--- \n+++ \n@@ -1,5 +1,5 @@\n L0\n-L1\n+CHANGED1\n L2\n L3\n L4\n\
             @@ -13,7 +13,7 @@\n L12\n L13\n L14\n-L15\n+CHANGED15\n L16\n L17\n L18"
        ),
    );
}

/// A change after a long unchanged prefix keeps only `n=3` lines of leading
/// context — the leading-opcode fixup in `grouped_opcodes`.
#[test]
fn leading_context_is_trimmed_to_three_lines() {
    let a = "a\nb\nc\nd\ne\nf\ng\nZ";
    let b = "a\nb\nc\nd\ne\nf\ng\nY";
    assert_eq!(
        diff(a, b).as_deref(),
        Some("--- \n+++ \n@@ -5,4 +5,4 @@\n e\n f\n g\n-Z\n+Y"),
    );
}

/// A change before a long unchanged suffix keeps only `n=3` lines of trailing
/// context — the trailing-opcode fixup in `grouped_opcodes`.
#[test]
fn trailing_context_is_trimmed_to_three_lines() {
    let a = "Z\na\nb\nc\nd\ne\nf\ng";
    let b = "Y\na\nb\nc\nd\ne\nf\ng";
    assert_eq!(
        diff(a, b).as_deref(),
        Some("--- \n+++ \n@@ -1,4 +1,4 @@\n-Z\n+Y\n a\n b\n c"),
    );
}

/// Two changes with exactly `2n = 6` unchanged lines between them stay in one
/// hunk — the boundary at which `grouped_opcodes` does *not* split.
#[test]
fn six_unchanged_lines_between_changes_stay_one_hunk() {
    let a: String = (0..9)
        .map(|i| format!("L{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let b: String = (0..9)
        .map(|i| match i {
            0 => "X0".to_string(),
            7 => "X7".to_string(),
            _ => format!("L{i}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        diff(&a, &b).as_deref(),
        Some("--- \n+++ \n@@ -1,9 +1,9 @@\n-L0\n+X0\n L1\n L2\n L3\n L4\n L5\n L6\n-L7\n+X7\n L8"),
    );
}

/// Two changes with `2n + 1 = 7` unchanged lines between them split into two
/// hunks — the boundary at which `grouped_opcodes` *does* split.
#[test]
fn seven_unchanged_lines_between_changes_split_into_two_hunks() {
    let a: String = (0..10)
        .map(|i| format!("L{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let b: String = (0..10)
        .map(|i| match i {
            0 => "X0".to_string(),
            8 => "X8".to_string(),
            _ => format!("L{i}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        diff(&a, &b).as_deref(),
        Some(
            "--- \n+++ \n@@ -1,4 +1,4 @@\n-L0\n+X0\n L1\n L2\n L3\n\
             @@ -6,5 +6,5 @@\n L5\n L6\n L7\n-L8\n+X8\n L9"
        ),
    );
}

#[test]
fn a_removed_line_is_a_delete_hunk() {
    assert_eq!(
        diff("a\nb\nc", "a\nc").as_deref(),
        Some("--- \n+++ \n@@ -1,3 +1,2 @@\n a\n-b\n c"),
    );
}

#[test]
fn an_added_line_is_an_insert_hunk() {
    assert_eq!(
        diff("a\nc", "a\nb\nc").as_deref(),
        Some("--- \n+++ \n@@ -1,2 +1,3 @@\n a\n+b\n c"),
    );
}

#[test]
fn non_string_values_never_get_a_diff() {
    let one = Value::from(serde_json::json!(1));
    let two = Value::from(serde_json::json!(2));
    assert_eq!(str_diff_field(&one, &two), None);
    assert_eq!(str_diff_field(&s("a\nb"), &one), None);
    assert_eq!(str_diff_field(&Value::Null, &s("a\nb")), None);
}

#[test]
fn splitlines_matches_python_semantics() {
    assert_eq!(splitlines("a\nb"), vec!["a", "b"]);
    assert_eq!(splitlines("a\nb\n"), vec!["a", "b"]);
    assert_eq!(splitlines("a\n\nb"), vec!["a", "", "b"]);
    assert_eq!(splitlines("a\r\nb"), vec!["a", "b"]);
    assert_eq!(splitlines("a\rb"), vec!["a", "b"]);
    assert_eq!(splitlines(""), Vec::<&str>::new());
    assert_eq!(splitlines("a"), vec!["a"]);
    assert_eq!(splitlines("\n"), vec![""]);
    // Every non-`\n` Unicode boundary Python's `splitlines()` recognizes.
    assert_eq!(splitlines("a\u{0b}b"), vec!["a", "b"]);
    assert_eq!(splitlines("a\u{0c}b"), vec!["a", "b"]);
    assert_eq!(splitlines("a\u{1c}b"), vec!["a", "b"]);
    assert_eq!(splitlines("a\u{1d}b"), vec!["a", "b"]);
    assert_eq!(splitlines("a\u{1e}b"), vec!["a", "b"]);
    assert_eq!(splitlines("a\u{85}b"), vec!["a", "b"]);
    assert_eq!(splitlines("a\u{2028}b"), vec!["a", "b"]);
    assert_eq!(splitlines("a\u{2029}b"), vec!["a", "b"]);
}

#[test]
fn format_range_unified_matches_cpython() {
    // Single line: just the 1-based beginning.
    assert_eq!(format_range_unified(0, 1), "1");
    assert_eq!(format_range_unified(4, 5), "5");
    // Multi-line: beginning,length.
    assert_eq!(format_range_unified(0, 2), "1,2");
    assert_eq!(format_range_unified(12, 19), "13,7");
    // Empty range begins at the line just before it.
    assert_eq!(format_range_unified(0, 0), "0,0");
    assert_eq!(format_range_unified(3, 3), "3,0");
}
