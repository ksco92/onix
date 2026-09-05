//! DeepDiff-style path rendering.
//!
//! A path locates a value inside a nested dict/list/set structure, e.g.
//! `root['a'][3]['b c']`. This module owns the (small) segment vocabulary and
//! the textual rendering rules, kept isolated from the diff engine. The
//! golden corpus (generated against real `DeepDiff`) is the final authority
//! on quoting; see [`quote_key`]'s doc for the (surprisingly escape-free)
//! rule it verified, and [`set_item_repr`]'s for the *different* — and
//! equally escape-free — rule a set item follows.

use std::fmt::Write as _;

use unicode_general_category::{GeneralCategory, get_general_category};

use crate::datetime::{SECONDS_PER_DAY, div_rem_euclid};
use crate::value::{Number, Value};

/// One step in a path: a dict key, a list index, or a set item.
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
    /// A set item, e.g. the `1` in `root[1]` for the set `{1}` — carrying
    /// the item **already rendered** by [`set_item_repr`], because that
    /// rendering is the only identity `DeepDiff` gives a set item: its
    /// `set_item_added`/`set_item_removed` entries are plain path strings
    /// built by formatting the item into the set's own path (see
    /// [`set_item_repr`]'s doc for the exact upstream code), never a
    /// subscript that could be resolved back to a position.
    SetItem(String),
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
            PathSegment::SetItem(item) => {
                rendered.push('[');
                rendered.push_str(item);
                rendered.push(']');
            }
        }
    }
    rendered
}

/// Quotes a dict key exactly the way `DeepDiff` does when rendering
/// `root['key']` segments — which, per the golden corpus generated against
/// real `DeepDiff`, is **not** Python `repr()`-style escaping.
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
/// byte-for-byte is the correctness bar, not producing a "more correct"
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

/// Renders one set item the way `DeepDiff` renders it inside a
/// `set_item_added`/`set_item_removed` entry — the text that becomes a
/// [`PathSegment::SetItem`].
///
/// `DeepDiff` builds those entries in
/// `model.py::TextResult._from_tree_set_item_added_or_removed`, which is
/// literally:
///
/// ```text
/// path = change.up.path()                  # the SET's own path
/// item = change.t2 if added else change.t1
/// if ADD_QUOTES_TO_STRINGS and isinstance(item, strings):
///     item = "'%s'" % item
/// "{}[{}]".format(path, str(item))
/// ```
///
/// So the rule has three halves, and none is [`quote_key`]'s:
///
/// - A `str` item is wrapped in **single quotes, unconditionally, with no
///   escaping of any kind** — `{"it's"}` renders `root['it's']`, where the
///   *dict key* `"it's"` renders `root["it's"]`. The two rules genuinely
///   differ; confirmed against `deepdiff==9.1.0`.
/// - A `datetime`/`date` item renders via Python's own `str()` for that
///   type — [`crate::datetime::DateTime::python_str`]/
///   [`crate::datetime::Date::python_str`], e.g. `{datetime(2024, 1, 1)}`
///   renders `root[2024-01-01 00:00:00]` and `{date(2024, 1, 1)}` renders
///   `root[2024-01-01]` — **not** `repr()`, unlike every other item kind:
///   Python's `str()` and `repr()` agree for `None`/`bool`/number/`tuple`/
///   `frozenset`, but a calendar value is the one type this model holds
///   where they genuinely differ. Confirmed against `deepdiff==9.1.0`.
/// - Every other item is rendered by Python's `str()`, which for the
///   remaining types a set can hold is `repr()` ([`python_repr`]) — so a
///   `str` *or a calendar value* nested **inside** a tuple or frozenset item
///   **is** rendered by `repr()`, unlike a top-level one:
///   `{("it's",)}` renders `root[("it's",)]` and
///   `{(datetime(2024, 1, 1),)}` renders
///   `root[(datetime.datetime(2024, 1, 1, 0, 0),)]`.
///
/// # Examples
///
/// ```
/// use onix_core::Value;
/// use onix_core::path::set_item_repr;
///
/// assert_eq!(set_item_repr(&Value::Str("it's".into())), "'it's'");
/// assert_eq!(set_item_repr(&Value::Bool(true)), "True");
/// ```
#[must_use]
pub fn set_item_repr(item: &Value) -> String {
    match item {
        Value::Str(s) => format!("'{s}'"),
        Value::DateTime(value) => value.python_str(),
        Value::Date(value) => value.python_str(),
        other => python_repr(other),
    }
}

/// Renders `value` as Python's `repr()` would.
///
/// `repr()` and `str()` agree on every type this model holds (Python only
/// separates them for `str` itself, and a container's `str()` uses `repr()`
/// for its elements either way), so this one function covers both sides of
/// [`set_item_repr`]'s rule.
///
/// A `set`/`frozenset` renders its members in the crate's canonical set
/// order (see [`crate::value::SetItems`]), where Python's own `str()` uses
/// the set's hash order — the one place a rendered set item can differ from
/// `DeepDiff`'s, and a documented one. See `tests/golden/README.md`.
///
/// Iterative (an explicit heap work-stack, no native recursion) all the way
/// through, sets included — their members are stored in canonical order, so
/// rendering one never sorts and never re-enters a comparator. This matches
/// [`Value`]'s own stack-safety posture: nothing bounds how deep a value a
/// caller may render, and a natively recursive renderer would be an
/// unguarded overflow sink on adversarially nested input.
#[must_use]
pub fn python_repr(value: &Value) -> String {
    let mut out = String::new();
    let mut stack: Vec<Work<'_>> = vec![Work::Value(value)];

    while let Some(work) = stack.pop() {
        match work {
            Work::Text(text) => out.push_str(text),
            Work::Key(key) => {
                out.push_str(&python_repr_str(key));
                out.push_str(": ");
            }
            Work::Value(value) => write_repr_head(&mut out, &mut stack, value),
        }
    }

    out
}

/// One step of [`python_repr`]'s work-stack: a value still to render, a
/// literal separator/bracket, or a dict key (rendered, then `": "`).
enum Work<'a> {
    Value(&'a Value),
    Text(&'static str),
    Key(&'a str),
}

/// Renders `value`'s own text into `out`, pushing any children it still
/// needs rendered onto `stack` (in reverse, so they pop in order).
fn write_repr_head<'a>(out: &mut String, stack: &mut Vec<Work<'a>>, value: &'a Value) {
    match value {
        Value::Null => out.push_str("None"),
        Value::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Value::Number(n) => out.push_str(&number_repr(n)),
        Value::Str(s) => out.push_str(&python_repr_str(s)),
        Value::DateTime(value) => out.push_str(&datetime_repr(*value)),
        Value::Date(value) => {
            let _ = write!(
                out,
                "datetime.date({}, {}, {})",
                value.year(),
                value.month(),
                value.day()
            );
        }
        Value::Array(items) => push_sequence(out, stack, items, "[", "]"),
        Value::Tuple(items) => {
            let close = if items.len() == 1 { ",)" } else { ")" };
            push_sequence(out, stack, items, "(", close);
        }
        Value::Set(items) => push_set(out, stack, items, "set()", "{", "}"),
        Value::FrozenSet(items) => {
            push_set(out, stack, items, "frozenset()", "frozenset({", "})");
        }
        Value::Object(map) => {
            out.push('{');
            stack.push(Work::Text("}"));
            for (index, (key, entry)) in map.iter().enumerate().rev() {
                stack.push(Work::Value(entry));
                stack.push(Work::Key(key));
                if index > 0 {
                    stack.push(Work::Text(", "));
                }
            }
        }
    }
}

/// Python's `repr()` for a `datetime`, which is what `str()` of a container
/// holding one shows — the form a calendar value takes *inside* a set item
/// (a bare top-level set item uses [`crate::datetime::DateTime::python_str`]
/// instead — see [`set_item_repr`]'s doc for both halves of the rule).
/// Python omits the trailing zero fields: seconds appear only when the
/// second or the microsecond is non-zero, and
/// microseconds only when non-zero.
fn datetime_repr(value: crate::datetime::DateTime) -> String {
    let date = value.date();
    let mut out = format!(
        "datetime.datetime({}, {}, {}, {}, {}",
        date.year(),
        date.month(),
        date.day(),
        value.hour(),
        value.minute()
    );

    if value.second() != 0 || value.microsecond() != 0 {
        let _ = write!(out, ", {}", value.second());
    }
    if value.microsecond() != 0 {
        let _ = write!(out, ", {}", value.microsecond());
    }

    match value.utc_offset_seconds() {
        None => {}
        // Python's own `timezone.utc` singleton reprs by name; every other
        // fixed offset reprs as the `timedelta` it was built from, which
        // normalizes a negative offset into whole days plus seconds.
        Some(0) => out.push_str(", tzinfo=datetime.timezone.utc"),
        Some(offset) => {
            let (days, seconds) = div_rem_euclid(i64::from(offset), SECONDS_PER_DAY);
            let day_part = if days == 0 {
                String::new()
            } else {
                format!("days={days}, ")
            };
            let _ = write!(
                out,
                ", tzinfo=datetime.timezone(datetime.timedelta({day_part}seconds={seconds}))"
            );
        }
    }

    out.push(')');
    out
}

/// Writes one set's repr: `empty` when it has no members, otherwise
/// `open`, its members (already in the crate's canonical order — see
/// [`crate::value::SetItems`]) and `close`.
fn push_set<'a>(
    out: &mut String,
    stack: &mut Vec<Work<'a>>,
    items: &'a crate::value::SetItems,
    empty: &'static str,
    open: &'static str,
    close: &'static str,
) {
    if items.is_empty() {
        out.push_str(empty);
        return;
    }

    out.push_str(open);
    stack.push(Work::Text(close));
    for (index, item) in items.iter().enumerate().rev() {
        stack.push(Work::Value(item));
        if index > 0 {
            stack.push(Work::Text(", "));
        }
    }
}

/// Writes `open` and schedules `items` comma-separated followed by `close`
/// — the shared shape of every bracketed Python container repr.
fn push_sequence<'a>(
    out: &mut String,
    stack: &mut Vec<Work<'a>>,
    items: &'a [Value],
    open: &'static str,
    close: &'static str,
) {
    out.push_str(open);
    stack.push(Work::Text(close));
    for (index, item) in items.iter().enumerate().rev() {
        stack.push(Work::Value(item));
        if index > 0 {
            stack.push(Work::Text(", "));
        }
    }
}

/// Python's `repr()` for a `str`: single quotes unless the string contains a
/// single quote and no double quote (then double quotes), with `\`, the
/// wrapping quote and every non-printable code point escaped as `\xXX`,
/// `\uXXXX` or `\UXXXXXXXX` per [`escape_non_printable`].
fn python_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };

    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '\t' => out.push_str(r"\t"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_non_printable(c) => escape_non_printable(&mut out, c),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` for an `int` or a `float`, split by the [`Number`]
/// representation the value was parsed or built with — the same int/float
/// distinction `crate::diff`'s `python_type_name` reports.
fn number_repr(n: &Number) -> String {
    if n.is_f64() {
        return python_float_repr(
            n.as_f64()
                .expect("Number::is_f64 guarantees as_f64 succeeds"),
        );
    }
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    n.as_u64()
        .expect("a non-float Number always has an i64 or u64 representation")
        .to_string()
}

/// Whether `c` is one of the code points Python's `repr()` escapes: every
/// character in Unicode general categories `Cc`, `Cf`, `Cs`, `Co`, `Cn`,
/// `Zl`, `Zp` or `Zs`, except the plain space (`U+0020`), which is `Zs` but
/// stays printable. This is `CPython`'s own rule
/// (`Tools/unicode/makeunicodedata.py`'s `PRINTABLE_MASK`, read from the
/// `unicode-general-category` table pinned to Unicode 16.0.0 — the same
/// version Python 3.14's `unicodedata` module ships).
fn is_non_printable(c: char) -> bool {
    if c == ' ' {
        return false;
    }
    matches!(
        get_general_category(c),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::SpaceSeparator
    )
}

/// Appends `c`'s `repr()` escape to `out`: `\xXX` below `U+0100`, `\uXXXX`
/// up to `U+FFFF`, `\UXXXXXXXX` above — the same three widths
/// `Objects/unicodeobject.c`'s `unicode_repr` picks by.
fn escape_non_printable(out: &mut String, c: char) {
    let code_point = u32::from(c);
    // Writing into a `String` is infallible.
    let _ = if code_point < 0x100 {
        write!(out, "\\x{code_point:02x}")
    } else if code_point < 0x1_0000 {
        write!(out, "\\u{code_point:04x}")
    } else {
        write!(out, "\\U{code_point:08x}")
    };
}

/// Python's `repr()` for a `float`, which is `float_repr_style="short"`: the
/// shortest decimal string that round-trips, formatted with an exponent
/// when the decimal point sits at or below `-4` or above `16`, and with a
/// `.0` suffix otherwise so the result always reads as a float.
///
/// # Why this is two formatting calls, not one
///
/// `CPython`'s `repr` is `dtoa` mode 0: among the shortest digit strings
/// that round-trip, the one *nearest* the float's exact value, ties broken
/// to an even last digit. Rust's `{:e}` produces a shortest string that
/// round-trips, which fixes the digit *count* but not always the last
/// digit: about one float in 3,800 sits close enough to a midpoint that
/// the two disagree (`160598971591683.12` renders as `...13`). So the digit
/// count comes from `{:e}`, and the digits themselves from a second,
/// fixed-precision `{:.*e}` at that count — Rust's exact mode, which is
/// correctly rounded with ties to even, i.e. mode 0's own rule. Both calls
/// are in `core::fmt`; no arbitrary-precision arithmetic and no dependency
/// is involved. Verified against real Python `repr()` over a million random
/// bit patterns in the bindings suite.
fn python_float_repr(value: f64) -> String {
    let shortest = format!("{value:e}");
    let significant = shortest
        .split_once('e')
        .map_or(shortest.as_str(), |(mantissa, _)| mantissa)
        .chars()
        .filter(char::is_ascii_digit)
        .count();
    let scientific = format!("{value:.*e}", significant.saturating_sub(1));
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("Rust's {:e} always emits an `e` separator");
    let exponent: i32 = exponent
        .parse()
        .expect("Rust's {:e} always emits a decimal exponent");
    let (sign, mantissa) = mantissa
        .strip_prefix('-')
        .map_or(("", mantissa), |rest| ("-", rest));
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    // Re-rounding to the shortest length can carry (`9.99` to `1.00e1`),
    // which pads with zeros Python's shortest form never keeps.
    let trimmed = digits.trim_end_matches('0').len().max(1);
    digits.truncate(trimmed);

    // `decimal_point` is Python's own `decpt`: the value is
    // `0.<digits> * 10^decimal_point`.
    let decimal_point = exponent + 1;

    if decimal_point <= -4 || decimal_point > 16 {
        let (lead, rest) = digits.split_at(1);
        let point = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!(
            "{sign}{lead}{point}e{exponent_sign}{:02}",
            exponent.unsigned_abs()
        );
    }

    if decimal_point <= 0 {
        // The decimal point sits at or before the first digit: `0.` then
        // enough leading zeros to push the digits down to their place.
        let zeros = "0".repeat(usize::try_from(-decimal_point).unwrap_or(0));
        return format!("{sign}0.{zeros}{digits}");
    }

    let decimal_point =
        usize::try_from(decimal_point).expect("the branch above rejected every non-positive value");

    if decimal_point >= digits.len() {
        let zeros = "0".repeat(decimal_point - digits.len());
        return format!("{sign}{digits}{zeros}.0");
    }

    let (whole, fraction) = digits.split_at(decimal_point);
    format!("{sign}{whole}.{fraction}")
}

#[cfg(test)]
mod tests {
    use super::{
        PathSegment, escape_non_printable, python_repr, quote_key, render_path, set_item_repr,
    };
    use crate::test_support::{cdate, cdt_at};
    use crate::value::{Builder, Number, SetItems, Value};

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
    /// real `DeepDiff` (verified in the golden corpus: `key_single_quote`).
    #[test]
    fn quote_key_with_single_quote_uses_double_quotes() {
        assert_eq!(quote_key("it's"), "\"it's\"");
    }

    /// A key containing only a double quote (no single quote) wraps in
    /// single quotes, with the double quote left bare — no escaping
    /// (golden: `key_double_quote`).
    #[test]
    fn quote_key_with_double_quote_only_uses_single_quotes_unescaped() {
        assert_eq!(quote_key(r#"he said "hi""#), r#"'he said "hi"'"#);
    }

    /// A key containing both quote kinds still wraps in double quotes (the
    /// single-quote rule takes priority), leaving the inner double quotes
    /// bare and unescaped — real `DeepDiff` produces this same
    /// "self-quoting" output rather than escaping it
    /// (golden: `key_both_quotes`).
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
    /// character, not two (golden: `key_backslash`).
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

    /// A set item renders as its own path segment, with no quoting applied
    /// on top of [`set_item_repr`]'s own (golden: `set_str_items`).
    #[test]
    fn set_item_segment_renders_its_text_verbatim() {
        assert_eq!(
            render_path(&[
                PathSegment::Key("a".to_string()),
                PathSegment::SetItem("(1, 2)".to_string()),
            ]),
            "root['a'][(1, 2)]"
        );
    }

    /// A top-level `str` set item is wrapped in single quotes
    /// unconditionally and with no escaping — deliberately **not**
    /// [`quote_key`]'s rule, which would double-quote the second of these
    /// (golden: `set_str_item_with_single_quote`,
    /// `set_str_item_with_double_quote`).
    #[test]
    fn set_item_str_always_uses_bare_single_quotes() {
        assert_eq!(set_item_repr(&Value::Str("a".into())), "'a'");
        assert_eq!(set_item_repr(&Value::Str("it's".into())), "'it's'");
        assert_eq!(
            set_item_repr(&Value::Str(r#"he said "hi""#.into())),
            r#"'he said "hi"'"#
        );
        assert_eq!(set_item_repr(&Value::Str("a\nb".into())), "'a\nb'");
        assert_ne!(
            set_item_repr(&Value::Str("it's".into())),
            quote_key("it's"),
            "the set-item rule and the dict-key rule genuinely differ"
        );
    }

    #[test]
    fn set_item_scalars_render_as_python_str() {
        assert_eq!(set_item_repr(&Value::Null), "None");
        assert_eq!(set_item_repr(&Value::Bool(true)), "True");
        assert_eq!(set_item_repr(&Value::Bool(false)), "False");
        assert_eq!(set_item_repr(&Value::Number(Number::from_i64(-7))), "-7");
        assert_eq!(
            set_item_repr(&Value::Number(Number::from_u64(u64::MAX))),
            "18446744073709551615"
        );
    }

    /// A `str` nested inside a container item goes through Python `repr()`,
    /// which *does* escape — the other half of the set-item rule (golden:
    /// `set_str_inside_tuple_item`).
    #[test]
    fn str_nested_in_a_tuple_item_uses_python_repr() {
        let tuple = Value::Tuple(Box::new([Value::Str("it's".into())]));
        assert_eq!(set_item_repr(&tuple), r#"("it's",)"#);

        let both = Value::Tuple(Box::new([Value::Str("it's \"x\"".into())]));
        assert_eq!(set_item_repr(&both), r#"('it\'s "x"',)"#);
    }

    #[test]
    fn python_repr_renders_every_container_kind() {
        let inner = Value::Tuple(Box::new([Value::Number(Number::from_u64(1))]));
        assert_eq!(python_repr(&inner), "(1,)");
        assert_eq!(
            python_repr(&Value::Tuple(Box::new([
                Value::Number(Number::from_u64(1)),
                Value::Number(Number::from_u64(2)),
            ]))),
            "(1, 2)"
        );
        assert_eq!(python_repr(&Value::Tuple(Box::new([]))), "()");
        assert_eq!(
            python_repr(&Value::Array(Box::new([
                Value::Null,
                Value::Str("a".into()),
            ]))),
            "[None, 'a']"
        );
        assert_eq!(python_repr(&Value::Array(Box::new([]))), "[]");
    }

    /// A set renders its members in the crate's canonical order, whatever
    /// order they are stored in.
    #[test]
    fn python_repr_renders_sets_in_canonical_order() {
        let members = || {
            vec![
                Value::Number(Number::from_u64(2)),
                Value::Number(Number::from_u64(1)),
            ]
        };

        assert_eq!(python_repr(&Value::Set(SetItems::new(members()))), "{1, 2}");
        assert_eq!(python_repr(&Value::Set(SetItems::new(vec![]))), "set()");
        assert_eq!(
            python_repr(&Value::FrozenSet(SetItems::new(members()))),
            "frozenset({1, 2})"
        );
        assert_eq!(
            python_repr(&Value::FrozenSet(SetItems::new(vec![]))),
            "frozenset()"
        );
    }

    /// Every float the tie-breaking rule was found to disagree on: Rust's
    /// own shortest form rounds these away from Python's `repr`, so this
    /// goes red on a renderer that trusts `{:e}`'s digits.
    #[test]
    fn python_float_repr_breaks_shortest_form_ties_pythons_way() {
        let cases = [
            (160_598_971_591_683.12_f64, "160598971591683.12"),
            (2_113_325_745_016_023.2, "2113325745016023.2"),
            (-20_243_279_817_481.062, "-20243279817481.062"),
            (245_712_874_376_162.12, "245712874376162.12"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                set_item_repr(&Value::Number(Number::from_f64(input).expect("finite"))),
                expected,
                "for {input:?}"
            );
        }
    }

    #[test]
    fn python_repr_renders_a_dict_with_repr_keys() {
        let mut builder = Builder::new();
        let object = builder.object(vec![
            ("b".to_string(), Value::Number(Number::from_u64(2))),
            ("a".to_string(), Value::Null),
        ]);
        assert_eq!(python_repr(&object), "{'a': None, 'b': 2}");
        assert_eq!(python_repr(&builder.object(vec![])), "{}");
    }

    /// A container nested several levels deep still renders element by
    /// element, proving the explicit work-stack composes rather than only
    /// handling one level.
    #[test]
    fn python_repr_nests_containers() {
        let value = Value::Tuple(Box::new([
            Value::Number(Number::from_u64(1)),
            Value::Tuple(Box::new([
                Value::Number(Number::from_u64(2)),
                Value::FrozenSet(SetItems::new(vec![Value::Str("x".into())])),
            ])),
        ]));
        assert_eq!(python_repr(&value), "(1, (2, frozenset({'x'})))");
    }

    /// The renderer is iterative, so a nest far deeper than any native
    /// stack tolerates renders instead of aborting the process.
    #[test]
    fn python_repr_of_a_very_deep_nest_does_not_overflow_the_stack() {
        let mut value = Value::Tuple(Box::new([]));
        for _ in 0..100_000 {
            value = Value::Tuple(Box::new([value]));
        }

        let rendered = python_repr(&value);

        assert!(rendered.starts_with("((((("));
        assert!(rendered.ends_with(",),),),),)"));
    }

    /// Python's own `repr` quoting: single quotes by default, double
    /// quotes only when the string holds a single quote and no double one,
    /// and a backslash escape when both appear.
    #[test]
    fn python_repr_str_picks_pythons_quote_and_escapes() {
        let cases = [
            ("a", "'a'"),
            ("it's", "\"it's\""),
            (r#"say "hi""#, r#"'say "hi"'"#),
            (r#"it's "x""#, r#"'it\'s "x"'"#),
            (r"a\b", r"'a\\b'"),
            ("a\tb\nc\rd", r"'a\tb\nc\rd'"),
            ("a\u{0}b", r"'a\x00b'"),
            ("a\u{7f}\u{a0}\u{ad}b", r"'a\x7f\xa0\xadb'"),
            ("héllo世界", "'héllo世界'"),
        ];
        for (input, expected) in cases {
            let item = Value::Tuple(Box::new([Value::Str(input.into())]));
            assert_eq!(
                python_repr(&item),
                format!("({expected},)"),
                "for {input:?}"
            );
        }
    }

    /// Every expectation here is real Python `repr()` output, verified
    /// against `CPython` 3.14 (Unicode 16.0.0).
    #[test]
    fn python_repr_str_escapes_non_printable_code_points_above_u0100() {
        let cases = [
            // U+00FF: printable Latin-1, the code point right after the old
            // below-U+0100 ceiling — left bare, not escaped.
            ("a\u{ff}b", "'a\u{ff}b'"),
            // U+200B: Cf (zero width space) — \uXXXX width.
            ("a\u{200b}b", r"'a\u200bb'"),
            // U+2028: Zl (line separator) — \uXXXX width.
            ("a\u{2028}b", r"'a\u2028b'"),
            // U+E000: Co (private use) — \uXXXX width.
            ("a\u{e000}b", r"'a\ue000b'"),
            // U+0378: Cn (unassigned in Unicode 16.0.0) — \uXXXX width.
            ("a\u{378}b", r"'a\u0378b'"),
            // U+FFFF: Cn (a BMP noncharacter) — the top of the \uXXXX width.
            ("a\u{ffff}b", r"'a\uffffb'"),
            // U+10000: Lo, printable astral text — left bare.
            ("a\u{10000}b", "'a\u{10000}b'"),
            // U+1F600: So, a printable astral emoji — left bare.
            ("a\u{1f600}b", "'a\u{1f600}b'"),
            // U+F0000: Co (a supplementary private-use plane) — \UXXXXXXXX
            // width, the one the old port could not reach at all.
            ("a\u{f0000}b", r"'a\U000f0000b'"),
            // U+10FFFF: Cn, the last valid Unicode scalar value.
            ("a\u{10ffff}b", r"'a\U0010ffffb'"),
        ];
        for (input, expected) in cases {
            let item = Value::Tuple(Box::new([Value::Str(input.into())]));
            assert_eq!(
                python_repr(&item),
                format!("({expected},)"),
                "for {input:?}"
            );
        }
    }

    /// The plain space (`U+0020`) is `Zs` like every other escaped space
    /// separator, but Python's own rule carves it out as printable — the
    /// one exception `is_non_printable` must apply.
    #[test]
    fn python_repr_str_does_not_escape_plain_space() {
        let item = Value::Tuple(Box::new([Value::Str("a b".into())]));
        assert_eq!(python_repr(&item), "('a b',)");
    }

    /// Exercises `escape_non_printable` directly, at the two code points
    /// each escape width's `<` comparison must reject vs. accept: `U+00FF`
    /// stays `\xXX`, `U+0100` switches to `\uXXXX`; `U+FFFF` stays `\uXXXX`,
    /// `U+10000` switches to `\UXXXXXXXX`.
    #[test]
    fn escape_non_printable_widths_switch_exactly_at_their_boundaries() {
        let cases = [
            (0xff, "\\xff"),
            (0x100, "\\u0100"),
            (0xffff, "\\uffff"),
            (0x1_0000, "\\U00010000"),
        ];
        for (code_point, expected) in cases {
            let mut out = String::new();
            escape_non_printable(
                &mut out,
                char::from_u32(code_point).expect("valid code point"),
            );
            assert_eq!(out, expected, "for U+{code_point:04X}");
        }
    }

    /// Fails `make check` if a `unicode-general-category` bump ever changes
    /// the table `is_non_printable` reads, so a Unicode-version drift is
    /// caught here rather than only by the Python bindings' own
    /// interpreter-version-gated differential test (`test_sets.py`'s
    /// `test_bmp_printability_table_matches_the_running_interpreter_on_3_14`),
    /// which needs a Python 3.14 interpreter to run at all.
    #[test]
    fn unicode_general_category_stays_pinned_to_16_0_0() {
        assert_eq!(unicode_general_category::UNICODE_VERSION, (16, 0, 0));
    }

    /// Python's `repr()` for a calendar value — the form a container holding
    /// one shows, and the form issue #21 will need once a calendar value may
    /// be a set member. Every expectation here is real Python `repr()`
    /// output, including the trailing-field trimming and the way a negative
    /// offset's `timedelta` normalizes into whole days plus seconds.
    #[test]
    fn calendar_values_render_as_python_repr() {
        let cases = [
            (
                cdt_at(2024, 1, 1, 0, 0, 0, 0, None),
                "datetime.datetime(2024, 1, 1, 0, 0)",
            ),
            (
                cdt_at(2024, 1, 1, 10, 30, 0, 0, None),
                "datetime.datetime(2024, 1, 1, 10, 30)",
            ),
            (
                cdt_at(2024, 1, 1, 10, 30, 5, 0, None),
                "datetime.datetime(2024, 1, 1, 10, 30, 5)",
            ),
            (
                cdt_at(2024, 1, 1, 10, 30, 5, 7, None),
                "datetime.datetime(2024, 1, 1, 10, 30, 5, 7)",
            ),
            (
                cdt_at(2024, 1, 1, 0, 0, 0, 7, None),
                "datetime.datetime(2024, 1, 1, 0, 0, 0, 7)",
            ),
            (
                cdt_at(2024, 1, 1, 0, 0, 0, 0, Some(0)),
                "datetime.datetime(2024, 1, 1, 0, 0, tzinfo=datetime.timezone.utc)",
            ),
            (
                cdt_at(2024, 1, 1, 0, 0, 0, 0, Some(3600)),
                "datetime.datetime(2024, 1, 1, 0, 0, \
                 tzinfo=datetime.timezone(datetime.timedelta(seconds=3600)))",
            ),
            (
                cdt_at(2024, 1, 1, 0, 0, 0, 0, Some(-18000)),
                "datetime.datetime(2024, 1, 1, 0, 0, \
                 tzinfo=datetime.timezone(datetime.timedelta(days=-1, seconds=68400)))",
            ),
            (cdate(2024, 1, 1), "datetime.date(2024, 1, 1)"),
        ];
        for (value, expected) in cases {
            assert_eq!(
                python_repr(&value),
                expected.replace("\n                 ", "")
            );
        }
    }

    /// Python's `float.__repr__`: always a decimal point or an exponent,
    /// the exponent form at `decpt <= -4` or `decpt > 16`, and an
    /// exponent of at least two digits with an explicit sign. Every
    /// expectation here is real Python `repr()` output (see also the
    /// bindings suite's seeded 1,000-float differential test).
    #[test]
    fn python_float_repr_matches_python() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (-2.5, "-2.5"),
            (0.1, "0.1"),
            (1.5, "1.5"),
            (100.0, "100.0"),
            (123.456, "123.456"),
            (0.0001, "0.0001"),
            (1e-5, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (1e100, "1e+100"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                set_item_repr(&Value::Number(Number::from_f64(input).expect("finite"))),
                expected,
                "for {input:?}"
            );
        }
    }

    /// No escaping of control characters either: newline, tab, NUL, and DEL
    /// all pass through as their literal (unescaped) characters (golden:
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
