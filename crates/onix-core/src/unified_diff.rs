//! The `diff` field `DeepDiff` attaches to a `values_changed` entry, at
//! `verbose_level=2`, when a `str`→`str` change involves a newline — a
//! faithful port of `DeepDiff._diff_str`'s convenience diff
//! (`deepdiff/diff.py`), which is a `difflib.unified_diff` of the two values
//! rendered with `lineterm=''`.
//!
//! The line-level matching reuses `crate::lcs` (a
//! [`difflib.SequenceMatcher`](crate::lcs) port); this module adds only what
//! sits *above* the matcher: splitting a
//! string into lines the way Python's `str.splitlines()` does, grouping the
//! opcodes into unified-diff hunks (`crate::lcs::grouped_opcodes`), and
//! formatting the header/range/body lines exactly as `CPython`'s
//! `difflib.unified_diff` and `_format_range_unified` do.
//!
//! # Trigger
//!
//! `DeepDiff` adds the field only when a literal `'\n'` occurs in *either*
//! value (`'\n' in t1 or '\n' in t2`), then splits *both* with
//! `splitlines()` — so a `str` joined only by `\r` (no `\n`) gets no field,
//! but once a `\n` triggers it, `splitlines()`'s full set of Unicode line
//! boundaries (`\r`, `\r\n`, `\x0b`, `\x0c`, `\x1c`–`\x1e`, `\x85`,
//! `\u{2028}`, `\u{2029}`) all split. Both facts verified against real
//! `deepdiff==9.1.0`. If the two line lists turn out identical (e.g. the
//! values differ only by a trailing newline, which `splitlines()` drops),
//! `unified_diff` yields nothing and no field is added — also verified.

use crate::lcs::{Tag, grouped_opcodes};
use crate::value::Value;

/// The number of context lines `difflib.unified_diff` keeps around each
/// change by default (`n=3`), which `DeepDiff` does not override.
const CONTEXT_LINES: usize = 3;

/// Returns the `diff` field for a `values_changed` entry whose two values
/// are `a` and `b`, or `None` when `DeepDiff` would attach no field.
///
/// `None` unless both values are strings (`DeepDiff._diff_str` only runs for
/// a `str`→`str` change), a literal newline occurs in one of them, and the
/// resulting unified diff is non-empty — see the module doc.
pub(crate) fn str_diff_field(a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Str(t1), Value::Str(t2)) => str_diff(t1, t2),
        _ => None,
    }
}

/// The `diff` string for two changed string values, or `None` when no field
/// is warranted (no `\n` in either, or an empty unified diff).
fn str_diff(t1: &str, t2: &str) -> Option<String> {
    if !t1.contains('\n') && !t2.contains('\n') {
        return None;
    }

    let a_lines = splitlines(t1);
    let b_lines = splitlines(t2);
    let a_values: Vec<Value> = a_lines
        .iter()
        .map(|line| Value::Str((*line).into()))
        .collect();
    let b_values: Vec<Value> = b_lines
        .iter()
        .map(|line| Value::Str((*line).into()))
        .collect();

    let groups = grouped_opcodes(&a_values, &b_values, CONTEXT_LINES);
    if groups.is_empty() {
        return None;
    }

    // `lineterm=''` and empty from-/to-file names, so the two header lines
    // are exactly `"--- "` and `"+++ "` (trailing space, no newline).
    let mut out: Vec<String> = vec!["--- ".to_string(), "+++ ".to_string()];
    for group in &groups {
        let first = group
            .first()
            .expect("grouped_opcodes never yields an empty group");
        let last = group
            .last()
            .expect("grouped_opcodes never yields an empty group");
        out.push(format!(
            "@@ -{} +{} @@",
            format_range_unified(first.a1, last.a2),
            format_range_unified(first.b1, last.b2),
        ));
        for op in group {
            match op.tag {
                Tag::Equal => {
                    for line in &a_lines[op.a1..op.a2] {
                        out.push(format!(" {line}"));
                    }
                }
                Tag::Delete => {
                    for line in &a_lines[op.a1..op.a2] {
                        out.push(format!("-{line}"));
                    }
                }
                Tag::Insert => {
                    for line in &b_lines[op.b1..op.b2] {
                        out.push(format!("+{line}"));
                    }
                }
                Tag::Replace => {
                    for line in &a_lines[op.a1..op.a2] {
                        out.push(format!("-{line}"));
                    }
                    for line in &b_lines[op.b1..op.b2] {
                        out.push(format!("+{line}"));
                    }
                }
            }
        }
    }

    Some(out.join("\n"))
}

/// Formats a hunk range the way `CPython`'s `difflib._format_range_unified`
/// does: `start`/`stop` are half-open 0-based indices, the output is 1-based
/// `ed`-style (`"{beginning}"` for a single line, `"{beginning},{length}"`
/// otherwise, and an empty range begins at the line just before it).
fn format_range_unified(start: usize, stop: usize) -> String {
    let beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return beginning.to_string();
    }
    if length == 0 {
        // An empty range begins at the line just before the range.
        return format!("{},{length}", beginning - 1);
    }
    format!("{beginning},{length}")
}

/// Returns `true` if `c` is one of the boundaries Python's
/// `str.splitlines()` breaks a string on.
fn is_line_boundary(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Splits `s` into lines exactly as Python's `str.splitlines()` (without
/// keepends): breaks on every boundary [`is_line_boundary`] recognizes,
/// treats `\r\n` as a single boundary, and never emits a trailing empty
/// line for a string that ends on a boundary.
fn splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !is_line_boundary(c) {
            continue;
        }
        lines.push(&s[start..i]);
        if c == '\r' && matches!(chars.peek(), Some(&(_, '\n'))) {
            chars.next();
        }
        start = chars.peek().map_or_else(|| s.len(), |&(next, _)| next);
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

#[cfg(test)]
#[path = "unified_diff_tests.rs"]
mod tests;
