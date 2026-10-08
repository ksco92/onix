//! List (JSON array) diffing: [`array_diff`]'s dispatch between the
//! LCS/`difflib`-style match and the plain index-aligned comparison — see
//! `docs/design/list-diff.md` for the full spec this implements.

use crate::value::{Value, class_name};

use crate::error::Error;
use crate::ignore_order::{self, IgnoreOrderMemo};
use crate::lcs;
use crate::path::PathSegment;
use crate::report::{Report, TypeChangeEntry, ValuesChangedEntry};

use super::{
    DiffOptions, check_traversal_depth, check_value_depth, diff_at, effective_type_name,
    normalized_pair, python_type_name, scoped,
};

/// Diffs two lists at `path`, `depth` levels deep: an LCS match when both hold only scalars,
/// else the index-aligned comparison. See `docs/design/list-diff.md`, "Condition and candidate
/// selection".
///
/// # Stack-footprint note
///
/// The scalar branch's [`Report`] locals live in [`lcs_or_positional_array_diff`], off this frame.
/// Plain-list `DEFAULT_MAX_DEPTH` fits a 2 MiB thread in debug (`docs/design/depth-budget.md`).
pub(crate) fn array_diff(
    path: &mut Vec<PathSegment>,
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    if opts.ignore_order {
        return ignore_order::ignore_order_array_diff(path, a, b, depth, opts, memo);
    }
    if lcs::all_basic_scalars(a) && lcs::all_basic_scalars(b) {
        lcs_or_positional_array_diff(path, a, b, depth, opts, memo)
    } else {
        positional_array_diff(path, a, b, depth, opts, memo)
    }
}
fn lcs_or_positional_array_diff(
    path: &mut Vec<PathSegment>,
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    let lcs_report = lcs_array_diff(path, a, b, depth, opts.max_depth)?;
    if lcs_report.finding_count() > 1 {
        let positional_report = positional_array_diff(path, a, b, depth, opts, memo)?;
        if lcs_report.finding_count() >= positional_report.finding_count() {
            return Ok(positional_report);
        }
    }
    Ok(lcs_report)
}
#[derive(Clone, Copy)]
struct LcsPair<'a> {
    old_idx: usize,
    new_idx: usize,
    old_value: &'a Value,
    new_value: &'a Value,
}
/// Records the `values_changed` or `type_changes` finding for one pair matched by an LCS
/// `Replace` opcode, at `old_idx`, with `new_path` set when `new_idx` differs. A naive/aware
/// datetime pair at the same instant records nothing. Checks traversal depth only. See
/// `docs/design/list-diff.md`, "Opcode-to-finding mapping".
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

        if let (Value::DateTime(old_typed), Value::DateTime(new_typed)) = (old_value, new_value) {
            // A `datetime` subclass (e.g. pandas `Timestamp`) still reports
            // `type_changes` against a base `datetime` (or a differently
            // named subclass) even at equal value — see `Typed`'s doc — so
            // this check must run before the normalize-and-compare path
            // below, which only ever produces `values_changed`.
            if old_typed.class_name() != new_typed.class_name() {
                report.insert_type_change(
                    path.clone(),
                    TypeChangeEntry {
                        old_type: effective_type_name(old_value),
                        new_type: effective_type_name(new_value),
                        old_value: old_value.clone(),
                        new_value: new_value.clone(),
                        new_path,
                    },
                );
                return Ok(());
            }

            let (old_norm, new_norm) = normalized_pair(path, old_typed.value(), new_typed.value())?;

            if old_norm != new_norm {
                report.insert_values_changed(
                    path.clone(),
                    ValuesChangedEntry {
                        diff: None,
                        old_value: Value::DateTime(old_norm.into()),
                        new_value: Value::DateTime(new_norm.into()),
                        new_path,
                    },
                );
            }

            return Ok(());
        }

        if python_type_name(old_value) == python_type_name(new_value)
            && class_name(old_value) == class_name(new_value)
        {
            report.insert_values_changed(
                path.clone(),
                ValuesChangedEntry {
                    diff: crate::unified_diff::str_diff_field(old_value, new_value),
                    old_value: old_value.clone(),
                    new_value: new_value.clone(),
                    new_path,
                },
            );
        } else {
            report.insert_type_change(
                path.clone(),
                TypeChangeEntry {
                    old_type: effective_type_name(old_value),
                    new_type: effective_type_name(new_value),
                    old_value: old_value.clone(),
                    new_value: new_value.clone(),
                    new_path,
                },
            );
        }
        Ok(())
    })
}
/// Diffs two scalar lists via a `difflib`-style LCS match.
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
/// The index-aligned comparison: same-index pairs recurse through [`diff_at`]; the longer list's
/// surplus tail becomes `iterable_item_removed`/`iterable_item_added` at its original indices.
/// Each surplus clone is checked with [`check_value_depth`] first
/// (`docs/design/depth-budget.md`).
fn positional_array_diff(
    path: &mut Vec<PathSegment>,
    a: &[Value],
    b: &[Value],
    depth: usize,
    opts: &DiffOptions,
    memo: &IgnoreOrderMemo,
) -> Result<Report, Error> {
    let mut report = Report::new();
    let min_len = a.len().min(b.len());

    for i in 0..min_len {
        let sub_report = scoped(path, PathSegment::Index(i), |path| {
            diff_at(path, &a[i], &b[i], depth + 1, opts, memo)
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
