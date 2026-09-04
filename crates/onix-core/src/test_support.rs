//! Shared `serde_json` -> compact-value converters for this crate's unit
//! tests.
//!
//! The engine consumes the compact [`crate::value::Value`]; these keep the
//! `serde_json`-literal-based tests converting their inputs at one place (the
//! same `From` bridge the CLI and bindings use) rather than re-declaring the
//! identical helpers in every test module.

use crate::value::{Number, Object, Value};

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

/// Compact [`Object`] from a `serde_json` map, built through the same
/// sort/dedup/intern path [`Value::from`] uses for objects.
pub(crate) fn cobj(map: &serde_json::Map<String, serde_json::Value>) -> Object {
    Object::from_pairs(
        map.iter()
            .map(|(key, value)| (std::sync::Arc::from(key.as_str()), cv(value)))
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
        Number::from_f64(n.as_f64().expect("serde Number is u64/i64/f64")).expect("finite")
    }
}
