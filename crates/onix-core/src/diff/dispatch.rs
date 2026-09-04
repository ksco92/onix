//! The recursive traversal core: [`diff_at`]'s type-dispatch switch, the
//! depth-guard invariants it enforces on every step, and the shared
//! path-buffer helper ([`scoped`]) every container loop in `super::array`
//! and `super::object` uses to push/pop path segments as they recurse.
//!
//! See the parent `diff` module's doc for the full recursion-depth hardening
//! (its "Hardening" section) this file implements.

use crate::value::{Object, Value};

use crate::error::Error;
use crate::ignore_order::IgnoreOrderMemo;
use crate::path::{PathSegment, render_path};
use crate::report::Report;

use super::{
    DiffOptions, array_diff, datetime_diff, numeric_diff, object_diff, scalar_diff,
    type_change_report,
};

/// The recursive core of [`diff_with_max_depth()`]: identical dispatch, but
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
            datetime_diff(path, *old, *new, depth, opts.max_depth)
        }
        (Value::Date(old), Value::Date(new)) => {
            scalar_diff(path, old == new, a, b, depth, opts.max_depth)
        }
        (Value::Array(old), Value::Array(new)) | (Value::Tuple(old), Value::Tuple(new)) => {
            array_diff(path, old, new, depth, opts, memo)
        }
        (Value::Object(old), Value::Object(new)) => object_diff(path, old, new, depth, opts, memo),
        _ => type_change_report(path, a, b, depth, opts.max_depth),
    }
}
/// Deep structural equality of two values, used by
/// [`diff_with_options`](super::diff_with_options) for its top-level
/// "equal inputs of any depth return an empty report" fast path.
///
/// Delegates to [`Value`]'s own [`PartialEq`], which is iterative (an
/// explicit heap work-stack, no native recursion — see the `value` module's
/// "Stack safety" doc) and whose semantics are exactly this engine's: an int
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
            Value::Object(map) => stack.extend(map.values().map(|item| (item, depth + 1))),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Str(_)
            | Value::DateTime(_)
            | Value::Date(_) => {}
        }
    }

    false
}
/// Guards every place a whole value is about to be cloned into a [`Report`]:
/// returns `Err(Error::MaxDepthExceeded)` if `value`'s own nesting (see
/// [`deeper_than`]) exceeds the *remaining* depth budget at `depth`, so the
/// clone that would otherwise hand an attacker-controlled deep value to the
/// compact `Value`'s natively recursive (depth-guarded) `Clone`, or to
/// [`crate::report::Report::to_json_value`]'s recursive render, never
/// happens. (The compact `Value`'s own `Drop` is iterative, so teardown is
/// safe regardless — see the `value` module's "Stack safety" note.)
///
/// `depth` is how deep the *path* to this finding already is (the same
/// convention as [`diff_at`]'s own `depth`: root `0`, one more per dict-key
/// step) — **not** re-checked against `max_depth` on its own. Instead the
/// value is checked against `max_depth.saturating_sub(depth)`: the path
/// depth already reached and the value's own nesting share one combined
/// `max_depth` budget, because both run as native recursion on the *same*
/// call stack (the traversal to reach this point, then `.clone()`'s own
/// recursion into `value`) and so their frame counts add rather than each
/// getting an independent `max_depth`. Checking the value against a flat
/// `max_depth` regardless of `depth` was exactly the compounding bug this
/// closes: a value could be accepted at up to `max_depth` deep *in addition
/// to* however deep the traversal already was, for a combined worst case of
/// roughly `2 * max_depth` native frames at the `.clone()` call — see
/// [`diff_with_max_depth`]'s doc for the full contract this restores.
pub(crate) fn check_value_depth(
    path: &[PathSegment],
    value: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<(), Error> {
    if deeper_than(value, max_depth.saturating_sub(depth)) {
        return Err(Error::MaxDepthExceeded {
            path: render_path(path),
            max_depth,
        });
    }
    Ok(())
}
/// Like [`deeper_than`], but walks a dict's fields directly instead of
/// requiring an owned `Value::Object` wrapping them.
///
/// Delegates to [`deeper_than`] per field rather than duplicating its
/// stack-walk: each field sits at depth `1` relative to `map` itself, so a
/// field's own subtree trips `limit` exactly when that field's value
/// (counted from its own depth `0`) trips `limit - 1` — matching what
/// wrapping `map` in a `Value::Object` and calling [`deeper_than`] on that
/// would compute. `limit == 0` is the one case that can't subtract `1`
/// (`usize` has no negative range) and doesn't need to: at `limit == 0`
/// *any* field at all already sits one level too deep, regardless of what
/// it contains, so the answer is simply whether `map` is non-empty.
/// `Iterator::any` short-circuits on the first offending field, matching
/// [`deeper_than`]'s own early-return, and introduces no native recursion
/// of its own — each delegated [`deeper_than`] call is independently
/// iterative, so this composes without compounding stack usage.
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
            path: render_path(path),
            max_depth,
        });
    }
    Ok(())
}
/// The plain traversal-recursion depth guard: `Err(Error::MaxDepthExceeded)`
/// if `depth` (the depth `path` itself already sits at) exceeds
/// `max_depth`, `Ok(())` otherwise.
///
/// This is [`diff_at`]'s own top check, factored out so
/// [`insert_lcs_pair_finding`] can enforce the *exact same* bound for an
/// LCS `'replace'`-opcode pairwise comparison as `diff_at` would have
/// enforced had the pair been reached by ordinary recursion instead (which
/// is what [`positional_array_diff`]'s equivalent same-index pairs go
/// through) — see [`array_diff`]'s module-level "List diffing" doc section.
/// Deliberately distinct from [`check_value_depth`]: that function bounds a
/// *value's own nesting* combined with the remaining budget, and is a
/// structural no-op for a scalar (nesting `0`) at any depth; this function
/// bounds the *path depth itself*, regardless of what shape the value at
/// that path is.
pub(crate) fn check_traversal_depth(
    path: &[PathSegment],
    depth: usize,
    max_depth: usize,
) -> Result<(), Error> {
    if depth > max_depth {
        return Err(Error::MaxDepthExceeded {
            path: render_path(path),
            max_depth,
        });
    }
    Ok(())
}
/// Pushes `seg` onto the shared path buffer, runs `f` with that segment in
/// place, then pops it again before returning `f`'s result — regardless of
/// whether `f` succeeded or failed — restoring `path` to exactly its
/// pre-call state before the caller moves on to the next sibling.
///
/// This is the one place the "push a segment, do work with it in scope, pop
/// it again, then propagate whatever the work returned" shape used by every
/// [`object_diff`]/[`array_diff`] loop below lives: each call site's
/// closure does only the work specific to that site (recurse via
/// [`diff_at`] and merge, or [`check_value_depth`] plus a single `insert_*`
/// call) and returns a `Result` describing its own outcome; `scoped` alone
/// owns getting the push/pop pairing right, so there is exactly one place
/// that can leak a stale segment across siblings, not five or six
/// independent copies of the same pattern.
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
