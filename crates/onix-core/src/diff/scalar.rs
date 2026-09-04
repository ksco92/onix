//! Leaf-level (non-container) comparison: scalar/numeric equality and the
//! `values_changed`/`type_changes` finding builders that `super::dispatch`'s
//! [`super::diff_at`] dispatches to whenever a pair is not two dicts or two
//! arrays.

use crate::datetime::DateTime;
use crate::value::{Number, Value};

use crate::error::Error;
use crate::path::PathSegment;
use crate::report::{Report, TypeChangeEntry, ValuesChangedEntry};

use super::check_value_depth;

/// The Python type name `DeepDiff` would report for a given [`Value`].
///
/// Numbers are split into `"int"` and `"float"` by the compact [`Number`]'s
/// preserved representation (which carries `serde_json`'s original parse: a
/// JSON literal with no decimal point or exponent, e.g. `1`, is an int; one
/// with either, e.g. `1.0`, is a float). This mirrors `DeepDiff`'s default
/// behavior of treating `1` and `1.0` as different types.
pub(crate) fn python_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::Str(_) => "str",
        Value::DateTime(_) => "datetime",
        Value::Date(_) => "date",
        Value::Array(_) => "list",
        Value::Tuple(_) => "tuple",
        Value::Object(_) => "dict",
    }
}
/// Builds a single-entry `type_changes` report at `path`, `depth` levels
/// deep.
///
/// Checks both `a` and `b` with [`check_value_depth`] before cloning either
/// one into the report — either side could be the deeply nested one (e.g. a
/// list vs number type mismatch where the list is attacker-controlled and
/// deep) — so a value whose own nesting, combined with `depth`, exceeds the
/// shared `max_depth` budget is rejected cleanly instead of overflowing the
/// stack on `.clone()`.
pub(crate) fn type_change_report(
    path: &[PathSegment],
    a: &Value,
    b: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<Report, Error> {
    check_value_depth(path, a, depth, max_depth)?;
    check_value_depth(path, b, depth, max_depth)?;

    let mut report = Report::new();
    report.insert_type_change(
        path.to_vec(),
        TypeChangeEntry {
            old_type: python_type_name(a).to_string(),
            new_type: python_type_name(b).to_string(),
            old_value: a.clone(),
            new_value: b.clone(),
            new_path: None,
        },
    );
    Ok(report)
}
/// Builds either an empty report (`equal`) or a single-entry
/// `values_changed` report at `path`, `depth` levels deep.
///
/// Same [`check_value_depth`] guard as [`type_change_report`], run only when
/// `!equal` (the only case that clones anything). Every current caller only
/// ever passes scalars here (bool/string/number, all inherently depth `0`),
/// so this can never actually trip today — the check is still here so the
/// guarantee holds structurally rather than by relying on today's call
/// graph never changing.
pub(crate) fn scalar_diff(
    path: &[PathSegment],
    equal: bool,
    a: &Value,
    b: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<Report, Error> {
    if equal {
        return Ok(Report::new());
    }
    check_value_depth(path, a, depth, max_depth)?;
    check_value_depth(path, b, depth, max_depth)?;

    let mut report = Report::new();
    report.insert_values_changed(
        path.to_vec(),
        ValuesChangedEntry {
            old_value: a.clone(),
            new_value: b.clone(),
            new_path: None,
        },
    );
    Ok(report)
}
/// Diffs two datetimes, which `DeepDiff` compares by *instant* after
/// normalizing each to UTC (`_diff_datetime` -> `datetime_normalize`, with a
/// naive value stamped as UTC rather than read in local time).
///
/// The normalization is not just a comparison step: `_diff_datetime` assigns
/// the normalized values back onto the level it then reports, so a
/// `values_changed` entry carries the pair *as UTC* — `10:00-05:00` is
/// reported as `15:00+00:00`. This is the one place in the engine a reported
/// value differs from the input value; every other category (`type_changes`,
/// the added/removed categories, and the `values_changed` that
/// `Report::merge_mutual_add_removes` folds a same-path add/remove pair into)
/// carries the raw value, because it never passes through this function.
pub(crate) fn datetime_diff(
    path: &[PathSegment],
    old: DateTime,
    new: DateTime,
    depth: usize,
    max_depth: usize,
) -> Result<Report, Error> {
    let (old, new) = (old.to_utc(), new.to_utc());

    scalar_diff(
        path,
        old == new,
        &Value::DateTime(old),
        &Value::DateTime(new),
        depth,
        max_depth,
    )
}
/// Diffs two same-JSON-variant numbers, first checking whether one is an int
/// and the other a float (a `type_changes` finding, regardless of numeric
/// value), then comparing numerically within the same type via
/// [`numbers_equal`].
pub(crate) fn numeric_diff(
    path: &[PathSegment],
    old: &Number,
    new: &Number,
    a: &Value,
    b: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<Report, Error> {
    if old.is_f64() != new.is_f64() {
        return type_change_report(path, a, b, depth, max_depth);
    }
    scalar_diff(path, numbers_equal(old, new), a, b, depth, max_depth)
}
/// The single definition of numeric equality shared by [`numeric_diff`] and
/// [`values_equal`], so there is exactly one place these rules live.
///
/// An int and a float are never equal (mirroring `DeepDiff` always reporting
/// that pairing as a `type_changes`, never a numeric comparison). Within the
/// same kind: floats compare by exact IEEE-754 `==` (see [`floats_equal`]);
/// ints compare by value across `i64`/`u64` representations (see
/// [`number_as_i128`]), so `9_000_000_000_000_000_000u64` and its `i64`
/// counterpart compare equal even though they use different representations.
pub(crate) fn numbers_equal(old: &Number, new: &Number) -> bool {
    if old.is_f64() != new.is_f64() {
        return false;
    }

    if old.is_f64() {
        let old_f = old
            .as_f64()
            .expect("Number::is_f64 guarantees as_f64 succeeds");
        let new_f = new
            .as_f64()
            .expect("Number::is_f64 guarantees as_f64 succeeds");
        floats_equal(old_f, new_f)
    } else {
        let old_int =
            number_as_i128(old).expect("non-float Number always has an i64 or u64 representation");
        let new_int =
            number_as_i128(new).expect("non-float Number always has an i64 or u64 representation");
        old_int == new_int
    }
}
/// Compares two floats for exact equality.
///
/// This mirrors Python's `==` semantics for floats (including
/// `0.0 == -0.0`), with no implicit epsilon; NaN/Infinity are out of scope
/// for this JSON value model, so exact IEEE-754 equality is the correct
/// (and only) rule here.
fn floats_equal(a: f64, b: f64) -> bool {
    #[allow(
        clippy::float_cmp,
        reason = "exact IEEE-754 equality is the intended rule (Python == semantics, including 0.0 == -0.0); NaN/Infinity are out of scope for this JSON value model"
    )]
    {
        a == b
    }
}
/// Converts a non-float [`Number`] to `i128`, so ints stored as `u64` and
/// `i64` compare equal by value regardless of which representation
/// the compact `Number` preserves from `serde_json`'s original parse.
pub(crate) fn number_as_i128(n: &Number) -> Option<i128> {
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}
