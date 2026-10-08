//! The [`Report`] type: a DeepDiff-compatible diff result, one
//! `BTreeMap` per finding category keyed by the *structural* path
//! (`Vec<PathSegment>`) the traversal visited, not [`render_path`]'s
//! rendered `String` (not injective on adversarial input). See
//! `tests/golden/README.md`'s "Path-rendering collision survivor" section for
//! the collision this avoids and its survivor rule.

use std::collections::BTreeMap;

use crate::value::{Builder, Str, Value};

use crate::path::{PathSegment, render_path};

/// A single `values_changed` entry: a scalar value changed but its type did
/// not.
#[derive(Debug, Clone, PartialEq)]
pub struct ValuesChangedEntry {
    /// The value before the change.
    pub old_value: Value,
    /// The value after the change.
    pub new_value: Value,
    /// The *structural* path for the new value's position, when it
    /// differs from the old one (only under list-LCS matching, see
    /// `docs/design/list-diff.md`); `None` when they coincide.
    pub new_path: Option<Vec<PathSegment>>,
    /// The unified diff `DeepDiff` attaches at `verbose_level=2` for a
    /// string change containing a newline (`unified_diff` module);
    /// `None` otherwise, including for a `merge_mutual_add_removes` entry.
    pub diff: Option<String>,
}

impl ValuesChangedEntry {
    fn to_json_value(&self) -> serde_json::Value {
        let mut map = serde_json::Map::with_capacity(4);
        map.insert("new_value".to_string(), self.new_value.to_serde_json());
        map.insert("old_value".to_string(), self.old_value.to_serde_json());
        if let Some(new_path) = &self.new_path {
            map.insert(
                "new_path".to_string(),
                serde_json::Value::String(render_path(new_path).to_string()),
            );
        }
        if let Some(diff) = &self.diff {
            map.insert("diff".to_string(), serde_json::Value::String(diff.clone()));
        }
        serde_json::Value::Object(map)
    }

    fn to_value(&self, builder: &mut Builder) -> Value {
        let mut entries = vec![
            ("new_value".to_string(), self.new_value.clone()),
            ("old_value".to_string(), self.old_value.clone()),
        ];
        if let Some(new_path) = &self.new_path {
            entries.push(("new_path".to_string(), rendered(new_path)));
        }
        if let Some(diff) = &self.diff {
            entries.push(("diff".to_string(), Value::Str(diff.clone().into())));
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
    fn to_json_value(&self) -> serde_json::Value {
        let mut map = serde_json::Map::with_capacity(5);
        map.insert(
            "old_type".to_string(),
            serde_json::Value::String(self.old_type.clone()),
        );
        map.insert(
            "new_type".to_string(),
            serde_json::Value::String(self.new_type.clone()),
        );
        map.insert("old_value".to_string(), self.old_value.to_serde_json());
        map.insert("new_value".to_string(), self.new_value.to_serde_json());
        if let Some(new_path) = &self.new_path {
            map.insert(
                "new_path".to_string(),
                serde_json::Value::String(render_path(new_path).to_string()),
            );
        }
        serde_json::Value::Object(map)
    }

    fn to_value(&self, builder: &mut Builder) -> Value {
        let mut entries = vec![
            (
                "old_type".to_string(),
                Value::Str(self.old_type.clone().into()),
            ),
            (
                "new_type".to_string(),
                Value::Str(self.new_type.clone().into()),
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
    Value::Str(render_path(path))
}

/// A DeepDiff-compatible diff result, one category map per finding kind, keyed by
/// structural path (see this module's doc) and serialized in `DeepDiff`'s
/// `to_json()` shape at `verbose_level=2`. The set categories serialize as bare
/// arrays of path strings; their values only feed `distance_leaf_length`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    type_changes: BTreeMap<Vec<PathSegment>, TypeChangeEntry>,
    values_changed: BTreeMap<Vec<PathSegment>, ValuesChangedEntry>,
    dictionary_item_added: BTreeMap<Vec<PathSegment>, Value>,
    dictionary_item_removed: BTreeMap<Vec<PathSegment>, Value>,
    iterable_item_added: BTreeMap<Vec<PathSegment>, Value>,
    iterable_item_removed: BTreeMap<Vec<PathSegment>, Value>,
    /// The two set categories, allocated on the first set finding. Boxed because a
    /// [`Report`] is returned by value through every native recursion level, so its
    /// size is part of the `max_depth` frame budget (`crate::diff::array_diff`'s
    /// "Stack-footprint note").
    set_items: Option<Box<SetCategories>>,
    /// The two attribute categories, boxed for the same frame-budget reason as
    /// [`Self::set_items`].
    attribute_items: Option<Box<AttributeCategories>>,
}

/// [`Report`]'s two set categories, behind one pointer.
#[derive(Debug, Clone, Default, PartialEq)]
struct SetCategories {
    added: BTreeMap<Vec<PathSegment>, Value>,
    removed: BTreeMap<Vec<PathSegment>, Value>,
}

/// [`Report`]'s two attribute categories, behind one pointer.
#[derive(Debug, Clone, Default, PartialEq)]
struct AttributeCategories {
    added: BTreeMap<Vec<PathSegment>, Value>,
    removed: BTreeMap<Vec<PathSegment>, Value>,
}

/// The empty pair, for the read paths that need one when nothing was found.
static NO_SET_ITEMS: std::sync::LazyLock<SetCategories> =
    std::sync::LazyLock::new(SetCategories::default);

/// [`NO_SET_ITEMS`]'s twin for the attribute categories.
static NO_ATTRIBUTE_ITEMS: std::sync::LazyLock<AttributeCategories> =
    std::sync::LazyLock::new(AttributeCategories::default);

impl Report {
    /// The two set categories, or an empty pair when no set finding exists.
    fn set_items(&self) -> &SetCategories {
        self.set_items.as_deref().unwrap_or(&NO_SET_ITEMS)
    }

    /// The two attribute categories, or an empty pair when no attribute
    /// finding exists.
    fn attribute_items(&self) -> &AttributeCategories {
        self.attribute_items
            .as_deref()
            .unwrap_or(&NO_ATTRIBUTE_ITEMS)
    }
}

/// Inserts `value` at `path`, debug-asserting `path` is new: a duplicate means
/// the traversal visited one node twice.
fn insert_checked<V>(map: &mut BTreeMap<Vec<PathSegment>, V>, path: Vec<PathSegment>, value: V) {
    debug_assert!(!map.contains_key(&path), "duplicate report path: {path:?}");
    map.insert(path, value);
}

/// Merges `src` into `dst` through [`insert_checked`], so the duplicate-path
/// assertion fires on a collision.
fn merge_map(dst: &mut BTreeMap<Vec<PathSegment>, Value>, src: BTreeMap<Vec<PathSegment>, Value>) {
    for (path, value) in src {
        insert_checked(dst, path, value);
    }
}

/// [`push_raw_category`] for [`Report::to_json_value`]; a rendered-string
/// collision likewise keeps the last structural path.
fn serialize_raw_category(
    root: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    map: &BTreeMap<Vec<PathSegment>, Value>,
) {
    if map.is_empty() {
        return;
    }
    let mut category = serde_json::Map::new();
    for (path, value) in map {
        category.insert(render_path(path).to_string(), value.to_serde_json());
    }
    root.insert(name.to_string(), serde_json::Value::Object(category));
}

/// Pushes `map` onto `root` under `name` as one rendered category, omitting an
/// empty one. Structural paths that render to the same string collapse to the
/// last in structural order (see this module's doc).
fn push_raw_category(
    root: &mut Vec<(String, Value)>,
    builder: &mut Builder,
    name: &str,
    map: &BTreeMap<Vec<PathSegment>, Value>,
) {
    if map.is_empty() {
        return;
    }
    let entries: Vec<(Str, Value)> = map
        .iter()
        .map(|(path, value)| (render_path(path), value.clone()))
        .collect();
    root.push((name.to_string(), builder.object(entries)));
}

/// A set category's entries in ascending rendered-path order, rendered-string
/// collisions collapsed. `DeepDiff` orders them by `PYTHONHASHSEED`; see
/// `tests/golden/README.md`.
fn rendered_set_entries(map: &BTreeMap<Vec<PathSegment>, Value>) -> Vec<Str> {
    let mut rendered: Vec<Str> = map.keys().map(|path| render_path(path)).collect();
    rendered.sort();
    rendered.dedup();
    rendered
}

/// [`push_raw_category`] for a set category: an array of rendered paths in
/// [`rendered_set_entries`] order.
fn push_set_category(
    root: &mut Vec<(String, Value)>,
    name: &str,
    map: &BTreeMap<Vec<PathSegment>, Value>,
) {
    if map.is_empty() {
        return;
    }
    let entries = rendered_set_entries(map)
        .into_iter()
        .map(Value::Str)
        .collect::<Vec<_>>();
    root.push((
        name.to_string(),
        Value::Array(entries.into_boxed_slice().into()),
    ));
}

/// [`push_set_category`]'s [`Report::to_json_value`] twin.
fn serialize_set_category(
    root: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    map: &BTreeMap<Vec<PathSegment>, Value>,
) {
    if map.is_empty() {
        return;
    }
    let entries = rendered_set_entries(map)
        .into_iter()
        .map(|path| serde_json::Value::String(path.to_string()))
        .collect();
    root.insert(name.to_string(), serde_json::Value::Array(entries));
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

    /// Records a `dictionary_item_added` finding at the structural `path`;
    /// `value` is the added value itself.
    pub(crate) fn insert_dictionary_item_added(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(&mut self.dictionary_item_added, path, value);
    }

    /// Records a `dictionary_item_removed` finding at the structural `path`;
    /// `value` is the removed value itself.
    pub(crate) fn insert_dictionary_item_removed(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(&mut self.dictionary_item_removed, path, value);
    }

    /// Records an `iterable_item_added` finding at the structural `path`;
    /// `value` is the added value itself.
    pub(crate) fn insert_iterable_item_added(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(&mut self.iterable_item_added, path, value);
    }

    /// Records an `iterable_item_removed` finding at the structural `path`;
    /// `value` is the removed value itself.
    pub(crate) fn insert_iterable_item_removed(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(&mut self.iterable_item_removed, path, value);
    }

    /// Records a `set_item_added` finding at `path`, whose last segment is the added
    /// item (see [`crate::path::PathSegment::SetItem`]); `value` is that item, kept
    /// for distance measurement only.
    pub(crate) fn insert_set_item_added(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(
            &mut self.set_items.get_or_insert_default().added,
            path,
            value,
        );
    }

    /// Records a `set_item_removed` finding at the structural `path` — see
    /// [`Self::insert_set_item_added`] for the shape.
    pub(crate) fn insert_set_item_removed(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(
            &mut self.set_items.get_or_insert_default().removed,
            path,
            value,
        );
    }

    /// Records an `attribute_added` finding at `path`; `value` is the added
    /// attribute's value, as for [`Self::insert_dictionary_item_added`].
    pub(crate) fn insert_attribute_added(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(
            &mut self.attribute_items.get_or_insert_default().added,
            path,
            value,
        );
    }

    /// Records an `attribute_removed` finding at the structural `path` — see
    /// [`Self::insert_attribute_added`] for the shape.
    pub(crate) fn insert_attribute_removed(&mut self, path: Vec<PathSegment>, value: Value) {
        insert_checked(
            &mut self.attribute_items.get_or_insert_default().removed,
            path,
            value,
        );
    }

    /// Folds `other` into `self` through the guarded `insert_*` methods and
    /// [`merge_map`], so a duplicate structural path trips the debug assertion.
    pub(crate) fn merge(&mut self, mut other: Report) {
        // The smaller report goes into the larger, so a finding at every level
        // of a deep chain is moved a logarithmic number of times, not once per
        // level.
        if other.finding_count() > self.finding_count() {
            std::mem::swap(self, &mut other);
        }
        #[cfg(test)]
        MERGE_MOVES.with(|moves| moves.set(moves.get() + other.finding_count()));
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
        if let Some(set_items) = other.set_items {
            let own = self.set_items.get_or_insert_default();
            merge_map(&mut own.added, set_items.added);
            merge_map(&mut own.removed, set_items.removed);
        }
        if let Some(attribute_items) = other.attribute_items {
            let own = self.attribute_items.get_or_insert_default();
            merge_map(&mut own.added, attribute_items.added);
            merge_map(&mut own.removed, attribute_items.removed);
        }
    }

    /// `DeepDiff`'s mutual add/remove pass: an `iterable_item_added`/`removed` pair
    /// with the same rendered path becomes one `values_changed` (never
    /// `type_changes`, no `new_path`); runs once after the traversal.
    pub(crate) fn merge_mutual_add_removes(&mut self) {
        let removed_by_rendered: BTreeMap<Str, Vec<PathSegment>> = self
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
                    diff: None,
                },
            );
        }
    }

    /// Sets `new_path[prefix_depth]` on every `values_changed`/`type_changes` entry,
    /// starting from an existing `new_path` so nested substitutions compose.
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

    /// `DeepDiff`'s `_get_item_length` over a trial-diff report, the
    /// `rough_distance` numerator; the per-entry rules are
    /// [`crate::ignore_order::item_length`] and
    /// [`crate::ignore_order::type_change_leaf_length`].
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
            .chain(self.set_items().added.values())
            .chain(self.set_items().removed.values())
            .chain(self.attribute_items().added.values())
            .chain(self.attribute_items().removed.values())
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
            && self.set_items().added.is_empty()
            && self.set_items().removed.is_empty()
            && self.attribute_items().added.is_empty()
            && self.attribute_items().removed.is_empty()
    }

    /// The total number of findings across every category, as `DeepDiff`'s
    /// `len(TreeResult)`; the list-LCS path keeps the candidate with fewer, the
    /// index-aligned one on a tie.
    #[must_use]
    pub(crate) fn finding_count(&self) -> usize {
        self.type_changes.len()
            + self.values_changed.len()
            + self.dictionary_item_added.len()
            + self.dictionary_item_removed.len()
            + self.iterable_item_added.len()
            + self.iterable_item_removed.len()
            + self.set_items().added.len()
            + self.set_items().removed.len()
            + self.attribute_items().added.len()
            + self.attribute_items().removed.len()
    }

    /// Renders the report in `DeepDiff`'s `to_json()` shape as a [`Value`], keeping
    /// types (a tuple stays a [`Value::Tuple`]), which [`Self::to_json_value`]
    /// cannot. Empty categories are omitted.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut builder = Builder::new();
        let mut root: Vec<(String, Value)> = Vec::new();

        if !self.type_changes.is_empty() {
            let entries: Vec<(Str, Value)> = self
                .type_changes
                .iter()
                .map(|(path, entry)| (render_path(path), entry.to_value(&mut builder)))
                .collect();
            root.push(("type_changes".to_string(), builder.object(entries)));
        }

        if !self.values_changed.is_empty() {
            let entries: Vec<(Str, Value)> = self
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
        push_set_category(&mut root, "set_item_added", &self.set_items().added);
        push_set_category(&mut root, "set_item_removed", &self.set_items().removed);
        push_raw_category(
            &mut root,
            &mut builder,
            "attribute_added",
            &self.attribute_items().added,
        );
        push_raw_category(
            &mut root,
            &mut builder,
            "attribute_removed",
            &self.attribute_items().removed,
        );

        builder.object(root)
    }

    /// Renders the report in `DeepDiff`'s `to_json()` shape as a
    /// [`serde_json::Value`], a tuple becoming an array. It walks the findings
    /// directly and must stay in step with [`Self::to_value`].
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        let mut root = serde_json::Map::new();

        if !self.type_changes.is_empty() {
            let mut category = serde_json::Map::new();
            for (path, entry) in &self.type_changes {
                category.insert(render_path(path).to_string(), entry.to_json_value());
            }
            root.insert(
                "type_changes".to_string(),
                serde_json::Value::Object(category),
            );
        }

        if !self.values_changed.is_empty() {
            let mut category = serde_json::Map::new();
            for (path, entry) in &self.values_changed {
                category.insert(render_path(path).to_string(), entry.to_json_value());
            }
            root.insert(
                "values_changed".to_string(),
                serde_json::Value::Object(category),
            );
        }

        serialize_raw_category(
            &mut root,
            "dictionary_item_added",
            &self.dictionary_item_added,
        );
        serialize_raw_category(
            &mut root,
            "dictionary_item_removed",
            &self.dictionary_item_removed,
        );
        serialize_raw_category(&mut root, "iterable_item_added", &self.iterable_item_added);
        serialize_raw_category(
            &mut root,
            "iterable_item_removed",
            &self.iterable_item_removed,
        );
        serialize_set_category(&mut root, "set_item_added", &self.set_items().added);
        serialize_set_category(&mut root, "set_item_removed", &self.set_items().removed);
        serialize_raw_category(&mut root, "attribute_added", &self.attribute_items().added);
        serialize_raw_category(
            &mut root,
            "attribute_removed",
            &self.attribute_items().removed,
        );

        serde_json::Value::Object(root)
    }
}

#[cfg(test)]
thread_local! {
    /// How many findings [`Report::merge`] has moved on this thread.
    pub(crate) static MERGE_MOVES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
