//! DeepDiff-style path rendering.
//!
//! A path locates a value inside a nested dict/list structure, e.g.
//! `root['a'][3]['b c']`. This module owns the (small) segment vocabulary and
//! the textual rendering rules, kept isolated from the diff engine. The M5
//! golden corpus (generated against real `DeepDiff`) is the final authority
//! on quoting; see [`quote_key`]'s doc for the (surprisingly escape-free)
//! rule it verified.

/// One step in a path: either a dict key or a list index.
///
/// Derives `Ord` so a full path (`Vec<PathSegment>`) can be used as a
/// `BTreeMap` key — see [`crate::report::Report`]'s doc for why: findings
/// are keyed by this *structural* path, not by [`render_path`]'s rendered
/// string, because two distinct structural paths can render to the same
/// string (see [`quote_key`]'s doc) and only the structural form is
/// guaranteed unique per traversal. The derived order (`Key` before
/// `Index`; otherwise by the wrapped `String`/`usize`) is an internal
/// implementation detail used only to pick a deterministic survivor when
/// such a rendering collision occurs — it carries no meaning relative to
/// `DeepDiff` and nothing outside `Report`'s serialization should depend on
/// it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathSegment {
    /// A dict key access, e.g. the `'a'` in `root['a']`.
    Key(String),
    /// A list index access, e.g. the `3` in `root[3]`.
    Index(usize),
}

/// Renders a path (a sequence of [`PathSegment`]s from the root) as a
/// DeepDiff-style string.
///
/// An empty slice renders as `"root"`. Dict keys are rendered with
/// `DeepDiff`-compatible quoting (see [`quote_key`]); indices are rendered
/// as plain `[N]`.
///
/// # Examples
///
/// ```
/// use onix_core::path::{render_path, PathSegment};
///
/// assert_eq!(render_path(&[]), "root");
/// assert_eq!(
///     render_path(&[
///         PathSegment::Key("a".to_string()),
///         PathSegment::Index(3),
///         PathSegment::Key("b c".to_string()),
///     ]),
///     "root['a'][3]['b c']"
/// );
/// ```
#[must_use]
pub fn render_path(segments: &[PathSegment]) -> String {
    let mut rendered = String::from("root");
    for segment in segments {
        match segment {
            PathSegment::Key(key) => {
                rendered.push('[');
                rendered.push_str(&quote_key(key));
                rendered.push(']');
            }
            PathSegment::Index(index) => {
                rendered.push('[');
                rendered.push_str(&index.to_string());
                rendered.push(']');
            }
        }
    }
    rendered
}

/// Quotes a dict key exactly the way `DeepDiff` does when rendering
/// `root['key']` segments — which, per the M5 golden corpus generated
/// against real `DeepDiff`, is **not** Python `repr()`-style escaping.
///
/// `DeepDiff`'s own path-rendering code
/// (`deepdiff/model.py::ChildRelationship.stringify_param`, via
/// `deepdiff/path.py::stringify_element`) never escapes anything —
/// backslashes, control characters, and unicode are all embedded in the
/// output byte-for-byte. The only decision it makes is which quote
/// character to wrap with:
///
/// - A key containing a single quote (`'`), regardless of whether it also
///   contains a double quote, is wrapped in double quotes: `"it's"`.
/// - Every other key (no single quote, whether or not it contains a double
///   quote, a backslash, or control characters) is wrapped in single
///   quotes: `'he said "hi"'`, `'a\b'` (one literal backslash, not two).
///
/// This can produce a key segment containing an unescaped copy of its own
/// wrapping quote character (e.g. a key that is both single- and
/// double-quoted, wrapped in double quotes with the inner double quote left
/// bare) — `DeepDiff` does this too, confirmed empirically; matching it
/// byte-for-byte is the M5 correctness bar, not producing a "more correct"
/// escaping of our own invention. See `tests/golden/README.md` for the
/// verification commands.
///
/// An empty string renders as `''`.
///
/// # Examples
///
/// ```
/// use onix_core::path::quote_key;
///
/// assert_eq!(quote_key("a"), "'a'");
/// assert_eq!(quote_key("it's"), "\"it's\"");
/// assert_eq!(quote_key("he said \"hi\""), "'he said \"hi\"'");
/// ```
#[must_use]
pub fn quote_key(key: &str) -> String {
    if key.contains('\'') {
        format!("\"{key}\"")
    } else {
        format!("'{key}'")
    }
}

#[cfg(test)]
mod tests {
    use super::{PathSegment, quote_key, render_path};

    #[test]
    fn empty_path_renders_as_root() {
        assert_eq!(render_path(&[]), "root");
    }

    #[test]
    fn single_key_segment() {
        assert_eq!(
            render_path(&[PathSegment::Key("a".to_string())]),
            "root['a']"
        );
    }

    #[test]
    fn single_index_segment() {
        assert_eq!(render_path(&[PathSegment::Index(0)]), "root[0]");
    }

    #[test]
    fn mixed_nested_segments() {
        let segments = vec![
            PathSegment::Key("a".to_string()),
            PathSegment::Index(3),
            PathSegment::Key("b c".to_string()),
        ];
        assert_eq!(render_path(&segments), "root['a'][3]['b c']");
    }

    #[test]
    fn empty_string_key_renders_empty_quotes() {
        assert_eq!(render_path(&[PathSegment::Key(String::new())]), "root['']");
    }

    #[test]
    fn quote_key_default_uses_single_quotes() {
        assert_eq!(quote_key("a"), "'a'");
    }

    /// A key containing a single quote wraps in double quotes, matching
    /// real `DeepDiff` (verified in the M5 golden corpus: `key_single_quote`).
    #[test]
    fn quote_key_with_single_quote_uses_double_quotes() {
        assert_eq!(quote_key("it's"), "\"it's\"");
    }

    /// A key containing only a double quote (no single quote) wraps in
    /// single quotes, with the double quote left bare — no escaping (M5
    /// golden: `key_double_quote`).
    #[test]
    fn quote_key_with_double_quote_only_uses_single_quotes_unescaped() {
        assert_eq!(quote_key(r#"he said "hi""#), r#"'he said "hi"'"#);
    }

    /// A key containing both quote kinds still wraps in double quotes (the
    /// single-quote rule takes priority), leaving the inner double quotes
    /// bare and unescaped — real `DeepDiff` produces this same
    /// "self-quoting" output rather than escaping it (M5 golden:
    /// `key_both_quotes`).
    #[test]
    fn quote_key_with_both_quote_kinds_uses_double_quotes_unescaped() {
        // key: it's "cool"    (single quote after "it", double-quoted "cool")
        let mut key = String::new();
        key.push_str("it's ");
        key.push('"');
        key.push_str("cool");
        key.push('"');

        // expected: "it's "cool""   (whole key re-wrapped in double quotes,
        // its own inner double quotes left bare and unescaped)
        let mut expected = String::new();
        expected.push('"');
        expected.push_str(&key);
        expected.push('"');

        assert_eq!(quote_key(&key), expected);
    }

    /// No escaping of any kind: a literal backslash passes through as one
    /// character, not two (M5 golden: `key_backslash`).
    #[test]
    fn quote_key_does_not_escape_backslashes() {
        assert_eq!(quote_key(r"a\b"), r"'a\b'");
    }

    #[test]
    fn quote_key_keeps_unicode_literal() {
        assert_eq!(quote_key("héllo世界"), "'héllo世界'");
    }

    #[test]
    fn quote_key_empty_string() {
        assert_eq!(quote_key(""), "''");
    }

    /// No escaping of control characters either: newline, tab, NUL, and DEL
    /// all pass through as their literal (unescaped) characters (M5 golden:
    /// `key_control_chars`, which combines all four in one key).
    #[test]
    fn quote_key_does_not_escape_control_characters() {
        let mut key = String::new();
        key.push('a');
        key.push('\n');
        key.push('\t');
        key.push('\0');
        key.push('\u{7F}');
        key.push('b');

        let mut expected = String::new();
        expected.push('\'');
        expected.push('a');
        expected.push('\n');
        expected.push('\t');
        expected.push('\0');
        expected.push('\u{7F}');
        expected.push('b');
        expected.push('\'');

        assert_eq!(quote_key(&key), expected);
    }
}
