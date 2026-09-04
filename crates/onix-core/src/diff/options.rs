//! The public API surface: [`DiffOptions`], [`DEFAULT_MAX_DEPTH`], and the
//! three entry points ([`diff()`], [`diff_with_options()`],
//! [`diff_with_max_depth()`]) — all thin wrappers around
//! `super::dispatch`'s recursive [`super::diff_at`], differing only in how
//! much of [`DiffOptions`] the caller controls.

use crate::value::Value;

use crate::error::Error;
use crate::report::Report;

use super::{diff_at, values_equal};

/// Default maximum recursion depth for [`diff()`].
///
/// The root pair is depth `0`; each step into a nested dict value adds one.
/// This bound only matters for *unequal* nested structures — see
/// [`diff_with_max_depth`]'s doc for the exact guarantee.
pub const DEFAULT_MAX_DEPTH: usize = 512;
/// The options a [`diff_with_options`] call runs with.
///
/// [`diff()`] and [`diff_with_max_depth()`] are unchanged, thinner
/// convenience wrappers that build one of these and delegate — see their own
/// docs. `Default` matches [`diff()`]'s own behavior: [`DEFAULT_MAX_DEPTH`],
/// ordered (non-`ignore_order`) comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOptions {
    /// The recursion-depth bound — see [`diff_with_max_depth`]'s doc for the
    /// exact contract (unchanged by `ignore_order`).
    pub max_depth: usize,
    /// Mirrors `DeepDiff(..., ignore_order=True)`: every list/tuple
    /// encountered anywhere in the tree, at any depth, is compared as a
    /// multiset-ish match (hash-based pairing) instead of the ordered
    /// index-aligned/LCS comparison — see `crate::ignore_order`'s
    /// module doc for the full, empirically-verified spec this implements.
    /// Dicts are unaffected (always key-compared); this only changes how
    /// *list-typed* values compare, recursively.
    pub ignore_order: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            ignore_order: false,
        }
    }
}
/// Diffs two JSON-shaped values and returns a DeepDiff-compatible
/// [`Report`], using [`DEFAULT_MAX_DEPTH`] as the recursion-depth bound.
///
/// # Errors
///
/// Returns [`Error::MaxDepthExceeded`] if comparing `a`/`b` would need to
/// recurse past [`DEFAULT_MAX_DEPTH`] to find a genuine difference, *or* if
/// a finding's own value (added, removed, changed, or type-changed) is
/// nested deeper than the budget remaining at that finding's path — a
/// deeper path leaves *less* room for the value, not a fresh
/// [`DEFAULT_MAX_DEPTH`] of its own — see [`diff_with_max_depth`] for the
/// exact contract and depth-counting convention.
///
/// # Examples
///
/// ```
/// use onix_core::Value;
/// use onix_core::diff::diff;
/// use serde_json::json;
///
/// let report = diff(&Value::from(json!(1)), &Value::from(json!(2))).unwrap();
/// assert!(!report.is_empty());
/// assert_eq!(
///     report.to_json_value(),
///     json!({"values_changed": {"root": {"new_value": 2, "old_value": 1}}})
/// );
/// ```
pub fn diff(a: &Value, b: &Value) -> Result<Report, Error> {
    diff_with_max_depth(a, b, DEFAULT_MAX_DEPTH)
}
/// Diffs two JSON-shaped values with a caller-chosen [`DiffOptions`] —
/// the general entry point [`diff()`] and [`diff_with_max_depth()`]
/// delegate to, unchanged themselves (both still run with
/// `ignore_order: false`).
///
/// See [`diff_with_max_depth`]'s doc for the full recursion-depth contract
/// (unaffected by `ignore_order`), and `crate::ignore_order`'s module
/// doc when `opts.ignore_order` is `true`.
///
/// # Errors
///
/// Same as [`diff_with_max_depth`]: [`Error::MaxDepthExceeded`] if
/// `opts.max_depth` would be exceeded.
///
/// # Examples
///
/// ```
/// use onix_core::Value;
/// use onix_core::diff::{DiffOptions, diff_with_options};
/// use serde_json::json;
///
/// let opts = DiffOptions {
///     ignore_order: true,
///     ..DiffOptions::default()
/// };
/// let report =
///     diff_with_options(&Value::from(json!([1, 2, 3])), &Value::from(json!([3, 2, 1])), &opts)
///         .unwrap();
/// assert!(report.is_empty());
/// ```
pub fn diff_with_options(a: &Value, b: &Value, opts: &DiffOptions) -> Result<Report, Error> {
    // The distance memo is created here, per diff invocation, and dropped
    // when this returns — no cross-call state. It only ever caches
    // `ignore_order` container-pair distances (see `crate::ignore_order`'s
    // `memo` module); for an ordered diff it is threaded but never consulted.
    diff_with_options_memo(a, b, opts, &crate::ignore_order::IgnoreOrderMemo::new())
}

/// The shared body of [`diff_with_options`], taking an explicit
/// [`crate::ignore_order::IgnoreOrderMemo`] so the decision-equivalence
/// differential test can run the exact same code path with the cache
/// disabled. Production always calls it via [`diff_with_options`] with a live
/// memo.
///
/// # Errors
///
/// Same as [`diff_with_options`].
pub(crate) fn diff_with_options_memo(
    a: &Value,
    b: &Value,
    opts: &DiffOptions,
    memo: &crate::ignore_order::IgnoreOrderMemo,
) -> Result<Report, Error> {
    if values_equal(a, b) {
        return Ok(Report::new());
    }
    let mut path = Vec::new();
    diff_at(&mut path, a, b, 0, opts, memo).map(|mut report| {
        report.merge_mutual_add_removes();
        report
    })
}
/// Diffs two JSON-shaped values like [`diff()`], but with a caller-chosen
/// recursion-depth bound instead of [`DEFAULT_MAX_DEPTH`].
///
/// # Depth-counting convention
///
/// The root pair `(a, b)` is depth `0`; stepping into a dict key — whether
/// it recurses (a shared key) or is a leaf finding (an added or removed
/// key) — adds one to the depth of that key's value.
///
/// **A finding's path depth and its value's own nesting share one combined
/// `max_depth` budget — they do not each get an independent `max_depth`.**
/// Precisely: at a finding whose path is at depth `d` (`0` for a
/// root-level finding), the value(s) cloned into that finding — measured
/// **standalone**, i.e. treating each value as if it were its own root
/// (depth `0`), the same convention as above — must have their own nesting
/// no greater than `max_depth - d`. A value of exactly `max_depth - d` is
/// accepted; one level more is rejected with [`Error::MaxDepthExceeded`] at
/// the finding's path. A root-level finding (`d = 0`) gets the full
/// `max_depth` budget for its value; a finding only reached after
/// recursing deep into the structure gets proportionally less. Likewise, a
/// structure whose deepest *reported* difference is only reached by
/// recursing to path depth exactly `max_depth` still diffs successfully
/// (that finding's value then has a budget of `0`, i.e. it must be a
/// scalar or an empty container); recursing one level deeper than that to
/// find *any* difference returns [`Error::MaxDepthExceeded`] with the path
/// at which the bound tripped.
///
/// This shared-budget rule is not an implementation quirk — it is the fix
/// for a real, reproduced vulnerability. The traversal that *reaches* a
/// finding and the native `Clone` that *records* its value run as native
/// recursion on the *same* call stack, one after the other with no
/// unwinding in between, so their frame counts **add**, they do not each
/// get their own independent allowance. Giving the value a full,
/// position-independent `max_depth` of its own (as an earlier version of
/// this guard did) let a deep traversal plus a deep value at the bottom
/// together demand roughly `2 * max_depth` native frames at the `.clone()`
/// call — safe at a small default, but a caller-raised `max_depth` (this
/// function's whole reason to exist) could still overflow the stack.
/// Splitting one flat budget between the two instead is what keeps combined
/// native stack usage bounded by `max_depth`, never `2 * max_depth`.
///
/// # Equal-inputs-of-any-depth guarantee
///
/// Before recursing at all, `a` and `b` (the whole inputs) are compared with
/// an internal iterative, heap-stack-based deep-equality check with no
/// native recursion. **Two fully-equal inputs of *any* nesting depth always
/// return an empty [`Report`], regardless of `max_depth`.** This check runs
/// once, at the top; it does *not* re-run per dict key while recursing (a
/// deliberate simplicity/perf trade-off — see the `diff.rs` module and
/// `object_diff`'s doc for why), so an equal subtree nested arbitrarily deep
/// *underneath an unrelated, shallower difference elsewhere* can still trip
/// [`Error::MaxDepthExceeded`] even though that particular subtree would
/// resolve to no findings on its own.
///
/// # Safety contract: no native recursion exceeds `O(max_depth)` frames
///
/// This is what actually makes the engine *safe* to run on untrusted,
/// adversarially-deep input, and the shared-budget rule above is exactly
/// what makes it *true*: the traversal consumes up to `d` native frames to
/// reach a finding, and recording that finding's value then consumes up to
/// `max_depth - d` more — a combined worst case of `max_depth` frames,
/// never more, no matter how the total splits between path depth and value
/// depth. No value whose combined (path depth + own nesting) exceeds
/// `max_depth` is ever cloned into a [`Report`]. Together, no native
/// recursion anywhere in `diff` itself, in a returned [`Report`]'s `Drop`,
/// or in [`Report::to_json_value`] can ever exceed `O(max_depth)` stack
/// frames (each traversal level costs a small constant number of native
/// frames — `diff_at` plus `object_diff` or `array_diff`, so roughly
/// `2 * max_depth` in the worst case — and recording a finding's value adds
/// no further compounding beyond that same linear cost): the worst case is
/// always a clean, catchable [`Error::MaxDepthExceeded`] — never a stack
/// overflow.
///
/// This practical depth limit is a property of the recursive engine: an
/// iterative work-stack rewrite would remove it (and the
/// nested-equal-subtree edge case above) entirely.
///
/// # Errors
///
/// Returns [`Error::MaxDepthExceeded`] if either the traversal or the
/// combined path-depth-plus-value-depth budget above is exceeded.
///
/// # Examples
///
/// ```
/// use onix_core::Value;
/// use onix_core::diff::{DEFAULT_MAX_DEPTH, diff_with_max_depth};
/// use serde_json::json;
///
/// // A tiny bound is enough for a shallow diff.
/// let report =
///     diff_with_max_depth(&Value::from(json!({"a": 1})), &Value::from(json!({"a": 2})), 3).unwrap();
/// assert!(!report.is_empty());
///
/// // Equal inputs never hit the bound, no matter how deep.
/// let deep = Value::from(json!({"a": {"b": {"c": {"d": {"e": 1}}}}}));
/// let report = diff_with_max_depth(&deep, &deep, 1).unwrap();
/// assert!(report.is_empty());
/// # let _ = DEFAULT_MAX_DEPTH;
/// ```
///
/// After the recursive traversal completes, runs the whole-tree
/// mutual-add-remove merge exactly once (see this module's "The
/// mutual-add-remove merge" doc section and
/// `crate::report::Report::merge_mutual_add_removes`) — matching
/// `DeepDiff`'s own once-per-call, post-traversal timing.
pub fn diff_with_max_depth(a: &Value, b: &Value, max_depth: usize) -> Result<Report, Error> {
    diff_with_options(
        a,
        b,
        &DiffOptions {
            max_depth,
            ignore_order: false,
        },
    )
}
