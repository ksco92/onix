//! Core diff engine for `onix`: deep structural diffing of JSON-shaped
//! values with DeepDiff-compatible reports.
//!
//! # Architecture map
//!
//! A caller hands two already-parsed [`Value`]s (the crate's own compact,
//! memory-frugal, byte-compatible JSON value model — see [`mod@value`]) to
//! [`diff()`] (or [`diff_with_options`]/[`diff_with_max_depth`]); this crate
//! does no parsing of its own. Callers produce a [`Value`] directly: `onix-cli`
//! stream-parses JSON text straight into one (via its `serde::Deserialize`
//! impl), and the Python bindings build one from live Python objects via
//! [`value::Builder`]. [`From`]`<`[`serde_json::Value`]`>` also exists, as
//! public API for a caller that already holds a `serde_json::Value` — but the
//! engine never itself materializes one on the input side; it operates
//! entirely on the compact model. From there:
//!
//! 1. **Dispatch** ([`mod@diff`], specifically its `dispatch` submodule):
//!    `diff_at` recurses through the pair by JSON type, enforcing the
//!    recursion-depth/value-depth invariants documented on that module's
//!    doc (the crate's core `DoS` hardening).
//! 2. **Container comparison**, depending on [`DiffOptions::ignore_order`]:
//!    - **ordered (default):** `diff::object` walks a dict's key set;
//!      `diff::array` picks between an index-aligned scan and an
//!      LCS/`difflib`-style match (`crate::lcs`) for scalar-only lists — see
//!      [`mod@diff`]'s "List diffing" doc section for the exact spec.
//!    - **`ignore_order=true`:** every list, at any depth, instead goes
//!      through `crate::ignore_order`: hash each item to a canonical key,
//!      gate on how much the two lists overlap, greedily pair the rest by
//!      structural distance, then recurse into each paired pair — see that
//!      module's own doc for the full, empirically-verified algorithm.
//! 3. **Report** ([`report`]): every finding (added/removed/changed/
//!    type-changed) accumulates into a [`Report`], keyed by structural path
//!    ([`path::PathSegment`]) rather than by rendered string, so distinct
//!    paths can never collide before serialization.
//! 4. **Render**: [`Report::to_json_value`] renders the accumulated findings
//!    into `DeepDiff`-compatible JSON — [`path::render_path`] is what turns
//!    a structural path into the `root['a'][0]`-style string `DeepDiff`
//!    itself produces.
//!
//! [`diff()`] handles scalars (`values_changed`, `type_changes`), dicts
//! (`dictionary_item_added`, `dictionary_item_removed`, plus recursion into
//! shared keys), and lists (`iterable_item_added`, `iterable_item_removed`,
//! index-aligned/LCS comparison, plus recursion into shared indices), with
//! findings reported at their full deep path, in any mix of nesting. See
//! the [`mod@diff`] module doc for the recursion-depth hardening against
//! untrusted deeply nested input. [`diff_with_options`] additionally
//! supports [`DiffOptions::ignore_order`] (mirroring `DeepDiff(...,
//! ignore_order=True)`) — see `crate::ignore_order`'s module doc (private,
//! read the source) for the full spec.

pub mod diff;
pub mod error;
mod ignore_order;
mod lcs;
pub mod path;
pub mod report;
pub mod value;

#[cfg(test)]
pub(crate) mod test_support;

pub use diff::{DEFAULT_MAX_DEPTH, DiffOptions, diff, diff_with_max_depth, diff_with_options};
pub use error::Error;
pub use report::Report;
pub use value::{Builder, Number, Value};

/// Returns `true` if `value` is nested strictly deeper than `limit` levels,
/// treating `value` itself as the root (depth `0`): a scalar
/// (null/bool/number/string) is depth `0`, and a non-empty array/object is
/// `1 + max(depth of its elements/values)` (`0` if empty).
///
/// This is the public entry point to the same depth check the diff engine
/// uses internally to bound native recursion before cloning a value (see the
/// [`mod@diff`] module doc). It is **iterative** — an explicit heap-allocated
/// work-stack, no native recursion — so it is itself safe to run on any input
/// depth, and it returns as soon as one node past `limit` is seen without
/// visiting the rest of `value`. Consumers that recurse into a `Value` on the
/// native stack (for example a binding sizing a worker thread) can use it to
/// reject or reroute over-deep input up front.
///
/// # Examples
///
/// ```
/// use onix_core::Value;
/// use serde_json::json;
///
/// assert!(!onix_core::exceeds_depth(&Value::from(json!([1, 2, 3])), 1));
/// assert!(onix_core::exceeds_depth(&Value::from(json!([[[1]]])), 2));
/// ```
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
        // Smoke test that the public wrapper forwards to the internal
        // `deeper_than` (whose own behavior is covered in `diff::tests`):
        // `[[[1]]]` is depth 3, so it exceeds limit 2 but not limit 3.
        assert!(exceeds_depth(&Value::from(json!([[[1]]])), 2));
        assert!(!exceeds_depth(&Value::from(json!([[[1]]])), 3));
    }
}
