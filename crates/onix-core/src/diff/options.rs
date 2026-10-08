//! The public API surface: [`DiffOptions`], [`DEFAULT_MAX_DEPTH`], and the
//! four entry points ([`diff()`], [`diff_with_options()`],
//! [`diff_with_max_depth()`], [`diff_with_resolver()`]) — all thin wrappers
//! around `super::dispatch`'s recursive [`super::diff_at`], differing in how
//! much of [`DiffOptions`] the caller controls and whether tokens resolve.

use crate::value::Value;

use crate::error::Error;
use crate::report::Report;

use super::{diff_at, values_equal};

/// Default recursion-depth bound for [`diff()`]; it caps unequal nesting.
/// See `docs/design/depth-budget.md`, "Depth and value budget".
pub const DEFAULT_MAX_DEPTH: usize = 512;
/// The options a [`diff_with_options`] call runs with; `Default` matches
/// [`diff()`]: [`DEFAULT_MAX_DEPTH`], ordered comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOptions {
    /// The recursion-depth bound, independent of `ignore_order`; see
    /// `docs/design/depth-budget.md`.
    pub max_depth: usize,
    /// Mirrors `DeepDiff(..., ignore_order=True)`: lists and tuples at any
    /// depth pair by hash instead of by index; dicts are unaffected. See
    /// `docs/design/ignore-order.md`.
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
/// Diffs two JSON-shaped values into a [`Report`], bounding recursion at
/// [`DEFAULT_MAX_DEPTH`].
///
/// # Errors
///
/// Same as [`diff_with_max_depth`] at [`DEFAULT_MAX_DEPTH`].
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
/// Diffs two JSON-shaped values with a caller-chosen [`DiffOptions`]; see
/// `crate::ignore_order` when `opts.ignore_order` is `true`.
///
/// # Errors
///
/// Same as [`diff_with_max_depth`] at `opts.max_depth`.
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
    // A fresh memo per call: no state survives across diffs.
    diff_with_options_memo(a, b, opts, &crate::ignore_order::IgnoreOrderMemo::new())
}

/// A value compared in place of a token: shared, or borrowed from a value
/// that outlives the diff.
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

/// The body of [`diff_with_options`], taking an explicit
/// [`crate::ignore_order::IgnoreOrderMemo`].
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
/// Diffs like [`diff()`] with a caller-chosen recursion-depth bound; path
/// depth plus finding-value depth share it. See `docs/design/depth-budget.md`,
/// "Depth and value budget".
///
/// # Errors
///
/// [`Error::MaxDepthExceeded`] when the bound is exceeded;
/// [`Error::DateTimeOutOfRange`] for compared datetimes with no UTC form.
///
/// # Examples
///
/// ```
/// use onix_core::Value;
/// use onix_core::diff::diff_with_max_depth;
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
