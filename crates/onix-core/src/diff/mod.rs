//! The diff engine's entry point.
//!
//! Two already-parsed values go through one recursive type-dispatch
//! (`diff_at` in `super::dispatch`), which handles, in the same pass:
//!
//! - **Scalars** — compared by value and type, producing `values_changed` or
//!   `type_changes`.
//! - **Dicts (JSON objects)** — compared by key set: a key present on only
//!   one side becomes `dictionary_item_added`/`dictionary_item_removed`, and
//!   a shared key recurses through the same dispatch, carrying the growing
//!   path with it.
//! - **Lists (JSON arrays)** — compared index-aligned by default, or via an
//!   LCS/`difflib`-style match when both lists are scalar-only (see "List
//!   diffing" below); a length mismatch reports the longer side's surplus
//!   tail as `iterable_item_added`/`iterable_item_removed` (see the internal
//!   `array_diff` function).
//!
//! Deep nesting of any mix of dicts/lists/scalars, deep
//! `values_changed`/`type_changes` paths, and a container found partway
//! through recursion (e.g. a list nested inside a dict, or vice versa) all
//! fall out of that single recursive dispatch with no special-casing.
//!
//! # Hardening against a stack-overflow `DoS` on deeply nested input
//!
//! Two independent native-overflow paths on untrusted deeply nested input
//! are both closed here:
//!
//! 1. **Traversal recursion.** The engine's own internal native recursion
//!    (walking into shared dict keys looking for a difference) is bounded by
//!    a configurable `max_depth` ([`diff_with_max_depth`],
//!    [`DEFAULT_MAX_DEPTH`]); exceeding it returns
//!    [`crate::Error::MaxDepthExceeded`] instead of overflowing the stack. The
//!    container equality checks that used to go through `serde_json`'s
//!    derived `PartialEq` (which recurses natively with no bound at all) now
//!    go through an internal iterative, heap-stack-based deep-equality check
//!    instead, so they cannot overflow the native call stack either,
//!    regardless of nesting depth.
//! 2. **Value depth.** A *finding* can sit at a shallow path while carrying
//!    a value that is itself deeply nested (e.g. a single
//!    `dictionary_item_added` whose added value is a 100,000-deep array) —
//!    the traversal bound above never sees that depth, because
//!    `dictionary_item_added`/`removed`/`type_changes` treat the value as an
//!    atomic leaf and never recurse into it. Every place a whole value would
//!    be cloned into a [`crate::Report`] first checks — again iteratively, no
//!    native recursion — whether *that value's own* nesting, combined with
//!    how deep its path already is, exceeds `max_depth`, and returns
//!    [`crate::Error::MaxDepthExceeded`] instead of cloning it. The two are checked
//!    against one *shared* `max_depth` budget rather than each getting their
//!    own independent `max_depth`: because both run as native recursion on
//!    the same call stack, checking the value against a flat,
//!    position-independent `max_depth` would let a deep traversal *plus* a
//!    deep value at the bottom together demand roughly `2 * max_depth` native
//!    frames at the `.clone()` call. See [`diff_with_max_depth`]'s doc for
//!    the exact contract.
//!
//! This practical depth limit is a property of the recursive engine; an
//! iterative work-stack rewrite would remove it entirely.
//!
//! The engine operates on the compact `onix_core::Value` model
//! (`crate::value`), not on `serde_json::Value`: its two input trees are the
//! memory-frugal representation, produced directly by each caller (see the
//! crate root's architecture map). Findings are still stored and rendered as
//! `serde_json::Value` on the output side (`crate::report`), converted from
//! the compact inputs only at the point a difference is recorded — the two
//! whole input trees never become `serde_json::Value`.
//!
//! # List diffing: scalar-list LCS matching
//!
//! `DeepDiff` does *not* always compare lists index by index. This section
//! is the precise, empirically-verified spec (`crate::lcs`'s doc, plus
//! `tests/golden/`'s scalar-list cases, are the executable form of it)
//! reverse engineered from real `deepdiff==9.1.0`'s `diff.py`/`model.py`.
//!
//! **The condition.** `DeepDiff` tries an LCS/`difflib`-style match instead
//! of (or alongside) the index-aligned scan whenever every element of
//! *both* lists is a JSON scalar — null, bool, number, or string
//! (`_all_values_basic_hashable`; see `crate::lcs::all_basic_scalars`). A
//! dict or a nested list *anywhere* in either list disqualifies the whole
//! comparison back to plain index-aligned, unconditionally — this is why
//! wrapping a scalar in a single-key dict reliably forces the index-aligned
//! algorithm (see `perf/RESULTS.md`'s "Correctness precheck" section, and
//! `tests/golden/README.md`).
//!
//! **The two candidates and the "keep the smaller" tie-break.** When the
//! condition holds, `DeepDiff` (`_diff_iterable_in_order`) computes the LCS
//! match (`lcs_array_diff`) first. If that produces at most one finding,
//! it is used as-is — no further work. Otherwise `DeepDiff` *also* computes
//! the plain index-aligned result (`positional_array_diff`) and keeps
//! whichever candidate has **fewer total findings** across every category
//! (`len(TreeResult)`, i.e. `Report::finding_count`), **favoring the
//! index-aligned candidate on an exact tie**. This is not a minor
//! implementation detail to skip: it is why, for example, `[1, 1.5, "a",
//! null, true]` vs `[true, "a", null, 1.5, 1]` keeps its 2-finding LCS
//! result (an add + a remove) instead of the 5-finding positional
//! all-type-changed result, while `[1.0, 2]` vs `[2, 1]` — where the LCS
//! pass also finds 2 findings but the positional pass ties it at 2 —
//! resolves to the positional (index-aligned) result instead. Both match
//! real `DeepDiff`.
//!
//! **Opcode-to-finding mapping** (`_diff_ordered_iterable_by_difflib`, see
//! `lcs_array_diff`): an `'equal'` opcode block is *never diffed further*
//! — not even for a type check (see the hashability finding below) — a
//! `'delete'` opcode reports each element as `iterable_item_removed` keyed
//! by its *old* (`a`-side) index, an `'insert'` opcode reports each element
//! as `iterable_item_added` keyed by its *new* (`b`-side) index, and a
//! `'replace'` opcode pairs up its two (possibly unequal-length) ranges
//! position by position: a position present on both sides becomes a
//! `values_changed`/`type_changes` finding keyed by its **old**-side index
//! (see the `new_path` finding below); a position present on only one side
//! becomes an added/removed finding on that side's own index. A `'replace'`
//! opcode's two ranges are proven (by construction of the underlying
//! matching algorithm — see `crate::lcs::compute_opcodes`'s doc) to never
//! share a matching element pair, so every paired position is guaranteed
//! to actually differ.
//!
//! **The `autojunk` finding.** `DeepDiff` constructs its matcher with
//! `isjunk=None, autojunk=False`, so `difflib`'s popular-element exclusion
//! never applies. `crate::lcs` implements no junk/autojunk logic at all —
//! see that module's doc for the full rule and its empirical basis.
//!
//! **The `[1]` vs `[1.0]` hashability finding.** The LCS match's own notion
//! of "equal" is Python's `==`, not this engine's own scalar equality:
//! Python treats `1 == 1.0 == True` (and `0 == 0.0 == False`) as equal
//! regardless of type. Combined with "an `'equal'` opcode is never diffed
//! further" above, this means **`DeepDiff` reports `[1]` vs `[1.0]` as
//! completely empty** — no `type_changes`, unlike every *other* numeric
//! comparison in `DeepDiff` (including this engine's own `numbers_equal`,
//! and including what the plain index-aligned candidate would compute for
//! the exact same pair) which always treats an int/float pairing as a type
//! change regardless of numeric value. This is implemented by
//! `crate::lcs`'s own matching-only equality
//! (`crate::lcs`'s `python_scalar_eq`, module-private), kept deliberately
//! separate from `numbers_equal` rather than generalizing the latter —
//! they are different, both intentional, rules.
//!
//! **The `new_path` finding.** `DeepDiff` renders every finding's path from
//! the *old* (`t1`) side by default, and at `verbose_level=2` additionally
//! reports a `new_path` field on `values_changed`/`type_changes` whenever
//! the *new*-side path would differ. For a dict key this never happens (a
//! key doesn't move), and neither did it for this engine's original
//! index-aligned list algorithm (which only ever compares same-index
//! pairs) — so `new_path` was unreachable before this fix. It first
//! appears when an LCS `'replace'` opcode's old-side and new-side offsets
//! have drifted apart because of an earlier insert/delete in the same list
//! (e.g. a value that moved from index `5` to index `3`); see
//! [`crate::report::ValuesChangedEntry::new_path`]'s doc.
//!
//! **`iterable_item_moved` does not need implementing.** `DeepDiff`'s
//! generic pairwise comparison has a branch for reporting a moved-but-equal
//! item as `iterable_item_moved` instead of recursing — but that branch is
//! only reachable from *within* a `'replace'` opcode's pairwise comparison,
//! and a `'replace'` opcode's ranges are proven to share no matching
//! element pair at all (not even at mismatched positions) — see
//! `crate::lcs::compute_opcodes`'s doc. So the "moved" branch can never
//! actually fire for a basic-hashable list.
//!
//! # The mutual-add-remove merge
//!
//! `DeepDiff` runs one more pass this port initially missed: after the
//! **entire** diff tree is built (not per-list — globally, once), it
//! merges any `iterable_item_added` and `iterable_item_removed` finding
//! that render to the **exact same path string** into a single
//! `values_changed` (old value from the removed side, new value from the
//! added side), purely because the path strings coincide — see
//! `crate::report::Report::merge_mutual_add_removes`'s doc for the full
//! mechanics, and why it always produces `values_changed` (never
//! `type_changes`) and never attaches `new_path`. Called once from
//! [`diff_with_max_depth`] after the whole recursive traversal completes,
//! matching `DeepDiff`'s own timing (`_get_view_results`, always active
//! since this engine has no `report_repetition` option to disable it).
//! This is *not* the array-level LCS/tie-break machinery above — it fires
//! on **any** list shape, including lists disqualified from LCS matching
//! entirely, whenever the plain index-aligned or LCS candidate happens to
//! leave a same-path add and remove both standing.
//!
//! # Internal layout
//!
//! This module is split by what a reader needs to hold in their head at
//! once, not by call order:
//!
//! - `options` — the public API surface: [`DiffOptions`], [`DEFAULT_MAX_DEPTH`],
//!   and the three entry points ([`diff()`], [`diff_with_options()`],
//!   [`diff_with_max_depth()`]).
//! - `dispatch` — the recursive traversal core: `diff_at` (the type-dispatch
//!   switch every recursion step goes through), the depth-guard invariants
//!   (`check_traversal_depth`, `check_value_depth` — see this doc's
//!   "Hardening" section above for *why* they exist), the iterative (non-recursive)
//!   `values_equal`/`deeper_than` used to check those invariants without
//!   risking the very overflow they guard against, and `scoped`, the shared
//!   push/pop path-buffer helper every container loop below uses.
//! - `scalar` — leaf-level comparison: scalar/numeric equality
//!   (`numbers_equal`, `floats_equal`, `number_as_i128`) and the
//!   `type_changes`/`values_changed` finding builders (`type_change_report`,
//!   `scalar_diff`, `numeric_diff`) `diff_at` dispatches to for a
//!   non-container pair.
//! - `array` — list (JSON array) diffing: `array_diff`'s LCS-vs-positional
//!   dispatch (see "List diffing" above) and its two
//!   candidate algorithms.
//! - `object` — dict (JSON object) diffing: `object_diff`'s key-set walk.
//!
//! `crate::ignore_order` sits beside this module, not inside it — it is
//! `array_diff`'s alternate list-comparison strategy when
//! [`DiffOptions::ignore_order`] is set, reached through the same dispatch
//! point but with its own hashing/pairing machinery (see that module's own
//! doc). The DoS/depth invariants enforced here (`check_traversal_depth`,
//! `check_value_depth`) are shared with it, not duplicated — see
//! `crate::ignore_order`'s "Depth safety" doc section.

mod array;
mod dispatch;
mod object;
mod options;
mod scalar;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use options::{DEFAULT_MAX_DEPTH, DiffOptions, diff, diff_with_max_depth, diff_with_options};

pub(crate) use array::array_diff;
pub(crate) use dispatch::{
    check_map_depth, check_traversal_depth, check_value_depth, deeper_than, diff_at, scoped,
    values_equal,
};
pub(crate) use object::object_diff;
pub(crate) use scalar::{
    numbers_equal, numeric_diff, python_type_name, scalar_diff, type_change_report,
};
