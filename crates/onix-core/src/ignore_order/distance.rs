//! Structural and numeric distance between two values (`DeepDiff`'s `_get_rough_distance`) and the
//! length helpers it is built from; `super::pairing::compute_pairs` ranks candidate pairs by it.

use crate::value::{Number, Object, ObjectKey, ObjectKind, Value, same_class};

use crate::datetime::DateTime;
use crate::diff::DiffOptions;
use crate::error::Error;
use crate::path::{entry_path_segment, object_key_path_segment};

use super::IgnoreOrderMemo;

/// A total-ordering wrapper for [`rough_distance`]'s non-negative finite distances, so they key a
/// [`BTreeMap`](std::collections::BTreeMap) and group candidates by exact float equality.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Distance(pub(crate) f64);

impl PartialEq for Distance {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for Distance {}

impl PartialOrd for Distance {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Distance {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl std::hash::Hash for Distance {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// `value`'s number for [`rough_distance`]'s fast path; `Bool` counts, as in Python.
pub(crate) fn numeric_value(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Which of `DeepDiff`'s `TYPES_TO_DIST_FUNC` families a value belongs to, with the numbers it is
/// measured by. A datetime carries a timestamp and an ordinal because against a bare date it is
/// measured by ordinal.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DistanceFamily {
    /// `only_numbers` -> `_get_numbers_distance`.
    Number(f64),
    /// `datetime.datetime`: `timestamp()` against a datetime, `toordinal()` against a bare date.
    DateTime { timestamp: f64, ordinal: f64 },
    /// `datetime.date`: `toordinal()`.
    Date(f64),
    /// `datetime.time`: seconds of day; no other family matches it.
    Time(f64),
    /// `datetime.timedelta`: `total_seconds()`; no other family matches it.
    TimeDelta(f64),
}

/// The distance family `value` belongs to, or `None` for a container.
///
/// A naive datetime is measured as UTC, where `DeepDiff` reads it in the process timezone; this
/// only ranks candidate pairs, never a reported value, and the two agree once the timezone is UTC.
#[allow(
    clippy::cast_precision_loss,
    reason = "mirrors Python's own int-to-float `timestamp()`/`toordinal()` conversion, which \
              is likewise inexact past 2^53"
)]
fn distance_family(value: &Value) -> Option<DistanceFamily> {
    match value {
        Value::Bool(_) | Value::Number(_) => numeric_value(value).map(DistanceFamily::Number),
        Value::DateTime(value) => Some(DistanceFamily::DateTime {
            timestamp: value.instant() as f64 / 1_000_000.0,
            ordinal: value.date().ordinal() as f64,
        }),
        Value::Date(value) => Some(DistanceFamily::Date(value.ordinal() as f64)),
        Value::Time(value) => Some(DistanceFamily::Time(value.hash_seconds_of_day() as f64)),
        Value::TimeDelta(value) => Some(DistanceFamily::TimeDelta(value.total_seconds())),
        Value::Null
        | Value::Str(_)
        | Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => None,
    }
}

/// `DeepDiff`'s `get_numeric_types_distance`: the distance between two values of one
/// [`DistanceFamily`], or `None` when the structural fallback must run.
fn family_distance(removed: &Value, added: &Value, cutoff: f64) -> Option<f64> {
    let (removed, added) = (distance_family(removed)?, distance_family(added)?);

    #[allow(
        clippy::match_same_arms,
        reason = "the arms differ in which field they read, not in what they return, and \
                  keeping them separate gives each combination its own coverage region"
    )]
    let (removed, added) = match (removed, added) {
        (DistanceFamily::Number(r), DistanceFamily::Number(a)) => (r, a),
        (
            DistanceFamily::DateTime { timestamp: r, .. },
            DistanceFamily::DateTime { timestamp: a, .. },
        ) => (r, a),
        (DistanceFamily::DateTime { ordinal: r, .. }, DistanceFamily::Date(a)) => (r, a),
        (DistanceFamily::Date(r), DistanceFamily::DateTime { ordinal: a, .. }) => (r, a),
        (DistanceFamily::Date(r), DistanceFamily::Date(a)) => (r, a),
        (DistanceFamily::Time(r), DistanceFamily::Time(a)) => (r, a),
        (DistanceFamily::TimeDelta(r), DistanceFamily::TimeDelta(a)) => (r, a),
        _ => return None,
    };

    Some(numeric_distance(removed, added, cutoff))
}

/// `DeepDiff`'s `_get_numbers_distance`. `max_` appears in both formula and threshold, so same-sign
/// pairs are almost never rejected by the cutoff; a zero divisor returns `cutoff` itself.
#[allow(
    clippy::float_cmp,
    reason = "mirrors DeepDiff's own exact `num1 == num2` short-circuit \
              (real Python `==`) before any divisor arithmetic runs"
)]
pub(crate) fn numeric_distance(n1: f64, n2: f64, cutoff: f64) -> f64 {
    if n1 == n2 {
        return 0.0;
    }
    let divisor = (n1 + n2) / cutoff;
    if divisor == 0.0 {
        return cutoff;
    }
    ((n1 - n2) / divisor).abs().min(cutoff)
}

/// `DeepHash`'s structural node count: `1` per scalar, `1` plus the children for a container, and
/// one more per dict key.
///
/// Recurses natively; callers first run [`crate::diff::check_value_depth`]
/// (`docs/design/ignore-order.md`'s "Depth safety").
pub(crate) fn rough_length(value: &Value) -> usize {
    match value {
        Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Str(_)
        | Value::DateTime(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::TimeDelta(_) => 1,
        Value::Array(items) | Value::Tuple(items) => {
            1 + items.iter().map(rough_length).sum::<usize>()
        }
        Value::Set(items) | Value::FrozenSet(items) => {
            1 + items.iter().map(rough_length).sum::<usize>()
        }
        Value::Object(map) => {
            1 + map.lengths().hidden_count
                + map
                    .iter()
                    .filter(|(key, _)| !map.is_class_attribute(key))
                    .map(|(_, v)| 1 + rough_length(v))
                    .sum::<usize>()
        }
    }
}

/// `DeepDiff`'s `_get_item_length` for one value: `null` counts `0`, any other scalar `1`, a
/// container the sum of its members; a dict entry whose key matches [`is_length_excluded_key`] is
/// skipped.
pub(crate) fn item_length(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Bool(_)
        | Value::Number(_)
        | Value::Str(_)
        | Value::DateTime(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::TimeDelta(_) => 1,
        Value::Array(items) | Value::Tuple(items) => items.iter().map(item_length).sum(),
        Value::Set(items) | Value::FrozenSet(items) => items.iter().map(item_length).sum(),
        // A custom object counts its attribute keys, never their values.
        Value::Object(map) if map.kind() == ObjectKind::Dict => item_length_of_map(map),
        Value::Object(map) => map.lengths().dict_len,
    }
}

/// [`item_length`]'s dict case, shared with [`count_object_diff_leaves`]'s collapse branch.
fn item_length_of_map(map: &Object) -> usize {
    map.iter()
        // A non-`str` key, or a `str` key with a lone surrogate (`as_str` is `None`), is counted.
        .filter(|(key, _)| key.as_str().is_none_or(|s| !is_length_excluded_key(s)))
        .map(|(_, v)| item_length(v))
        .sum()
}

/// The literal dict-key exclusion list `_get_item_length` applies before
/// counting a mapping entry — see [`item_length`]'s doc.
pub(crate) fn is_length_excluded_key(key: &str) -> bool {
    key.starts_with('_')
        || key == "deep_distance"
        || key == "new_path"
        || key == "old_type"
        || key == "old_value"
}

/// A `Report`-free mirror of the recursive diff dispatch, counting what
/// [`Report::distance_leaf_length`](crate::report::Report::distance_leaf_length) would sum from the
/// real diff. Arrays alone delegate to a trial [`crate::diff::array_diff`]. `depth` is the depth
/// the pair's real diff runs at; an error is that diff's, its path relative to the pair.
pub(crate) fn count_diff_leaves(
    a: &Value,
    b: &Value,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<usize, Box<Error>> {
    if IgnoreOrderMemo::skips(a, b) {
        return Ok(0);
    }
    let resolved_a = memo.resolve(a, b, true);
    let resolved_b = memo.resolve(b, resolved_a.as_deref().unwrap_or(a), true);
    if resolved_a.is_some() || resolved_b.is_some() {
        let a = resolved_a.as_deref().unwrap_or(a);
        let b = resolved_b.as_deref().unwrap_or(b);
        return count_diff_leaves(a, b, depth, opts, memo);
    }
    let count = match (a, b) {
        // `diff_at`'s class rule: two classes are one type change, whatever
        // the variant, so no arm below compares across classes.
        _ if !same_class(a, b) => type_change_leaf_length(a, b),
        (Value::Null, Value::Null) => 0,
        (Value::Bool(x), Value::Bool(y)) => usize::from(x != y),
        (Value::Str(x), Value::Str(y)) => usize::from(x != y),
        (Value::DateTime(x), Value::DateTime(y)) => {
            return count_datetime_diff_leaves(x.value(), y.value());
        }
        (Value::Date(x), Value::Date(y)) => usize::from(x != y),
        // Plain `_diff_time` equality (see `docs/design/value-model.md`),
        // matching `diff_at`'s own dispatch for `Time`.
        (Value::Time(x), Value::Time(y)) => {
            usize::from(!crate::datetime::times_equal(x.value(), y.value()))
        }
        (Value::TimeDelta(x), Value::TimeDelta(y)) => usize::from(x != y),
        (Value::Number(x), Value::Number(y)) => {
            if x.is_f64() == y.is_f64() {
                usize::from(!crate::diff::numbers_equal(x, y))
            } else {
                type_change_leaf_length(a, b)
            }
        }
        (Value::Array(x), Value::Array(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
            return count_array_diff_leaves(x, y, depth, opts, memo);
        }
        (Value::Set(x), Value::Set(y)) | (Value::FrozenSet(x), Value::FrozenSet(y)) => {
            count_set_diff_leaves(x, y, memo)
        }
        (Value::Object(x), Value::Object(y)) => {
            return count_object_diff_leaves(x, y, depth, opts, memo);
        }
        _ => type_change_leaf_length(a, b),
    };
    Ok(count)
}

/// [`count_diff_leaves`]'s datetime case, normalized and failing as
/// `datetime_diff` does; its own frame keeps the pair off the recursive one.
fn count_datetime_diff_leaves(x: DateTime, y: DateTime) -> Result<usize, Box<Error>> {
    let (x, y) = crate::diff::normalized_pair(&[], x, y)?;
    Ok(usize::from(x != y))
}

/// [`count_diff_leaves`]'s type-mismatch count: `new_type`'s length plus [`item_length`] of the new
/// value, which `DeepDiff` omits when `new_type(old_value)` reproduces it.
pub(crate) fn type_change_leaf_length(old_value: &Value, new_value: &Value) -> usize {
    // `new_type`'s own length: an `Enum` class is iterable.
    let type_len = match new_value {
        Value::Object(map) => map.lengths().type_len,
        _ => 1,
    };
    if new_value_reproduced_by_coercion(old_value, new_value) {
        type_len
    } else {
        type_len + item_length(new_value)
    }
}

/// Whether `new_type(old_value)` reproduces `new_value`. Sequence and set pairs are answered from
/// the item slices by [`python_eq`], not by building a coerced copy on the pairing hot path.
fn new_value_reproduced_by_coercion(old_value: &Value, new_value: &Value) -> bool {
    match (old_value, new_value) {
        (Value::Tuple(old_items), Value::Array(new_items))
        | (Value::Array(old_items), Value::Tuple(new_items)) => {
            sequences_python_eq(old_items, new_items)
        }
        (
            Value::Array(old_items) | Value::Tuple(old_items),
            Value::Set(new_items) | Value::FrozenSet(new_items),
        ) => unordered_python_eq(old_items, new_items),
        (
            Value::Set(old_items) | Value::FrozenSet(old_items),
            Value::Set(new_items) | Value::FrozenSet(new_items),
        ) => unordered_python_eq(old_items, new_items),
        // Python's `list(a_set)` depends on set iteration order; membership decides here
        // (`tests/golden/README.md`'s "Set iteration order").
        (
            Value::Set(old_items) | Value::FrozenSet(old_items),
            Value::Array(new_items) | Value::Tuple(new_items),
        ) => unordered_python_eq(old_items, new_items),
        _ => coerce_for_type_change(old_value, new_value)
            .is_some_and(|coerced| coerced == *new_value),
    }
}

/// Python's `==`: scalars compare by [`crate::lcs::python_scalar_key`] (`1`, `1.0` and `True` are
/// equal), containers element-wise and kind-distinct (a list never equals a tuple). Recurses
/// natively.
fn python_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) | (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| python_eq(a, b))
        }
        // Unlike a list and a tuple, a `set` and a `frozenset` of equal members are equal.
        (Value::Set(x) | Value::FrozenSet(x), Value::Set(y) | Value::FrozenSet(y)) => {
            unordered_python_eq(x, y)
        }
        // Kept off this frame for the reason `object_diff`'s own dispatch documents.
        (Value::Object(x), Value::Object(y)) => {
            // A `str`-only dict never equals one with a non-`str` key, by either branch.
            if x.has_non_str_keys() || y.has_non_str_keys() {
                dict_python_eq_mixed(x, y)
            } else {
                x.len() == y.len()
                    && x.iter()
                        .zip(y.iter())
                        .all(|((x_key, x_value), (y_key, y_value))| {
                            x_key == y_key && python_eq(x_value, y_value)
                        })
            }
        }
        _ => match (
            crate::lcs::python_scalar_key(a),
            crate::lcs::python_scalar_key(b),
        ) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

/// [`python_eq`]'s dict case when either side has a non-`str` key, matched by [`match_dict_keys`].
fn dict_python_eq_mixed(a: &Object, b: &Object) -> bool {
    let matched = match_dict_keys(a, b);
    matched.only_a.is_empty()
        && matched.only_b.is_empty()
        && matched
            .shared
            .iter()
            .all(|(_, a_value, b_value)| python_eq(a_value, b_value))
}

/// Element-wise [`python_eq`] over two sequences of the same length — what
/// `list(x) == y` compares once `list()` has copied `x`'s items in order.
fn sequences_python_eq(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| python_eq(a, b))
}

/// Python's `set(a) == set(b)`. Quadratic: it runs only on a candidate pair's distance
/// measurement, where a set is small.
fn unordered_python_eq(a: &[Value], b: &[Value]) -> bool {
    a.iter().all(|x| b.iter().any(|y| python_eq(x, y)))
        && b.iter().all(|y| a.iter().any(|x| python_eq(x, y)))
}

/// Python's `new_type(old_value)` for the numeric family, `str` and `bool` targets. `None` when
/// Python would raise or the coercion is not modelled (a container or `None` target, a container
/// into `str`); `None` only keeps `new_value` in the length, it never drops it wrongly.
fn coerce_for_type_change(old_value: &Value, new_value: &Value) -> Option<Value> {
    match new_value {
        Value::Bool(_) => Some(Value::Bool(is_truthy(old_value))),
        Value::Number(n) if n.is_f64() => {
            coerce_to_f64(old_value).map(|f| Value::Number(Number::from_f64(f)))
        }
        Value::Number(_) => coerce_to_i64(old_value).map(|i| Value::Number(Number::from_i64(i))),
        Value::Str(_) => coerce_to_python_str(old_value).map(|s| Value::Str(s.into())),
        Value::Null
        | Value::DateTime(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::TimeDelta(_)
        | Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => None,
    }
}

/// Python's `bool(value)`.
#[allow(
    clippy::float_cmp,
    reason = "comparing a coerced numeric value against exact zero mirrors Python's own `bool(x)`               rule, not a computed arithmetic result"
)]
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        // Always true, including a `time` at midnight.
        Value::DateTime(_) | Value::Date(_) | Value::Time(_) => true,
        Value::TimeDelta(value) => {
            value.days() != 0 || value.seconds() != 0 || value.microseconds() != 0
        }
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        // A `Str::Wtf8` is never empty (see `Str::is_empty`).
        Value::Str(s) => !s.is_empty(),
        Value::Array(items) | Value::Tuple(items) => !items.is_empty(),
        Value::Set(items) | Value::FrozenSet(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python's `float(value)`: a string parses after whitespace trimming, a container is `None`.
fn coerce_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Null
        | Value::DateTime(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::TimeDelta(_)
        | Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => None,
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        // A `Str::Wtf8` (a lone surrogate present) is never a valid float
        // literal, matching `float("\udc80")` raising in real Python.
        Value::Str(s) => s.as_utf8().and_then(|s| s.trim().parse::<f64>().ok()),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a range-check boundary constant, not an arithmetic result — exactness beyond \
              f64's mantissa is not needed to test whether a float is grossly out of i64 range"
)]
const I64_MIN_AS_F64: f64 = i64::MIN as f64;
#[allow(
    clippy::cast_precision_loss,
    reason = "a range-check boundary constant, not an arithmetic result — exactness beyond \
              f64's mantissa is not needed to test whether a float is grossly out of i64 range"
)]
const I64_MAX_AS_F64: f64 = i64::MAX as f64;

/// Python's `int(value)`: `bool` maps to `1`/`0`, a float truncates toward
/// zero, a string parses like [`coerce_to_f64`] without a decimal point, and
/// a container is `None`. An out-of-`i64`-range float is `None`.
fn coerce_to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Null
        | Value::DateTime(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::TimeDelta(_)
        | Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => None,
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64()?;
                if f.is_finite() && (I64_MIN_AS_F64..=I64_MAX_AS_F64).contains(&f) {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "explicitly range-checked against I64_MIN_AS_F64/I64_MAX_AS_F64 immediately above"
                    )]
                    Some(f.trunc() as i64)
                } else {
                    None
                }
            } else {
                n.as_i64()
                    .or_else(|| n.as_u64().and_then(|u| i64::try_from(u).ok()))
            }
        }
        // See `coerce_to_f64`'s doc for the `Str::Wtf8` case.
        Value::Str(s) => s.as_utf8().and_then(|s| s.trim().parse::<i64>().ok()),
    }
}

/// Python's `str(value)` for a scalar, `None` for a container. A float gets a trailing `.0` when
/// Rust's output has no `.` or exponent; a mismatch on exponential floats only keeps `new_value`.
fn coerce_to_python_str(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some("None".to_string()),
        // Python's `str()` of a calendar value is an ordinary string; see `DateTime::python_str`.
        Value::DateTime(value) => Some(value.python_str()),
        Value::Date(value) => Some(value.python_str()),
        Value::Time(value) => Some(value.python_str()),
        Value::TimeDelta(value) => Some(value.python_str()),
        Value::Array(_)
        | Value::Tuple(_)
        | Value::Set(_)
        | Value::FrozenSet(_)
        | Value::Object(_) => None,
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64()?;
                let mut rendered = f.to_string();
                if !rendered.contains(['.', 'e', 'E']) {
                    rendered.push_str(".0");
                }
                Some(rendered)
            } else if let Some(i) = n.as_i64() {
                Some(i.to_string())
            } else {
                n.as_u64()
                    .map(|u| u.to_string())
                    .or_else(|| n.as_big().map(ToString::to_string))
            }
        }
        // A lone-surrogate `Str::Wtf8` cannot become a `String`; `None` only keeps `new_value`.
        Value::Str(s) => s.as_utf8().map(ToString::to_string),
    }
}

/// `DeepDiff`'s default `threshold_to_diff_deeper`: a dict pair whose key overlap (intersection
/// over union) is below it collapses to one `values_changed`, with or without `ignore_order`; both
/// callers share [`is_below_threshold_to_diff_deeper`].
pub(crate) const THRESHOLD_TO_DIFF_DEEPER: f64 = 0.33;

/// A dict key's Python-equality identity — see [`match_dict_keys`]'s doc
/// for the matching rule this backs and what a `None` result means.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum DictKeyIdentity {
    Scalar(crate::lcs::ScalarKey),
    Tuple(Vec<crate::lcs::ScalarKey>),
}

fn dict_key_identity(key: &ObjectKey) -> Option<DictKeyIdentity> {
    match key {
        ObjectKey::Str(s) => Some(DictKeyIdentity::Scalar(crate::lcs::ScalarKey::Str(
            s.as_bytes().to_vec(),
        ))),
        ObjectKey::Other(value) => match value.as_ref() {
            Value::Tuple(items) => items
                .iter()
                .map(crate::lcs::python_scalar_key)
                .collect::<Option<Vec<_>>>()
                .map(DictKeyIdentity::Tuple),
            other => crate::lcs::python_scalar_key(other).map(DictKeyIdentity::Scalar),
        },
    }
}

/// The result of matching two [`Object`]s' keys by [`DictKeyIdentity`] — see
/// [`match_dict_keys`].
pub(crate) struct DictKeyMatch<'a> {
    /// A key on both sides: `b`'s key (the one `DeepDiff` renders), `a`'s value, then `b`'s value.
    pub(crate) shared: Vec<(&'a ObjectKey, &'a Value, &'a Value)>,
    /// A key present only in `a`.
    pub(crate) only_a: Vec<(&'a ObjectKey, &'a Value)>,
    /// A key present only in `b`.
    pub(crate) only_b: Vec<(&'a ObjectKey, &'a Value)>,
}

/// Matches `a`'s and `b`'s keys by Python equality ([`DictKeyIdentity`]: `1`, `1.0` and `True` are
/// one key, tuple keys compare element-wise); a key with no identity is always added or removed.
/// `O((n + m) log(n + m))`: one map of `b`'s keys, probed once per `a` key. Callers use it only
/// once [`Object::has_non_str_keys`] says either side needs it.
pub(crate) fn match_dict_keys<'a>(a: &'a Object, b: &'a Object) -> DictKeyMatch<'a> {
    let mut b_by_identity: std::collections::BTreeMap<DictKeyIdentity, usize> =
        std::collections::BTreeMap::new();
    let b_entries: Vec<(&ObjectKey, &Value)> = b.iter().collect();
    for (index, (key, _)) in b_entries.iter().enumerate() {
        if let Some(identity) = dict_key_identity(key) {
            b_by_identity.insert(identity, index);
        }
    }

    let mut matched_b: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut shared = Vec::new();
    let mut only_a = Vec::new();

    for (key, a_value) in a {
        let found =
            dict_key_identity(key).and_then(|identity| b_by_identity.get(&identity).copied());
        match found {
            Some(index) => {
                matched_b.insert(index);
                let (b_key, b_value) = b_entries[index];
                shared.push((b_key, a_value, b_value));
            }
            None => only_a.push((key, a_value)),
        }
    }

    let only_b = b_entries
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !matched_b.contains(index))
        .map(|(_, entry)| entry)
        .collect();

    DictKeyMatch {
        shared,
        only_a,
        only_b,
    }
}

/// The `threshold_to_diff_deeper` ratio check shared by [`count_object_diff_leaves`] and
/// `crate::diff::object_diff`. All-`str` keys are counted by merging the two ascending key
/// sequences, with no hashing.
pub(crate) fn is_below_threshold_to_diff_deeper(a: &Object, b: &Object) -> bool {
    // Non-`str` keys sort last, so with one such side the merge below counts the same
    // intersection as `match_dict_keys`.
    let (union_len, intersect_len) = if a.has_non_str_keys() || b.has_non_str_keys() {
        let matched = match_dict_keys(a, b);
        (
            matched.shared.len() + matched.only_a.len() + matched.only_b.len(),
            matched.shared.len(),
        )
    } else {
        let (mut a_keys, mut b_keys) = (a.keys().peekable(), b.keys().peekable());
        let mut intersect_len = 0;
        while let (Some(a_key), Some(b_key)) = (a_keys.peek(), b_keys.peek()) {
            match a_key.cmp(b_key) {
                std::cmp::Ordering::Less => {
                    a_keys.next();
                }
                std::cmp::Ordering::Greater => {
                    b_keys.next();
                }
                std::cmp::Ordering::Equal => {
                    intersect_len += 1;
                    a_keys.next();
                    b_keys.next();
                }
            }
        }
        (a.len() + b.len() - intersect_len, intersect_len)
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "key counts are small, far under f64's exact-integer range"
    )]
    {
        union_len > 1 && (intersect_len as f64 / union_len as f64) < THRESHOLD_TO_DIFF_DEEPER
    }
}

/// [`count_diff_leaves`]'s dict case: [`item_length`] for an added or removed key, a recursion for
/// a shared one, or [`item_length_of_map`] of `b` when [`is_below_threshold_to_diff_deeper`]
/// collapses the pair.
pub(crate) fn count_object_diff_leaves(
    a: &Object,
    b: &Object,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<usize, Box<Error>> {
    if is_below_threshold_to_diff_deeper(a, b) {
        return Ok(if b.is_custom_object() {
            b.lengths().dict_len
        } else {
            item_length_of_map(b)
        });
    }

    // Dispatched to a separate function, kept off this frame for the
    // reason `crate::diff::object::object_diff`'s own dispatch documents.
    if a.has_non_str_keys() || b.has_non_str_keys() {
        return count_object_diff_leaves_mixed(a, b, depth, opts, memo);
    }

    let mut total = 0;

    for (key, old_value) in a {
        total += match b.get(key) {
            None => item_length(old_value),
            Some(new_value) => count_diff_leaves(old_value, new_value, depth + 1, opts, memo)
                .map_err(|error| error.under(&[entry_path_segment(a.kind(), key)]))?,
        };
    }
    for (key, new_value) in b {
        if !a.contains_key(key) {
            total += item_length(new_value);
        }
    }

    Ok(total)
}

/// [`count_object_diff_leaves`]'s walk when either side has a non-`str` key, matched by
/// [`match_dict_keys`].
fn count_object_diff_leaves_mixed(
    a: &Object,
    b: &Object,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<usize, Box<Error>> {
    let matched = match_dict_keys(a, b);
    let mut total = 0;

    for (key, old_value, new_value) in &matched.shared {
        total += count_diff_leaves(old_value, new_value, depth + 1, opts, memo)
            .map_err(|error| error.under(&[object_key_path_segment(key)]))?;
    }
    for (_, old_value) in &matched.only_a {
        total += item_length(old_value);
    }
    for (_, new_value) in &matched.only_b {
        total += item_length(new_value);
    }

    Ok(total)
}

/// [`count_diff_leaves`]'s set case: [`item_length`] summed over every added and removed member,
/// found by the same [`super::set_difference`] and shared `memo` the real set diff uses.
fn count_set_diff_leaves(a: &[Value], b: &[Value], memo: &IgnoreOrderMemo) -> usize {
    let (removed, added) = super::set_difference(a, b, memo);

    removed.into_iter().chain(added).map(item_length).sum()
}

/// [`count_diff_leaves`]'s array case: a trial [`crate::diff::array_diff`] at the array's own
/// `depth` under the caller's `max_depth`.
#[inline(always)]
#[allow(
    clippy::inline_always,
    reason = "`stack_frame_cost`'s release `pairing` shape costs 2,730 bytes/level with this \
              as its own frame (plain `#[inline]` the same) and 2,455 inlined"
)]
pub(crate) fn count_array_diff_leaves(
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<usize, Box<Error>> {
    match crate::diff::array_diff(&mut Vec::new(), a, b, depth, opts, memo) {
        Ok(mut sub_report) => {
            // Runs before `diff_length` is measured; a no-op on the positional path.
            sub_report.merge_mutual_add_removes();
            Ok(sub_report.distance_leaf_length())
        }
        Err(error) => Err(Box::new(error)),
    }
}

/// `DeepDiff`'s `_get_rough_distance`: the [`family_distance`] fast path, else
/// `count_diff_leaves(removed, added) / (rough_length(removed) + rough_length(added))`.
///
/// `depth` is the depth of the list doing the pairing, which is also the depth a paired item's own
/// diff runs at, so the trial runs under that diff's `max_depth` budget
/// (`docs/design/depth-budget.md`).
pub(crate) fn rough_distance(
    removed: &Value,
    added: &Value,
    cutoff: f64,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<f64, Box<Error>> {
    if let Some(distance) = family_distance(removed, added, cutoff) {
        return Ok(distance);
    }

    let diff_length = count_diff_leaves(removed, added, depth, opts, memo)?;
    if diff_length == 0 {
        return Ok(0.0);
    }
    let rough_len = rough_length(removed) + rough_length(added);
    #[allow(
        clippy::cast_precision_loss,
        reason = "diff_length/rough_len are small structural node counts, \
                  far under f64's exact-integer range"
    )]
    {
        Ok(diff_length as f64 / rough_len as f64)
    }
}
