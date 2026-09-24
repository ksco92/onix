//! The drop-in `DeepDiff` class: accepts live Python objects, converts them
//! to `onix_core`'s value model exactly once, diffs natively, and exposes
//! the result as `.to_json()`/`.to_dict()` — see this module's `DeepDiff`
//! doc for the full, documented MVP surface.

use onix_core::Value;
use onix_core::diff::Resolution;
use pyo3::prelude::*;

use crate::convert::{
    Held, objects_by_identity, render_report, resolve_token, to_value, token_error,
    value_to_pyobject,
};
use crate::guard::{diff_to_value, is_deep, resolve_options, run_on_worker, serialize_value};

/// A drop-in subset of `deepdiff.DeepDiff`.
///
/// ```text
/// from deepdiff_rs import DeepDiff
///
/// diff = DeepDiff({"a": 1}, {"a": 2})
/// if diff:
///     print(diff.to_json())
/// ```
///
/// # Constructor
///
/// `DeepDiff(t1, t2, ignore_order=False, max_depth=None)`:
///
/// - `t1`/`t2`: any of `None`, `bool`, `int`, `float`
///   (`NaN`/`Infinity`/`-Infinity` included), `str` (a lone, unpaired surrogate
///   code point — legal in Python, not encodable as UTF-8 — is accepted and
///   compared like any other `str`; see `tests/golden/README.md`'s lone-surrogate
///   bullet), `dict` (a key may be `str`, `None`, `bool`, `int`, `float`,
///   `datetime.datetime`, `datetime.date`, or a `tuple` of those, never nested),
///   `list`, `tuple`, `set`, `frozenset`, `datetime.datetime`, `datetime.date`,
///   `datetime.time`, or `datetime.timedelta`, arbitrarily nested, and a
///   *subclass* of the last nine (a `namedtuple`, a `set` subclass, a pandas
///   `Timestamp`), which converts and compares as its base type but carries its
///   own class name into a `type_changes` entry — because `DeepDiff` reports
///   every value under its own type name — with one divergence: a `namedtuple`
///   diffs positionally, not by field (see `tests/golden/README.md`'s "Known
///   `DeepDiff` quirks" section). A `set`/`frozenset` member is restricted further,
///   to whichever of the above are hashable in Python — every type except `list`,
///   `dict` and `set` — plus a `datetime`/`date`/`time`/`timedelta` subclass, but
///   not a `tuple`/`frozenset` subclass or a `namedtuple`; the restriction is
///   transitive: a `list`, `dict` or `set` anywhere inside a set member is
///   refused. A user-defined class instance (and an `Enum` member) is diffed as a
///   **custom object**, by its attributes, matching `DeepDiff`'s `_diff_obj`
///   (`attribute_added`/`attribute_removed`, `root.attr` paths, `type_changes`
///   between classes; see `tests/golden/README.md`'s "Custom objects" section for
///   the enumeration and its documented divergences). Converted to `onix_core`'s
///   value model exactly once, up front — see `crate::convert`'s module doc for
///   the full conversion table and every error this can raise: `TypeError` for a
///   value `DeepDiff` routes to a handler onix lacks (a number such as
///   `complex`/`Decimal`, `bytes`/`bytearray` or any other iterable, `uuid`,
///   `ipaddress`, a class object, a module, or a bare attribute-less object), for
///   a `dict` key or `set` member of an unsupported type, or for a
///   `tuple`/`frozenset` subclass as a set member; `ValueError` for an
///   out-of-range int, a non-finite float, or a sub-second UTC offset.
/// - `ignore_order`: mirrors `DeepDiff(..., ignore_order=True)`.
/// - `max_depth`: caller-chosen recursion-depth bound; defaults to
///   `onix_core::DEFAULT_MAX_DEPTH` (512) when omitted. Exceeding it —
///   during either the Python-object conversion above or the diff itself —
///   raises `deepdiff_rs.MaxDepthError` (a `ValueError` subclass), never a
///   native crash. `max_depth` itself may not exceed
///   `deepdiff_rs.MAX_DEPTH_CEILING` (see `crate::guard`): a larger value is
///   rejected up front with a plain `ValueError`, because the recursive diff
///   engine cannot safely run past that depth. The diff runs on a
///   stack-sized worker thread (GIL released) so that no in-range
///   `max_depth`, however high, can overflow the native stack — see
///   `crate::guard`'s module doc.
///
/// This intentionally does not attempt `deepdiff.DeepDiff`'s full option
/// surface (`exclude_paths`, `significant_digits`, custom operators,
/// `verbose_level`, …) — this is the documented MVP surface, matched at
/// `verbose_level=2` (the level `onix_core`'s report shape always
/// corresponds to).
#[pyclass(module = "deepdiff_rs")]
pub(crate) struct DeepDiff {
    report_value: Value,
    /// Whether `report_value` is nested past the inline-depth threshold, and
    /// so must be rendered to JSON on the sized worker thread rather than the
    /// calling thread. Computed once in `new`, so repeated `to_json` calls do
    /// not each re-walk the report. See `crate::guard::is_deep`.
    report_is_deep: bool,
    /// Whether either input held a lone surrogate code point — a byproduct
    /// of `crate::convert::to_value`'s own walk (see its doc), never a
    /// second pass. Lets `to_json` skip `onix_core::value::contains_wtf8`'s
    /// own tree walk when this is `false`, the overwhelming common case.
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
        // Conversion is iterative (no native recursion — see `crate::convert`)
        // and needs the GIL to read the live Python objects, so it stays on
        // the calling thread. It builds the compact `onix_core::Value`
        // directly. If converting `t2` fails after `a` is already a (possibly
        // deep) legal value, the `?` drops `a` here on the early return — its
        // iterative `Drop` cannot overflow the calling thread, so no
        // sized-worker hand-off is needed for it.
        let mut held = Held::new(opts.max_depth);
        let (a, a_may_have_wtf8) = to_value(t1, opts.max_depth, &mut held)?;
        let (b, b_may_have_wtf8) = to_value(t2, opts.max_depth, &mut held)?;
        // The diff is natively recursive: it runs inline when both inputs are
        // shallow, else on the stack-sized worker (GIL released). The report
        // comes back in the same compact value model the inputs use, so it can
        // carry a tuple all the way out to `to_dict`.
        // A class attribute stays an opaque token until the report compares
        // it; each such token is converted and the diff run again.
        // A class-attribute or cycle token is resolved by the diff the first
        // time it compares one: a cycle target is lent from the inputs, a
        // class attribute converted once under the GIL.
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
        // A conservative upper bound: the report only ever carries values
        // (or coerced copies, which coercion always renders as plain UTF-8
        // — see `crate::guard`) that already existed in `t1`/`t2`, so
        // neither input holding one guarantees the report holds none.
        let may_have_wtf8 = a_may_have_wtf8 || b_may_have_wtf8 || held.saw_wtf8;

        Ok(Self {
            report_value,
            report_is_deep,
            may_have_wtf8,
        })
    }

    /// Byte-compatible with real `DeepDiff(...).to_json()` at
    /// `verbose_level=2` — the whole point of this crate. A tuple or a set
    /// renders as the JSON array `DeepDiff`'s own `to_json()` shows for one,
    /// a datetime as the `isoformat()` string it shows for one, and the
    /// `set_item_added`/`set_item_removed` categories as arrays of path
    /// strings. Four documented differences, all in
    /// `tests/golden/README.md`: `datetime.date`/`datetime.time` render as
    /// `isoformat()`'s bytes and `datetime.timedelta` as `str()`'s, and a
    /// report holding a `frozenset` renders as an array, all where real
    /// `DeepDiff`'s `to_json()` raises `TypeError`; and a set's members (and
    /// the two set categories' entries) come out in `onix`'s canonical order
    /// rather than the process's own unreproducible set iteration order (see
    /// that file's "Set iteration order" section).
    ///
    /// Rendering a report to JSON is natively recursive, so a report deep
    /// enough to matter is rendered on the sized worker thread; a shallow one
    /// (the overwhelmingly common case) renders inline. See
    /// `crate::guard::serialize_value`.
    fn to_json(&self, py: Python<'_>) -> PyResult<String> {
        serialize_value(
            py,
            &self.report_value,
            self.report_is_deep,
            self.may_have_wtf8,
        )
    }

    /// The report as a native Python `dict` — [`Self::to_json`]'s content
    /// with the Python types intact rather than their JSON renderings, so a
    /// value the diff found in a `tuple`, `set` or `frozenset` comes back as
    /// one and a datetime comes back as a real `datetime.datetime`, exactly
    /// as real `DeepDiff`'s own `to_dict()` does. Two documented
    /// differences: type *names* in a `type_changes` entry stay strings
    /// here, where real `DeepDiff` returns the type objects themselves, and
    /// an aware datetime carries a fixed-offset `datetime.timezone` rather
    /// than whatever `tzinfo` class it went in with (see
    /// `tests/golden/README.md`'s "Fixed-offset tzinfo round-trip" section).
    /// Conversion back to Python objects is iterative
    /// ([`crate::convert::value_to_pyobject`]), safe at any depth.
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
