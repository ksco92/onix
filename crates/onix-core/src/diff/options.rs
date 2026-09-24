//! The public API surface: [`DiffOptions`], [`DEFAULT_MAX_DEPTH`], and the
//! four entry points ([`diff()`], [`diff_with_options()`],
//! [`diff_with_max_depth()`], [`diff_with_resolver()`]) — all thin wrappers
//! around `super::dispatch`'s recursive [`super::diff_at`], differing in how
//! much of [`DiffOptions`] the caller controls and whether tokens resolve.

use crate::value::Value;

use crate::error::Error;
use crate::report::Report;

use super::{diff_at, values_equal};

/// Default maximum recursion depth for [`diff()`].
///
/// See `docs/design/depth-budget.md` for the depth-counting convention
/// and the exact guarantee this bound gives unequal nested structures.
pub const DEFAULT_MAX_DEPTH: usize = 512;
/// The options a [`diff_with_options`] call runs with.
///
/// [`diff()`] and [`diff_with_max_depth()`] are unchanged, thinner
/// convenience wrappers that build one of these and delegate — see their own
/// docs. `Default` matches [`diff()`]'s own behavior: [`DEFAULT_MAX_DEPTH`],
/// ordered (non-`ignore_order`) comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOptions {
    /// The recursion-depth bound (unchanged by `ignore_order`) — see
    /// `docs/design/depth-budget.md` for the exact contract.
    pub max_depth: usize,
    /// Mirrors `DeepDiff(..., ignore_order=True)`: every list/tuple
    /// encountered anywhere in the tree, at any depth, is compared as a
    /// multiset-ish match (hash-based pairing) instead of the ordered
    /// index-aligned/LCS comparison — see `docs/design/ignore-order.md`
    /// for the full spec this implements.
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
/// recurse past [`DEFAULT_MAX_DEPTH`], per the shared depth budget in
/// `docs/design/depth-budget.md`.
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
/// See `docs/design/depth-budget.md` for the recursion-depth contract
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

/// A value the diff compares in place of a token: shared with the caller, or
/// borrowed from a value that outlives the diff.
#[derive(Clone)]
pub enum Resolution<'r> {
    /// A value the caller converted for the token.
    Shared(std::sync::Arc<Value>),
    /// A value the caller already holds, such as the object a cycle token
    /// points back at.
    Borrowed(&'r Value),
}

impl std::ops::Deref for Resolution<'_> {
    type Target = Value;

    fn deref(&self) -> &Value {
        match self {
            Resolution::Shared(value) => value,
            Resolution::Borrowed(value) => value,
        }
    }
}

/// What the diff calls with a token's identity the first time it compares the
/// token with anything but itself; `None` leaves the token as it is.
pub type Resolver<'r> = dyn FnMut(&str) -> Option<Resolution<'r>> + Send + 'r;

/// [`diff_with_options`], comparing each token `resolver` resolves as its
/// value. A cycle token is compared that way only against an object of the
/// class it points back at, and otherwise stays a token.
///
/// # Errors
///
/// Same as [`diff_with_options`].
pub fn diff_with_resolver<'r>(
    a: &Value,
    b: &Value,
    opts: &DiffOptions,
    resolver: &'r mut Resolver<'r>,
) -> Result<Report, Error> {
    diff_with_options_memo(
        a,
        b,
        opts,
        &crate::ignore_order::IgnoreOrderMemo::with_resolver(resolver),
    )
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
/// recursion-depth bound instead of [`DEFAULT_MAX_DEPTH`]. See
/// `docs/design/depth-budget.md` for the depth-counting convention and
/// the shared path-plus-value budget this enforces.
///
/// # Errors
///
/// Returns [`Error::MaxDepthExceeded`] if either the traversal or the
/// combined path-depth-plus-value-depth budget is exceeded.
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
