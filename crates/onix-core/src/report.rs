//! The [`Report`] type: a DeepDiff-compatible diff result.
//!
//! `DeepDiff` groups findings into named categories (`values_changed`,
//! `type_changes`, and others) keyed by path string within each
//! category. `Report` mirrors that shape with one `BTreeMap` per category,
//! which keeps output deterministic (sorted by path) without any extra
//! sorting step. Empty categories are omitted from [`Report::to_json_value`],
//! matching `DeepDiff`'s own `to_json()` behavior.
//!
//! # Structural keys, not rendered strings
//!
//! Each category is keyed by the *structural* path (`Vec<PathSegment>`) the
//! traversal visited, not by [`render_path`]'s rendered `String`. This
//! matters because [`render_path`]/[`crate::path::quote_key`] are **not** injective on
//! adversarial input: a dict key whose own text happens to contain `']['`-
//! shaped syntax can render identically to an unrelated, differently-nested
//! path (confirmed against real `DeepDiff`, which has the same property —
//! see `tests/golden/README.md`'s "known `DeepDiff` quirk" section for a
//! worked example). Keying by the structural path instead means:
//!
//! - `insert_checked`'s duplicate-path guard only ever fires on a genuine
//!   *engine* bug (the same node visited twice in one traversal) — never on
//!   two legitimately different nodes whose rendered strings happen to
//!   collide. The old string-keyed version could `debug_assert`-panic on
//!   that legitimate input; that was the bug this module fixes.
//! - The one-true-collision-handling step moves to serialization time:
//!   [`Report::to_json_value`] renders each structural key and inserts into
//!   a fresh per-category map, so two structural paths that render
//!   identically collapse into a single JSON entry — the *same* outcome
//!   `DeepDiff` itself has (its `to_json()` is also a string-keyed dict, so
//!   a rendering collision collapses there too). Which of the colliding
//!   findings survives is an internal, structural-order tie-break (see
//!   [`PathSegment`]'s doc) that is not guaranteed to match `DeepDiff`'s own
//!   (insertion-order-dependent) survivor choice on such input — an
//!   accepted, documented divergence rather than a bug to chase, since
//!   matching it would require threading original JSON key order through
//!   the whole engine for a vanishingly rare edge case. See
//!   `tests/golden/README.md`.

use std::collections::BTreeMap;

use crate::value::{Builder, Value};

use crate::path::{PathSegment, render_path};

/// A single `values_changed` entry: a scalar value changed but its type did
/// not.
#[derive(Debug, Clone, PartialEq)]
pub struct ValuesChangedEntry {
    /// The value before the change.
    pub old_value: Value,
    /// The value after the change.
    pub new_value: Value,
    /// The *structural* path this finding would sit at if keyed by the
    /// *new* value's position instead of the old one, when the two differ
    /// — rendered to a string only at serialization (`to_json_value`) time.
    ///
    /// `DeepDiff` renders every finding's path from the *old* (`t1`) side by
    /// default; at `verbose_level=2` it additionally reports `new_path`
    /// whenever the new-side path would differ. This is `None` whenever old
    /// and new paths coincide — a dict key never moves, and index-aligned
    /// list comparison always pairs same-index elements. It becomes `Some`
    /// for a
    /// `values_changed`/`type_changes` pair matched by the list-LCS path
    /// (see [`mod@crate::diff`]'s module doc) at two *different* absolute
    /// indices, e.g. a value that shifted from index `5` to index `3`
    /// because of an earlier insert/delete elsewhere in the same list.
    ///
    /// Kept as structural segments (not a pre-rendered `String`) so
    /// `Report::retag_new_path` (crate-private) can *compose* more than one independent
    /// index substitution (one per `ignore_order` list level a doubly- or
    /// triply-nested pairing crosses) by mutating a single segment in
    /// place, rather than needing to re-parse an already-rendered path
    /// string — see that method's own doc for the case this fixes.
    pub new_path: Option<Vec<PathSegment>>,
}

impl ValuesChangedEntry {
    fn to_value(&self, builder: &mut Builder) -> Value {
        let mut entries = vec![
            ("new_value".to_string(), self.new_value.clone()),
            ("old_value".to_string(), self.old_value.clone()),
        ];
        if let Some(new_path) = &self.new_path {
            entries.push(("new_path".to_string(), rendered(new_path)));
        }
        builder.object(entries)
    }
}

/// A single `type_changes` entry: the Python type itself changed between the
/// two values (e.g. `int` to `str`).
#[derive(Debug, Clone, PartialEq)]
pub struct TypeChangeEntry {
    /// The Python type name of the old value (e.g. `"int"`).
    pub old_type: String,
    /// The Python type name of the new value (e.g. `"str"`).
    pub new_type: String,
    /// The value before the change.
    pub old_value: Value,
    /// The value after the change.
    pub new_value: Value,
    /// See [`ValuesChangedEntry::new_path`]'s doc — the same mechanism,
    /// shared by both categories.
    pub new_path: Option<Vec<PathSegment>>,
}

impl TypeChangeEntry {
    fn to_value(&self, builder: &mut Builder) -> Value {
        let mut entries = vec![
            (
                "old_type".to_string(),
                Value::Str(self.old_type.clone().into_boxed_str()),
            ),
            (
                "new_type".to_string(),
                Value::Str(self.new_type.clone().into_boxed_str()),
            ),
            ("old_value".to_string(), self.old_value.clone()),
            ("new_value".to_string(), self.new_value.clone()),
        ];
        if let Some(new_path) = &self.new_path {
            entries.push(("new_path".to_string(), rendered(new_path)));
        }
        builder.object(entries)
    }
}

/// A structural path rendered into the string [`Value`] a report entry
/// carries it as (`new_path`).
fn rendered(path: &[PathSegment]) -> Value {
    Value::Str(render_path(path).into_boxed_str())
}

/// A DeepDiff-compatible diff result, grouped into categories.
///
/// Categories implemented so far: `type_changes`, `values_changed`,
/// `dictionary_item_added`, `dictionary_item_removed`,
/// `iterable_item_added`, `iterable_item_removed`. Further categories would
/// be added the same way (an additive change, no restructuring of existing
/// ones).
///
/// Every category is keyed by the structural path (`Vec<PathSegment>`), not
/// the rendered string — see this module's doc for why.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    type_changes: BTreeMap<Vec<PathSegment>, TypeChangeEntry>,
    values_changed: BTreeMap<Vec<PathSegment>, ValuesChangedEntry>,
    dictionary_item_added: BTreeMap<Vec<PathSegment>, Value>,
    dictionary_item_removed: BTreeMap<Vec<PathSegment>, Value>,
    iterable_item_added: BTreeMap<Vec<PathSegment>, Value>,
    iterable_item_removed: BTreeMap<Vec<PathSegment>, Value>,
}

/// Inserts `value` at `path` into `map`, debug-asserting `path` wasn't
/// already present.
///
/// `path` is the *structural* path (see this module's doc), so a duplicate
/// here means the traversal visited the exact same node twice in one
/// [`crate::diff::diff`] call — a genuine engine bug, never a legitimate
/// rendered-string collision (those are handled separately, at
/// serialization time, in [`Report::to_json_value`]). `debug_assert!` is a
/// no-op in release builds, so this has no effect on release behavior or
/// performance.
fn insert_checked<V>(map: &mut BTreeMap<Vec<PathSegment>, V>, path: Vec<PathSegment>, value: V) {
    debug_assert!(!map.contains_key(&path), "duplicate report path: {path:?}");
    map.insert(path, value);
}

/// Merges `src` into `dst`, one entry at a time through [`insert_checked`]
/// (not a blind [`BTreeMap::extend`]) so the duplicate-path debug assertion
/// still fires on a collision. Shared by [`Report::merge`]'s four raw-`Value`
/// categories (`dictionary_item_added`/`removed`, `iterable_item_added`/
/// `removed`), which otherwise differ only in which field they merge.
fn merge_map(dst: &mut BTreeMap<Vec<PathSegment>, Value>, src: BTreeMap<Vec<PathSegment>, Value>) {
    for (path, value) in src {
        insert_checked(dst, path, value);
    }
}

/// Pushes `map` onto `root` under `name` as one rendered category, skipping
/// it entirely when `map` is empty (matching `DeepDiff`'s own `to_json()`
/// behavior of omitting empty categories). Shared by [`Report::to_value`]'s
/// four raw-`Value` categories (`dictionary_item_added`/`removed`,
/// `iterable_item_added`/`removed`), which otherwise differ only in `name`
/// and which field they read.
///
/// Rendering the structural keys **collapses any rendered-string collision**
/// (two structural paths whose [`render_path`] output is identical — see
/// this module's doc) into a single entry: [`Builder::object`] keeps the
/// last value seen for a repeated key, so the survivor is whichever
/// structural path is greatest, i.e. the *last* one visited in `map`'s
/// ascending structural order. This is the one place that tie-break happens;
/// everywhere else in this file treats `map`'s structural keys as
/// already-unique (which, structurally, they always are — see
/// [`insert_checked`]).
fn push_raw_category(
    root: &mut Vec<(String, Value)>,
    builder: &mut Builder,
    name: &str,
    map: &BTreeMap<Vec<PathSegment>, Value>,
) {
    if map.is_empty() {
        return;
    }
    let entries: Vec<(String, Value)> = map
        .iter()
        .map(|(path, value)| (render_path(path), value.clone()))
        .collect();
    root.push((name.to_string(), builder.object(entries)));
}

impl Report {
    /// Creates an empty report.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records a `type_changes` finding at the structural `path`.
    pub(crate) fn insert_type_change(&mut self, path: Vec<PathSegment>, entry: TypeChangeEntry) {
        insert_checked(&mut self.type_changes, path, entry);
    }

    /// Records a `values_changed` finding at the structural `path`.
    pub(crate) fn insert_values_changed(
        &mut self,
        path: Vec<PathSegment>,
        entry: ValuesChangedEntry,
    ) {
        insert_checked(&mut self.values_changed, path, entry);
    }

    /// Records a `dictionary_item_added` finding at the structural `path`:
    /// `value` is the added value itself (not wrapped in an old/new-value
    /// object), matching `DeepDiff`'s `to_json()` shape at
    /// `verbose_level=2`.
    pub(crate) fn insert_dictionary_item_added(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(&mut self.dictionary_item_added, path, value);
    }

    /// Records a `dictionary_item_removed` finding at the structural `path`:
    /// `value` is the removed value itself (not wrapped in an old/new-value
    /// object), matching `DeepDiff`'s `to_json()` shape at
    /// `verbose_level=2`.
    pub(crate) fn insert_dictionary_item_removed(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(&mut self.dictionary_item_removed, path, value);
    }

    /// Records an `iterable_item_added` finding at the structural `path`:
    /// `value` is the added value itself (not wrapped in an old/new-value
    /// object), matching `DeepDiff`'s `to_json()` shape at
    /// `verbose_level=2`.
    pub(crate) fn insert_iterable_item_added(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(&mut self.iterable_item_added, path, value);
    }

    /// Records an `iterable_item_removed` finding at the structural `path`:
    /// `value` is the removed value itself (not wrapped in an old/new-value
    /// object), matching `DeepDiff`'s `to_json()` shape at
    /// `verbose_level=2`.
    pub(crate) fn insert_iterable_item_removed(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(&mut self.iterable_item_removed, path, value);
    }

    /// Folds another report's findings into `self`, one entry at a time
    /// (through the guarded `insert_*` methods for `type_changes`/
    /// `values_changed`, and through [`merge_map`] — which shares the same
    /// [`insert_checked`] guard — for the four raw-`Value` categories) so
    /// the duplicate-path debug assertion still fires if two subtrees ever
    /// produce the exact same *structural* path (a genuine engine bug: each
    /// node in a traversal is visited exactly once, so this cannot happen
    /// today — see [`crate::diff::object_diff`]'s doc). This is independent
    /// of whether two *different* structural paths render to the same
    /// string, which is expected on some input and handled separately at
    /// serialization time (see this module's doc).
    pub(crate) fn merge(&mut self, other: Report) {
        for (path, entry) in other.type_changes {
            self.insert_type_change(path, entry);
        }
        for (path, entry) in other.values_changed {
            self.insert_values_changed(path, entry);
        }
        merge_map(&mut self.dictionary_item_added, other.dictionary_item_added);
        merge_map(
            &mut self.dictionary_item_removed,
            other.dictionary_item_removed,
        );
        merge_map(&mut self.iterable_item_added, other.iterable_item_added);
        merge_map(&mut self.iterable_item_removed, other.iterable_item_removed);
    }

    /// `DeepDiff`'s global "mutual add/remove becomes a value change" pass
    /// (`model.py::TreeResult.mutual_add_removes_to_become_value_changes`,
    /// invoked once, after the whole diff tree is built, from
    /// `DeepDiff._get_view_results` whenever `report_repetition=False` —
    /// always, for this engine).
    ///
    /// Whenever an `iterable_item_added` and an `iterable_item_removed`
    /// finding render to the **exact same path string** (`DeepDiff` matches
    /// by `i.path()`, the rendered string, not a structural identity — see
    /// [`crate::path::render_path`]), they are purely coincidental
    /// same-slot events, not a real pairing: this collapses each such pair
    /// into one `values_changed` (`old_value` from the removed side,
    /// `new_value` from the added side) and removes both originals.
    ///
    /// **Always produces `values_changed`, never `type_changes`, regardless
    /// of whether the two values' types differ** — confirmed against real
    /// `DeepDiff`: its own merge (`_from_tree_value_changed`) never
    /// inspects type at all, unlike the ordinary scalar-comparison paths
    /// elsewhere in this engine. **Never attaches `new_path`**: `DeepDiff`'s
    /// merged level reuses the *removed* side's own `t2_child_rel` (`None`,
    /// since the removed item never had a `t2`), so its new-side path
    /// resolution falls back to the same string as its old-side path —
    /// confirmed empirically (`DeepDiff` never emits `new_path` on a
    /// merge-produced `values_changed`, even when the removed/added values
    /// sit at structurally distant original indices).
    ///
    /// Matching by *rendered string* rather than structural path is a
    /// deliberate fidelity choice, not an approximation: for the
    /// `iterable_item_added`/`removed` categories specifically, a path is
    /// always an ancestor prefix plus one trailing numeric index (no key
    /// quoting involved at the final segment), so a rendered-string
    /// collision without a structural one could only arise from an
    /// ancestor prefix collision — the same, already-documented and
    /// accepted `render_path` non-injectivity class described in this
    /// module's own doc and `tests/golden/README.md`'s "known `DeepDiff`
    /// quirks" section, not a new divergence this pass introduces.
    ///
    /// Runs once, globally, over the whole tree — called from
    /// [`crate::diff::diff_with_max_depth`] after the entire recursive
    /// traversal completes, matching `DeepDiff`'s own timing exactly (never
    /// from inside `array_diff`'s per-list tie-break, which must still
    /// compare *pre-merge* finding counts, exactly like `DeepDiff`'s own
    /// per-list decision happens before this whole-tree pass ever runs).
    pub(crate) fn merge_mutual_add_removes(&mut self) {
        let removed_by_rendered: BTreeMap<String, Vec<PathSegment>> = self
            .iterable_item_removed
            .keys()
            .map(|path| (render_path(path), path.clone()))
            .collect();

        let colliding_paths: Vec<(Vec<PathSegment>, Vec<PathSegment>)> = self
            .iterable_item_added
            .keys()
            .filter_map(|added_path| {
                removed_by_rendered
                    .get(&render_path(added_path))
                    .map(|removed_path| (added_path.clone(), removed_path.clone()))
            })
            .collect();

        for (added_path, removed_path) in colliding_paths {
            let new_value = self
                .iterable_item_added
                .remove(&added_path)
                .expect("added_path was just read from this same map");
            let old_value = self
                .iterable_item_removed
                .remove(&removed_path)
                .expect("removed_path was just read from removed_by_rendered, built from this map");
            self.insert_values_changed(
                removed_path,
                ValuesChangedEntry {
                    old_value,
                    new_value,
                    new_path: None,
                },
            );
        }
    }

    /// Rewrites [`ValuesChangedEntry::new_path`]/[`TypeChangeEntry::new_path`]
    /// for every `values_changed`/`type_changes` finding currently in `self`
    /// to reflect an ancestor list-index substitution: the entry's own
    /// structural path had `PathSegment::Index(old_idx)` at position
    /// `prefix_depth` when it was recorded, and `new_path` should resolve as
    /// if that one segment had instead been `PathSegment::Index(new_idx)`,
    /// with every other segment (including anything further down inside
    /// this finding's own subtree, and including any segment a *different*,
    /// independent substitution already rewrote) unchanged.
    ///
    /// Used by [`crate::ignore_order`]'s paired-item recursion: `DeepDiff`
    /// attaches `new_path` to *every* `values_changed`/`type_changes`
    /// finding inside a hash-paired list item whose old (`t1`) and new
    /// (`t2`) indices differ — not just a single top-level one — confirmed
    /// empirically against real `DeepDiff` (a nested field change two levels
    /// inside a paired dict still carries `new_path` with the outer index
    /// swapped and the rest of the path identical).
    ///
    /// **Composes with an already-retagged entry, rather than skipping it.**
    /// A finding whose `new_path` is already `Some` (set by a *deeper*,
    /// independent index substitution — e.g. a nested `ignore_order` list
    /// inside this same paired item that itself needed pairing) starts this
    /// call's substitution from that *already-substituted* structural
    /// vector, not from the entry's own base `path` key — so an outer call
    /// composes cleanly on top of an inner one instead of clobbering or
    /// ignoring it. This matters for doubly-nested drift: an item whose *own*
    /// recursive diff needs a nested `ignore_order` pairing, inside an item
    /// that *itself* needs pairing with index drift, must carry *both* index
    /// substitutions in its `new_path`, composing the outer and inner
    /// rewrites rather than letting the outer one overwrite the inner.
    /// `new_path` is kept as structural segments rather than a pre-rendered
    /// string specifically so this composition is a plain "overwrite one
    /// element" mutation, not string surgery on an already-rendered,
    /// possibly-quoted path.
    ///
    /// `dictionary_item_added`/`removed`/`iterable_item_added`/`removed`
    /// never carry a second path field at all (confirmed empirically), so
    /// this only ever touches the two value-comparison categories.
    pub(crate) fn retag_new_path(&mut self, prefix_depth: usize, new_idx: usize) {
        for (path, entry) in &mut self.values_changed {
            let base = entry.new_path.get_or_insert_with(|| path.clone());
            base[prefix_depth] = PathSegment::Index(new_idx);
        }
        for (path, entry) in &mut self.type_changes {
            let base = entry.new_path.get_or_insert_with(|| path.clone());
            base[prefix_depth] = PathSegment::Index(new_idx);
        }
    }

    /// `DeepDiff`'s `_get_item_length` (distance.py) applied to a whole
    /// trial-diff report — [`crate::ignore_order::rough_distance`]'s
    /// structural-fallback numerator (`diff_length`).
    ///
    /// Mirrors exactly which sub-value `_get_item_length` would see for
    /// each report category once its own key-exclusion rule
    /// ([`crate::ignore_order::item_length`]'s doc) is applied: a
    /// `values_changed` entry contributes only its `new_value` (`old_value`/
    /// `new_path` are excluded keys); a `type_changes` entry delegates to
    /// [`crate::ignore_order::type_change_leaf_length`] (not a flat `1 +
    /// item_length(new_value)` — `new_value` is conditionally *omitted*
    /// entirely by real `DeepDiff`'s own delta view, see that function's
    /// doc; an earlier version of this method reimplemented an incomplete,
    /// special-cased version of that same rule inline, which is exactly
    /// the kind of duplication this crate's own conventions warn against);
    /// the four raw-value categories contribute their value in full (their
    /// own top-level key is a path string, never one of the excluded
    /// names).
    #[must_use]
    pub(crate) fn distance_leaf_length(&self) -> usize {
        let values_changed: usize = self
            .values_changed
            .values()
            .map(|entry| crate::ignore_order::item_length(&entry.new_value))
            .sum();
        let type_changes: usize = self
            .type_changes
            .values()
            .map(|entry| {
                crate::ignore_order::type_change_leaf_length(&entry.old_value, &entry.new_value)
            })
            .sum();
        let added_removed: usize = self
            .dictionary_item_added
            .values()
            .chain(self.dictionary_item_removed.values())
            .chain(self.iterable_item_added.values())
            .chain(self.iterable_item_removed.values())
            .map(crate::ignore_order::item_length)
            .sum();

        values_changed + type_changes + added_removed
    }

    /// Returns `true` if no differences were found in any category.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.type_changes.is_empty()
            && self.values_changed.is_empty()
            && self.dictionary_item_added.is_empty()
            && self.dictionary_item_removed.is_empty()
            && self.iterable_item_added.is_empty()
            && self.iterable_item_removed.is_empty()
    }

    /// The total number of findings across every category.
    ///
    /// Mirrors `DeepDiff`'s own `len(TreeResult)` (a flat count over every
    /// report category, not per-category) — used by the list-LCS path (see
    /// [`mod@crate::diff`]'s module doc) to pick between the LCS-matched and
    /// the plain index-aligned candidate report for a given list: `DeepDiff`
    /// runs both and keeps whichever has *fewer* total findings, favoring
    /// the index-aligned one on a tie.
    #[must_use]
    pub(crate) fn finding_count(&self) -> usize {
        self.type_changes.len()
            + self.values_changed.len()
            + self.dictionary_item_added.len()
            + self.dictionary_item_removed.len()
            + self.iterable_item_added.len()
            + self.iterable_item_removed.len()
    }

    /// Renders the report into the `DeepDiff` `to_json()` shape at
    /// `verbose_level=2`, as the crate's own [`Value`] model.
    ///
    /// This is the type-preserving rendering: a finding whose value is a
    /// [`Value::Tuple`] still carries a tuple here, where
    /// [`Self::to_json_value`] (and any JSON text rendered from it) can only
    /// show the array a tuple serializes as. A consumer that reconstructs
    /// native values from a report — the Python bindings' `to_dict()` —
    /// reads this; a consumer that only needs JSON reads
    /// [`Self::to_json_value`].
    ///
    /// Empty categories are omitted entirely (an empty report renders to an
    /// empty object), matching `DeepDiff`'s own behavior. Rendering each
    /// category's structural keys can collapse two entries into one on
    /// adversarial input (see this module's doc and [`push_raw_category`]).
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut builder = Builder::new();
        let mut root: Vec<(String, Value)> = Vec::new();

        if !self.type_changes.is_empty() {
            let entries: Vec<(String, Value)> = self
                .type_changes
                .iter()
                .map(|(path, entry)| (render_path(path), entry.to_value(&mut builder)))
                .collect();
            root.push(("type_changes".to_string(), builder.object(entries)));
        }

        if !self.values_changed.is_empty() {
            let entries: Vec<(String, Value)> = self
                .values_changed
                .iter()
                .map(|(path, entry)| (render_path(path), entry.to_value(&mut builder)))
                .collect();
            root.push(("values_changed".to_string(), builder.object(entries)));
        }

        push_raw_category(
            &mut root,
            &mut builder,
            "dictionary_item_added",
            &self.dictionary_item_added,
        );
        push_raw_category(
            &mut root,
            &mut builder,
            "dictionary_item_removed",
            &self.dictionary_item_removed,
        );
        push_raw_category(
            &mut root,
            &mut builder,
            "iterable_item_added",
            &self.iterable_item_added,
        );
        push_raw_category(
            &mut root,
            &mut builder,
            "iterable_item_removed",
            &self.iterable_item_removed,
        );

        builder.object(root)
    }

    /// Serializes the report to the `DeepDiff` `to_json()` shape at
    /// `verbose_level=2` as a [`serde_json::Value`] — [`Self::to_value`]
    /// rendered out to JSON's own value model, where a
    /// [`Value::Tuple`] becomes the JSON array `DeepDiff`'s own
    /// `to_json()` shows for a tuple.
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        self.to_value().to_serde_json()
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
