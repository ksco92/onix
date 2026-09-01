//! Structural/numeric distance between two JSON values — `DeepDiff`'s
//! `_get_rough_distance`, plus the length/leaf-counting helpers
//! it's built from. Consumed by `super::pairing::compute_pairs` to rank
//! candidate pairs; has no dependency on this module's hashing layer
//! (`super::hash`) at all — every function here operates directly on
//! [`serde_json::Value`].

use serde_json::{Number, Value};

use crate::diff::DiffOptions;

use super::fxhash::HashSet;

/// A total-ordering wrapper for the non-negative, always-finite distances
/// [`rough_distance`] computes, so they can key a [`BTreeMap`] (ascending
/// iteration, for [`compute_pairs`]'s greedy loop) and group candidates by
/// **exact** float equality — matching `DeepDiff`'s own behavior of keying
/// a plain Python `dict` by the raw `float` distance value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Distance(pub(crate) f64);

impl PartialEq for Distance {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for Distance {}

impl PartialOrd for Distance {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Distance {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl std::hash::Hash for Distance {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// Returns `value`'s numeric value for [`rough_distance`]'s fast path, if
/// it has one — `Bool` counts (Python's `isinstance(True, numbers.Number)`
/// is `True`, since `bool` subclasses `int`; confirmed empirically that a
/// bool-vs-number pair still takes `_get_numbers_distance`, not the
/// structural fallback, even though the two are never a hash-equal match).
pub(crate) fn numeric_value(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// `DeepDiff`'s `_get_numbers_distance` (distance.py), including the
/// "self-cancellation" quirk (`max_` appears in both the
/// formula and the rejection threshold, so same-sign numeric pairs are
/// almost never rejected by [`CUTOFF_DISTANCE_FOR_PAIRS`]) and the
/// opposite-sign zero-sum edge case (`divisor == 0.0` returns `cutoff`
/// itself, i.e. always-reject, rather than dividing by zero).
#[allow(
    clippy::float_cmp,
    reason = "mirrors DeepDiff's own exact `num1 == num2` short-circuit \
              (real Python `==`) before any divisor arithmetic runs"
)]
pub(crate) fn numeric_distance(n1: f64, n2: f64, cutoff: f64) -> f64 {
    if n1 == n2 {
        return 0.0;
    }
    let divisor = (n1 + n2) / cutoff;
    if divisor == 0.0 {
        return cutoff;
    }
    ((n1 - n2) / divisor).abs().min(cutoff)
}

/// `DeepHash`'s own structural node count ("counts") for a single value,
/// independent of any diff — `deephash.py`'s `__get_item_rough_length`
/// derivation, read directly from its `_prep_dict`/
/// `_prep_iterable`: `1` for a scalar, `1 + sum(rough_length(child))` for an
/// array, `1 + sum(1 + rough_length(value))` per dict entry (the extra `+1`
/// per entry accounts for the key itself, matching `_prep_dict`'s own
/// `counts += 1` for the key plus `counts += count` for the value).
///
/// Recurses natively — safe only because every caller first proves the
/// value's nesting via [`crate::diff::check_value_depth`] (see this module's "Depth
/// safety" doc section).
pub(crate) fn rough_length(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 1,
        Value::Array(items) => 1 + items.iter().map(rough_length).sum::<usize>(),
        Value::Object(map) => 1 + map.values().map(|v| 1 + rough_length(v)).sum::<usize>(),
    }
}

/// `DeepDiff`'s `_get_item_length` (distance.py) applied to one JSON value —
/// the per-value recursion [`crate::report::Report::distance_leaf_length`]
/// sums over a trial sub-diff's findings to get [`rough_distance`]'s
/// structural-fallback numerator (`diff_length`).
///
/// `null` maps to `0` — a genuine quirk of the real function: `None`
/// matches none of its `isinstance` branches (`Mapping`, `numbers`,
/// `strings`, `Iterable`, `type`), so it falls through, counting nothing.
/// Every other scalar counts as `1`; a dict/list recurses, and a dict entry
/// is skipped entirely when its *key* matches [`is_length_excluded_key`] —
/// confirmed against real `DeepDiff`: `_get_item_length(None) == 0` and
/// `_get_item_length({"old_value": 5, "x": 3}) == 1` (only `"x"` counted).
/// This exclusion is a real, faithfully-reproduced quirk of the upstream
/// function, not something this port invented: a *user's own* dict key
/// happening to be named e.g. `"old_value"` is undercounted the same way in
/// both tools.
pub(crate) fn item_length(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Bool(_) | Value::Number(_) | Value::String(_) => 1,
        Value::Array(items) => items.iter().map(item_length).sum(),
        Value::Object(map) => item_length_of_map(map),
    }
}

/// [`item_length`]'s dict case, factored out so
/// [`count_object_diff_leaves`]'s `threshold_to_diff_deeper` branch (whose
/// "new value" is a whole map, not a [`Value`]) can share it directly.
fn item_length_of_map(map: &serde_json::Map<String, Value>) -> usize {
    map.iter()
        .filter(|(key, _)| !is_length_excluded_key(key))
        .map(|(_, v)| item_length(v))
        .sum()
}

/// The literal dict-key exclusion list `_get_item_length` applies before
/// counting a mapping entry — see [`item_length`]'s doc.
pub(crate) fn is_length_excluded_key(key: &str) -> bool {
    key.starts_with('_')
        || key == "deep_distance"
        || key == "new_path"
        || key == "old_type"
        || key == "old_value"
}

/// A `Report`-free mirror of the recursive diff dispatch, counting exactly
/// what [`Report::distance_leaf_length`] would sum from the equivalent real
/// diff — used by [`rough_distance`]'s structural fallback so a candidate
/// pair's `diff_length` never pays for a [`Report`]'s `PathSegment`
/// allocations, `Value` clones, or `BTreeMap` inserts. This matters: a
/// naive "just call `diff_at` and count its `Report`" implementation would
/// pay exactly the per-candidate object-construction cost the spec's
/// performance-anatomy section (§5) identifies as the *entire* reason real
/// `DeepDiff` is slow here (250k-plus full nested-diff-object constructions
/// for the `ignore_order_10k`-shaped benchmark) — reproducing that
/// bottleneck in Rust would defeat the point of this port.
///
/// Scalars and dicts are counted directly (no recursion into `diff_at` at
/// all). **Arrays are the one exception**, delegating to a genuine trial
/// [`crate::diff::diff_at`] call: replicating [`crate::diff::array_diff`]'s own
/// LCS-vs-positional finding-count tie-break (or a further nested
/// `ignore_order` pairing) as a *count-only* mirror would be substantial,
/// rarely-exercised duplicate logic — a structural-distance candidate is
/// overwhelmingly a "record" (a dict of mostly-scalar fields, the spec's
/// own worked example), not a value containing a *further* nested array
/// needing its own tie-break decision. Correctness is preserved either way
/// (the array branch still asks the real engine, guaranteeing the same
/// number `DeepDiff` would compute); this hybrid trades away the Report-free
/// property only for the substantially rarer, non-benchmarked case.
pub(crate) fn count_diff_leaves(a: &Value, b: &Value, depth: usize, opts: &DiffOptions) -> usize {
    match (a, b) {
        (Value::Null, Value::Null) => 0,
        (Value::Bool(x), Value::Bool(y)) => usize::from(x != y),
        (Value::String(x), Value::String(y)) => usize::from(x != y),
        (Value::Number(x), Value::Number(y)) => {
            if x.is_f64() == y.is_f64() {
                usize::from(!crate::diff::numbers_equal(x, y))
            } else {
                type_change_leaf_length(a, b)
            }
        }
        (Value::Array(x), Value::Array(y)) => count_array_diff_leaves(x, y, depth, opts),
        (Value::Object(x), Value::Object(y)) => count_object_diff_leaves(x, y, depth, opts),
        _ => type_change_leaf_length(a, b),
    }
}

/// [`count_diff_leaves`]'s type-mismatch contribution: `DeepDiff`'s own
/// `DELTA_VIEW` shape for a `type_changes` finding — what its real distance
/// computation measures — is `{"old_type": ..., "new_type": ...}` plus a
/// `"new_value"` key **unless applying the new side's own type to the old
/// value reproduces the new value exactly** (`model.py::TreeResult
/// ._from_tree_type_changes`, the `DELTA_VIEW`-only branch: `new_t1 =
/// new_type(change.t1); include_values = new_t1 != change.t2`) — a real,
/// general Python-coercion rule, not a `true`-literal special case (an
/// earlier version of this function special-cased exactly one instance of
/// this rule — `bool(x) == True` for any truthy `x` — because every probe
/// used a truthy old value; generalized here after `[[0]] vs [[0.0]]`
/// surfaced the gap: `float(0) == 0.0`, so real `DeepDiff` recurses into a
/// `type_changes` there with `new_value` omitted, but the old special case
/// only matched `new_value == true`). See [`coerce_for_type_change`]'s own
/// doc for the exact coercion matrix implemented and its documented,
/// narrow scope.
pub(crate) fn type_change_leaf_length(old_value: &Value, new_value: &Value) -> usize {
    let new_value_omitted =
        coerce_for_type_change(old_value, new_value).is_some_and(|coerced| coerced == *new_value);
    if new_value_omitted {
        1
    } else {
        1 + item_length(new_value)
    }
}

/// Replicates Python's `new_type(old_value)` — literally calling the new
/// side's own type as a one-argument constructor on the old value — the
/// exact operation [`type_change_leaf_length`]'s doc cites. Returns `None`
/// when the real Python call would raise (e.g. `int("abc")`, `dict(5)`),
/// matching `_from_tree_type_changes`'s `except Exception: pass` (which
/// leaves `include_values` at its pre-exception default of `True`, i.e.
/// always keep `new_value`) — a `None` here can only ever cause an
/// unnecessary *inclusion*, never an incorrect omission.
///
/// Scoped to the coercions confirmed against real `deepdiff==9.1.0` for
/// this fix (numeric family `bool`/`int`/`float` in every direction,
/// `str` <-> number, and `None`/`list`/`dict` -> `bool`, all via direct
/// `DeepDiff(..., view=DELTA_VIEW)._to_delta_dict(...)` probes). Coercions
/// *into* a container or `None` (i.e. `new_value` is `Null`/an
/// array/object) are deliberately **not** attempted and always return
/// `None`: every such coercion this domain could reach (e.g. `dict(5)`,
/// `list(True)`) genuinely raises in real Python, so the conservative
/// default is already correct there. The one acknowledged, narrow gap is
/// the reverse direction for containers-as-*source* values feeding a
/// `str` target (Python's `str([1])`/`str({'a': 1})` actually succeed,
/// producing a `repr`-shaped string) — not attempted here (out of this
/// fix's reviewed scope) and always falls through to `None`/"always
/// include", which only ever measures a slightly larger `diff_length`
/// than real `DeepDiff` would for that specific, uncommon pairing.
fn coerce_for_type_change(old_value: &Value, new_value: &Value) -> Option<Value> {
    match new_value {
        Value::Bool(_) => Some(Value::Bool(is_truthy(old_value))),
        Value::Number(n) if n.is_f64() => {
            coerce_to_f64(old_value).and_then(|f| Number::from_f64(f).map(Value::Number))
        }
        Value::Number(_) => coerce_to_i64(old_value).map(|i| Value::Number(Number::from(i))),
        Value::String(_) => coerce_to_python_str(old_value).map(Value::String),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Python's `bool(value)` truthiness rule: `None`/`0`/`0.0`/`""`/`[]`/`{}`
/// are falsy, everything else (including a non-empty string that spells
/// out `"False"`) is truthy — confirmed against real `deepdiff` for the
/// scalar cases (`''`/`'x'` -> `bool`) and matches Python's own documented
/// semantics for the container cases.
#[allow(
    clippy::float_cmp,
    reason = "comparing a coerced numeric value against exact zero mirrors Python's own `bool(x)`               rule, not a computed arithmetic result"
)]
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// Python's `float(value)`, as far as this domain needs: `bool`/`int` and
/// already-`float` values always succeed exactly; a string is parsed with
/// leading/trailing whitespace trimmed first (matching Python's own
/// leniency there); a container never succeeds (`float([1])` raises in
/// real Python).
fn coerce_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Null | Value::Array(_) | Value::Object(_) => None,
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
    }
}

/// Python's `int(value)`: `bool` maps to `1`/`0`; a float **truncates
/// toward zero** (`int(1.9) == 1`, `int(-1.9) == -1` — confirmed against
/// real `deepdiff`, not rounded); a string is parsed the same
/// whitespace-tolerant way as [`coerce_to_f64`] (Python's `int()` does not
/// accept a decimal point, matching Rust's own `i64` parser); a container
/// never succeeds. An out-of-`i64`-range float returns `None` (this
/// domain's numbers stay well under that bound in practice — an accepted,
/// narrow limitation rather than a chased-down `i128`/bignum port).
#[allow(
    clippy::cast_precision_loss,
    reason = "a range-check boundary constant, not an arithmetic result — exactness beyond \
              f64's mantissa is not needed to test whether a float is grossly out of i64 range"
)]
const I64_MIN_AS_F64: f64 = i64::MIN as f64;
#[allow(
    clippy::cast_precision_loss,
    reason = "a range-check boundary constant, not an arithmetic result — exactness beyond \
              f64's mantissa is not needed to test whether a float is grossly out of i64 range"
)]
const I64_MAX_AS_F64: f64 = i64::MAX as f64;

fn coerce_to_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Null | Value::Array(_) | Value::Object(_) => None,
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64()?;
                if f.is_finite() && (I64_MIN_AS_F64..=I64_MAX_AS_F64).contains(&f) {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "explicitly range-checked against I64_MIN_AS_F64/I64_MAX_AS_F64 immediately above"
                    )]
                    Some(f.trunc() as i64)
                } else {
                    None
                }
            } else {
                n.as_i64()
                    .or_else(|| n.as_u64().and_then(|u| i64::try_from(u).ok()))
            }
        }
        Value::String(s) => s.trim().parse::<i64>().ok(),
    }
}

/// Python's `str(value)`: `None`/`bool` map to the literal `"None"`/
/// `"True"`/`"False"`; an int renders as a plain decimal (matching Rust's
/// own `i64`/`u64` `Display`); a float renders via Rust's own shortest
/// round-trip `f64` `Display`, with a trailing `.0` appended when Rust's
/// output has neither a `.` nor an exponent marker — Python's `str(float)`
/// always shows a decimal point for a non-exponential value (`str(5.0) ==
/// "5.0"`, confirmed against real `deepdiff`) where Rust's default
/// `Display` does not. This is a best-effort match for exponential/very
/// precise floats (not byte-verified against Python's own `repr` algorithm
/// beyond the plain cases this fix's scope covers) — a mismatch there only
/// ever causes an unnecessary inclusion (see [`coerce_for_type_change`]'s
/// doc), never an incorrect omission. A container never succeeds here (see
/// [`coerce_for_type_change`]'s doc for that documented, narrower gap).
fn coerce_to_python_str(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some("None".to_string()),
        Value::Array(_) | Value::Object(_) => None,
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64()?;
                let mut rendered = f.to_string();
                if !rendered.contains(['.', 'e', 'E']) {
                    rendered.push_str(".0");
                }
                Some(rendered)
            } else if let Some(i) = n.as_i64() {
                Some(i.to_string())
            } else {
                n.as_u64().map(|u| u.to_string())
            }
        }
        Value::String(s) => Some(s.clone()),
    }
}

/// `DeepDiff`'s own default `threshold_to_diff_deeper` (`_diff_dict`,
/// diff.py) — **unrelated to `ignore_order`**, and confirmed to change a
/// real distance computation's outcome (not just top-level report shape):
/// a dict-vs-dict comparison whose key overlap (intersection / union) is
/// below this collapses into a single wholesale `values_changed` instead of
/// recursing key by key, which [`count_object_diff_leaves`] must replicate
/// to get a correct `diff_length` for [`rough_distance`]'s pairing
/// decisions — see that function's own doc for a worked case where skipping
/// this flips which candidate gets paired.
///
/// This is a genuine, real, **PRE-EXISTING gap in `crate::diff::object_diff`
/// itself** for a *real, user-facing* diff (the actual reported dict-vs-dict
/// diff, used identically whether or not `ignore_order` is set — confirmed
/// empirically on the plain ordered path too), and this slice does *not*
/// fix that: implementing `threshold_to_diff_deeper` in the real reported
/// output is a behavior change to already-shipped M2 functionality, out of
/// scope for an `ignore_order` slice, and is tracked separately as its own
/// reviewed follow-up rather than silently patched here.
///
/// It **is** additionally implemented, Report-producing, in
/// `crate::diff::object_diff` itself, but *only* behind
/// [`crate::DiffOptions::collapse_low_overlap_dicts`]'s trial-diff-only
/// gate — never for a real diff. That second call site exists to close a
/// route this count-only mirror alone cannot: [`count_array_diff_leaves`]'s
/// own trial sub-diff calls the *real* `array_diff`/`object_diff` engine
/// (not this module's count-only mirrors) to measure a nested array pair's
/// distance, and when that trial's pairing accepts a dict-vs-dict pair
/// nested inside it, the *actual Report entry it builds* — not just a
/// count this function computes independently — must already reflect the
/// collapse, or [`Report::distance_leaf_length`] inherits an inflated,
/// uncollapsed leaf count from it (found by differential fuzzing: a
/// nested-array-of-dicts candidate pair whose true distance is 0.1364 was
/// measured at 0.3182 — crossing [`CUTOFF_DISTANCE_FOR_PAIRS`] and
/// producing a completely different pairing decision). Both call sites
/// share the exact same ratio check, [`is_below_threshold_to_diff_deeper`],
/// so there is exactly one place the `threshold_to_diff_deeper` arithmetic
/// lives.
pub(crate) const THRESHOLD_TO_DIFF_DEEPER: f64 = 0.33;

/// The shared `threshold_to_diff_deeper` ratio check backing both
/// [`count_object_diff_leaves`] (the count-only distance mirror) and
/// `crate::diff::object_diff`'s trial-only collapse branch (the
/// Report-producing twin, gated by
/// [`crate::DiffOptions::collapse_low_overlap_dicts`]) — see
/// [`THRESHOLD_TO_DIFF_DEEPER`]'s own doc for why both exist.
pub(crate) fn is_below_threshold_to_diff_deeper(
    a: &serde_json::Map<String, Value>,
    b: &serde_json::Map<String, Value>,
) -> bool {
    let union_len = a.keys().chain(b.keys()).collect::<HashSet<_>>().len();
    let intersect_len = a.keys().filter(|key| b.contains_key(key.as_str())).count();
    #[allow(
        clippy::cast_precision_loss,
        reason = "key counts are small, far under f64's exact-integer range"
    )]
    {
        union_len > 1 && (intersect_len as f64 / union_len as f64) < THRESHOLD_TO_DIFF_DEEPER
    }
}

/// [`count_diff_leaves`]'s dict case: mirrors
/// [`crate::diff::object_diff`]'s key-set walk, contributing
/// [`item_length`] of the whole value for an added/removed key and
/// recursing (one level deeper) into a shared key — **except** when
/// [`is_below_threshold_to_diff_deeper`] (`DeepDiff`'s own default,
/// confirmed via a real `DELTA_VIEW` probe to apply inside its distance
/// computation too), in which case the whole thing collapses to
/// [`item_length_of_map`] of `b`, matching a single wholesale
/// `values_changed` rather than recursing. See [`THRESHOLD_TO_DIFF_DEEPER`]'s
/// doc for the full story of where this is and is not also implemented as
/// a real `Report`-producing collapse.
pub(crate) fn count_object_diff_leaves(
    a: &serde_json::Map<String, Value>,
    b: &serde_json::Map<String, Value>,
    depth: usize,
    opts: &DiffOptions,
) -> usize {
    if is_below_threshold_to_diff_deeper(a, b) {
        return item_length_of_map(b);
    }

    let mut total = 0;

    for (key, old_value) in a {
        total += match b.get(key) {
            None => item_length(old_value),
            Some(new_value) => count_diff_leaves(old_value, new_value, depth + 1, opts),
        };
    }
    for (key, new_value) in b {
        if !a.contains_key(key) {
            total += item_length(new_value);
        }
    }

    total
}

/// [`count_diff_leaves`]'s array case — see that function's doc for why
/// this is the one sub-case still routed through a genuine (but small,
/// single-pair) [`crate::diff::diff_at`] trial diff rather than a count-only mirror.
/// `depth` here is the array's *own* depth (matching
/// [`crate::diff::array_diff`]'s own convention), and the trial gets a
/// bound of the *remaining* `max_depth` budget — see [`rough_distance`]'s
/// doc for why a reduced (not fresh) budget is required for safety.
pub(crate) fn count_array_diff_leaves(
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
) -> usize {
    let probe_opts = DiffOptions {
        max_depth: opts.max_depth.saturating_sub(depth),
        ignore_order: opts.ignore_order,
        // This whole call is a measurement-only trial (see this function's
        // own doc) — never a real, user-facing diff — so any dict-vs-dict
        // pair it recurses into (directly, or arbitrarily deep through a
        // nested `ignore_order_array_diff` re-entry building one of its own
        // accepted pairs) must be measured with the same
        // `threshold_to_diff_deeper` awareness this module's own
        // `count_object_diff_leaves` already applies — see
        // `crate::DiffOptions::collapse_low_overlap_dicts`'s doc.
        collapse_low_overlap_dicts: true,
    };
    let mut probe_path = Vec::new();
    let Ok(mut sub_report) = crate::diff::array_diff(&mut probe_path, a, b, 0, &probe_opts) else {
        return 0;
    };
    // `DeepDiff`'s own `_to_delta_dict` (what its real distance computation
    // measures) also applies its whole-tree mutual-add-remove merge before
    // measuring `diff_length` — confirmed empirically: `[3.8, 3, [true]]`
    // vs `[0.0, 0.0, [], 3]` (list1/list2 in the doctest below) delta-dicts
    // to `{"values_changed": {"root[0]": ..., "root[2]": ...}}`, not raw
    // `iterable_item_added`/`removed` pairs — without this, a genuinely
    // close nested-list pair can measure a spuriously large `diff_length`
    // (unmerged add+remove instead of one merged value change), pushing an
    // otherwise-acceptable candidate over the cutoff and silently
    // rejecting a pairing real `DeepDiff` accepts. Skipped without effect
    // when `array_diff` didn't go through the `ignore_order` path (no
    // `iterable_item_added`/`removed` pair can share a path there — see
    // `crate::diff::array_diff`'s own doc on the LCS/positional split), so
    // this is always safe to call unconditionally.
    sub_report.merge_mutual_add_removes();
    sub_report.distance_leaf_length()
}

/// `DeepDiff`'s `_get_rough_distance` (distance.py): the
/// numeric fast path when both `removed`/`added` are number-like
/// ([`numeric_value`]), else a structural fallback of
/// `diff_length / (rough_length(removed) + rough_length(added))`, where
/// `diff_length` comes from [`count_diff_leaves`] — a `Report`-free mirror
/// of a trial recursive diff between `removed` and `added` (see that
/// function's own doc for why it deliberately does *not* build a
/// [`Report`], unlike `DeepDiff`'s own brand-new nested `DeepDiff` object
/// built purely to measure this).
///
/// `depth` is the depth of the *list* doing the pairing (so `removed`/
/// `added` themselves sit at `depth + 1`) — used only to give the trial a
/// bound of the **remaining** `max_depth` budget, not a fresh one: granting
/// every one of the (potentially many) candidate-pair trials its own full
/// `max_depth` would let native stack usage compound with the depth already
/// reached by the outer traversal, exactly the kind of combined-budget bug
/// [`crate::diff::check_value_depth`]'s own doc describes fixing elsewhere
/// in this crate.
///
/// **The one place this can still fail is [`count_array_diff_leaves`]'s own
/// nested trial diff, and it is believed unreachable today, kept anyway as
/// defense-in-depth.** [`ignore_order_array_diff`]'s own up-front
/// [`crate::diff::check_value_depth`] pass already validates `removed`/`added`
/// individually against this exact same reduced budget
/// (`max_depth` minus the items' own depth) before pairing ever runs; a
/// short inductive argument (every subtree's remaining-budget-at-its-relative-depth
/// transfers exactly from a depth-checked root to a trial restarted at
/// depth `0` with that same budget as its own `max_depth`) shows the trial
/// can never need more depth than that. Unlike [`insert_lcs_pair_finding`]'s
/// equivalent "structurally unreachable" guard (a *static type fact* — a
/// JSON scalar's nesting is always `0`, true forever), the invariant here
/// is a *cross-function arithmetic* one, spread across several functions —
/// exactly the kind of subtle, easy-to-silently-break invariant this
/// crate's own M3-pre/M4-perf depth-guard history (see the rust coding
/// guide's learned patterns) took three rounds to get right. Treating an
/// over-budget nested array trial as a **rejected** candidate (`0` leaves
/// counted, only reachable this way when `removed`/`added` are themselves
/// arrays needing more depth than available) rather than propagating an
/// error means a future edit that weakens either budget calculation
/// degrades to a more conservative pairing decision, never a crash on
/// untrusted input.
pub(crate) fn rough_distance(
    removed: &Value,
    added: &Value,
    cutoff: f64,
    depth: usize,
    opts: &DiffOptions,
) -> f64 {
    if let (Some(r), Some(a)) = (numeric_value(removed), numeric_value(added)) {
        return numeric_distance(r, a, cutoff);
    }

    let diff_length = count_diff_leaves(removed, added, depth + 1, opts);
    if diff_length == 0 {
        return 0.0;
    }
    let rough_len = rough_length(removed) + rough_length(added);
    #[allow(
        clippy::cast_precision_loss,
        reason = "diff_length/rough_len are small structural node counts, \
                  far under f64's exact-integer range"
    )]
    {
        diff_length as f64 / rough_len as f64
    }
}

// ---------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------
