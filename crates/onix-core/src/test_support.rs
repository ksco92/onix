//! Shared `serde_json` -> compact-value converters for this crate's unit
//! tests.
//!
//! The engine consumes the compact [`crate::value::Value`]; these keep the
//! `serde_json`-literal-based tests converting their inputs at one place (the
//! same `From` bridge the CLI and bindings use) rather than re-declaring the
//! identical helpers in every test module.

use crate::datetime::{Date, DateTime, Time, TimeDelta};
use crate::value::{Number, Object, ObjectKey, SetItems, Value};

/// Compact value from a borrowed `serde_json` value.
pub(crate) fn cv(value: &serde_json::Value) -> Value {
    Value::from(value.clone())
}

/// Compact values for a slice of `serde_json` values.
pub(crate) fn cvec(items: &[serde_json::Value]) -> Vec<Value> {
    items.iter().map(cv).collect()
}

/// Compact tuple value from a slice of `serde_json` values — the one shape
/// a JSON literal cannot express, so tests that need a tuple build it here.
pub(crate) fn ctup(items: &[serde_json::Value]) -> Value {
    Value::Tuple(cvec(items).into_boxed_slice())
}

/// Compact set value from a slice of `serde_json` values, in the order
/// given — the source order a real Python set would supply.
pub(crate) fn cset(items: &[serde_json::Value]) -> Value {
    Value::Set(SetItems::new(cvec(items)))
}

/// Compact frozenset value — [`cset`]'s twin.
pub(crate) fn cfrozen(items: &[serde_json::Value]) -> Value {
    Value::FrozenSet(SetItems::new(cvec(items)))
}

/// Compact `date` value — the other shape a JSON literal cannot express.
pub(crate) fn cdate(year: i32, month: u8, day: u8) -> Value {
    Value::Date(Date::new(year, month, day).expect("test date is a real calendar date"))
}

/// Compact `datetime` value at midnight, naive unless `offset` is given.
pub(crate) fn cdt(year: i32, month: u8, day: u8, offset: Option<i32>) -> Value {
    cdt_at(year, month, day, 0, 0, 0, 0, offset)
}

/// Compact `datetime` value with every field spelled out.
#[allow(clippy::too_many_arguments, reason = "one argument per datetime field")]
pub(crate) fn cdt_at(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    microsecond: u32,
    offset: Option<i32>,
) -> Value {
    let date = Date::new(year, month, day).expect("test date is a real calendar date");
    Value::DateTime(
        DateTime::new(date, hour, minute, second, microsecond, offset)
            .expect("test datetime fields are in range"),
    )
}

/// Compact `time` value, naive unless `offset` is given.
pub(crate) fn ctime(
    hour: u8,
    minute: u8,
    second: u8,
    microsecond: u32,
    offset: Option<i32>,
) -> Value {
    Value::Time(
        Time::new(hour, minute, second, microsecond, offset)
            .expect("test time fields are in range"),
    )
}

/// Compact `timedelta` value from Python's own `(days, seconds,
/// microseconds)` triple.
pub(crate) fn ctimedelta(days: i64, seconds: i64, microseconds: i64) -> Value {
    Value::TimeDelta(
        TimeDelta::new(days, seconds, microseconds).expect("test timedelta fields are in range"),
    )
}

/// Compact [`Object`] from a `serde_json` map, built through the same
/// sort/dedup/intern path [`Value::from`] uses for objects.
pub(crate) fn cobj(map: &serde_json::Map<String, serde_json::Value>) -> Object {
    Object::from_pairs(
        map.iter()
            .map(|(key, value)| {
                (
                    ObjectKey::Str(std::sync::Arc::from(key.as_str())),
                    cv(value),
                )
            })
            .collect(),
    )
}

/// Compact [`Number`] from a `serde_json` number.
pub(crate) fn cnum(n: &serde_json::Number) -> Number {
    if let Some(u) = n.as_u64() {
        Number::from_u64(u)
    } else if let Some(i) = n.as_i64() {
        Number::from_i64(i)
    } else {
        Number::from_f64(n.as_f64().expect("serde Number is u64/i64/f64"))
    }
}
