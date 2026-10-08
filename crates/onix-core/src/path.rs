//! `DeepDiff`-style path rendering. The quoting rules live on [`quote_key`] and
//! [`set_item_repr`].

use std::fmt::Write as _;

use unicode_general_category::{GeneralCategory, get_general_category};

use crate::datetime::{SECONDS_PER_DAY, div_rem_euclid};
use crate::value::{Number, ObjectKey, ObjectKind, Str, Value, Wtf8Char, Wtf8Chars};

/// One path step. `Ord` is internal: it picks a deterministic survivor on a
/// rendered-string collision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathSegment {
    /// A `str` dict key, quoted per [`quote_key`].
    Key(Str),
    /// A non-`str` dict key already rendered by [`dict_key_repr`], e.g. `[1]` or `[1][2]`.
    KeyRepr(String),
    /// A custom object's attribute, rendered `.name` with no brackets or quoting.
    Attribute(Str),
    /// A list index, rendered `[3]`.
    Index(usize),
    /// A set item already rendered by [`set_item_repr`], e.g. `[1]`.
    SetItem(String),
}

/// Renders a path (a sequence of [`PathSegment`]s from the root) as a
/// DeepDiff-style string; an empty slice is `"root"`.
///
/// Returns `Str` so a lone-surrogate key survives byte-exact.
///
/// # Examples
///
/// ```
/// use onix_core::path::{render_path, PathSegment};
///
/// assert_eq!(render_path(&[]).to_string(), "root");
/// assert_eq!(
///     render_path(&[
///         PathSegment::Key("a".into()),
///         PathSegment::Index(3),
///         PathSegment::Key("b c".into()),
///     ])
///     .to_string(),
///     "root['a'][3]['b c']"
/// );
/// ```
#[must_use]
pub fn render_path(segments: &[PathSegment]) -> Str {
    let mut rendered: Vec<u8> = b"root".to_vec();
    for segment in segments {
        match segment {
            PathSegment::Key(key) => {
                rendered.push(b'[');
                rendered.extend_from_slice(quote_key(key).as_bytes());
                rendered.push(b']');
            }
            PathSegment::KeyRepr(key) => {
                rendered.push(b'[');
                rendered.extend_from_slice(key.as_bytes());
                rendered.push(b']');
            }
            PathSegment::Attribute(name) => {
                rendered.push(b'.');
                rendered.extend_from_slice(name.as_bytes());
            }
            PathSegment::Index(index) => {
                rendered.push(b'[');
                rendered.extend_from_slice(index.to_string().as_bytes());
                rendered.push(b']');
            }
            PathSegment::SetItem(item) => {
                rendered.push(b'[');
                rendered.extend_from_slice(item.as_bytes());
                rendered.push(b']');
            }
        }
    }
    bytes_to_str(rendered)
}

/// Wraps WTF-8 `bytes` as `Utf8` when valid, else `Wtf8`.
fn bytes_to_str(bytes: Vec<u8>) -> Str {
    match String::from_utf8(bytes) {
        Ok(s) => Str::Utf8(s.into_boxed_str()),
        Err(err) => Str::Wtf8(err.into_bytes().into_boxed_slice()),
    }
}

/// Quotes a dict key the way `DeepDiff` renders `root['key']`: nothing is
/// escaped, only the wrapping quote varies.
///
/// - A key containing a single quote (`'`) is wrapped in double quotes:
///   `"it's"`.
/// - Every other key is wrapped in single quotes: `'he said "hi"'`, `'a\b'`
///   (one literal backslash).
///
/// Distinct paths can therefore render identically (a key holding both quote
/// kinds keeps its inner `"` bare). An empty string renders as `''`. A lone
/// surrogate is embedded as its raw WTF-8 bytes, never Python's `\udcXX`
/// escape.
///
/// # Examples
///
/// ```
/// use onix_core::path::quote_key;
///
/// assert_eq!(quote_key(&"a".into()).to_string(), "'a'");
/// assert_eq!(quote_key(&"it's".into()).to_string(), "\"it's\"");
/// assert_eq!(quote_key(&"he said \"hi\"".into()).to_string(), "'he said \"hi\"'");
/// ```
#[must_use]
pub fn quote_key(key: &Str) -> Str {
    let quote = if key.as_bytes().contains(&b'\'') {
        b'"'
    } else {
        b'\''
    };

    let mut out = Vec::with_capacity(key.as_bytes().len() + 2);
    out.push(quote);
    out.extend_from_slice(key.as_bytes());
    out.push(quote);
    bytes_to_str(out)
}

/// Writes `s` with each lone surrogate as `\uXXXX`.
fn push_wtf8_unescaped(out: &mut String, s: &Str) {
    for c in s.chars() {
        match c {
            Wtf8Char::Scalar(c) => out.push(c),
            Wtf8Char::Surrogate(cp) => {
                let _ = write!(out, "\\u{cp:04x}");
            }
        }
    }
}

/// Renders one set item the way `DeepDiff` renders it inside a
/// `set_item_added`/`set_item_removed` entry, the text of a
/// [`PathSegment::SetItem`].
///
/// Upstream (`model.py::TextResult._from_tree_set_item_added_or_removed`):
///
/// ```text
/// path = change.up.path()                  # the SET's own path
/// item = change.t2 if added else change.t1
/// if ADD_QUOTES_TO_STRINGS and isinstance(item, strings):
///     item = "'%s'" % item
/// "{}[{}]".format(path, str(item))
/// ```
///
/// - A `str` item is wrapped in single quotes with no escaping: `{"it's"}`
///   renders `root['it's']`, where the dict key `"it's"` renders `root["it's"]`.
/// - A `datetime`/`date`/`time`/`timedelta` item renders via Python's `str()`
///   ([`crate::datetime::DateTime::python_str`] and its siblings), not
///   `repr()`: `{date(2024, 1, 1)}` renders `root[2024-01-01]`.
/// - Every other item renders as `repr()` ([`python_repr`]), so a `str` or
///   calendar value nested inside a tuple or frozenset is escaped or
///   `repr`-formed: `{("it's",)}` renders `root[("it's",)]`.
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
        Value::Str(s) => {
            let mut out = String::with_capacity(s.as_bytes().len() + 2);
            out.push('\'');
            push_wtf8_unescaped(&mut out, s);
            out.push('\'');
            out
        }
        Value::DateTime(value) => value.python_str(),
        Value::Date(value) => value.python_str(),
        Value::Time(value) => value.python_str(),
        Value::TimeDelta(value) => value.python_str(),
        other => python_repr(other),
    }
}

/// Renders a non-`str` [`ObjectKey::Other`] as `DeepDiff` does: a `tuple` key
/// is `']['.join(map(repr, key))`, so `(1, 2)` gives `1][2` and
/// [`render_path`] yields `root[1][2]`; any other key is [`python_repr`].
#[must_use]
pub fn dict_key_repr(key: &Value) -> String {
    match key {
        Value::Tuple(items) => items.iter().map(python_repr).collect::<Vec<_>>().join("]["),
        other => python_repr(other),
    }
}

/// The [`PathSegment`] one [`ObjectKey`] contributes: [`quote_key`] for a `str`
/// key, [`dict_key_repr`] for any other. Shared by the diff engine and the
/// Python bindings' conversion-error paths.
#[must_use]
pub fn object_key_path_segment(key: &ObjectKey) -> PathSegment {
    match key {
        ObjectKey::Str(s) => PathSegment::Key(s.into()),
        ObjectKey::Other(value) => PathSegment::KeyRepr(dict_key_repr(value)),
    }
}

/// The path segment one entry of an [`Object`](crate::value::Object) of
/// `kind` contributes: [`object_key_path_segment`] for a `dict`, a dotted
/// [`PathSegment::Attribute`] (`root.name`) for a custom object's `str` key.
#[must_use]
pub fn entry_path_segment(kind: ObjectKind, key: &ObjectKey) -> PathSegment {
    match (kind, key) {
        (
            ObjectKind::CustomObject | ObjectKind::Opaque | ObjectKind::Cycle | ObjectKind::Failed,
            ObjectKey::Str(s),
        ) => PathSegment::Attribute(s.into()),
        _ => object_key_path_segment(key),
    }
}

/// Python `repr()` of `value`, iteratively, because no depth guard bounds what
/// a caller renders and a recursive renderer would overflow the native stack on
/// adversarial nesting; sets render in canonical order
/// ([`crate::value::SetItems`]), not Python's hash order.
#[must_use]
pub fn python_repr(value: &Value) -> String {
    let mut out = String::new();
    let mut stack: Vec<Work<'_>> = vec![Work::Value(value)];

    while let Some(work) = stack.pop() {
        match work {
            Work::Text(text) => out.push_str(text),
            Work::Key(key) => {
                out.push_str(&object_key_repr(key));
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
    Key(&'a ObjectKey),
}

/// Renders one [`ObjectKey`] as the whole dict's `repr()` shows it:
/// `{1: 'x'}`, not `{'1': 'x'}`.
fn object_key_repr(key: &ObjectKey) -> String {
    match key {
        ObjectKey::Str(s) => python_repr_bytes(s.as_bytes()),
        ObjectKey::Other(value) => python_repr(value),
    }
}

/// Renders `value`'s own text into `out`, pushing any children it still
/// needs rendered onto `stack` (in reverse, so they pop in order).
fn write_repr_head<'a>(out: &mut String, stack: &mut Vec<Work<'a>>, value: &'a Value) {
    match value {
        Value::Null => out.push_str("None"),
        Value::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Value::Number(n) => out.push_str(&number_repr(n)),
        Value::Str(s) => out.push_str(&python_repr_bytes(s.as_bytes())),
        Value::DateTime(value) => out.push_str(&datetime_repr(value.value())),
        Value::Date(value) => {
            let _ = write!(
                out,
                "datetime.date({}, {}, {})",
                value.year(),
                value.month(),
                value.day()
            );
        }
        Value::Time(value) => out.push_str(&time_repr(value.value())),
        Value::TimeDelta(value) => out.push_str(&timedelta_repr(value.value())),
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

/// Python `repr()` for a `datetime`; trailing zero seconds and microseconds
/// are omitted.
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

    out.push_str(&tzinfo_repr_suffix(value.utc_offset_seconds()));
    out.push(')');
    out
}

/// Python `repr()` for a `time`, with [`datetime_repr`]'s omission rules.
fn time_repr(value: crate::datetime::Time) -> String {
    let mut out = format!("datetime.time({}, {}", value.hour(), value.minute());

    if value.second() != 0 || value.microsecond() != 0 {
        let _ = write!(out, ", {}", value.second());
    }
    if value.microsecond() != 0 {
        let _ = write!(out, ", {}", value.microsecond());
    }

    out.push_str(&tzinfo_repr_suffix(value.utc_offset_seconds()));
    out.push(')');
    out
}

/// The `tzinfo=...` suffix of a `datetime`/`time` repr; a non-zero offset
/// normalizes into whole days plus seconds.
fn tzinfo_repr_suffix(offset: Option<i32>) -> String {
    match offset {
        None => String::new(),
        Some(0) => ", tzinfo=datetime.timezone.utc".to_string(),
        Some(offset) => {
            let (days, seconds) = div_rem_euclid(i64::from(offset), SECONDS_PER_DAY);
            let day_part = if days == 0 {
                String::new()
            } else {
                format!("days={days}, ")
            };
            format!(", tzinfo=datetime.timezone(datetime.timedelta({day_part}seconds={seconds}))")
        }
    }
}

/// Python `repr()` for a `timedelta`: non-zero fields only, and
/// `datetime.timedelta(0)` when all are zero.
fn timedelta_repr(value: crate::datetime::TimeDelta) -> String {
    let (days, seconds, microseconds) = (value.days(), value.seconds(), value.microseconds());

    if days == 0 && seconds == 0 && microseconds == 0 {
        return "datetime.timedelta(0)".to_string();
    }

    let mut fields = Vec::with_capacity(3);
    if days != 0 {
        fields.push(format!("days={days}"));
    }
    if seconds != 0 {
        fields.push(format!("seconds={seconds}"));
    }
    if microseconds != 0 {
        fields.push(format!("microseconds={microseconds}"));
    }

    format!("datetime.timedelta({})", fields.join(", "))
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

/// Python `repr()` for a `str` given as WTF-8 bytes, so a lone surrogate
/// escapes as `\udcXX`. Uses double quotes only when the text holds a single
/// quote and no double quote.
fn python_repr_bytes(bytes: &[u8]) -> String {
    let quote = if bytes.contains(&b'\'') && !bytes.contains(&b'"') {
        '"'
    } else {
        '\''
    };

    let mut out = String::with_capacity(bytes.len() + 2);
    out.push(quote);
    for c in Wtf8Chars::new(bytes) {
        match c {
            Wtf8Char::Surrogate(cp) => {
                let _ = write!(out, "\\u{cp:04x}");
            }
            Wtf8Char::Scalar('\\') => out.push_str(r"\\"),
            Wtf8Char::Scalar('\t') => out.push_str(r"\t"),
            Wtf8Char::Scalar('\n') => out.push_str(r"\n"),
            Wtf8Char::Scalar('\r') => out.push_str(r"\r"),
            Wtf8Char::Scalar(c) if c == quote => {
                out.push('\\');
                out.push(c);
            }
            Wtf8Char::Scalar(c) if is_non_printable(c) => escape_non_printable(&mut out, c),
            Wtf8Char::Scalar(c) => out.push(c),
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
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    n.as_big()
        .expect("a non-float Number is an i64, a u64, or an arbitrary-precision integer")
        .to_string()
}

/// Whether Python's `repr()` escapes `c`: general categories `Cc`, `Cf`, `Cs`,
/// `Co`, `Cn`, `Zl`, `Zp` and `Zs`, except the plain space.
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

/// Appends `c`'s `repr()` escape: `\xXX` below `U+0100`, `\uXXXX` up to
/// `U+FFFF`, `\UXXXXXXXX` above.
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

/// Python `repr()` for a `float`: the shortest round-tripping digits, in
/// exponent form when the decimal point sits at or below `-4` or above `16`.
/// The digit count comes from `{:e}`; the digits from `{:.*e}` at that count,
/// correctly rounded with ties to even.
pub(crate) fn python_float_repr(value: f64) -> String {
    if !value.is_finite() {
        // `{:e}` has no exponent form for these, so the digit-count logic
        // below (which expects an `e` separator) never applies to them.
        return if value.is_nan() {
            "nan".to_string()
        } else if value.is_sign_positive() {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
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
        PathSegment, entry_path_segment, escape_non_printable, python_repr, quote_key, render_path,
        set_item_repr,
    };
    use crate::test_support::{cdate, cdt_at, ctime, ctimedelta};
    use crate::value::{Builder, Number, ObjectKey, ObjectKind, SetItems, Value};

    #[test]
    fn empty_path_renders_as_root() {
        assert_eq!(render_path(&[]).to_string(), "root");
    }

    #[test]
    fn single_key_segment() {
        assert_eq!(
            render_path(&[PathSegment::Key("a".to_string().into())]).to_string(),
            "root['a']"
        );
    }

    #[test]
    fn single_index_segment() {
        assert_eq!(render_path(&[PathSegment::Index(0)]).to_string(), "root[0]");
    }

    #[test]
    fn mixed_nested_segments() {
        let segments = vec![
            PathSegment::Key("a".to_string().into()),
            PathSegment::Index(3),
            PathSegment::Key("b c".to_string().into()),
        ];
        assert_eq!(render_path(&segments).to_string(), "root['a'][3]['b c']");
    }

    #[test]
    fn empty_string_key_renders_empty_quotes() {
        assert_eq!(
            render_path(&[PathSegment::Key(String::new().into())]).to_string(),
            "root['']"
        );
    }

    #[test]
    fn quote_key_default_uses_single_quotes() {
        assert_eq!(quote_key(&"a".into()).to_string(), "'a'");
    }

    #[test]
    fn quote_key_with_single_quote_uses_double_quotes() {
        assert_eq!(quote_key(&"it's".into()).to_string(), "\"it's\"");
    }

    #[test]
    fn quote_key_with_double_quote_only_uses_single_quotes_unescaped() {
        assert_eq!(
            quote_key(&r#"he said "hi""#.into()).to_string(),
            r#"'he said "hi"'"#
        );
    }

    #[test]
    fn quote_key_with_both_quote_kinds_uses_double_quotes_unescaped() {
        let mut key = String::new();
        key.push_str("it's ");
        key.push('"');
        key.push_str("cool");
        key.push('"');

        let mut expected = String::new();
        expected.push('"');
        expected.push_str(&key);
        expected.push('"');

        assert_eq!(quote_key(&key.as_str().into()).to_string(), expected);
    }

    #[test]
    fn quote_key_does_not_escape_backslashes() {
        assert_eq!(quote_key(&r"a\b".into()).to_string(), r"'a\b'");
    }

    #[test]
    fn quote_key_keeps_unicode_literal() {
        assert_eq!(quote_key(&"héllo世界".into()).to_string(), "'héllo世界'");
    }

    #[test]
    fn quote_key_empty_string() {
        assert_eq!(quote_key(&"".into()).to_string(), "''");
    }

    /// A set item renders as its own path segment, with no quoting applied
    /// on top of [`set_item_repr`]'s own.
    #[test]
    fn set_item_segment_renders_its_text_verbatim() {
        assert_eq!(
            render_path(&[
                PathSegment::Key("a".to_string().into()),
                PathSegment::SetItem("(1, 2)".to_string()),
            ])
            .to_string(),
            "root['a'][(1, 2)]"
        );
    }

    /// A top-level `str` set item is wrapped in single quotes with no escaping,
    /// unlike [`quote_key`]'s rule, which would double-quote the second of these.
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
            quote_key(&"it's".into()).to_string(),
            "set-item and dict-key quoting differ"
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

    /// A `str` nested in a tuple item is rendered by `python_repr`, which escapes.
    #[test]
    fn str_nested_in_a_tuple_item_uses_python_repr() {
        let tuple = Value::Tuple(Box::new([Value::Str("it's".into())]).into());
        assert_eq!(set_item_repr(&tuple), r#"("it's",)"#);

        let both = Value::Tuple(Box::new([Value::Str("it's \"x\"".into())]).into());
        assert_eq!(set_item_repr(&both), r#"('it\'s "x"',)"#);
    }

    #[test]
    fn python_repr_renders_every_container_kind() {
        let inner = Value::Tuple(Box::new([Value::Number(Number::from_u64(1))]).into());
        assert_eq!(python_repr(&inner), "(1,)");
        assert_eq!(
            python_repr(&Value::Tuple(
                Box::new([
                    Value::Number(Number::from_u64(1)),
                    Value::Number(Number::from_u64(2)),
                ])
                .into()
            )),
            "(1, 2)"
        );
        assert_eq!(python_repr(&Value::Tuple(Box::new([]).into())), "()");
        assert_eq!(
            python_repr(&Value::Array(
                Box::new([Value::Null, Value::Str("a".into())]).into()
            )),
            "[None, 'a']"
        );
        assert_eq!(python_repr(&Value::Array(Box::new([]).into())), "[]");
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

    /// Floats near a shortest-form tie render with Python's last digit.
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
                set_item_repr(&Value::Number(Number::from_f64(input))),
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
        assert_eq!(
            python_repr(&builder.object(Vec::<(String, Value)>::new())),
            "{}"
        );
    }

    /// A nested container renders element by element.
    #[test]
    fn python_repr_nests_containers() {
        let value = Value::Tuple(
            Box::new([
                Value::Number(Number::from_u64(1)),
                Value::Tuple(
                    Box::new([
                        Value::Number(Number::from_u64(2)),
                        Value::FrozenSet(SetItems::new(vec![Value::Str("x".into())])),
                    ])
                    .into(),
                ),
            ])
            .into(),
        );
        assert_eq!(python_repr(&value), "(1, (2, frozenset({'x'})))");
    }

    /// The renderer is iterative, so a nest far deeper than any native
    /// stack tolerates renders instead of aborting the process.
    #[test]
    fn python_repr_of_a_very_deep_nest_does_not_overflow_the_stack() {
        let mut value = Value::Tuple(Box::new([]).into());
        for _ in 0..100_000 {
            value = Value::Tuple(Box::new([value]).into());
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
            let item = Value::Tuple(Box::new([Value::Str(input.into())]).into());
            assert_eq!(
                python_repr(&item),
                format!("({expected},)"),
                "for {input:?}"
            );
        }
    }

    #[test]
    fn python_repr_str_escapes_non_printable_code_points_above_u0100() {
        let cases = [
            // U+00FF: printable Latin-1 — left bare, not escaped.
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
            // width.
            ("a\u{f0000}b", r"'a\U000f0000b'"),
            // U+10FFFF: Cn, the last valid Unicode scalar value.
            ("a\u{10ffff}b", r"'a\U0010ffffb'"),
        ];
        for (input, expected) in cases {
            let item = Value::Tuple(Box::new([Value::Str(input.into())]).into());
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
        let item = Value::Tuple(Box::new([Value::Str("a b".into())]).into());
        assert_eq!(python_repr(&item), "('a b',)");
    }

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

    /// Fails if a `unicode-general-category` bump changes the table
    /// `is_non_printable` reads.
    #[test]
    fn unicode_general_category_stays_pinned_to_16_0_0() {
        assert_eq!(unicode_general_category::UNICODE_VERSION, (16, 0, 0));
    }

    /// Python's `repr()` for a calendar value, including the trailing-field
    /// trimming and a negative offset's `timedelta` normalizing into whole
    /// days plus seconds.
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
            (ctime(0, 0, 0, 0, None), "datetime.time(0, 0)"),
            (ctime(10, 30, 0, 0, None), "datetime.time(10, 30)"),
            (ctime(10, 30, 5, 0, None), "datetime.time(10, 30, 5)"),
            (ctime(10, 30, 5, 7, None), "datetime.time(10, 30, 5, 7)"),
            (ctime(0, 0, 0, 7, None), "datetime.time(0, 0, 0, 7)"),
            (
                ctime(0, 0, 0, 0, Some(0)),
                "datetime.time(0, 0, tzinfo=datetime.timezone.utc)",
            ),
            (
                ctime(0, 0, 0, 0, Some(-18000)),
                "datetime.time(0, 0, \
                 tzinfo=datetime.timezone(datetime.timedelta(days=-1, seconds=68400)))",
            ),
            (ctimedelta(0, 0, 0), "datetime.timedelta(0)"),
            (ctimedelta(1, 0, 0), "datetime.timedelta(days=1)"),
            (ctimedelta(0, 1, 0), "datetime.timedelta(seconds=1)"),
            (ctimedelta(0, 0, 1), "datetime.timedelta(microseconds=1)"),
            (
                ctimedelta(2, 11_045, 6),
                "datetime.timedelta(days=2, seconds=11045, microseconds=6)",
            ),
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
    /// exponent of at least two digits with an explicit sign.
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
                set_item_repr(&Value::Number(Number::from_f64(input))),
                expected,
                "for {input:?}"
            );
        }
    }

    /// `quote_key` leaves control characters unescaped.
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

        assert_eq!(quote_key(&key.as_str().into()).to_string(), expected);
    }

    #[test]
    fn entry_path_segment_renders_an_attribute_for_an_object_and_a_subscript_for_a_dict() {
        let key = ObjectKey::Str(crate::value::Key::Utf8(std::sync::Arc::from("x")));
        let rendered = |kind| render_path(&[entry_path_segment(kind, &key)]).to_string();
        assert_eq!(
            [
                rendered(ObjectKind::CustomObject),
                rendered(ObjectKind::Opaque),
                rendered(ObjectKind::Dict),
            ],
            ["root.x", "root.x", "root['x']"]
        );
    }

    #[test]
    fn entry_path_segment_renders_a_non_str_object_key_as_a_subscript() {
        let key = ObjectKey::Other(Box::new(Value::Number(Number::from_u64(1))));
        assert_eq!(
            render_path(&[entry_path_segment(ObjectKind::CustomObject, &key)]).to_string(),
            "root[1]"
        );
    }
}
