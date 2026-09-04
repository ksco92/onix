//! Regression guard for the `ignore_order` distance-memo cache footprint,
//! isolated in its own integration-test binary.
//!
//! `ignore_order` pairing ranks every `(removed, added)` candidate pair by a
//! subtree distance and caches the result keyed by the two items' structural
//! keys, so a list with `A` added and `R` removed distinct containers records
//! `A * R` cache entries. Each key is a whole record's structural identity;
//! cloning that identity into every cache entry (rather than sharing it behind
//! a refcount) made the cache cost scale with `pairs * record_size`, driving
//! the peak on set/tuple/datetime-bearing records to ~9x the ordered diff and
//! ~9x `DeepDiff` (roughly 1 GB for 10k records -- see issue #31). Sharing the
//! key means a cache entry costs a bounded number of allocations no matter how
//! large the record is.
//!
//! This test pins that invariant with an *allocation count* rather than a
//! wall-clock or peak-RSS number (peak RSS is not observable in-process
//! without a peak-tracking allocator; a wall-clock scaling test is already
//! known-flaky, issue #33). It diffs the same shuffled records twice with the
//! same pairing structure -- once narrow, once with many extra equal fields
//! that widen each record's key without changing which records differ -- and
//! asserts that widening the records does not multiply the diff's allocations.
//! On the pre-fix code (keys deep-cloned into the cache) the wide run
//! allocated ~5x the narrow one; sharing the key keeps the two within ~12%.
//!
//! A dedicated binary, matching `memory_footprint.rs`'s rationale: the
//! allocator's counters are process-global, so the measurement must run as the
//! only test in its process, free of concurrent allocation activity.

use std::alloc::System;

use onix_core::datetime::{Date, DateTime};
use onix_core::value::{Builder, SetItems};
use onix_core::{DiffOptions, Number, Value, diff_with_options};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Records per side.
const N: usize = 600;
/// Records mutated on the `b` side -- each becomes one added/removed hash, so
/// the greedy pairing evaluates `MUTATED * MUTATED` candidate distances (and
/// records that many container-pair cache entries).
const MUTATED: usize = 120;

const TAG_POOL: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
];

/// One record carrying the field kinds issue #31 measured -- a datetime, a
/// tuple coordinate, and a small string set -- plus `wide` extra string fields.
/// The extra fields hold the *same* value in `a` and `b`, so they enlarge each
/// record's structural key without changing which records differ or how they
/// pair.
fn make_record(
    builder: &mut Builder,
    i: usize,
    day_shift: i64,
    extra_tag: Option<usize>,
    wide: usize,
) -> Value {
    let date = Date::from_ordinal(737_000 + i64::try_from(i).unwrap() + day_shift).unwrap();
    let dt = DateTime::new(
        date,
        u8::try_from(i % 24).unwrap(),
        u8::try_from(i % 60).unwrap(),
        u8::try_from(i % 60).unwrap(),
        0,
        if i.is_multiple_of(2) { Some(0) } else { None },
    )
    .unwrap();

    let coordinate = Value::Tuple(
        vec![
            Value::Number(Number::from_i64(i64::try_from(i % 2000).unwrap() - 1000)),
            Value::Number(Number::from_i64(i64::try_from(i % 400).unwrap() - 200)),
        ]
        .into_boxed_slice(),
    );

    let mut tags: Vec<Value> = (0..=(i % 4))
        .map(|k| Value::Str(Box::from(TAG_POOL[(i + k) % TAG_POOL.len()])))
        .collect();
    if let Some(tag) = extra_tag {
        tags.push(Value::Str(Box::from(TAG_POOL[tag % TAG_POOL.len()])));
    }

    let mut entries = vec![
        (
            "id".to_owned(),
            Value::Number(Number::from_i64(i64::try_from(i).unwrap())),
        ),
        (
            "name".to_owned(),
            Value::Str(Box::from(format!("typed_{i:07}"))),
        ),
        ("created_at".to_owned(), Value::DateTime(dt)),
        ("coordinate".to_owned(), coordinate),
        ("tags".to_owned(), Value::Set(SetItems::new(tags))),
    ];
    for f in 0..wide {
        entries.push((
            format!("field_{f:03}"),
            Value::Str(Box::from(format!("value_{i:07}_{f:03}_padding_padding"))),
        ));
    }
    builder.object(entries)
}

/// `(a, b)` where `b` copies `a` and shifts `MUTATED` records' datetimes (and
/// adds a tag), each record `wide` extra fields wide. Each mutated record stays
/// close to its own original -- the datetime and one tag change against ~10
/// unchanged structural nodes -- so pairing matches every mutation to its
/// original as a `values_changed`, exactly the shape issue #31 measured, and
/// the extra equal fields never enter the report.
fn build_case(wide: usize) -> (Value, Value) {
    let mut builder = Builder::new();
    let a: Vec<Value> = (0..N)
        .map(|i| make_record(&mut builder, i, 0, None, wide))
        .collect();
    let mut b: Vec<Value> = (0..N)
        .map(|i| make_record(&mut builder, i, 0, None, wide))
        .collect();
    let step = (N / MUTATED).max(1);
    let mut i = 0;
    while i < N {
        b[i] = make_record(&mut builder, i, 7, Some(i), wide);
        i += step;
    }
    (
        Value::Array(a.into_boxed_slice()),
        Value::Array(b.into_boxed_slice()),
    )
}

/// The number of allocations one `ignore_order` diff of `wide`-wide records
/// performs. Returns the report's JSON length too, so the caller can confirm
/// widening the records left the diff output structurally unchanged.
fn diff_allocations(wide: usize) -> (u64, usize) {
    let (a, b) = build_case(wide);
    let opts = DiffOptions {
        ignore_order: true,
        ..DiffOptions::default()
    };

    let region = Region::new(GLOBAL);
    let report = diff_with_options(&a, &b, &opts).unwrap();
    let allocations = region.change().allocations;

    (
        u64::try_from(allocations).unwrap(),
        report.to_json_value().to_string().len(),
    )
}

#[test]
fn widening_records_does_not_multiply_ignore_order_allocations() {
    let (narrow_allocs, narrow_len) = diff_allocations(0);
    let (wide_allocs, wide_len) = diff_allocations(50);

    // Widening the records enlarges each cached key but changes neither the
    // set of differing records nor how they pair, so the report is the same
    // shape either way -- the extra fields are equal on both sides.
    assert_eq!(
        narrow_len, wide_len,
        "widening equal fields must not change the diff output"
    );

    // The cache holds one entry per candidate pair. If a cache entry costs
    // O(record size) (a deep key clone), the wide run allocates several times
    // the narrow one; if it costs O(1) (a shared key), the two stay close.
    // Pre-fix this ratio was ~5x; the shared key keeps it near 1x. The 2x
    // ceiling sits well clear of both.
    assert!(
        wide_allocs < narrow_allocs * 2,
        "widening records must not multiply ignore_order allocations: \
         narrow={narrow_allocs}, wide={wide_allocs} (ceiling {})",
        narrow_allocs * 2
    );
}
