//! Core diff engine for `onix`: deep structural diffing of JSON-shaped
//! values with DeepDiff-compatible reports.
//!
//! # Architecture map
//!
//! A caller hands two already-parsed `serde_json::Value`s to [`diff()`] (or
//! [`diff_with_options`]/[`diff_with_max_depth`]); this crate does no
//! parsing of its own (see `onix-cli`'s `run` module for where the JSON text
//! a real CLI invocation reads actually gets parsed). From there:
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

pub use diff::{DEFAULT_MAX_DEPTH, DiffOptions, diff, diff_with_max_depth, diff_with_options};
pub use error::Error;
pub use report::Report;

/// Returns the version of the `onix-core` crate.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_matches_manifest() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }
}
