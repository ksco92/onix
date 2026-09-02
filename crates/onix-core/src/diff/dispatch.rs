//! The recursive traversal core: [`diff_at`]'s type-dispatch switch, the
//! depth-guard invariants it enforces on every step, and the shared
//! path-buffer helper ([`scoped`]) every container loop in `super::array`
//! and `super::object` uses to push/pop path segments as they recurse.
//!
//! See the parent `diff` module's doc for the full recursion-depth hardening
//! story (the "M3-pre" section) this file implements.

use serde_json::{Map, Value};

use crate::error::Error;
use crate::path::{PathSegment, render_path};
use crate::report::Report;

use super::{
    DiffOptions, array_diff, numbers_equal, numeric_diff, object_diff, scalar_diff,
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
        (Value::String(old), Value::String(new)) => {
            scalar_diff(path, old == new, a, b, depth, opts.max_depth)
        }
        (Value::Array(old), Value::Array(new)) => array_diff(path, old, new, depth, opts),
        (Value::Object(old), Value::Object(new)) => object_diff(path, old, new, depth, opts),
        _ => type_change_report(path, a, b, depth, opts.max_depth),
    }
}
/// Iteratively (no native recursion) checks whether two JSON values are
/// deeply equal, using an explicit heap-allocated work-stack instead of the
/// call stack. This exists so container equality can be checked on
/// arbitrarily deep input without risking a stack overflow — unlike
/// `serde_json::Value`'s derived `PartialEq`, which recurses natively and
/// has no depth bound at all.
///
/// Equality semantics deliberately match the diff engine's own, not
/// `serde_json`'s derived `PartialEq`, for numbers: two numbers are equal
/// only if they are the same "kind" (both ints or both floats — an int and a
/// float holding the same numeric value are never equal here, matching
/// `DeepDiff` always reporting that as a `type_changes`) and, within a kind,
/// numerically equal (ints compare by value across `i64`/`u64`
/// representations; floats compare by exact IEEE-754 `==`). This shares a
/// single internal `numbers_equal` helper with the recursive engine's own
/// scalar comparison, so there is exactly one place numeric-equality rules
/// live.
///
/// Objects compare by identical key sets plus equal values per shared key
/// (key order does not matter); arrays compare by equal length plus equal
/// values per index.
///
/// Native-stack safety is the only guarantee this makes: pushing every
/// element/value pair of a container onto the work-stack in one go means
/// peak heap usage is bounded by *input size* (roughly width × depth for a
/// wide-and-deep adversarial shape), not by nesting depth alone. That is an
/// acceptable, deliberate trade — the goal is eliminating native-stack
/// overflow (an uncatchable process abort), not bounding total memory (an
/// ordinary, catchable allocation failure).
#[must_use]
pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    let mut stack: Vec<(&Value, &Value)> = vec![(a, b)];

    while let Some((x, y)) = stack.pop() {
        match (x, y) {
            (Value::Null, Value::Null) => {}
            (Value::Bool(x), Value::Bool(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::Number(x), Value::Number(y)) => {
                if !numbers_equal(x, y) {
                    return false;
                }
            }
            (Value::String(x), Value::String(y)) => {
                if x != y {
                    return false;
                }
            }
            (Value::Array(x), Value::Array(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                stack.extend(x.iter().zip(y.iter()));
            }
            (Value::Object(x), Value::Object(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                for (key, x_value) in x {
                    match y.get(key) {
                        Some(y_value) => stack.push((x_value, y_value)),
                        None => return false,
                    }
                }
            }
            _ => return false,
        }
    }

    true
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
            Value::Array(items) => stack.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(map) => stack.extend(map.values().map(|item| (item, depth + 1))),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    false
}
/// Guards every place a whole value is about to be cloned into a [`Report`]:
/// returns `Err(Error::MaxDepthExceeded)` if `value`'s own nesting (see
/// [`deeper_than`]) exceeds the *remaining* depth budget at `depth`, so the
/// clone that would otherwise hand an attacker-controlled deep value to
/// `serde_json`'s natively recursive `Clone`/`Drop`/serialization never
/// happens.
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
/// requiring an owned `Value::Object` wrapping them — every field starts at
/// depth `1`, exactly matching what wrapping `map` in a `Value::Object` and
/// calling [`deeper_than`] on that would compute (an empty map is nesting
/// `0` either way, since there are no fields to push).
pub(crate) fn map_deeper_than(map: &Map<String, Value>, limit: usize) -> bool {
    let mut stack: Vec<(&Value, usize)> = map.values().map(|value| (value, 1)).collect();

    while let Some((v, depth)) = stack.pop() {
        if depth > limit {
            return true;
        }
        match v {
            Value::Array(items) => stack.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(map) => stack.extend(map.values().map(|item| (item, depth + 1))),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    false
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
    map: &Map<String, Value>,
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
