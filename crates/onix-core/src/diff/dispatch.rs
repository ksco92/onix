//! The recursive traversal core: [`diff_at`]'s type-dispatch switch, the
//! depth-guard invariants it enforces on every step, and the shared
//! path-buffer helper ([`scoped`]) every container loop in `super::array`
//! and `super::object` uses to push/pop path segments as they recurse.
//! See `docs/design/depth-budget.md` for the depth-guard proof.

use crate::value::{Object, ObjectKind, Value, same_class};

use crate::error::Error;
use crate::ignore_order::IgnoreOrderMemo;
use crate::path::{PathSegment, render_path};
use crate::report::Report;

use super::{
    DiffOptions, array_diff, datetime_diff, numeric_diff, object_diff, scalar_diff, set_diff,
    type_change_report,
};

/// The recursive core of [`diff_with_max_depth()`](super::diff_with_max_depth): identical dispatch, but
/// carrying the path and depth accumulated so far, so that nested findings
/// get their full deep path and the recursion-depth bound can be enforced.
///
/// `path` is a single buffer *shared* across the whole traversal, not a
/// fresh copy per call: [`object_diff`]/[`array_diff`] push the child
/// segment before recursing one level deeper and pop it again immediately
/// after (see their docs), so a traversal to depth `D` allocates each path
/// segment once, not once per level it is copied through — `O(D)` total
/// rather than the `O(D²)` a naive "clone the whole path at every step"
/// approach costs. Every read of the path (rendering it, or measuring a
/// found value's depth budget against it) takes a `&[PathSegment]` slice
/// view of the buffer *at that point in the traversal*, which is exactly
/// the path to the current call — the mutable buffer and the immutable path
/// it represents are the same data, just viewed at different moments.
pub(crate) fn diff_at(
    path: &mut Vec<PathSegment>,
    a: &Value,
    b: &Value,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    check_traversal_depth(path, depth, opts.max_depth)?;

    match (a, b) {
        (Value::Null, Value::Null) => Ok(Report::new()),
        (Value::Bool(old), Value::Bool(new)) => {
            scalar_diff(path, old == new, a, b, depth, opts.max_depth)
        }
        (Value::Number(old), Value::Number(new)) => {
            numeric_diff(path, old, new, a, b, depth, opts.max_depth)
        }
        (Value::Str(old), Value::Str(new)) => {
            scalar_diff(path, old == new, a, b, depth, opts.max_depth)
        }
        (Value::DateTime(old), Value::DateTime(new)) => {
            if same_class(a, b) {
                datetime_diff(path, old.value(), new.value(), depth, opts.max_depth)
            } else {
                type_change_report(path, a, b, depth, opts.max_depth)
            }
        }
        (Value::Date(old), Value::Date(new)) => {
            if same_class(a, b) {
                scalar_diff(path, old == new, a, b, depth, opts.max_depth)
            } else {
                type_change_report(path, a, b, depth, opts.max_depth)
            }
        }
        (Value::Time(old), Value::Time(new)) => {
            if same_class(a, b) {
                // Plain `_diff_time` equality — no normalization step,
                // unlike `datetime_diff` (see `docs/design/value-model.md`).
                scalar_diff(
                    path,
                    crate::datetime::times_equal(old.value(), new.value()),
                    a,
                    b,
                    depth,
                    opts.max_depth,
                )
            } else {
                type_change_report(path, a, b, depth, opts.max_depth)
            }
        }
        (Value::TimeDelta(old), Value::TimeDelta(new)) => {
            if same_class(a, b) {
                scalar_diff(path, old == new, a, b, depth, opts.max_depth)
            } else {
                type_change_report(path, a, b, depth, opts.max_depth)
            }
        }
        (Value::Array(old), Value::Array(new)) | (Value::Tuple(old), Value::Tuple(new)) => {
            if same_class(a, b) {
                array_diff(path, old, new, depth, opts, memo)
            } else {
                type_change_report(path, a, b, depth, opts.max_depth)
            }
        }
        (Value::Set(old), Value::Set(new)) | (Value::FrozenSet(old), Value::FrozenSet(new)) => {
            if same_class(a, b) {
                set_diff(path, old, new, depth, opts, memo)
            } else {
                type_change_report(path, a, b, depth, opts.max_depth)
            }
        }
        (Value::Object(old), Value::Object(new))
            if old.kind() == ObjectKind::Dict && new.kind() == ObjectKind::Dict =>
        {
            if same_class(a, b) {
                object_diff(path, old, new, depth, opts, memo)
            } else {
                type_change_report(path, a, b, depth, opts.max_depth)
            }
        }
        (Value::Object(_), _) | (_, Value::Object(_)) => {
            object_pair_diff(path, a, b, depth, opts, memo)
        }
        _ => type_change_report(path, a, b, depth, opts.max_depth),
    }
}

/// [`diff_at`] for a pair with a custom object or token on either side, kept
/// off its frame: nothing for a pair [`IgnoreOrderMemo::skips`], a resolved
/// token's value through [`diff_at`] again, a finding carrying a failed
/// object's token where a walk meets it, else the object walk or a
/// `type_changes`.
#[inline(never)]
fn object_pair_diff(
    path: &mut Vec<PathSegment>,
    a: &Value,
    b: &Value,
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    if IgnoreOrderMemo::skips(a, b) {
        return Ok(Report::new());
    }
    let resolved_a = memo.resolve(a, b, false);
    let resolved_b = memo.resolve(b, resolved_a.as_deref().unwrap_or(a), false);
    if resolved_a.is_some() || resolved_b.is_some() {
        let a = resolved_a.as_deref().unwrap_or(a);
        let b = resolved_b.as_deref().unwrap_or(b);
        return diff_at(path, a, b, depth, opts, memo);
    }
    match (a, b) {
        (Value::Object(old), Value::Object(new)) if same_class(a, b) => {
            match (old.failure_token(), new.failure_token()) {
                (None, None) => object_diff(path, old, new, depth, opts, memo),
                (old_token, new_token) => scalar_diff(
                    path,
                    false,
                    old_token.as_ref().unwrap_or(a),
                    new_token.as_ref().unwrap_or(b),
                    depth,
                    opts.max_depth,
                ),
            }
        }
        _ => type_change_report(path, a, b, depth, opts.max_depth),
    }
}
/// Deep structural equality of two values, used by
/// [`diff_with_options`](super::diff_with_options) for its top-level
/// "equal inputs of any depth return an empty report" fast path.
///
/// Delegates to [`Value`]'s own [`PartialEq`], which is iterative (an
/// explicit heap work-stack, no native recursion — see
/// `docs/design/value-model.md`) and whose semantics are exactly this engine's: an int
/// and a float are never equal, ints compare by value, floats by exact
/// IEEE-754 `==`, objects by key set plus per-key values, arrays by length
/// plus per-index values. Because every value the engine sees comes from
/// [`Value`]'s canonical construction (`From`/`Deserialize`), a given
/// integer has exactly one representation, so `PartialEq`'s
/// variant-sensitive `Number` comparison and the
/// separately-maintained [`numbers_equal`](super::numbers_equal) walk agree
/// on every reachable input.
#[must_use]
pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    a == b
}
/// Returns `true` if `value`'s own internal nesting exceeds `limit`,
/// treating `value` as if it were its own root (depth `0`) — independent of
/// whatever path depth it may be found at within a diff.
///
/// A scalar (null/bool/number/string) is depth `0`; a non-empty
/// array/object is `1 + max(depth of its elements/values)` (`0` if empty) —
/// the same root-is-depth-`0` convention used throughout this module.
///
/// Iterative (an explicit heap-allocated work-stack, no native recursion),
/// so this cannot itself overflow the very thing it exists to guard
/// against. It exits as soon as one node's depth exceeds `limit`, without
/// visiting the rest of `value`; when it does *not* trip, it visits every
/// node of `value` once (`O(nodes)`). As with [`values_equal`], pushing a
/// whole container's children at once means peak heap usage tracks input
/// size, not depth alone — again an acceptable trade for eliminating native
/// stack recursion.
pub(crate) fn deeper_than(value: &Value, limit: usize) -> bool {
    let mut stack: Vec<(&Value, usize)> = vec![(value, 0)];

    while let Some((v, depth)) = stack.pop() {
        if depth > limit {
            return true;
        }
        match v {
            Value::Array(items) | Value::Tuple(items) => {
                stack.extend(items.iter().map(|item| (item, depth + 1)));
            }
            Value::Set(items) | Value::FrozenSet(items) => {
                stack.extend(items.iter().map(|item| (item, depth + 1)));
            }
            Value::Object(map) => stack.extend(map.values().map(|item| (item, depth + 1))),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Str(_)
            | Value::DateTime(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::TimeDelta(_) => {}
        }
    }

    false
}
/// Rejects `value` (see [`deeper_than`]) if its nesting exceeds the
/// budget remaining at `depth` (`max_depth.saturating_sub(depth)`), not
/// a flat `max_depth` — shared with path depth (`docs/design/depth-budget.md`).
pub(crate) fn check_value_depth(
    path: &[PathSegment],
    value: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<(), Error> {
    if deeper_than(value, max_depth.saturating_sub(depth)) {
        return Err(Error::MaxDepthExceeded {
            path: render_path(path).to_string(),
            max_depth,
        });
    }
    Ok(())
}
/// Like [`deeper_than`], but walks a dict's fields directly. `limit`
/// is the caller's already-reduced remaining budget, shared between
/// path depth and value depth (`docs/design/depth-budget.md`).
pub(crate) fn map_deeper_than(map: &Object, limit: usize) -> bool {
    if limit == 0 {
        !map.is_empty()
    } else {
        map.values().any(|value| deeper_than(value, limit - 1))
    }
}
/// [`check_value_depth`]'s twin for a dict that hasn't been cloned into a
/// `Value` yet: checks `map`'s own nesting directly ([`map_deeper_than`])
/// so a caller that is *deciding whether to clone* an untrusted dict — like
/// [`object_diff`]'s `threshold_to_diff_deeper` collapse, which would
/// otherwise clone the whole dict into a finding before any depth check
/// could reject it — can check first and only pay for the clone once this
/// passes.
pub(crate) fn check_map_depth(
    path: &[PathSegment],
    map: &Object,
    depth: usize,
    max_depth: usize,
) -> Result<(), Error> {
    if map_deeper_than(map, max_depth.saturating_sub(depth)) {
        return Err(Error::MaxDepthExceeded {
            path: render_path(path).to_string(),
            max_depth,
        });
    }
    Ok(())
}
/// Rejects if the path depth itself (`depth`) exceeds `max_depth`;
/// [`check_value_depth`] enforces the other half of this same shared
/// budget (`docs/design/depth-budget.md`).
pub(crate) fn check_traversal_depth(
    path: &[PathSegment],
    depth: usize,
    max_depth: usize,
) -> Result<(), Error> {
    if depth > max_depth {
        return Err(Error::MaxDepthExceeded {
            path: render_path(path).to_string(),
            max_depth,
        });
    }
    Ok(())
}
/// Pushes `seg`, runs `f`, then pops it again — even on failure —
/// restoring `path` for the next sibling; `path.len()` is what every
/// depth check measures against the shared budget (`docs/design/depth-budget.md`).
pub(crate) fn scoped<T>(
    path: &mut Vec<PathSegment>,
    seg: PathSegment,
    f: impl FnOnce(&mut Vec<PathSegment>) -> T,
) -> T {
    path.push(seg);
    let result = f(path);
    path.pop();
    result
}
