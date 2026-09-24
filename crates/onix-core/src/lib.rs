//! Core diff engine: deep structural diffing of JSON-shaped [`Value`]s,
//! produced by a caller, into `DeepDiff`-compatible reports.
//!
//! - **Dispatch**: `diff_at` recurses by type under a shared depth budget.
//! - **Container comparison**: ordered/LCS matching
//!   (`docs/design/list-diff.md`) or, under `ignore_order`, hashed
//!   pairing (`docs/design/ignore-order.md`).
//! - **Report**: findings accumulate into a [`Report`], keyed by path.
//! - **Render**: [`Report::to_json_value`] renders `DeepDiff` JSON.

pub mod datetime;
pub mod diff;
pub mod error;
mod ignore_order;
mod lcs;
pub mod path;
pub mod report;
mod unified_diff;
pub mod value;

#[cfg(test)]
pub(crate) mod test_support;

pub use datetime::{Date, DateTime, Time, TimeDelta};
pub use diff::{DEFAULT_MAX_DEPTH, DiffOptions, diff, diff_with_max_depth, diff_with_options};
pub use error::Error;
pub use report::Report;
pub use value::{Builder, Number, Value};

/// Whether `value` nests strictly deeper than `limit` levels (`value`
/// itself is depth `0`). Iterative — an explicit heap work-stack, safe on
/// any input depth — and returns as soon as one node past `limit` is
/// seen. See [`diff_with_max_depth`]'s doc for the depth budget it backs.
#[must_use]
pub fn exceeds_depth(value: &Value, limit: usize) -> bool {
    diff::deeper_than(value, limit)
}

/// Returns the version of the `onix-core` crate.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::{exceeds_depth, version};
    use crate::Value;
    use serde_json::json;

    #[test]
    fn version_matches_manifest() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn exceeds_depth_delegates_to_the_core_depth_check() {
        // `[[[1]]]` is depth 3: exceeds limit 2, not limit 3.
        assert!(exceeds_depth(&Value::from(json!([[[1]]])), 2));
        assert!(!exceeds_depth(&Value::from(json!([[[1]]])), 3));
    }
}
