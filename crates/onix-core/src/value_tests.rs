//! Tests for the compact [`crate::value`] model: number-distinction edges,
//! object lookup/order semantics, the streaming `Deserialize` visitor, a
//! `serde_json::Value` round-trip property, and the iterative `Drop` and
//! `PartialEq` stack-safety guards.
//!
//! The memory-footprint smoke check lives in its own integration-test binary
//! (`tests/memory_footprint.rs`) so its process-global counting allocator is
//! not shared with — and polluted by — the other unit tests running
//! concurrently in this binary.

use std::sync::Arc;

use proptest::prelude::*;
use serde::de::Deserialize;
use serde::de::value::{
    BytesDeserializer, F64Deserializer, I128Deserializer, StringDeserializer, U128Deserializer,
};

use serde_json::json;

use super::{
    Builder, Key, Number, Object, ObjectKey, SetItems, Str, Value, Wtf8Char, Wtf8Chars,
    contains_wtf8, write_json_str_content,
};
use crate::test_support::{
    carr, cdate, cdt, cdt_at, cfrozen, cset, ctime, ctimedelta, ctup, ctuple, cv,
};

/// The WTF-8 bytes for one lone surrogate code point, matching `CPython`'s own
/// `str.encode('utf-8', 'surrogatepass')` — see `crates/onix-py/src/convert.rs`'s
/// module doc. `0xDC80` throughout these tests (an arbitrary low surrogate);
/// [`wtf8_surrogate_bytes`] covers both halves and boundary values.
fn wtf8_surrogate_bytes(code_point: u16) -> [u8; 3] {
    [
        0xE0 | ((code_point >> 12) as u8),
        0x80 | (((code_point >> 6) & 0x3F) as u8),
        0x80 | ((code_point & 0x3F) as u8),
    ]
}

/// The convenience alias for `serde`'s in-memory deserializer error type.
type DeError = serde::de::value::Error;

// --- size ----------------------------------------------------------------

#[test]
fn value_is_compact() {
    // Grew from 32 to 40 bytes with subclass-name tracking (`Typed<T>` on
    // `DateTime`/`Date`/`Time`/`TimeDelta`/`Array`/`Tuple` adds one
    // `Option<Arc<str>>`, niche-optimized to 16 bytes) — the cost of
    // carrying a subclass's own class name (e.g. pandas `Timestamp`)
    // through the value model; see `crate::value::Typed`'s doc.
    let size = std::mem::size_of::<Value>();
    println!("size_of::<Value>() = {size} bytes");
    assert!(size <= 40, "Value must be <= 40 bytes, got {size}");
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
    let neg_zero = Number::from_f64(-0.0);
    assert!(neg_zero.is_f64());
    assert_eq!(neg_zero.as_i64(), None);
    assert_eq!(neg_zero.as_u64(), None);
    assert_eq!(neg_zero.as_f64(), Some(-0.0));

    // Non-finite floats have no JSON representation, but `Number` itself
    // stores one like any other float (see `Number::from_f64`'s own doc):
    // this is the Python-`float` boundary, not the JSON one.
    let nan = Number::from_f64(f64::NAN);
    assert!(nan.is_f64());
    assert!(nan.as_f64().is_some_and(f64::is_nan));

    let inf = Number::from_f64(f64::INFINITY);
    assert_eq!(inf.as_f64(), Some(f64::INFINITY));

    let neg_inf = Number::from_f64(f64::NEG_INFINITY);
    assert_eq!(neg_inf.as_f64(), Some(f64::NEG_INFINITY));

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
    let keys: Vec<&str> = obj.keys().filter_map(ObjectKey::as_str).collect();
    assert_eq!(keys, ["a", "b", "c"]);
    let values: Vec<&Value> = obj.values().collect();
    assert_eq!(values.len(), 3);
    assert_eq!(values[0], &Value::Number(Number::from_u64(2)));

    // Binary-search lookup, hit and miss.
    assert_eq!(obj.get_str("a"), Some(&Value::Number(Number::from_u64(2))));
    assert!(obj.get_str("missing").is_none());
    assert!(obj.contains_key_str("c"));
    assert!(!obj.contains_key_str("missing"));

    // Entries iterator: size_hint, ExactSizeIterator, and item order.
    let mut entries = obj.iter();
    assert_eq!(entries.size_hint(), (3, Some(3)));
    assert_eq!(entries.len(), 3);
    let (first_key, first_value) = entries.next().expect("at least one entry");
    assert_eq!(first_key.as_str(), Some("a"));
    assert_eq!(first_value, &Value::Number(Number::from_u64(2)));

    // &Object: IntoIterator yields the same ascending sequence.
    let collected: Vec<(&ObjectKey, &Value)> = obj.into_iter().collect();
    let collected_keys: Vec<&str> = collected.iter().filter_map(|(k, _)| k.as_str()).collect();
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
    assert!(obj.get_str("anything").is_none());
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
    assert_eq!(obj.get_str("k"), Some(&Value::Number(Number::from_u64(3))));
}

// --- non-str object keys (issue #62) --------------------------------------

#[test]
fn object_key_as_str_is_none_for_a_non_str_key() {
    assert_eq!(
        ObjectKey::Str(Key::Utf8(Arc::from("a"))).as_str(),
        Some("a")
    );
    assert_eq!(
        ObjectKey::Other(Box::new(Value::Number(Number::from_u64(1)))).as_str(),
        None
    );
}

#[test]
fn object_key_ordering_puts_every_str_before_every_other_key() {
    // See `ObjectKey`'s own `Ord` doc: this is what keeps a `str`-only
    // object's entry order unchanged from before this variant existed.
    let str_key = ObjectKey::Str(Key::Utf8(Arc::from("z")));
    let other_key = ObjectKey::Other(Box::new(Value::Number(Number::from_i64(-1000))));
    assert!(str_key < other_key);
    assert_ne!(str_key, other_key);
}

#[test]
fn object_get_and_contains_key_accept_a_non_str_object_key() {
    let int_key = ObjectKey::Other(Box::new(Value::Number(Number::from_u64(1))));
    let missing_key = ObjectKey::Other(Box::new(Value::Number(Number::from_u64(2))));
    let obj = Object::from_pairs(vec![
        (ObjectKey::Str(Key::Utf8(Arc::from("a"))), Value::Bool(true)),
        (int_key.clone(), Value::Bool(false)),
    ]);

    assert_eq!(obj.get(&int_key), Some(&Value::Bool(false)));
    assert_eq!(obj.get(&missing_key), None);
    assert!(obj.contains_key(&int_key));
    assert!(!obj.contains_key(&missing_key));
}

#[test]
fn object_get_str_on_a_mixed_object_skips_past_the_non_str_key() {
    // `get_str`'s binary search must treat the `Other` entry as sorting
    // after every `str` one (see its own doc) — exercised only when a
    // mixed object's `str`-key lookup actually probes an index at or past
    // that `Other` entry, which a single-`str`-key object never does.
    let obj = Object::from_pairs(vec![
        (ObjectKey::Str(Key::Utf8(Arc::from("a"))), Value::Bool(true)),
        (
            ObjectKey::Other(Box::new(Value::Number(Number::from_u64(1)))),
            Value::Bool(false),
        ),
    ]);

    assert_eq!(obj.get_str("a"), Some(&Value::Bool(true)));
    assert!(obj.contains_key_str("a"));
    assert_eq!(obj.get_str("z"), None);
    assert!(!obj.contains_key_str("z"));
}

#[test]
fn object_with_non_str_keys_has_non_str_keys_is_true() {
    let obj = Object::from_pairs(vec![
        (ObjectKey::Str(Key::Utf8(Arc::from("a"))), Value::Null),
        (
            ObjectKey::Other(Box::new(Value::Number(Number::from_u64(1)))),
            Value::Null,
        ),
    ]);
    assert!(obj.has_non_str_keys());

    let str_only = Object::from_pairs(vec![(
        ObjectKey::Str(Key::Utf8(Arc::from("a"))),
        Value::Null,
    )]);
    assert!(!str_only.has_non_str_keys());
}

/// See `tests/golden/README.md`'s nested-non-`str`-dict-key `to_json()`
/// section, where this test is pinned as the `tuple`-key case.
#[test]
fn to_serde_json_stringifies_a_tuple_key_via_python_repr_where_deepdiff_would_crash() {
    let obj = Value::Object(Object::from_pairs(vec![(
        ObjectKey::Other(Box::new(ctup(&[json!(1), json!(2)]))),
        Value::Str("x".into()),
    )]));
    assert_eq!(obj.to_serde_json(), json!({"(1, 2)": "x"}));
}

/// [`to_serde_json_stringifies_a_tuple_key_via_python_repr_where_deepdiff_would_crash`]'s
/// `datetime`-key twin.
#[test]
fn to_serde_json_stringifies_a_datetime_key_via_python_repr_where_deepdiff_would_crash() {
    let obj = Value::Object(Object::from_pairs(vec![(
        ObjectKey::Other(Box::new(cdt_at(2024, 1, 1, 10, 30, 0, 0, None))),
        Value::Str("x".into()),
    )]));
    assert_eq!(
        obj.to_serde_json(),
        json!({"datetime.datetime(2024, 1, 1, 10, 30)": "x"})
    );
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

#[test]
fn partial_eq_covers_all_early_exits() {
    use serde_json::json;

    // Equal within each variant (the no-early-exit paths).
    assert_eq!(cv(&json!(null)), cv(&json!(null)));
    assert_eq!(cv(&json!(true)), cv(&json!(true)));
    assert_eq!(cv(&json!(1)), cv(&json!(1)));
    assert_eq!(cv(&json!("a")), cv(&json!("a")));
    assert_eq!(cv(&json!([1, 2])), cv(&json!([1, 2])));
    assert_eq!(cv(&json!({"a": 1, "b": 2})), cv(&json!({"a": 1, "b": 2})));

    // Every early-exit inequality path.
    assert_ne!(cv(&json!(true)), cv(&json!(false))); // Bool value differs
    assert_ne!(cv(&json!(1)), cv(&json!(2))); // Number value differs
    assert_ne!(cv(&json!(1)), cv(&json!(1.0))); // int vs float (Number variant)
    assert_ne!(cv(&json!("a")), cv(&json!("b"))); // Str value differs
    assert_ne!(cv(&json!([1])), cv(&json!([1, 2]))); // Array length differs
    assert_ne!(cv(&json!([1])), cv(&json!([2]))); // Array element differs
    assert_ne!(cv(&json!({"a": 1})), cv(&json!({"a": 1, "b": 2}))); // Object length differs
    assert_ne!(cv(&json!({"a": 1})), cv(&json!({"b": 1}))); // Object key differs
    assert_ne!(cv(&json!({"a": 1})), cv(&json!({"a": 2}))); // Object value differs
    assert_ne!(cv(&json!(null)), cv(&json!(true))); // different variants
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
                    value = carr(vec![value]);
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
                nested_arrays = carr(vec![nested_arrays]);
            }
            drop(nested_arrays);

            let mut nested_objects = Value::Null;
            for _ in 0..DEPTH {
                nested_objects = Value::Object(Object::from_pairs(vec![(
                    ObjectKey::Str(Key::Utf8(Arc::from("k"))),
                    nested_objects,
                )]));
            }
            drop(nested_objects);
        })
        .expect("probe thread spawns");
    handle
        .join()
        .expect("iterative Drop completes on a small stack");
}

#[test]
fn builder_object_sorts_dedups_and_interns_repeated_keys() {
    use super::Builder;

    let mut builder = Builder::new();
    // Unsorted input with a duplicate key: rendered back in sorted order,
    // last value wins for the duplicate.
    let first = builder.object(vec![
        ("b".to_owned(), Value::Number(Number::from_u64(1))),
        ("a".to_owned(), Value::Null),
        ("b".to_owned(), Value::Number(Number::from_u64(2))),
    ]);
    assert_eq!(first.to_serde_json().to_string(), r#"{"a":null,"b":2}"#);

    // A second object reuses the same keys, exercising the interner's
    // cache-hit path across objects built by one builder.
    let second = builder.object(vec![
        ("a".to_owned(), Value::Bool(true)),
        ("b".to_owned(), Value::Bool(false)),
    ]);
    assert_eq!(
        second.to_serde_json().to_string(),
        r#"{"a":true,"b":false}"#
    );
}

#[test]
fn builder_default_matches_new() {
    let mut from_default = super::Builder::default();
    let value = from_default.object(vec![("k".to_owned(), Value::Null)]);
    assert_eq!(value.to_serde_json().to_string(), r#"{"k":null}"#);
}

// --- tuples --------------------------------------------------------------

#[test]
fn a_tuple_renders_as_a_json_array_but_is_never_equal_to_one() {
    let tuple = ctup(&[serde_json::json!(1), serde_json::Value::Null]);

    assert_eq!(tuple.to_serde_json(), serde_json::json!([1, null]));
    assert_ne!(tuple, cv(&serde_json::json!([1, null])));
    assert_eq!(
        tuple,
        ctup(&[serde_json::json!(1), serde_json::Value::Null])
    );
    // Same length, different item: the tuple arm's own inequality path.
    assert_ne!(
        tuple,
        ctup(&[serde_json::json!(1), serde_json::json!(true)])
    );
    assert!(format!("{tuple:?}").contains("Tuple"));
}

#[test]
fn json_parsing_never_produces_a_tuple() {
    // The tagged encoding the golden corpus uses to express a tuple in a
    // JSON file is decoded by test-only code; the product parse paths must
    // treat it as the ordinary dict it literally is.
    let parsed: Value = serde_json::from_str(r#"{"$tuple": [1, 2]}"#).expect("valid JSON");
    let converted = Value::from(serde_json::json!({"$tuple": [1, 2]}));

    assert!(matches!(parsed, Value::Object(_)));
    assert_eq!(parsed, converted);
    assert_eq!(
        parsed.to_serde_json(),
        serde_json::json!({"$tuple": [1, 2]})
    );
}

#[test]
fn deeply_nested_tuples_compare_and_drop_without_native_recursion() {
    // Tuples nest through the same `Box<[Value]>` arrays do, so the
    // iterative `Drop` and `PartialEq` must cover them: a derived
    // implementation would overflow this small stack.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            const DEPTH: usize = 200_000;
            let build = |leaf: Value| {
                let mut value = leaf;
                for _ in 0..DEPTH {
                    value = ctuple(vec![value]);
                }
                value
            };

            let a = build(Value::Null);
            let b = build(Value::Null);
            assert!(a == b, "equal deep tuples compare equal");
            assert!(
                a != build(Value::Bool(true)),
                "deep tuples differing at the leaf are unequal"
            );
            drop(a);
            drop(b);
        })
        .expect("probe thread spawns");
    handle
        .join()
        .expect("iterative equality and Drop complete on a small stack");
}

// --- datetimes and dates -------------------------------------------------

#[test]
fn a_datetime_renders_as_its_isoformat_string_but_is_never_equal_to_one() {
    let value = cdt_at(2024, 1, 1, 10, 0, 0, 123_456, Some(-5 * 3600));

    assert_eq!(
        value.to_serde_json(),
        serde_json::json!("2024-01-01T10:00:00.123456-05:00")
    );
    assert_ne!(
        value,
        cv(&serde_json::json!("2024-01-01T10:00:00.123456-05:00"))
    );
    assert!(format!("{value:?}").contains("DateTime"));
}

#[test]
fn a_date_renders_as_its_isoformat_string_but_is_never_equal_to_one() {
    let value = cdate(2024, 1, 1);

    assert_eq!(value.to_serde_json(), serde_json::json!("2024-01-01"));
    assert_ne!(value, cv(&serde_json::json!("2024-01-01")));
    assert!(format!("{value:?}").contains("Date"));
}

#[test]
fn datetime_equality_is_by_instant_and_reads_a_naive_value_as_utc() {
    let naive = cdt_at(2024, 1, 1, 10, 0, 0, 0, None);
    let utc = cdt_at(2024, 1, 1, 10, 0, 0, 0, Some(0));
    let plus_two = cdt_at(2024, 1, 1, 12, 0, 0, 0, Some(2 * 3600));
    let later = cdt_at(2024, 1, 1, 11, 0, 0, 0, Some(0));

    assert_eq!(naive, utc);
    assert_eq!(utc, plus_two);
    assert_ne!(utc, later);
}

#[test]
fn a_date_is_never_equal_to_a_datetime_at_the_same_midnight() {
    assert_ne!(cdate(2024, 1, 1), cdt(2024, 1, 1, None));
    assert_ne!(cdate(2024, 1, 1), cdate(2024, 1, 2));
    assert_eq!(cdate(2024, 1, 1), cdate(2024, 1, 1));
}

#[test]
fn a_time_renders_as_its_isoformat_string_but_is_never_equal_to_one() {
    let value = ctime(10, 30, 0, 123_456, Some(-5 * 3600));

    assert_eq!(
        value.to_serde_json(),
        serde_json::json!("10:30:00.123456-05:00")
    );
    assert_ne!(value, cv(&serde_json::json!("10:30:00.123456-05:00")));
    assert!(format!("{value:?}").contains("Time"));
}

#[test]
fn time_equality_never_reads_a_naive_value_as_aware() {
    let naive = ctime(10, 0, 0, 0, None);
    let utc = ctime(10, 0, 0, 0, Some(0));
    let plus_two = ctime(12, 0, 0, 0, Some(2 * 3600));

    // Unlike DateTime, a naive time is NEVER equal to an aware one — no
    // "read naive as UTC" rule applies (see `crate::datetime`'s module doc).
    assert_ne!(naive, utc);
    // Two aware values at the same offset-adjusted instant ARE equal.
    assert_eq!(utc, plus_two);
}

#[test]
fn a_timedelta_renders_as_its_python_str_but_is_never_equal_to_a_string() {
    let value = ctimedelta(1, 3600, 0);

    assert_eq!(value.to_serde_json(), serde_json::json!("1 day, 1:00:00"));
    assert_ne!(value, cv(&serde_json::json!("1 day, 1:00:00")));
    assert!(format!("{value:?}").contains("TimeDelta"));
    assert_eq!(value, ctimedelta(1, 3600, 0));
    assert_ne!(value, ctimedelta(1, 3601, 0));
}

#[test]
fn json_parsing_never_produces_a_datetime_or_a_date() {
    // Same contract the tuple tag has: the corpus's `$datetime`/`$date`
    // encoding is decoded by test-only code, and every product parse path
    // must read one as the ordinary dict it literally is.
    let parsed: Value =
        serde_json::from_str(r#"{"$datetime": "2024-01-01T00:00:00", "$date": "2024-01-01"}"#)
            .expect("valid JSON");
    let datetime_only: Value =
        serde_json::from_str(r#"{"$datetime": "2024-01-01T00:00:00"}"#).expect("valid JSON");

    assert!(matches!(parsed, Value::Object(_)));
    assert!(matches!(datetime_only, Value::Object(_)));
    assert_eq!(
        datetime_only.to_serde_json(),
        serde_json::json!({"$datetime": "2024-01-01T00:00:00"})
    );
}

#[test]
fn calendar_values_nested_in_deep_containers_drop_without_native_recursion() {
    // A datetime is a leaf, so `take_children` must contribute nothing for
    // one — a wrong arm here would leave the work-stack unbalanced on
    // exactly the shapes the iterative `Drop` exists for.
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            const DEPTH: usize = 100_000;
            let mut value = cdt(2024, 1, 1, Some(0));
            for _ in 0..DEPTH {
                value = carr(vec![value, cdate(2024, 1, 1)]);
            }
            drop(value);
        })
        .expect("thread spawns");

    assert!(handle.join().is_ok());
}

// --- sets ----------------------------------------------------------------

/// Canonical order ranks the kinds `None` < `bool` < `int` < `float` <
/// `str` < `tuple` < `frozenset` < `list` < `set` < `dict`, and orders
/// within a kind by value — numbers numerically, not by their text.
#[test]
fn canonical_order_ranks_by_kind_then_value() {
    let items = SetItems::new(vec![
        Value::Str("b".into()),
        Value::Number(Number::from_u64(10)),
        Value::Null,
        Value::Number(Number::from_u64(2)),
        Value::Number(Number::from_f64(1.5)),
        Value::Bool(true),
        ctup(&[json!(1)]),
        cfrozen(&[json!(1)]),
    ]);

    let rendered: Vec<String> = items.iter().map(crate::path::set_item_repr).collect();
    assert_eq!(
        rendered,
        [
            "None",
            "True",
            "2",
            "10",
            "1.5",
            "'b'",
            "(1,)",
            "frozenset({1})"
        ]
    );
}

/// The source order is dropped at construction, so every rendering is
/// canonical without sorting anything.
#[test]
fn a_set_renders_to_a_json_array_in_canonical_order() {
    let set = cset(&[json!(3), json!(1), json!(2)]);
    assert_eq!(set.to_serde_json(), json!([1, 2, 3]));

    let frozen = cfrozen(&[json!("b"), json!("a")]);
    assert_eq!(frozen.to_serde_json(), json!(["a", "b"]));
}

/// A member equal to an earlier one is dropped: two of them would render
/// to one report path, which `Report` requires to be unique. A real Python
/// set cannot produce the pair, but the lossy `str` conversion boundary and
/// this crate's own public API can.
#[test]
fn set_items_drop_a_member_equal_to_an_earlier_one() {
    let items = SetItems::new(vec![
        Value::Str("x".into()),
        Value::Null,
        Value::Str("x".into()),
        Value::Null,
    ]);

    assert_eq!(items.len(), 2);
    assert_eq!(&*items, &[Value::Null, Value::Str("x".into())]);
}

/// Deduplication is structural, so every one of these survives: an integral
/// float stays distinct from the equal-valued integer (`1`/`1.0`), and every
/// number below is a genuinely different value. Signed zero is the one
/// exception — see `set_items_dedup_signed_zero_like_a_real_python_set`.
#[test]
fn set_items_keep_members_that_only_look_alike() {
    let items = SetItems::new(vec![
        Value::Number(Number::from_u64(1)),
        Value::Number(Number::from_f64(1.0)),
        Value::Bool(true),
        Value::Number(Number::from_f64(2.0)),
        Value::Number(Number::from_u64(u64::MAX)),
        Value::Number(Number::from_i64(-1)),
    ]);

    assert_eq!(items.len(), 6);
}

/// A real Python `set` can never hold both `-0.0` and `0.0` — they compare
/// and hash equal, so the second `.add()` is a no-op — confirmed against
/// `deepdiff==9.1.0`: `DeepDiff({0.0, -0.0}, {0.0})` is `{}`. `SetItems::new`
/// must dedup the pair the same way, keeping the first-inserted
/// representative (whichever the caller's `items` order put first), exactly
/// like the existing same-value dedup for any other equal pair.
#[test]
fn set_items_dedup_signed_zero_like_a_real_python_set() {
    let items = SetItems::new(vec![
        Value::Number(Number::from_f64(-0.0)),
        Value::Number(Number::from_f64(0.0)),
    ]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0], Value::Number(Number::from_f64(0.0)));
    assert_eq!(items[0].to_serde_json().to_string(), "-0.0");

    // Reversed input order keeps the other representative — first inserted
    // wins either way.
    let items = SetItems::new(vec![
        Value::Number(Number::from_f64(0.0)),
        Value::Number(Number::from_f64(-0.0)),
    ]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].to_serde_json().to_string(), "0.0");

    // The pair still dedups nested inside a container that is itself a
    // set member: `(-0.0,)` and `(0.0,)` are `Value`-equal (tuples compare
    // element-wise, and `Number` equality treats the two zeros as one), so a
    // set built directly from both keeps only one, matching what a real
    // Python `{(-0.0,), (0.0,)}` does.
    let items = SetItems::new(vec![ctup(&[json!(-0.0)]), ctup(&[json!(0.0)])]);
    assert_eq!(items.len(), 1);
}

/// See `SetItems::new`'s own doc for why a bit-identical `NaN` pair dedups
/// but a differently-signed pair does not.
#[test]
fn set_items_dedup_bit_identical_nan_but_keep_differently_signed_nan_apart() {
    let items = SetItems::new(vec![
        Value::Number(Number::from_f64(f64::NAN)),
        Value::Number(Number::from_f64(f64::NAN)),
    ]);
    assert_eq!(items.len(), 1);

    let items = SetItems::new(vec![
        Value::Number(Number::from_f64(f64::NAN)),
        Value::Number(Number::from_f64(-f64::NAN)),
    ]);
    assert_eq!(items.len(), 2);
}

/// `Value::to_serde_json` cannot hold a non-finite float (`serde_json`'s own
/// `Number::from_f64` rejects it, matching the JSON spec) — it falls back to
/// `null`, the same collapse the streaming `Deserialize` path already uses
/// for one arriving that way (see `non_finite_float_deserializes_to_null_like_serde_json`).
#[test]
fn to_serde_json_renders_a_non_finite_number_as_null() {
    for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let value = Value::Number(Number::from_f64(f));
        assert_eq!(value.to_serde_json(), serde_json::Value::Null, "for {f:?}");
    }

    // Nested inside a container, too — not just the bare top-level case.
    let nested = carr(vec![Value::Number(Number::from_f64(f64::NAN))]);
    assert_eq!(nested.to_serde_json(), serde_json::json!([null]));
}

/// [`fold_signed_zero`] must leave a `NaN`'s bits alone (see that function's
/// own doc): IEEE `+ 0.0` is not guaranteed to be the identity on a `NaN`
/// (it quiets a signaling `NaN` on this crate's targets), which would
/// otherwise make [`canonical_cmp`]/hashing silently key a `NaN` on a bit
/// pattern it never actually had.
#[test]
fn fold_signed_zero_preserves_every_nan_bit_pattern() {
    use super::fold_signed_zero;

    for bits in [
        0x7ff8_0000_0000_0000_u64, // canonical quiet NaN
        0x7ff0_0000_0000_0001_u64, // a signaling NaN payload
        0xfff8_0000_0000_0000_u64, // negative-signed quiet NaN
        0xfff0_0000_0000_0001_u64, // negative-signed signaling NaN
    ] {
        let f = f64::from_bits(bits);
        assert_eq!(
            fold_signed_zero(f).to_bits(),
            bits,
            "fold_signed_zero must not change a NaN's bits"
        );
    }

    // Still folds an ordinary signed zero, and leaves every other float
    // untouched.
    assert_eq!(fold_signed_zero(-0.0).to_bits(), 0.0_f64.to_bits());
    assert_eq!(fold_signed_zero(1.5).to_bits(), 1.5_f64.to_bits());
}

/// Two members Python would call equal but that are structurally different
/// survive construction — no Python set can hold the pair — while the *diff*
/// still treats them as one member, which is what makes `{(1,)}` versus
/// `{(1.0,)}` empty.
#[test]
fn set_items_keep_members_only_python_would_call_equal() {
    assert_eq!(
        SetItems::new(vec![ctup(&[json!(1)]), ctup(&[json!(1.0)])]).len(),
        2
    );
    assert!(
        crate::diff::diff(
            &Value::Set(SetItems::new(vec![ctup(&[json!(1)])])),
            &Value::Set(SetItems::new(vec![ctup(&[json!(1.0)])])),
        )
        .expect("shallow sets diff cleanly")
        .is_empty()
    );
}

/// Equality is element-wise in stored order — which is canonical, so two
/// sets built from the same members in different orders *are* equal, and the
/// diff's equal-inputs fast path sees them as such.
#[test]
fn sets_with_the_same_items_in_different_orders_are_structurally_equal() {
    assert_eq!(cset(&[json!(1), json!(2)]), cset(&[json!(2), json!(1)]));
    assert!(
        crate::diff::diff(&cset(&[json!(1), json!(2)]), &cset(&[json!(2), json!(1)]))
            .expect("shallow sets diff cleanly")
            .is_empty()
    );
}

#[test]
fn a_set_never_equals_another_container_kind_holding_the_same_items() {
    let items = [json!(1), json!(2)];
    assert_ne!(cset(&items), cfrozen(&items));
    assert_ne!(cset(&items), cv(&json!([1, 2])));
    assert_ne!(cset(&items), ctup(&items));
    assert_ne!(cset(&items), cset(&[json!(1)]));
}

#[test]
fn int_and_float_set_items_stay_distinct() {
    assert_ne!(cset(&[json!(1)]), cset(&[json!(1.0)]));
}

/// The iterative `Drop` covers sets too: a nest far deeper than the native
/// stack tolerates tears down cleanly rather than aborting the process.
#[test]
fn dropping_a_deeply_nested_set_does_not_overflow_the_stack() {
    let mut value = Value::FrozenSet(SetItems::new(vec![]));
    for _ in 0..200_000 {
        value = Value::FrozenSet(SetItems::new(vec![value]));
    }
    drop(value);
}

#[test]
fn set_items_expose_the_slice_and_iterate() {
    let set = SetItems::new(vec![Value::Null, Value::Bool(false)]);
    assert_eq!(set.len(), 2);
    assert!(!set.is_empty());
    assert!(SetItems::new(vec![]).is_empty());
    assert_eq!((&set).into_iter().count(), 2);
    assert_eq!(set.first(), Some(&Value::Null));
    assert_eq!(&*set, &[Value::Null, Value::Bool(false)]);
}

#[test]
fn object_entries_iterate_from_both_ends() {
    let value = cv(&json!({"a": 1, "b": 2, "c": 3}));
    let Value::Object(object) = &value else {
        panic!("a JSON object converts to Value::Object");
    };
    let keys: Vec<&str> = object
        .iter()
        .rev()
        .filter_map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(keys, ["c", "b", "a"]);
}

/// Canonical order compares two containers of the same kind element by
/// element and then by length, and reaches every kind a `Value` can be —
/// including the ones no Python set could hold, which only a directly-built
/// value can carry.
#[test]
fn canonical_order_compares_same_kind_containers_element_wise() {
    let mut builder = Builder::new();
    let object =
        |builder: &mut Builder, key: &str| builder.object(vec![(key.to_string(), Value::Null)]);
    let first = object(&mut builder, "a");
    let second = object(&mut builder, "b");
    let longer = builder.object(vec![
        ("a".to_string(), Value::Null),
        ("b".to_string(), Value::Null),
    ]);

    let items = SetItems::new(vec![
        cv(&json!([2])),
        cv(&json!([1, 9])),
        cv(&json!([1])),
        ctup(&[json!(2)]),
        ctup(&[json!(1), json!(9)]),
        ctup(&[json!(1)]),
        cfrozen(&[json!(2)]),
        cfrozen(&[json!(1), json!(9)]),
        cfrozen(&[json!(1)]),
        cset(&[json!(2)]),
        cset(&[json!(1)]),
        second,
        longer,
        first,
        Value::Null,
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::Number(Number::from_u64(u64::MAX)),
        Value::Number(Number::from_u64(u64::MAX - 1)),
        Value::Number(Number::from_i64(-3)),
    ]);

    let rendered: Vec<String> = items.iter().map(crate::path::set_item_repr).collect();
    assert_eq!(
        rendered,
        [
            "None",
            "False",
            "True",
            "-3",
            "18446744073709551614",
            "18446744073709551615",
            "(1,)",
            "(1, 9)",
            "(2,)",
            "frozenset({1})",
            "frozenset({1, 9})",
            "frozenset({2})",
            "[1]",
            "[1, 9]",
            "[2]",
            "{1}",
            "{2}",
            "{'a': None}",
            "{'a': None, 'b': None}",
            "{'b': None}",
        ]
    );
}

/// Two structurally equal containers are one member, whatever kind they are.
#[test]
fn set_items_drop_an_equal_container_member() {
    let items = SetItems::new(vec![
        ctup(&[json!(1)]),
        cv(&json!([1])),
        ctup(&[json!(1)]),
        cv(&json!([1])),
        cfrozen(&[json!(1)]),
        cfrozen(&[json!(1)]),
    ]);

    assert_eq!(items.len(), 3);
}

/// A calendar value's canonical order and top-level set-item rendering are
/// pinned here: both kinds sort after every other, datetimes by instant and
/// dates by ordinal, and each renders with Python's own `str()`
/// ([`crate::path::set_item_repr`]'s rule for a bare set item — `repr()` is
/// what a container holding one shows instead, pinned by `python_repr`'s own
/// `calendar_values_render_as_python_repr` test in `path.rs`).
#[test]
fn canonical_order_ranks_calendar_values_last() {
    let items = SetItems::new(vec![
        cdate(2024, 1, 2),
        cdt(2024, 1, 1, None),
        cdate(2024, 1, 1),
        cdt(2024, 1, 1, Some(3600)),
        cv(&json!("z")),
    ]);

    let rendered: Vec<String> = items.iter().map(crate::path::set_item_repr).collect();
    assert_eq!(
        rendered,
        [
            "'z'",
            "2024-01-01 00:00:00+01:00",
            "2024-01-01 00:00:00",
            "2024-01-01",
            "2024-01-02",
        ]
    );
}

/// `Time` and `TimeDelta` sort after every other kind including `Date`, and
/// a naive `Time` sorts before an aware one (an arbitrary but total split —
/// see `crate::datetime::Time::sort_instant`'s doc).
#[test]
fn canonical_order_ranks_time_and_timedelta_last_of_all() {
    let items = SetItems::new(vec![
        ctimedelta(0, 2, 0),
        cdate(2024, 1, 1),
        ctime(10, 0, 0, 0, Some(0)),
        ctime(1, 0, 0, 0, None),
        ctimedelta(0, 1, 0),
    ]);

    let rendered: Vec<String> = items.iter().map(crate::path::set_item_repr).collect();
    assert_eq!(
        rendered,
        [
            "2024-01-01",
            "01:00:00",
            "10:00:00+00:00",
            "0:00:01",
            "0:00:02",
        ]
    );
}

// --- Str / Key: the WTF-8 (lone surrogate) representation -----------------

#[test]
fn str_wtf8_accessors() {
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let s = Str::Wtf8(Box::from(bytes.as_slice()));

    assert_eq!(s.as_bytes(), bytes.as_slice());
    assert_eq!(s.as_utf8(), None);
    assert!(!s.is_empty());
    assert_eq!(s.chars().collect::<Vec<_>>(), [Wtf8Char::Surrogate(0xDC80)]);
}

#[test]
fn str_utf8_accessors_unaffected() {
    let s: Str = "hello".into();

    assert_eq!(s.as_bytes(), b"hello");
    assert_eq!(s.as_utf8(), Some("hello"));
    assert!(!s.is_empty());
    assert!(Str::from(String::new()).is_empty());
    assert_eq!(Str::from(Box::from("boxed")).as_utf8(), Some("boxed"));
}

#[test]
fn str_wtf8_display_is_lossy() {
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let s = Str::Wtf8(Box::from(bytes.as_slice()));

    assert_eq!(s.to_string(), "\u{FFFD}\u{FFFD}\u{FFFD}");
}

#[test]
fn str_eq_ord_hash_cross_variant() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let plain: Str = "a".into();
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let surrogate = Str::Wtf8(Box::from(bytes.as_slice()));

    // Byte order: 'a' (0x61) sorts before the surrogate's 3-byte WTF-8
    // encoding (0xED..), matching code-point order.
    assert!(plain < surrogate);
    assert_ne!(plain, surrogate);
    assert_eq!(surrogate, Str::Wtf8(Box::from(bytes.as_slice())));

    let hash_of = |s: &Str| {
        let mut hasher = DefaultHasher::new();
        s.hash(&mut hasher);
        hasher.finish()
    };

    // Equal content hashes equal...
    assert_eq!(
        hash_of(&surrogate),
        hash_of(&Str::Wtf8(Box::from(bytes.as_slice())))
    );
    // ...and content actually reaches the hasher (a no-op `Hash` impl would
    // make every value collide, including these two clearly distinct ones).
    assert_ne!(hash_of(&plain), hash_of(&surrogate));
    let other_surrogate = Str::Wtf8(Box::from(wtf8_surrogate_bytes(0xDFFF).as_slice()));
    assert_ne!(hash_of(&surrogate), hash_of(&other_surrogate));
}

#[test]
fn key_wtf8_accessors_and_conversions() {
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let key = Key::Wtf8(Box::from(bytes.as_slice()));

    assert_eq!(key.as_bytes(), bytes.as_slice());
    assert_eq!(key.as_utf8(), None);
    assert_eq!(
        key.chars().collect::<Vec<_>>(),
        [Wtf8Char::Surrogate(0xDC80)]
    );
    assert_eq!(key.to_lossy_string(), "\u{FFFD}\u{FFFD}\u{FFFD}");

    // Key -> Str is lossless (used when a report path embeds a key's own
    // content — see `crate::path::render_path`'s doc).
    let as_str: Str = (&key).into();
    assert_eq!(as_str, Str::Wtf8(Box::from(bytes.as_slice())));

    // Str -> Key (the Wtf8 arm of `Interner::intern_key`, exercised
    // end-to-end via `Builder::object` below, and pinned directly here).
    let round_trip: Key = match as_str {
        Str::Utf8(_) => unreachable!("built as Wtf8 above"),
        Str::Wtf8(b) => Key::Wtf8(b),
    };
    assert_eq!(round_trip.as_bytes(), bytes.as_slice());
}

#[test]
fn key_ord_cross_variant() {
    let utf8_key = Key::Utf8(Arc::from("a"));
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let wtf8_key = Key::Wtf8(Box::from(bytes.as_slice()));

    assert!(utf8_key < wtf8_key);
    assert_eq!(
        utf8_key.partial_cmp(&wtf8_key),
        Some(std::cmp::Ordering::Less)
    );
    assert_ne!(utf8_key, wtf8_key);
}

#[test]
fn key_eq_is_true_for_equal_content_same_and_cross_variant_construction() {
    // `key_ord_cross_variant` above only ever asserts inequality, which a
    // `PartialEq` impl unconditionally returning `false` would also satisfy;
    // this pins the positive case directly, for both variants and for two
    // separately-allocated `Utf8` keys (not the same `Arc`).
    assert_eq!(Key::Utf8(Arc::from("a")), Key::Utf8(Arc::from("a")));

    let bytes = wtf8_surrogate_bytes(0xDC80);
    assert_eq!(
        Key::Wtf8(Box::from(bytes.as_slice())),
        Key::Wtf8(Box::from(bytes.as_slice()))
    );
}

#[test]
fn key_hash_reaches_content() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let hash_of = |k: &Key| {
        let mut hasher = DefaultHasher::new();
        k.hash(&mut hasher);
        hasher.finish()
    };

    let utf8_key = Key::Utf8(Arc::from("a"));
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let wtf8_key = Key::Wtf8(Box::from(bytes.as_slice()));

    // Equal content hashes equal...
    assert_eq!(hash_of(&utf8_key), hash_of(&Key::Utf8(Arc::from("a"))));
    // ...and content actually reaches the hasher (a no-op `Hash` impl would
    // make every value collide, including these two clearly distinct ones).
    assert_ne!(hash_of(&utf8_key), hash_of(&wtf8_key));
}

#[test]
fn builder_object_carries_a_wtf8_key_through_without_interning() {
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let key: Str = Str::Wtf8(Box::from(bytes.as_slice()));

    let mut builder = Builder::new();
    let value = builder.object(vec![(key, Value::Number(Number::from_u64(1)))]);

    let Value::Object(obj) = &value else {
        panic!("expected object");
    };
    assert_eq!(obj.len(), 1);
    let (found_key, found_value) = obj.iter().next().expect("one entry");
    let ObjectKey::Str(found_key) = found_key else {
        panic!("expected a str key");
    };
    assert_eq!(found_key.as_bytes(), bytes.as_slice());
    assert_eq!(found_value, &Value::Number(Number::from_u64(1)));
    assert!(found_key.as_utf8().is_none());
}

#[test]
fn builder_intern_key_interns_a_plain_str_but_not_a_wtf8_one() {
    // `Builder::intern_key` is the public entry point `onix-py`'s
    // conversion calls directly (for a dict key it has already classified,
    // see `crates/onix-py/src/convert.rs`); this exercises it the same way
    // `Builder::object`'s own `Utf8`/`Wtf8` split is exercised above, but
    // through this standalone wrapper rather than the whole-object path.
    let mut builder = Builder::new();

    let first = builder.intern_key(Str::from("shared"));
    let second = builder.intern_key(Str::from("shared"));
    let Key::Utf8(first_arc) = &first else {
        panic!("expected an interned Utf8 key");
    };
    let Key::Utf8(second_arc) = &second else {
        panic!("expected an interned Utf8 key");
    };
    assert!(std::sync::Arc::ptr_eq(first_arc, second_arc));

    let bytes = wtf8_surrogate_bytes(0xDC80);
    let wtf8_key = builder.intern_key(Str::Wtf8(Box::from(bytes.as_slice())));
    assert_eq!(wtf8_key.as_bytes(), bytes.as_slice());
}

// --- Wtf8Chars: the shared WTF-8 decoder -----------------------------------

#[test]
fn wtf8_chars_decodes_plain_ascii_in_one_call() {
    let chars: Vec<Wtf8Char> = Wtf8Chars::new(b"ab").collect();
    assert_eq!(chars, [Wtf8Char::Scalar('a'), Wtf8Char::Scalar('b')]);
}

#[test]
fn wtf8_chars_decodes_a_valid_prefix_before_a_surrogate() {
    // "ab" (two single-byte chars) followed by one surrogate's 3-byte WTF-8
    // encoding: exercises both the `Ok(valid)` fast path (no invalid bytes
    // left) and the `Err(..) if valid_up_to() > 0` partial-prefix path (a
    // valid run sits before the surrogate).
    let mut bytes = b"ab".to_vec();
    bytes.extend_from_slice(&wtf8_surrogate_bytes(0xDC80));

    let chars: Vec<Wtf8Char> = Wtf8Chars::new(&bytes).collect();
    assert_eq!(
        chars,
        [
            Wtf8Char::Scalar('a'),
            Wtf8Char::Scalar('b'),
            Wtf8Char::Surrogate(0xDC80),
        ]
    );
}

#[test]
fn wtf8_chars_decodes_a_multibyte_scalar_then_a_surrogate() {
    // 'é' (2-byte UTF-8) immediately followed by a surrogate: the invalid
    // byte sits at offset 0 of the *remaining* slice only after 'é' is
    // consumed, so this also exercises the `valid_up_to() == 0` branch
    // directly (no valid prefix at all once 'é' has been taken).
    let mut bytes = "é".as_bytes().to_vec();
    bytes.extend_from_slice(&wtf8_surrogate_bytes(0xDFFF));

    let chars: Vec<Wtf8Char> = Wtf8Chars::new(&bytes).collect();
    assert_eq!(chars, [Wtf8Char::Scalar('é'), Wtf8Char::Surrogate(0xDFFF)]);
}

#[test]
fn wtf8_chars_decodes_two_adjacent_surrogates_and_a_trailing_scalar() {
    let mut bytes = wtf8_surrogate_bytes(0xD800).to_vec();
    bytes.extend_from_slice(&wtf8_surrogate_bytes(0xDFFF));
    bytes.push(b'z');

    let chars: Vec<Wtf8Char> = Wtf8Chars::new(&bytes).collect();
    assert_eq!(
        chars,
        [
            Wtf8Char::Surrogate(0xD800),
            Wtf8Char::Surrogate(0xDFFF),
            Wtf8Char::Scalar('z'),
        ]
    );
}

#[test]
fn wtf8_chars_empty_bytes_yield_nothing() {
    assert_eq!(Wtf8Chars::new(b"").collect::<Vec<_>>(), []);
}

#[test]
fn utf8_sequence_width_boundary_at_the_ascii_continuation_split() {
    // Pins the `< 0x80` boundary directly: 0x7F (the last ASCII byte) is
    // width 1; 0x80 (a continuation byte, never a real lead byte at a
    // WTF-8 character boundary, but a valid `u8` all the same) must NOT be
    // read as width 1 too — it matches none of the multi-byte lead-byte
    // patterns either, so it falls all the way to the 4-byte default,
    // distinguishing `< 0x80` from a `<= 0x80` that would misroute it to
    // width 1 instead. This function is never called with 0x80 through the
    // public `Wtf8Chars` iterator, so only a direct call exercises it.
    assert_eq!(super::utf8_sequence_width(0x7F), 1);
    assert_eq!(super::utf8_sequence_width(0x80), 4);
}

#[test]
fn wtf8_chars_decode_time_scales_linearly_not_quadratically() {
    // Quadrupling the input should roughly 4x the decode time; bounded
    // generously at 8x to absorb machine noise without masking a real
    // regression. Run for both a plain-ASCII string and one ending in a
    // lone surrogate, since the two decode paths (`Ok`/`Err` in
    // `Wtf8Chars::next`) have independent per-call costs. See `Wtf8Chars`'s
    // own doc for why the decode must stay linear.
    fn decode_all(bytes: &[u8]) -> usize {
        Wtf8Chars::new(bytes).count()
    }

    fn time_decode(bytes: &[u8]) -> std::time::Duration {
        // A handful of repeats, keeping only the minimum, damps scheduler
        // noise without inflating the total runtime much.
        (0..5)
            .map(|_| {
                let start = std::time::Instant::now();
                assert!(decode_all(bytes) > 0);
                start.elapsed()
            })
            .min()
            .expect("5 repeats always yields a minimum")
    }

    fn build(chars: usize, with_surrogate: bool) -> Vec<u8> {
        let mut bytes = "a".repeat(chars).into_bytes();
        if with_surrogate {
            bytes.extend_from_slice(&wtf8_surrogate_bytes(0xDC80));
        }
        bytes
    }

    const SMALL: usize = 25_000;
    const LARGE: usize = SMALL * 4;
    const MAX_RATIO: u32 = 8;

    for with_surrogate in [false, true] {
        let small = time_decode(&build(SMALL, with_surrogate));
        let large = time_decode(&build(LARGE, with_surrogate));
        assert!(
            large <= small * MAX_RATIO,
            "decoding {LARGE} chars (surrogate: {with_surrogate}) took {large:?}, more than              {MAX_RATIO}x the {SMALL}-char time ({small:?}) — looks quadratic, not linear"
        );
    }
}

// --- contains_wtf8 ----------------------------------------------------------

#[test]
fn contains_wtf8_false_for_every_ordinary_shape() {
    assert!(!contains_wtf8(&Value::Null));
    assert!(!contains_wtf8(&cv(
        &json!({"a": [1, "x", true, 1.5], "b": null})
    )));
    assert!(!contains_wtf8(&Value::Tuple(
        Box::new([cv(&json!(1))]).into()
    )));
    assert!(!contains_wtf8(&Value::Set(SetItems::new(vec![cv(
        &json!("x")
    )]))));
    assert!(!contains_wtf8(&Value::FrozenSet(SetItems::new(vec![cv(
        &json!("x")
    )]))));
}

#[test]
fn contains_wtf8_true_for_a_bare_wtf8_string() {
    let bytes = wtf8_surrogate_bytes(0xDC80);
    assert!(contains_wtf8(&Value::Str(Str::Wtf8(Box::from(
        bytes.as_slice()
    )))));
}

#[test]
fn contains_wtf8_true_transitively_through_every_container_kind() {
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let leaf = Value::Str(Str::Wtf8(Box::from(bytes.as_slice())));

    assert!(contains_wtf8(&Value::Array(
        Box::new([leaf.clone()]).into()
    )));
    assert!(contains_wtf8(&Value::Tuple(
        Box::new([leaf.clone()]).into()
    )));
    assert!(contains_wtf8(&Value::Set(SetItems::new(vec![
        leaf.clone()
    ]))));
    assert!(contains_wtf8(&Value::FrozenSet(SetItems::new(vec![
        leaf.clone()
    ]))));

    let mut builder = Builder::new();
    let nested = builder.object(vec![("a".to_string(), leaf)]);
    assert!(contains_wtf8(&Value::Array(Box::new([nested]).into())));
}

#[test]
fn contains_wtf8_true_for_a_wtf8_object_key_with_an_ordinary_value() {
    let bytes = wtf8_surrogate_bytes(0xDC80);
    let key: Str = Str::Wtf8(Box::from(bytes.as_slice()));
    let mut builder = Builder::new();
    let value = builder.object(vec![(key, Value::Number(Number::from_u64(1)))]);

    assert!(contains_wtf8(&value));
}

// --- write_json_str_content: the byte-exact JSON string-content writer ----

fn write_json_str_content_string(bytes: &[u8]) -> String {
    let mut out = String::new();
    write_json_str_content(bytes, &mut out);
    out
}

#[test]
fn write_json_str_content_matches_serde_json_for_ordinary_content() {
    assert_eq!(write_json_str_content_string(b"d\"e"), r#"d\"e"#);
}

#[test]
fn write_json_str_content_escapes_a_bare_surrogate() {
    let bytes = wtf8_surrogate_bytes(0xDC80);
    assert_eq!(write_json_str_content_string(&bytes), r"\udc80");
}

#[test]
fn write_json_str_content_escapes_a_surrogate_between_two_plain_runs() {
    // "a" + surrogate + "b" exercises the run-flush (a non-empty run pushed
    // before the escape) and the trailing-run flush after the loop.
    let mut bytes = b"a".to_vec();
    bytes.extend_from_slice(&wtf8_surrogate_bytes(0xDC80));
    bytes.push(b'b');
    assert_eq!(write_json_str_content_string(&bytes), r"a\udc80b");
}

#[test]
fn write_json_str_content_escapes_a_surrogate_needing_escaped_neighbors_too() {
    // The plain run around the surrogate itself needs ordinary JSON
    // escaping (a literal quote), proving `push_escaped_run` — not a raw
    // byte copy — renders it.
    let mut bytes = b"a\"".to_vec();
    bytes.extend_from_slice(&wtf8_surrogate_bytes(0xDC80));
    assert_eq!(write_json_str_content_string(&bytes), r#"a\"\udc80"#);
}

#[test]
fn write_json_str_content_leading_surrogate_with_no_prefix_run() {
    // A surrogate as the very first code point: `run` is empty when the
    // `Surrogate` arm fires, exercising the `!run.is_empty()` guard's false
    // branch.
    let bytes = wtf8_surrogate_bytes(0xD800);
    assert_eq!(write_json_str_content_string(&bytes), r"\ud800");
}

#[test]
fn write_json_str_content_two_adjacent_surrogates_have_no_run_between_them() {
    // Two surrogates back-to-back: `run` stays empty across the whole
    // decode, so the `!run.is_empty()` flush never fires between them —
    // only the trailing flush check after the loop (also a no-op here) is
    // exercised on the empty-run side.
    let mut bytes = wtf8_surrogate_bytes(0xD800).to_vec();
    bytes.extend_from_slice(&wtf8_surrogate_bytes(0xDFFF));
    assert_eq!(write_json_str_content_string(&bytes), r"\ud800\udfff");
}
