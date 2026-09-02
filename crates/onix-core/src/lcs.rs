//! A faithful, from-scratch port of Python's `difflib.SequenceMatcher`
//! opcode algorithm, restricted to the one configuration `DeepDiff` actually
//! uses for ordered-list comparison: `isjunk=None, autojunk=False`.
//!
//! This module is deliberately *pure*: it knows nothing about
//! [`crate::report::Report`], [`crate::path::PathSegment`], or recursion
//! depth — it only turns two slices of [`serde_json::Value`] scalars into an
//! ordered list of [`Opcode`]s. [`crate::diff::array_diff`] is what maps
//! those opcodes into report findings; see that module's doc for the full,
//! empirically-verified `DeepDiff` list-compat spec this exists to serve.
//!
//! # Why this exists
//!
//! `DeepDiff`'s default (non-`ignore_order`) list comparison is *not*
//! always a simple index-aligned scan: when
//! every element of *both* lists is a JSON scalar (`DeepDiff`'s own
//! `_all_values_basic_hashable` check — null/bool/number/string; a dict or
//! list anywhere in either list disqualifies the whole comparison), it
//! additionally runs a `difflib.SequenceMatcher`-based "cheapest edit"
//! match and, only when that produces more than one finding, compares its
//! finding *count* against the plain index-aligned algorithm's, keeping
//! whichever is smaller (a tie keeps the index-aligned result). See
//! `crate::diff`'s module doc for the full write-up, including the
//! surprising matching-equality and `new_path` details this module's
//! algorithm alone doesn't explain.
//!
//! # No junk, no autojunk
//!
//! `DeepDiff` always constructs its matcher with `isjunk=None` and
//! `autojunk=False` (`deepdiff/diff.py::_diff_ordered_iterable_by_difflib`).
//! `autojunk` is `difflib`'s *default-on* heuristic that treats an
//! element appearing in more than 1% of a ≥200-item sequence as "popular"
//! and excludes it from matching — `DeepDiff` explicitly opts out, so this
//! port implements no such thing, and there is no ≥200-item behavior change
//! to replicate (confirmed empirically against real `deepdiff==9.1.0` with a
//! 250-item, one-popular-value fixture: the popular value matches exactly
//! like any other). Skipping junk/autojunk also sidesteps `SequenceMatcher`'s
//! own two-pass junk-then-popular pruning of its `b2j` index, considerably
//! simplifying this port relative to the full standard-library algorithm.

use std::collections::HashMap;

use crate::value::Value;

/// A bucket key for grouping list elements that compare equal the way
/// Python's `==` (and therefore `difflib`'s matcher, and dict/set hashing)
/// does — **not** the way [`crate::diff`]'s own scalar comparison does.
///
/// This is the "hashable" finding: Python treats `1 == 1.0 == True` (and
/// `0 == 0.0 == False`) as equal regardless of type, so `difflib` can match
/// an `int` in one list against a `float` (or `bool`) of the same numeric
/// value in the other — and, critically, a `difflib` `'equal'` opcode is
/// *never* diffed further (see `crate::diff::array_diff`'s doc), so two
/// cross-type-equal numbers matched this way produce **no** `type_changes`
/// finding at all, unlike every other numeric comparison in this engine
/// (which always treats int/float and bool/int as a type change — see
/// `crate::diff::numbers_equal`'s doc). Confirmed empirically: real
/// `DeepDiff` reports `{}` for `[1]` vs `[1.0]`.
///
/// Integral values (including both bools and float values with no
/// fractional part, within the range a `f64` can represent exactly) are
/// normalized to [`ScalarKey::Int`] so `1`, `1.0`, and `True` all produce the
/// same key; `-0.0` and `0.0` both normalize to `Int(0)`. A float outside
/// that exact range keeps its own [`ScalarKey::Float`] bucket rather than
/// risk a lossy, non-reversible integer cast — an accepted, documented
/// limitation for numbers whose magnitude exceeds `2^53` (real Python
/// itself does exact big-int/float comparison here, which this port does
/// not replicate; see [`MAX_EXACT_F64_INT`]).
#[derive(Clone, PartialEq, Eq, Hash)]
enum ScalarKey {
    Null,
    Str(String),
    Int(i128),
    /// Bit pattern of a non-integral (or too-large-to-be-exact) float.
    Float(u64),
}

/// The largest magnitude an `f64` can represent every integer up to,
/// exactly (`2^53`). Beyond this, adjacent integers can be indistinguishable
/// in floating point, so [`scalar_key`] does not attempt an exact integer
/// cast past this bound.
const MAX_EXACT_F64_INT: f64 = 9_007_199_254_740_992.0;

/// Returns `true` if every element of `items` is a JSON scalar (null, bool,
/// number, or string) — `DeepDiff`'s `_all_values_basic_hashable` check.
///
/// A dict or list anywhere in `items` returns `false`. An empty slice
/// returns `true` (vacuously — matches `DeepDiff`, whose equivalent check is
/// also vacuously true over an empty iterable).
#[must_use]
pub(crate) fn all_basic_scalars(items: &[Value]) -> bool {
    items.iter().all(|item| {
        matches!(
            item,
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Str(_)
        )
    })
}

/// Computes `value`'s [`ScalarKey`].
///
/// # Panics
///
/// Panics if `value` is an array or object. Every caller in this module
/// only ever reaches this after [`all_basic_scalars`] has confirmed the
/// whole slice is scalar-only, so this can never actually fire — see
/// `crate::diff::array_diff`'s dispatch, which gates the entire LCS path on
/// that check.
fn scalar_key(value: &Value) -> ScalarKey {
    match value {
        Value::Null => ScalarKey::Null,
        Value::Str(s) => ScalarKey::Str(s.to_string()),
        Value::Bool(b) => ScalarKey::Int(i128::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n
                .as_i64()
                .map(i128::from)
                .or_else(|| n.as_u64().map(i128::from))
            {
                return ScalarKey::Int(i);
            }
            let f = n.as_f64().expect("a serde_json Number is i64, u64, or f64");
            if f.fract() == 0.0 && f.abs() <= MAX_EXACT_F64_INT {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "fract() == 0.0 and the magnitude bound above together guarantee an exact round trip"
                )]
                ScalarKey::Int(f as i128)
            } else {
                // `0.0`/`-0.0` are both integral (`fract() == 0.0`), so they
                // are already normalized to `Int(0)` by the branch above —
                // this branch only ever sees a genuinely non-integral (or
                // too-large-to-cast-exactly) float, never needing its own
                // zero-sign normalization.
                ScalarKey::Float(f.to_bits())
            }
        }
        Value::Array(_) | Value::Object(_) => {
            unreachable!("scalar_key called on a non-scalar; caller must check all_basic_scalars")
        }
    }
}

/// One `difflib`-style edit opcode over `a`/`b` index ranges (half-open,
/// like `difflib.SequenceMatcher.get_opcodes()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Opcode {
    pub(crate) tag: Tag,
    pub(crate) a1: usize,
    pub(crate) a2: usize,
    pub(crate) b1: usize,
    pub(crate) b2: usize,
}

/// The kind of edit an [`Opcode`] describes, matching `difflib`'s own tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tag {
    /// `a[a1..a2]` and `b[b1..b2]` are the same (by [`ScalarKey`] equality),
    /// element for element. Never diffed further — see `crate::diff`'s
    /// module doc.
    Equal,
    /// `a[a1..a2]` should be replaced by `b[b1..b2]`; the two ranges never
    /// share a matching element (see [`compute_opcodes`]'s doc).
    Replace,
    /// `a[a1..a2]` should be deleted (`b1 == b2`).
    Delete,
    /// `b[b1..b2]` should be inserted at `a[a1..a1]` (`a1 == a2`).
    Insert,
}

/// One matching block: `a[a..a+size] == b[b..b+size]` (by
/// [`ScalarKey`] equality).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Match {
    a: usize,
    b: usize,
    size: usize,
}

/// Finds the longest matching block within `a[alo..ahi]` / `b[blo..bhi]` —
/// a port of `difflib.SequenceMatcher.find_longest_match`'s core
/// sparse-DP chaining, with its `isjunk`/`autojunk` branches dropped
/// (`DeepDiff` never enables either; see this module's doc). Ties (multiple
/// equal-longest candidates) resolve exactly like `difflib`'s: earliest
/// `i`, then earliest `j` — a direct consequence of scanning `i` ascending
/// and only updating on a *strictly greater* `k`.
///
/// **Deliberately omits `difflib`'s post-DP greedy-extension step**
/// (`while ... and not isbjunk(...): besti -= 1; ...`, run twice — backward
/// then forward). That step exists in the standard library to bridge a
/// match across an *excluded* `b` element (junk, or an autojunk-pruned
/// popular one) that the DP's `b2j` chain skips entirely because such
/// elements are deleted from `b2j` (`__chain_b`). With `isjunk=None` (this
/// module's only configuration — see its doc), `b2j` never excludes
/// anything, so the DP chain alone already finds every genuinely
/// contiguous run the extension step could ever find; re-extending
/// afterwards can only ever re-derive a run already reflected in some
/// `run_length` the DP itself computed. Dropping it therefore changes no
/// opcode output for this configuration — verified by a 400,000-trial
/// randomized differential test against real `difflib` (zero divergences),
/// not only by the argument above.
///
/// `b2j` maps each of `b`'s [`ScalarKey`]s to the (ascending) list of
/// indices it occurs at in `b`; built once per `a`/`b` pair and reused
/// across every call this function makes during [`get_matching_blocks`]'s
/// traversal, exactly like `difflib`'s own `self.b2j`.
///
/// Returns `(best_a, best_b, best_size)`; `best_size == 0` means no match
/// was found in the given range at all.
fn find_longest_match(
    a: &[Value],
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
    b2j: &HashMap<ScalarKey, Vec<usize>>,
) -> (usize, usize, usize) {
    let (mut best_a, mut best_b, mut best_size) = (alo, blo, 0);
    let mut run_length_by_b_index: HashMap<usize, usize> = HashMap::new();

    for (offset, item) in a[alo..ahi].iter().enumerate() {
        let a_index = alo + offset;
        let mut next_run_length_by_b_index: HashMap<usize, usize> = HashMap::new();
        if let Some(b_indices) = b2j.get(&scalar_key(item)) {
            for &b_index in b_indices {
                if b_index < blo {
                    continue;
                }
                if b_index >= bhi {
                    break;
                }
                let run_length = if b_index == 0 {
                    1
                } else {
                    run_length_by_b_index
                        .get(&(b_index - 1))
                        .copied()
                        .unwrap_or(0)
                        + 1
                };
                next_run_length_by_b_index.insert(b_index, run_length);
                if run_length > best_size {
                    best_a = a_index + 1 - run_length;
                    best_b = b_index + 1 - run_length;
                    best_size = run_length;
                }
            }
        }
        run_length_by_b_index = next_run_length_by_b_index;
    }

    (best_a, best_b, best_size)
}

/// Returns every non-empty matching block between `a` and `b`, sorted and
/// with adjacent blocks collapsed, terminated by a dummy zero-size block at
/// `(a.len(), b.len())` — a direct, iterative (explicit work-stack, no
/// native recursion) port of `difflib.SequenceMatcher.get_matching_blocks`.
/// `difflib` itself switched to this iterative shape for the same reason
/// this whole engine avoids native recursion on untrusted input: naive
/// recursion here overflowed the stack for some real-world inputs.
fn get_matching_blocks(a: &[Value], b: &[Value]) -> Vec<Match> {
    let mut b2j: HashMap<ScalarKey, Vec<usize>> = HashMap::new();
    for (b_index, item) in b.iter().enumerate() {
        b2j.entry(scalar_key(item)).or_default().push(b_index);
    }

    let mut stack = vec![(0_usize, a.len(), 0_usize, b.len())];
    let mut raw_matches = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = stack.pop() {
        let (match_a, match_b, match_size) = find_longest_match(a, alo, ahi, blo, bhi, &b2j);
        if match_size > 0 {
            raw_matches.push(Match {
                a: match_a,
                b: match_b,
                size: match_size,
            });
            if alo < match_a && blo < match_b {
                stack.push((alo, match_a, blo, match_b));
            }
            if match_a + match_size < ahi && match_b + match_size < bhi {
                stack.push((match_a + match_size, ahi, match_b + match_size, bhi));
            }
        }
    }
    raw_matches.sort_by_key(|m| (m.a, m.b));

    let mut collapsed = Vec::new();
    let (mut pending_a, mut pending_b, mut pending_size) = (0_usize, 0_usize, 0_usize);
    for m in raw_matches {
        if pending_a + pending_size == m.a && pending_b + pending_size == m.b {
            pending_size += m.size;
        } else {
            if pending_size > 0 {
                collapsed.push(Match {
                    a: pending_a,
                    b: pending_b,
                    size: pending_size,
                });
            }
            (pending_a, pending_b, pending_size) = (m.a, m.b, m.size);
        }
    }
    if pending_size > 0 {
        collapsed.push(Match {
            a: pending_a,
            b: pending_b,
            size: pending_size,
        });
    }
    collapsed.push(Match {
        a: a.len(),
        b: b.len(),
        size: 0,
    });
    collapsed
}

/// Computes the `difflib`-style [`Opcode`] list turning `a` into `b`.
///
/// A direct port of `difflib.SequenceMatcher.get_opcodes`. Every gap
/// between two consecutive matching blocks becomes exactly one `Replace`
/// (both sides non-empty), `Delete` (only `a` non-empty), or `Insert` (only
/// `b` non-empty) opcode — never more than one per gap, and, by
/// construction of [`get_matching_blocks`] (which finds *every* match in
/// any given sub-range before giving up on it), **a `Replace` opcode's two
/// ranges never contain a matching element pair at all**, not even a
/// non-contiguous or out-of-position one. Every `'equal'` matching block
/// also becomes an `Opcode` with [`Tag::Equal`].
///
/// # Panics
///
/// Never, for any `a`/`b` for which [`all_basic_scalars`] holds on both —
/// see [`scalar_key`]'s doc for the sole (structurally unreachable here)
/// panic path this function's callees have.
#[must_use]
pub(crate) fn compute_opcodes(a: &[Value], b: &[Value]) -> Vec<Opcode> {
    let mut opcodes = Vec::new();
    let (mut i, mut j) = (0_usize, 0_usize);

    for m in get_matching_blocks(a, b) {
        let tag = if i < m.a && j < m.b {
            Some(Tag::Replace)
        } else if i < m.a {
            Some(Tag::Delete)
        } else if j < m.b {
            Some(Tag::Insert)
        } else {
            None
        };
        if let Some(tag) = tag {
            opcodes.push(Opcode {
                tag,
                a1: i,
                a2: m.a,
                b1: j,
                b2: m.b,
            });
        }
        (i, j) = (m.a + m.size, m.b + m.size);
        if m.size > 0 {
            opcodes.push(Opcode {
                tag: Tag::Equal,
                a1: m.a,
                a2: i,
                b1: m.b,
                b2: j,
            });
        }
    }

    opcodes
}

#[cfg(test)]
#[path = "lcs_tests.rs"]
mod tests;
