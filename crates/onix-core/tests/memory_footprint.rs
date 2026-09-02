//! Memory-footprint smoke check for the compact [`onix_core::Value`] model,
//! isolated in its own integration-test binary.
//!
//! It installs an instrumented global allocator (counting only, delegating to
//! the system allocator) and measures the live heap a small-map-heavy tree
//! retains as `serde_json::Value` versus the compact `onix_core::Value`,
//! asserting the compact model is at least 3x smaller.
//!
//! Why a dedicated binary rather than a `#[cfg(test)]` unit test: the
//! allocator's counters are process-global, so a net measurement taken while
//! other test threads allocate and free concurrently is meaningless (their
//! frees can cancel this thread's allocations, yielding a zero/NaN ratio). A
//! separate integration binary runs this as the only test in its own process,
//! so the measurement is taken with no competing allocation activity.

use std::alloc::System;

use onix_core::Value;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Net bytes still allocated (allocated minus freed) over `region` — the live
/// footprint of whatever the region retained.
fn retained_bytes(region: &Region<'_, System>) -> usize {
    let change = region.change();
    change
        .bytes_allocated
        .saturating_sub(change.bytes_deallocated)
}

/// An array of `n` small `{"id": <int>, "tag": "x"}` objects.
fn build_small_map_synthetic(n: usize) -> serde_json::Value {
    let items = (0..n)
        .map(|i| {
            let id = u64::try_from(i).expect("index fits u64");
            let mut map = serde_json::Map::new();
            map.insert("id".to_owned(), serde_json::Value::Number(id.into()));
            map.insert("tag".to_owned(), serde_json::Value::String("x".to_owned()));
            serde_json::Value::Object(map)
        })
        .collect();
    serde_json::Value::Array(items)
}

#[test]
fn compact_footprint_beats_serde_json_on_small_maps() {
    // Small-maps-heavy synthetic: an array of many two-key objects, the shape
    // where serde_json's fixed-size BTreeMap nodes waste the most.
    const N: usize = 100_000;
    let synthetic = build_small_map_synthetic(N);

    let region = Region::new(GLOBAL);
    let serde_tree = synthetic.clone();
    let serde_bytes = retained_bytes(&region);

    let region = Region::new(GLOBAL);
    let compact_tree = Value::from(synthetic.clone());
    let compact_bytes = retained_bytes(&region);

    // Keep both live until measured.
    assert!(serde_tree.is_array());
    assert!(matches!(compact_tree, Value::Array(_)));

    #[allow(
        clippy::cast_precision_loss,
        reason = "byte counts feed an approximate ratio; sub-ULP precision is \
                  irrelevant to a >=3x smoke assertion"
    )]
    let ratio = serde_bytes as f64 / compact_bytes as f64;
    println!(
        "memory smoke: serde_json={serde_bytes} B, compact={compact_bytes} B, ratio={ratio:.2}x"
    );
    assert!(
        ratio >= 3.0,
        "compact footprint should be >=3x smaller; got {ratio:.2}x \
         (serde_json={serde_bytes} B, compact={compact_bytes} B)"
    );

    drop(serde_tree);
    drop(compact_tree);
}
