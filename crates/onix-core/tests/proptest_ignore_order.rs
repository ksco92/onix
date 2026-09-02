//! Property-based tests for `ignore_order=True`'s algebraic invariants.
//!
//! Complements `proptest_diff.rs` (the ordered-path invariants) with
//! the one invariant genuinely specific to `ignore_order`: a shuffled copy of
//! any list diffs to an empty report, for arbitrary JSON-shaped list
//! elements (scalars and small nested containers), not just the hand-picked
//! examples in `ignore_order.rs`'s own unit tests. Reuses the same bounded,
//! seeded generator shape as `proptest_diff.rs` — see that file's doc for
//! the rationale behind the depth/node/case-count bounds and the fixed seed.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use serde_json::{Map, Number, Value};

use onix_core::{DiffOptions, diff_with_options};

const MAX_GENERATED_DEPTH: u32 = 4;
const MAX_GENERATED_NODES: u32 = 32;
const MAX_COLLECTION_BRANCH: u32 = 4;
const PROPTEST_CASES: u32 = 256;
const PROPTEST_SEED: u64 = 0x0451_1745_0999_0002;

fn config() -> Config {
    Config {
        cases: PROPTEST_CASES,
        rng_seed: RngSeed::Fixed(PROPTEST_SEED),
        ..Config::default()
    }
}

fn arb_key() -> impl Strategy<Value = String> {
    r#"[^'"\\\x00-\x1f\x7f]{0,8}"#
}

fn arb_json_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i32>().prop_map(|n| Value::Number(Number::from(n))),
        any::<f32>()
            .prop_filter("JSON has no NaN/Infinity", |f| f.is_finite())
            .prop_map(|f| Value::Number(
                Number::from_f64(f64::from(f)).expect("filtered to finite above")
            )),
        ".{0,8}".prop_map(Value::String),
    ]
}

/// An arbitrary JSON-shaped value bounded to [`MAX_GENERATED_DEPTH`] — a
/// list element, so it can itself be a nested list/dict (exercising
/// [`crate::ignore_order`]'s canonical-key recursion), not just a scalar.
fn arb_json_value() -> impl Strategy<Value = Value> {
    arb_json_leaf().prop_recursive(
        MAX_GENERATED_DEPTH,
        MAX_GENERATED_NODES,
        MAX_COLLECTION_BRANCH,
        |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..MAX_COLLECTION_BRANCH as usize)
                    .prop_map(Value::Array),
                proptest::collection::btree_map(
                    arb_key(),
                    inner,
                    0..MAX_COLLECTION_BRANCH as usize
                )
                .prop_map(|m| Value::Object(Map::from_iter(m))),
            ]
        },
    )
}

/// A list of [`arb_json_value`]s, plus a Fisher-Yates-style shuffle
/// permutation of the same length (proptest's own `Just`-index shuffle
/// strategy, not a hand-rolled RNG) — used to build `b` as a genuine
/// reordering of `a`, never a resampled list that merely happens to look
/// similar.
fn arb_list_and_permutation() -> impl Strategy<Value = (Vec<Value>, Vec<usize>)> {
    proptest::collection::vec(arb_json_value(), 0..10).prop_flat_map(|list| {
        let len = list.len();
        Just(list).prop_flat_map(move |list| {
            proptest::sample::subsequence((0..len).collect::<Vec<_>>(), len)
                .prop_map(move |perm| (list.clone(), perm))
        })
    })
}

fn ignore_order_diff_ok(a: &Value, b: &Value) -> Value {
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };
    diff_with_options(a, b, &opts)
        .expect("generated values are far under DEFAULT_MAX_DEPTH")
        .to_json_value()
}

proptest! {
    #![proptest_config(config())]

    /// The one invariant genuinely specific to `ignore_order`: shuffling a
    /// list's elements (any permutation, including the identity one) never
    /// produces a difference, for arbitrary JSON-shaped elements — not just
    /// scalars.
    #[test]
    fn shuffled_copy_of_any_list_diffs_to_empty((a, perm) in arb_list_and_permutation()) {
        let shuffled: Vec<Value> = perm.into_iter().map(|i| a[i].clone()).collect();
        let report = ignore_order_diff_ok(&Value::Array(a), &Value::Array(shuffled));
        prop_assert_eq!(report, serde_json::json!({}));
    }

    /// A basic robustness property: diffing two independently-generated
    /// lists under `ignore_order` never panics and always returns valid
    /// JSON (an empty object at minimum) — generated depth/size are both
    /// far under `DEFAULT_MAX_DEPTH`, so `Err` would itself be a bug.
    #[test]
    fn two_independent_lists_never_panic_under_ignore_order(
        a in proptest::collection::vec(arb_json_value(), 0..10),
        b in proptest::collection::vec(arb_json_value(), 0..10),
    ) {
        let report = ignore_order_diff_ok(&Value::Array(a), &Value::Array(b));
        prop_assert!(report.is_object());
    }
}
