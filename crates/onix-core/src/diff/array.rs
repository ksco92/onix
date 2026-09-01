//! List (JSON array) diffing: [`array_diff`]'s dispatch between the
//! LCS/`difflib`-style match and the plain index-aligned comparison — see
//! the parent `diff` module's "List diffing: the M6 list-compat fix" doc
//! section for the full, empirically-verified spec this implements.

use serde_json::Value;

use crate::error::Error;
use crate::ignore_order;
use crate::lcs;
use crate::path::PathSegment;
use crate::report::{Report, TypeChangeEntry, ValuesChangedEntry};

use super::{
    DiffOptions, check_traversal_depth, check_value_depth, diff_at, python_type_name, scoped,
};

/// Diffs two lists (JSON arrays) at `path`, `depth` levels deep.
///
/// Dispatches between two candidate algorithms, matching `DeepDiff`'s own
/// `_diff_iterable_in_order` dispatch exactly (see this module's own doc
/// section "List diffing: the M6 list-compat fix" for the full,
/// empirically-verified spec):
///
/// - **When every element of *both* `a` and `b` is a JSON scalar** (null,
///   bool, number, or string — `DeepDiff`'s "basic hashable" check, see
///   [`crate::lcs::all_basic_scalars`]), an LCS/`difflib`-style "cheapest
///   edit" match ([`lcs_array_diff`]) is tried first. `DeepDiff` only trusts
///   that match unconditionally when it produces at most one finding;
///   otherwise it *also* computes the plain index-aligned result below and
///   keeps whichever has **fewer total findings**, favoring the
///   index-aligned one on a tie — replicated exactly here via
///   [`Report::finding_count`].
/// - **Otherwise** (either list contains a dict or a nested list anywhere),
///   [`positional_array_diff`] alone is used, unconditionally — `DeepDiff`
///   never even attempts the LCS match in this case.
///
/// # Stack-footprint note (debug-build depth-guard fidelity)
///
/// The scalar-only branch's own locals (two [`Report`]s, one for each
/// candidate) are deliberately kept in [`lcs_or_positional_array_diff`], a
/// separate, non-tail-called function, rather than inline here — even
/// though this function's own recursion never actually *builds* them for a
/// list containing a dict or nested list. In an unoptimized (debug) build,
/// a function's stack frame is sized for the union of every local it
/// declares in its source, regardless of which branch runs at
/// runtime — so those two extra `Report` locals inflated *every* frame of
/// the native list-of-list recursion below, not just the scalar-list leaf
/// that actually needs them, measurably lowering the debug-build recursion
/// depth a default (2 MiB) thread's stack tolerates before
/// [`Error::MaxDepthExceeded`] can even fire. Splitting them out restores
/// depth-512 traversal on a default-size stack — see
/// `array_diff_at_depth_512_on_a_default_stack_completes_without_crashing`.
pub(crate) fn array_diff(
    path: &mut Vec<PathSegment>,
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
) -> Result<Report, Error> {
    if opts.ignore_order {
        return ignore_order::ignore_order_array_diff(path, a, b, depth, opts);
    }
    if lcs::all_basic_scalars(a) && lcs::all_basic_scalars(b) {
        lcs_or_positional_array_diff(path, a, b, depth, opts)
    } else {
        positional_array_diff(path, a, b, depth, opts)
    }
}
/// The scalar-only-list candidate computation [`array_diff`] dispatches to
/// — split out purely to keep `array_diff`'s own stack frame (on the *hot*
/// native-recursion path for every list, scalar-only or not) small; see
/// that function's "Stack-footprint note".
fn lcs_or_positional_array_diff(
    path: &mut Vec<PathSegment>,
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
) -> Result<Report, Error> {
    let lcs_report = lcs_array_diff(path, a, b, depth, opts.max_depth)?;
    if lcs_report.finding_count() > 1 {
        let positional_report = positional_array_diff(path, a, b, depth, opts)?;
        if lcs_report.finding_count() >= positional_report.finding_count() {
            return Ok(positional_report);
        }
    }
    Ok(lcs_report)
}
/// One pair matched by an LCS `'replace'` opcode's pairwise comparison,
/// bundled into a struct so [`insert_lcs_pair_finding`]'s signature stays
/// under clippy's argument-count lint.
#[derive(Clone, Copy)]
struct LcsPair<'a> {
    old_idx: usize,
    new_idx: usize,
    old_value: &'a Value,
    new_value: &'a Value,
}
/// Records the `values_changed` or `type_changes` finding for one pair
/// matched by an LCS `Replace` opcode (see [`lcs_array_diff`]), at
/// `old_idx`, attaching [`ValuesChangedEntry::new_path`] whenever `new_idx`
/// differs from `old_idx`.
///
/// Never needs an "equal" branch: a `Replace` opcode's two index ranges
/// never share a [`crate::lcs`]-equal (Python-`==`-equal) element pair (see
/// `crate::lcs::compute_opcodes`'s doc), and this engine's own scalar
/// equality is always at least as strict as Python's (it additionally
/// distinguishes int/float and bool/int, which Python's `==` does not) — so
/// `old_value`/`new_value` are always different by *this engine's* equality
/// too, and this always records exactly one finding once past the guard
/// below.
///
/// Checks [`check_traversal_depth`] at `depth + 1` (this pair's own path
/// depth) before recording anything — the same bound
/// [`positional_array_diff`]'s equivalent same-index pair enforces by
/// recursing through [`diff_at`] (whose own top check *is*
/// [`check_traversal_depth`]). This finding is reached without ever calling
/// `diff_at`, so without this explicit check it would silently accept a
/// pairwise difference one level deeper than `max_depth` permits — see this
/// module's "List diffing" doc section.
///
/// No [`check_value_depth`] guard is needed here (unlike every other
/// clone-into-[`Report`] sink in this module): both values are guaranteed
/// to be JSON scalars by [`array_diff`]'s own dispatch condition, and a
/// scalar's intrinsic nesting is always `0` — [`check_value_depth`] can
/// structurally never reject a scalar, at any `depth`/`max_depth`, so a
/// call here would be dead-weight guard code with no reachable failure
/// path (and an unkillable `cargo mutants` mutant to go with it). This is
/// exactly the traversal-depth-vs-value-depth distinction
/// [`check_traversal_depth`]'s doc draws.
fn insert_lcs_pair_finding(
    report: &mut Report,
    path: &mut Vec<PathSegment>,
    pair: LcsPair<'_>,
    depth: usize,
    max_depth: usize,
) -> Result<(), Error> {
    let LcsPair {
        old_idx,
        new_idx,
        old_value,
        new_value,
    } = pair;

    scoped(path, PathSegment::Index(old_idx), |path| {
        check_traversal_depth(path, depth + 1, max_depth)?;

        let new_path = (old_idx != new_idx).then(|| {
            let mut new_segments = path.clone();
            *new_segments
                .last_mut()
                .expect("scoped just pushed the old_idx segment") = PathSegment::Index(new_idx);
            new_segments
        });

        if python_type_name(old_value) == python_type_name(new_value) {
            report.insert_values_changed(
                path.clone(),
                ValuesChangedEntry {
                    old_value: old_value.clone(),
                    new_value: new_value.clone(),
                    new_path,
                },
            );
        } else {
            report.insert_type_change(
                path.clone(),
                TypeChangeEntry {
                    old_type: python_type_name(old_value).to_string(),
                    new_type: python_type_name(new_value).to_string(),
                    old_value: old_value.clone(),
                    new_value: new_value.clone(),
                    new_path,
                },
            );
        }
        Ok(())
    })
}
/// Diffs two lists of JSON scalars via a `difflib`-style LCS match — see
/// [`array_diff`]'s doc for when this is tried, and this module's "List
/// diffing" doc section for the opcode-to-finding mapping this implements
/// (a direct port of `deepdiff/diff.py::_diff_ordered_iterable_by_difflib`).
///
/// The only possible [`Error::MaxDepthExceeded`] source is
/// [`insert_lcs_pair_finding`]'s traversal-depth check on a `'replace'`
/// pair. `'delete'`/`'insert'` findings need **no** depth guard at all
/// (unlike every other clone-into-[`Report`] sink in this module,
/// including [`positional_array_diff`]'s own surplus tail, which still
/// calls [`check_value_depth`] defensively): every value reaching this
/// function is a JSON scalar, guaranteed by [`array_diff`]'s dispatch
/// condition, and a scalar's intrinsic nesting is always `0` — exactly the
/// reasoning [`insert_lcs_pair_finding`]'s own doc gives for skipping the
/// same check on its `'replace'` pairs. Calling it here too would only add
/// dead-weight guard code with no reachable failure path (and an
/// unkillable `cargo mutants` mutant to go with it, found and removed
/// during this fix's own mutation-testing round).
///
/// `path` is the shared traversal buffer (see [`diff_at`]'s doc); every
/// finding below is recorded through [`scoped`], so `path` is restored to
/// its entry state before returning, on every path (success or error).
fn lcs_array_diff(
    path: &mut Vec<PathSegment>,
    a: &[Value],
    b: &[Value],
    depth: usize,
    max_depth: usize,
) -> Result<Report, Error> {
    let mut report = Report::new();

    for op in lcs::compute_opcodes(a, b) {
        match op.tag {
            lcs::Tag::Equal => {}
            lcs::Tag::Delete => {
                for (offset, old_value) in a[op.a1..op.a2].iter().enumerate() {
                    let idx = op.a1 + offset;
                    scoped(path, PathSegment::Index(idx), |path| {
                        report.insert_iterable_item_removed(path.clone(), old_value.clone());
                    });
                }
            }
            lcs::Tag::Insert => {
                for (offset, new_value) in b[op.b1..op.b2].iter().enumerate() {
                    let idx = op.b1 + offset;
                    scoped(path, PathSegment::Index(idx), |path| {
                        report.insert_iterable_item_added(path.clone(), new_value.clone());
                    });
                }
            }
            lcs::Tag::Replace => {
                let a_len = op.a2 - op.a1;
                let b_len = op.b2 - op.b1;
                for offset in 0..a_len.max(b_len) {
                    if offset >= b_len {
                        let old_idx = op.a1 + offset;
                        let old_value = &a[old_idx];
                        scoped(path, PathSegment::Index(old_idx), |path| {
                            report.insert_iterable_item_removed(path.clone(), old_value.clone());
                        });
                    } else if offset >= a_len {
                        let new_idx = op.b1 + offset;
                        let new_value = &b[new_idx];
                        scoped(path, PathSegment::Index(new_idx), |path| {
                            report.insert_iterable_item_added(path.clone(), new_value.clone());
                        });
                    } else {
                        let old_idx = op.a1 + offset;
                        let new_idx = op.b1 + offset;
                        insert_lcs_pair_finding(
                            &mut report,
                            path,
                            LcsPair {
                                old_idx,
                                new_idx,
                                old_value: &a[old_idx],
                                new_value: &b[new_idx],
                            },
                            depth,
                            max_depth,
                        )?;
                    }
                }
            }
        }
    }

    Ok(report)
}
/// The plain index-aligned list comparison: for every index present in
/// *both* `a` and `b`, the pair at that index recurses through the engine's
/// own internal dispatch one level deeper with the index appended to the
/// path (so a changed, type-changed, or further-nested-container
/// difference at that index surfaces with its own deep path, exactly like
/// [`object_diff`]'s shared-key recursion). Once the shorter list is
/// exhausted, the longer list's surplus tail becomes
/// `iterable_item_removed` findings (if `a` is longer) or
/// `iterable_item_added` findings (if `b` is longer), one per surplus index,
/// keyed by that index's *original* position in the longer list — e.g.
/// removing index `3` of a 4-element list reports `root[3]`, not `root[0]`
/// relative to the surplus.
///
/// This is [`array_diff`]'s *only* algorithm whenever either list contains
/// a non-scalar element, and the tie-break/fallback candidate compared
/// against [`lcs_array_diff`]'s result otherwise — see [`array_diff`]'s
/// doc.
///
/// Like [`object_diff`], this always recurses into same-index pairs rather
/// than re-checking [`values_equal`] first — see that function's doc for why
/// the single top-level equality check in [`diff_with_max_depth`] is enough.
///
/// Every surplus-tail clone is checked with [`check_value_depth`] first,
/// exactly like [`object_diff`]'s added/removed leaf clones: a surplus
/// element can itself be an arbitrarily deep value, so the same combined
/// path-depth-plus-value-depth budget applies here too (see
/// [`diff_with_max_depth`]'s doc for the full contract).
///
/// `path` is the single buffer shared across the whole traversal (see
/// [`diff_at`]'s doc): each iteration below runs its work through
/// [`scoped`], which pushes the index segment, runs the closure with that
/// segment in place, then pops it again before moving to the next index —
/// restoring `path` to exactly what it was on entry before touching any
/// sibling, so one sibling's finding never leaks a stale segment into
/// another's path.
fn positional_array_diff(
    path: &mut Vec<PathSegment>,
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
) -> Result<Report, Error> {
    let mut report = Report::new();
    let min_len = a.len().min(b.len());

    // Index-aligned recursion, same depth-counting convention as
    // object_diff: stepping into an index — whether it recurses (present on
    // both sides) or is a leaf finding (surplus tail) — always adds one to
    // depth.
    for i in 0..min_len {
        let sub_report = scoped(path, PathSegment::Index(i), |path| {
            diff_at(path, &a[i], &b[i], depth + 1, opts)
        })?;
        report.merge(sub_report);
    }

    for (i, old_value) in a.iter().enumerate().skip(min_len) {
        scoped(path, PathSegment::Index(i), |path| {
            check_value_depth(path, old_value, depth + 1, opts.max_depth).map(|()| {
                report.insert_iterable_item_removed(path.clone(), old_value.clone());
            })
        })?;
    }

    for (i, new_value) in b.iter().enumerate().skip(min_len) {
        scoped(path, PathSegment::Index(i), |path| {
            check_value_depth(path, new_value, depth + 1, opts.max_depth).map(|()| {
                report.insert_iterable_item_added(path.clone(), new_value.clone());
            })
        })?;
    }

    Ok(report)
}
