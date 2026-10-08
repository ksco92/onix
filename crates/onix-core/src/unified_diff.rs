// Portions of this module reimplement algorithms from CPython 3.14.6's
// `difflib` standard-library module (`unified_diff` and
// `_format_range_unified`), used under the PSF License Agreement version 2.
// See THIRD-PARTY-NOTICES.md at the repository root.
//! The `diff` field `DeepDiff` adds to a `str`→`str` `values_changed` at `verbose_level=2`: a
//! `difflib.unified_diff` with `lineterm=''` over the `str.splitlines()` lines of both values,
//! matched by [`crate::lcs`].
//!
//! # Trigger
//!
//! The field is added only when a literal `'\n'` occurs in either value; both are then split on
//! every `splitlines()` boundary (`\r`, `\r\n`, `\x0b`, `\x0c`, `\x1c`–`\x1e`, `\x85`, `\u{2028}`,
//! `\u{2029}`), so a `\r`-only string gets no field. Identical line lists (values differing only by
//! a trailing newline) yield an empty diff and no field.

use crate::lcs::{Tag, grouped_opcodes};
use crate::value::Value;

/// `difflib.unified_diff`'s default context (`n=3`), which `DeepDiff` keeps.
const CONTEXT_LINES: usize = 3;

/// The `diff` field for a `values_changed` between `a` and `b`, or `None` when `DeepDiff` attaches
/// none (see the module doc). Also `None` when either side holds a lone surrogate (`Str::Wtf8`),
/// which line-splitting cannot read; the `values_changed` entry itself is unaffected.
pub(crate) fn str_diff_field(a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Str(t1), Value::Str(t2)) => match (t1.as_utf8(), t2.as_utf8()) {
            (Some(t1), Some(t2)) => str_diff(t1, t2),
            _ => None,
        },
        _ => None,
    }
}

/// The `diff` string for two changed strings, or `None` when no field is warranted.
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

    // `lineterm=''` and empty file names: the headers are `"--- "` and `"+++ "`.
    let mut out: Vec<String> = vec!["--- ".to_string(), "+++ ".to_string()];
    for group in &groups {
        let (first, last) = group
            .first()
            .zip(group.last())
            .expect("grouped_opcodes yields only non-empty groups");
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

/// `difflib._format_range_unified`: `start`/`stop` are half-open 0-based, the output 1-based; an
/// empty range begins at the line just before it.
fn format_range_unified(start: usize, stop: usize) -> String {
    let beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return beginning.to_string();
    }
    if length == 0 {
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

/// Python's `str.splitlines()` without keepends: `\r\n` is one boundary and a trailing boundary
/// adds no empty line.
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
