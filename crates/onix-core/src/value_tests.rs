//! Tests for the compact [`crate::value`] model: number-distinction edges,
//! object lookup/order semantics, the streaming `Deserialize` visitor, a
//! `serde_json::Value` round-trip property, an iterative-`Drop` stack-safety
//! guard, and a memory-footprint smoke check against `serde_json::Value`.
//!
//! The footprint check installs an instrumented global allocator for the
//! whole lib unit-test binary; it only counts allocations (delegating to the
//! system allocator), so it does not change any other test's behavior.

use std::sync::Arc;

use proptest::prelude::*;
use serde::de::Deserialize;
use serde::de::value::{
    BytesDeserializer, F64Deserializer, I128Deserializer, StringDeserializer, U128Deserializer,
};

use super::{Number, Object, Value};

/// The convenience alias for `serde`'s in-memory deserializer error type.
type DeError = serde::de::value::Error;

// --- size ----------------------------------------------------------------

#[test]
fn value_is_compact() {
    let size = std::mem::size_of::<Value>();
    println!("size_of::<Value>() = {size} bytes");
    assert!(size <= 32, "Value must be <= 32 bytes, got {size}");
}

// --- number distinction edges -------------------------------------------

#[test]
fn number_int_ranges_preserve_representation() {
    let min = Number::from_i64(i64::MIN);
    assert_eq!(min.as_i64(), Some(i64::MIN));
    assert_eq!(min.as_u64(), None);
    assert!(!min.is_f64());

    let max = Number::from_i64(i64::MAX);
    assert_eq!(max.as_i64(), Some(i64::MAX));
    assert_eq!(
        max.as_u64(),
        Some(u64::try_from(i64::MAX).expect("i64::MAX fits u64")),
    );

    // A u64 above i64::MAX stays a u64 and reports no i64 view.
    let big = Number::from_u64(u64::MAX);
    assert_eq!(big.as_u64(), Some(u64::MAX));
    assert_eq!(big.as_i64(), None);
    assert!(!big.is_f64());
}

#[test]
fn number_float_specials_and_conversions() {
    let neg_zero = Number::from_f64(-0.0).expect("-0.0 is finite");
    assert!(neg_zero.is_f64());
    assert_eq!(neg_zero.as_i64(), None);
    assert_eq!(neg_zero.as_u64(), None);
    assert_eq!(neg_zero.as_f64(), Some(-0.0));

    // Non-finite floats have no JSON representation.
    assert!(Number::from_f64(f64::NAN).is_none());
    assert!(Number::from_f64(f64::INFINITY).is_none());
    assert!(Number::from_f64(f64::NEG_INFINITY).is_none());

    // Integer -> f64 views.
    assert_eq!(Number::from_u64(9).as_f64(), Some(9.0));
    assert_eq!(Number::from_i64(-9).as_f64(), Some(-9.0));
    assert!(!Number::from_u64(3).is_f64());
}

// --- object lookup and ordering -----------------------------------------

#[test]
fn object_lookup_and_sorted_iteration() {
    // Deliberately unsorted insertion order.
    let value = Value::from(serde_json::json!({ "b": 1, "a": 2, "c": 3 }));
    let Value::Object(obj) = &value else {
        panic!("expected object");
    };

    assert_eq!(obj.len(), 3);
    assert!(!obj.is_empty());

    // Iteration is in ascending key order regardless of insertion order.
    let keys: Vec<&str> = obj.keys().collect();
    assert_eq!(keys, ["a", "b", "c"]);
    let values: Vec<&Value> = obj.values().collect();
    assert_eq!(values.len(), 3);
    assert_eq!(values[0], &Value::Number(Number::from_u64(2)));

    // Binary-search lookup, hit and miss.
    assert_eq!(obj.get("a"), Some(&Value::Number(Number::from_u64(2))));
    assert!(obj.get("missing").is_none());
    assert!(obj.contains_key("c"));
    assert!(!obj.contains_key("missing"));

    // Entries iterator: size_hint, ExactSizeIterator, and item order.
    let mut entries = obj.iter();
    assert_eq!(entries.size_hint(), (3, Some(3)));
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries.next(),
        Some(("a", &Value::Number(Number::from_u64(2)))),
    );

    // &Object: IntoIterator yields the same ascending sequence.
    let collected: Vec<(&str, &Value)> = obj.into_iter().collect();
    let collected_keys: Vec<&str> = collected.iter().map(|(k, _)| *k).collect();
    assert_eq!(collected_keys, ["a", "b", "c"]);
}

#[test]
fn empty_object_is_empty() {
    let value = Value::from(serde_json::json!({}));
    let Value::Object(obj) = &value else {
        panic!("expected object");
    };
    assert_eq!(obj.len(), 0);
    assert!(obj.is_empty());
    assert!(obj.get("anything").is_none());
    assert_eq!(obj.iter().count(), 0);
}

#[test]
fn duplicate_keys_keep_last_matching_serde_json() {
    // serde_json collapses duplicate object keys keeping the last value; the
    // compact parser must agree. This also exercises the interner cache hit
    // ("k" interned three times: one miss, two hits).
    let text = r#"{"k": 1, "k": 2, "k": 3}"#;
    let compact: Value = serde_json::from_str(text).expect("valid JSON");
    let expected: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
    assert_eq!(compact.to_serde_json(), expected);

    let Value::Object(obj) = &compact else {
        panic!("expected object");
    };
    assert_eq!(obj.len(), 1);
    assert_eq!(obj.get("k"), Some(&Value::Number(Number::from_u64(3))));
}

// --- conversions ---------------------------------------------------------

#[test]
fn from_serde_json_round_trips_all_shapes() {
    let original = serde_json::json!({
        "null": null,
        "bool": false,
        "neg_int": -7,
        "big_uint": 9_000_000_000_000_000_000_u64,
        "float": 2.5,
        "string": "text",
        "array": [1, 2, [3, {"deep": true}]],
        "object": {"x": 1, "unicode\u{1f600}": "emoji-key"}
    });
    let compact = Value::from(original.clone());
    assert_eq!(compact.to_serde_json(), original);
}

// --- structural equality ------------------------------------------------

/// Convenience: build a compact value from a `serde_json` literal.
fn cv(json: serde_json::Value) -> Value {
    Value::from(json)
}

#[test]
fn partial_eq_covers_all_early_exits() {
    use serde_json::json;

    // Equal within each variant (the no-early-exit paths).
    assert_eq!(cv(json!(null)), cv(json!(null)));
    assert_eq!(cv(json!(true)), cv(json!(true)));
    assert_eq!(cv(json!(1)), cv(json!(1)));
    assert_eq!(cv(json!("a")), cv(json!("a")));
    assert_eq!(cv(json!([1, 2])), cv(json!([1, 2])));
    assert_eq!(cv(json!({"a": 1, "b": 2})), cv(json!({"a": 1, "b": 2})));

    // Every early-exit inequality path.
    assert_ne!(cv(json!(true)), cv(json!(false))); // Bool value differs
    assert_ne!(cv(json!(1)), cv(json!(2))); // Number value differs
    assert_ne!(cv(json!(1)), cv(json!(1.0))); // int vs float (Number variant)
    assert_ne!(cv(json!("a")), cv(json!("b"))); // Str value differs
    assert_ne!(cv(json!([1])), cv(json!([1, 2]))); // Array length differs
    assert_ne!(cv(json!([1])), cv(json!([2]))); // Array element differs
    assert_ne!(cv(json!({"a": 1})), cv(json!({"a": 1, "b": 2}))); // Object length differs
    assert_ne!(cv(json!({"a": 1})), cv(json!({"b": 1}))); // Object key differs
    assert_ne!(cv(json!({"a": 1})), cv(json!({"a": 2}))); // Object value differs
    assert_ne!(cv(json!(null)), cv(json!(true))); // different variants
}

// --- streaming deserialize ----------------------------------------------

#[test]
fn streaming_deserialize_covers_json_shapes() {
    let text = r#"{"arr": [true, -3, 4, 1.5, "s", null], "nested": {"z": 1}}"#;
    let compact: Value = serde_json::from_str(text).expect("valid JSON");
    let expected: serde_json::Value = serde_json::from_str(text).expect("valid JSON");
    assert_eq!(compact.to_serde_json().to_string(), expected.to_string());
}

#[test]
fn deserializes_owned_string_and_scalars() {
    let owned: Value = Value::deserialize(StringDeserializer::<DeError>::new("hi".to_owned()))
        .expect("string deserializes");
    assert_eq!(owned, Value::Str("hi".into()));

    let boolean: Value = serde_json::from_str("true").expect("bool deserializes");
    assert_eq!(boolean, Value::Bool(true));

    let null: Value = serde_json::from_str("null").expect("null deserializes");
    assert_eq!(null, Value::Null);
}

#[test]
fn deserializes_128_bit_integers_in_range() {
    // Negative i128 that fits i64.
    let neg =
        Value::deserialize(I128Deserializer::<DeError>::new(-42_i128)).expect("in-range i128");
    assert_eq!(neg, Value::Number(Number::from_i64(-42)));

    // Positive i128 that fits u64 but not i64.
    let big = Value::deserialize(I128Deserializer::<DeError>::new(i128::from(u64::MAX)))
        .expect("in-range i128");
    assert_eq!(big, Value::Number(Number::from_u64(u64::MAX)));

    // u128 that fits u64.
    let u = Value::deserialize(U128Deserializer::<DeError>::new(7_u128)).expect("in-range u128");
    assert_eq!(u, Value::Number(Number::from_u64(7)));
}

#[test]
fn deserializes_out_of_range_128_bit_integers_as_error() {
    let too_big_i = i128::from(u64::MAX) + 1;
    assert!(Value::deserialize(I128Deserializer::<DeError>::new(too_big_i)).is_err());

    let too_big_u = u128::from(u64::MAX) + 1;
    assert!(Value::deserialize(U128Deserializer::<DeError>::new(too_big_u)).is_err());
}

#[test]
fn non_finite_float_deserializes_to_null_like_serde_json() {
    let value =
        Value::deserialize(F64Deserializer::<DeError>::new(f64::NAN)).expect("visitor accepts f64");
    assert_eq!(value, Value::Null);
}

#[test]
fn unhandled_token_reports_expecting_message() {
    let deserializer: BytesDeserializer<'_, DeError> = BytesDeserializer::new(b"x");
    let error = Value::deserialize(deserializer).expect_err("bytes are not a JSON value");
    assert!(
        error.to_string().contains("any valid JSON value"),
        "error should surface the visitor's `expecting` text, got: {error}"
    );
}

// --- round-trip property -------------------------------------------------

/// Generates arbitrary `serde_json`-shaped values: all six JSON shapes,
/// integer extremes, finite floats, unicode strings/keys, and bounded deep
/// nesting. Duplicate-key-free by construction (object keys come from a
/// `HashMap`).
fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i64>().prop_map(|i| serde_json::json!(i)),
        any::<u64>().prop_map(|u| serde_json::json!(u)),
        any::<f64>()
            .prop_filter("JSON numbers are finite", |f| f.is_finite())
            .prop_map(|f| serde_json::json!(f)),
        ".*".prop_map(serde_json::Value::String),
    ];
    leaf.prop_recursive(6, 64, 10, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(serde_json::Value::Array),
            prop::collection::hash_map("\\PC*", inner, 0..8)
                .prop_map(|map| serde_json::Value::Object(map.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// `serde_json::Value -> Value -> serde_json::Value` is the identity,
    /// checked byte-for-byte via canonical serialization.
    #[test]
    fn serde_compact_round_trip_is_identity(original in arb_json()) {
        let round_tripped = Value::from(original.clone()).to_serde_json();
        prop_assert_eq!(original.to_string(), round_tripped.to_string());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// `Value`'s iterative `PartialEq` must agree with an independent oracle:
    /// `serde_json::Value` equality, which shares the exact semantics a
    /// derived `PartialEq` would have (number-variant sensitivity, so 1 and
    /// 1.0 differ; order-insensitive object equality). Both an equal pair
    /// (same source) and a mixed pair (independent sources) are checked.
    #[test]
    fn partial_eq_matches_serde_json_oracle(a_src in arb_json(), b_src in arb_json()) {
        let a = Value::from(a_src.clone());
        let a_clone = Value::from(a_src);
        let b = Value::from(b_src);
        prop_assert!(a == a_clone);
        prop_assert_eq!(a == a_clone, a.to_serde_json() == a_clone.to_serde_json());
        prop_assert_eq!(a == b, a.to_serde_json() == b.to_serde_json());
    }
}

// --- stack safety --------------------------------------------------------

#[test]
fn debug_formatting_covers_all_shapes() {
    let value = Value::from(serde_json::json!({
        "n": null, "b": true, "i": -1, "f": 1.5, "s": "x", "arr": [1, {"k": 2}]
    }));
    let rendered = format!("{value:?}");
    assert!(rendered.contains("Object"));
    assert!(rendered.contains("Array"));
    assert!(rendered.contains("Number"));
    assert!(rendered.contains("Bool"));
    assert!(rendered.contains("Null"));
}

#[test]
fn deep_equality_does_not_overflow_native_stack() {
    // Equality on deeply nested values must not recurse natively: a derived
    // `PartialEq` would overflow this small stack, the iterative one does not.
    // Covers both an equal pair and a pair differing only at the deepest leaf.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            const DEPTH: usize = 200_000;
            let build = |leaf: Value| {
                let mut value = leaf;
                for _ in 0..DEPTH {
                    value = Value::Array(vec![value].into_boxed_slice());
                }
                value
            };
            let a = build(Value::Null);
            let b = build(Value::Null);
            assert!(a == b, "equal deep values compare equal");

            let differ_at_depth = build(Value::Bool(true));
            assert!(
                a != differ_at_depth,
                "values differing at depth are unequal"
            );
        })
        .expect("probe thread spawns");
    handle
        .join()
        .expect("iterative equality completes on a small stack");
}

#[test]
fn deeply_nested_values_drop_without_native_recursion() {
    // A naive recursive `Drop` (like a derived one) would overflow this
    // small stack; the iterative `Drop` tears the structure down with O(1)
    // native frames. Build and drop entirely on the sized thread so a
    // regression surfaces here as an overflow rather than passing silently.
    // Both container shapes are exercised (array nesting and object nesting)
    // so each arm of the iterative teardown is covered at depth.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            const DEPTH: usize = 200_000;

            let mut nested_arrays = Value::Null;
            for _ in 0..DEPTH {
                nested_arrays = Value::Array(vec![nested_arrays].into_boxed_slice());
            }
            drop(nested_arrays);

            let mut nested_objects = Value::Null;
            for _ in 0..DEPTH {
                nested_objects =
                    Value::Object(Object::from_pairs(vec![(Arc::from("k"), nested_objects)]));
            }
            drop(nested_objects);
        })
        .expect("probe thread spawns");
    handle
        .join()
        .expect("iterative Drop completes on a small stack");
}
