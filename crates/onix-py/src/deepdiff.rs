//! The drop-in `DeepDiff` class: accepts live Python objects, converts them
//! to `onix_core`'s value model exactly once, diffs natively, and exposes
//! the result as `.to_json()`/`.to_dict()`. Supported types and every
//! raised error: `crate::convert`'s module doc.

use onix_core::Value;
use onix_core::diff::Resolution;
use pyo3::prelude::*;

use crate::convert::{
    Held, objects_by_identity, render_report, resolve_token, to_value, token_error,
    value_to_pyobject,
};
use crate::guard::{diff_to_value, is_deep, resolve_options, run_on_worker, serialize_value};

/// A drop-in subset of `deepdiff.DeepDiff`, diffing `t1`/`t2` at
/// `verbose_level=2`. `max_depth` defaults to 512, capped at
/// `MAX_DEPTH_CEILING` (else `ValueError`); past it raises `MaxDepthError`;
/// deeper-than-inline input diffs on a sized worker thread. Supported types
/// and every raised error are listed in the module doc of
/// `crates/onix-py/src/convert.rs` in the onix repository; the depth bound and
/// its errors in `crates/onix-py/src/guard.rs`.
#[pyclass(module = "deepdiff_rs")]
pub(crate) struct DeepDiff {
    report_value: Value,
    /// Whether `report_value` needs the worker thread to render; computed once
    /// so `to_json` never re-walks it. See `crate::guard::is_deep`.
    report_is_deep: bool,
    /// Whether either input held a lone surrogate code point, found during
    /// conversion; lets `to_json` skip its own WTF-8 tree walk when `false`.
    may_have_wtf8: bool,
}

#[pymethods]
impl DeepDiff {
    #[new]
    #[pyo3(signature = (t1, t2, ignore_order=false, max_depth=None))]
    fn new(
        py: Python<'_>,
        t1: &Bound<'_, PyAny>,
        t2: &Bound<'_, PyAny>,
        ignore_order: bool,
        max_depth: Option<usize>,
    ) -> PyResult<Self> {
        let opts = resolve_options(max_depth, ignore_order)?;
        // Conversion stays here (needs the GIL); if `t2` fails, `?` drops a
        // deep `a`, safe because `Value`'s `Drop` is iterative.
        let mut held = Held::new(opts.max_depth);
        let (a, a_may_have_wtf8) = to_value(t1, opts.max_depth, &mut held)?;
        let (b, b_may_have_wtf8) = to_value(t2, opts.max_depth, &mut held)?;
        // Shallow inputs diff inline; deeper ones move to the sized worker
        // thread (GIL released) — see `crate::guard`.
        // A class attribute or cycle token is resolved the first time the
        // diff compares one, then the diff reruns with it available.
        let mut index = None;
        let mut deep = false;
        let report_value = loop {
            let report_value = {
                let held = &mut held;
                let index = &mut index;
                let (a, b) = (&a, &b);
                let mut resolver = |identity: &str| {
                    if held.cycle_targets.contains(identity) {
                        let index = index.get_or_insert_with(|| objects_by_identity(&[a, b]));
                        if let Some(value) = index.get(identity) {
                            return Some(Resolution::Borrowed(value));
                        }
                    }
                    Python::attach(|py| resolve_token(py, identity, held, !deep))
                        .map(Resolution::Shared)
                };
                diff_to_value(py, a, b, opts, &mut resolver, deep)?
            };
            if held.needs_worker && !deep {
                deep = true;
                continue;
            }
            break report_value;
        };
        if let Some(err) = held.interrupt(py) {
            return Err(err);
        }
        let report_value = if held.needs_render {
            let rendered = if is_deep(&report_value) {
                run_on_worker(py, || render_report(&report_value))?
            } else {
                render_report(&report_value)
            };
            match rendered {
                Ok(rendered) => rendered,
                Err(tokens) => {
                    let token = &tokens[0];
                    return Err(token_error(
                        py,
                        &token.identity,
                        &token.type_name,
                        &token.path,
                        &held,
                    ));
                }
            }
        } else {
            report_value
        };
        let report_is_deep = is_deep(&report_value);
        // Conservative: the report only ever carries values already in
        // `t1`/`t2`, so this may overcount but never undercounts.
        let may_have_wtf8 = a_may_have_wtf8 || b_may_have_wtf8 || held.saw_wtf8;

        Ok(Self {
            report_value,
            report_is_deep,
            may_have_wtf8,
        })
    }

    /// The report as a `DeepDiff`-compatible JSON string at
    /// `verbose_level=2`; a deep report renders on the sized worker thread
    /// rather than inline. Differences from `DeepDiff`'s rendering are
    /// documented in the onix repository's `tests/golden/README.md`, 'The
    /// `date` superset', 'The `time`/`timedelta` superset' and 'Set
    /// iteration order' sections.
    fn to_json(&self, py: Python<'_>) -> PyResult<String> {
        serialize_value(
            py,
            &self.report_value,
            self.report_is_deep,
            self.may_have_wtf8,
        )
    }

    /// The report as a native Python `dict`, with Python types (tuples, sets,
    /// datetimes) intact rather than rendered to JSON; conversion is iterative
    /// (`value_to_pyobject` in `crates/onix-py/src/convert.rs`), safe at any
    /// depth. Differences from `DeepDiff`'s `to_dict()` are documented in the
    /// onix repository's `tests/golden/README.md`: its 'Normalized versus raw
    /// datetimes' section, 'Fixed-offset `tzinfo` round-trip' point, and its
    /// 'Known `DeepDiff` quirks' section, '`to_dict()` reports a
    /// `type_changes` entry's types as names, not classes' point.
    fn to_dict(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        value_to_pyobject(py, &self.report_value)
    }

    /// `if diff:` truthiness — falsy exactly when `t1`/`t2` had no
    /// differences (an empty report).
    fn __bool__(&self) -> bool {
        !is_empty_report(&self.report_value)
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("DeepDiff({})", self.to_json(py)?))
    }
}

/// A [`DeepDiff`] report renders to an empty object via
/// [`onix_core::Report::to_value`] when there are no findings — see that
/// function's own doc.
fn is_empty_report(value: &Value) -> bool {
    matches!(value, Value::Object(map) if map.is_empty())
}
