//! Schema comparison: which columns were added, removed, or changed type
//! between two Arrow schemas.

use std::collections::HashMap;
use std::sync::Arc;

use arrow_schema::{DataType, Field, FieldRef, Schema};
use serde::Serialize;

use crate::error::{Side, TableDiffError};

/// The kind of change a column underwent between the two schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// The column is present on the right but not the left.
    Added,
    /// The column is present on the left but not the right.
    Removed,
    /// The column is present on both sides but its data type differs.
    TypeChanged,
}

impl ChangeKind {
    /// The lowercase word used for this change in JSON and the Arrow view.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Removed => "removed",
            ChangeKind::TypeChanged => "type_changed",
        }
    }
}

/// One column that differs between the two schemas.
///
/// Only changed columns appear: a column present on both sides with the same
/// type (ignoring nullability) is not recorded. Nullability never causes a
/// column to be recorded, but the two sides' nullability is reported here
/// when the column is recorded for another reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaChange {
    /// The column name.
    pub column: String,
    /// What changed.
    pub change: ChangeKind,
    /// The column's type on the left, rendered with full Arrow parameters
    /// (timestamp unit and timezone, decimal precision and scale, and the
    /// dictionary wrapper if the column is dictionary-encoded). `None` for an
    /// added column.
    pub left_type: Option<String>,
    /// The column's type on the right, in the same rendering. `None` for a
    /// removed column.
    pub right_type: Option<String>,
    /// Whether the column is nullable on the left. `None` for an added column.
    pub left_nullable: Option<bool>,
    /// Whether the column is nullable on the right. `None` for a removed
    /// column.
    pub right_nullable: Option<bool>,
}

/// Normalizes a data type to the canonical form used for comparison, so that
/// columns whose types differ only in a *physical* encoding compare equal —
/// what a data engineer means by "the same type". The rules, applied
/// recursively through list, struct, and map children:
///
/// - a dictionary-encoded type becomes its value type (dictionary-encoded
///   string == plain string; `list<dictionary<int32, string>>` ==
///   `list<string>` — polars and `DuckDB` emit dictionary/categorical
///   encodings routinely);
/// - `Utf8View` and `LargeUtf8` become `Utf8`, `BinaryView`/`LargeBinary`
///   become `Binary`;
/// - every variable-length list variant (`List`/`LargeList`/`ListView`/
///   `LargeListView`) becomes `List`, and its element field's name and
///   nullability are dropped (canonicalized to `item`, nullable), because
///   producers disagree on both (`DuckDB` names the element `l`, pyarrow
///   `item`, Parquet `element`); a `FixedSizeList` keeps its width but has its
///   element normalized the same way;
/// - a `Map`, and the specific `LargeList<Struct<key, value>>` polars
///   re-exports an Arrow map as (see [`map_entries`]), both become one
///   canonical `Map` shape, so a map column compares equal across libraries.
///
/// Every other parameter — timestamp unit and timezone, decimal precision and
/// scale — is preserved and counts as a difference. Nullability never counts
/// and is canonicalized away at every level.
///
/// Recursive, but only ever called on a type already checked by
/// [`depth_exceeds`], so its native recursion is bounded by
/// [`crate::MAX_NESTING_DEPTH`].
fn normalized_type(data_type: &DataType) -> DataType {
    match data_type {
        DataType::Dictionary(_, value) => normalized_type(value),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => DataType::Utf8,
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => DataType::Binary,
        // A `List` or `LargeList` whose element carries the map signature (see
        // [`map_entries`]) is read as a map, on every library path: polars has
        // no map type and re-exports both a real Arrow map and an ordinary list
        // of key/value structs as the identical `LargeList<Struct<key, value>>`,
        // while pyarrow uses `List` for such a list, so the two must normalize
        // the same way to keep cross-library identity. `ListView`/
        // `LargeListView` are never used for maps and stay lists.
        DataType::List(field) | DataType::LargeList(field) => match map_entries(field) {
            Some((key, value)) => canonical_map(key, value),
            None => canonical_list(field.data_type()),
        },
        DataType::ListView(field) | DataType::LargeListView(field) => {
            canonical_list(field.data_type())
        }
        DataType::FixedSizeList(field, size) => DataType::FixedSizeList(
            Arc::new(Field::new("item", normalized_type(field.data_type()), true)),
            *size,
        ),
        DataType::Map(entries, _) => {
            if let DataType::Struct(fields) = entries.data_type()
                && fields.len() == 2
            {
                return canonical_map(fields[0].data_type(), fields[1].data_type());
            }
            data_type.clone()
        }
        DataType::Struct(fields) => DataType::Struct(
            fields
                .iter()
                .map(|f| Field::new(f.name(), normalized_type(f.data_type()), true))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The canonical `List` a variable-length list variant normalizes to: element
/// name and nullability dropped, element type normalized.
fn canonical_list(element: &DataType) -> DataType {
    DataType::List(Arc::new(Field::new("item", normalized_type(element), true)))
}

/// The one canonical shape both an Arrow `Map` and polars'
/// `LargeList<Struct<key, value>>` map export normalize to: a `Map` whose
/// entries are a struct with a non-null `key` and a nullable `value`, both
/// types normalized. Field names, the sorted flag, and the entries-field
/// nullability are all canonicalized away.
fn canonical_map(key: &DataType, value: &DataType) -> DataType {
    DataType::Map(
        Arc::new(Field::new(
            "entries",
            DataType::Struct(
                vec![
                    Field::new("key", normalized_type(key), false),
                    Field::new("value", normalized_type(value), true),
                ]
                .into(),
            ),
            false,
        )),
        false,
    )
}

/// If `element` is a struct of exactly two fields named `key` (nullable) and
/// `value` — the signature both an Arrow map and polars' map export carry —
/// returns the key and value types so the enclosing `List`/`LargeList` is
/// treated as a map. `None` otherwise (different names, a non-null key, or not
/// a two-field struct), so an ordinary list of a two-field struct is left as a
/// list.
///
/// This applies to `List` and `LargeList` alike, which forces one accepted
/// false negative: polars has no map type and re-exports both a real Arrow
/// `map<k, v>` and an ordinary `list<struct{key, value}>` as the byte-identical
/// `LargeList<Struct<key, value>>`. To keep the same table read through
/// different libraries reporting no spurious change, onix treats a list of
/// structs named exactly `key`/`value` with a nullable key as a map on every
/// path — so a migration between those two shapes (a real map ⇄ a list of
/// key/value structs) is not reported as a type change. The README documents
/// this.
fn map_entries(element: &FieldRef) -> Option<(&DataType, &DataType)> {
    if let DataType::Struct(fields) = element.data_type()
        && fields.len() == 2
        && fields[0].name() == "key"
        && fields[0].is_nullable()
        && fields[1].name() == "value"
    {
        return Some((fields[0].data_type(), fields[1].data_type()));
    }

    None
}

/// Whether two column types are considered equal: equal after
/// [`normalized_type`] canonicalizes both. Nullability is not part of this
/// comparison.
fn types_equal(left: &DataType, right: &DataType) -> bool {
    normalized_type(left) == normalized_type(right)
}

/// Whether `data_type` is nested deeper than `limit` levels, counting each
/// nesting wrapper (dictionary, any list, struct, map, union, run-end) as one
/// level. **Iterative** — an explicit heap work-stack, no native recursion —
/// so it is itself safe to run on any input depth, and it is the guard that
/// keeps [`normalized_type`], the type's `Display` (used to render the report),
/// and the type's own `Drop` from overflowing the native stack on
/// adversarially deep input.
fn depth_exceeds(data_type: &DataType, limit: usize) -> bool {
    let mut stack: Vec<(&DataType, usize)> = vec![(data_type, 0)];

    while let Some((dt, depth)) = stack.pop() {
        if depth > limit {
            return true;
        }

        let child_depth = depth + 1;

        match dt {
            DataType::Dictionary(_, value) => stack.push((value, child_depth)),
            DataType::List(f)
            | DataType::LargeList(f)
            | DataType::ListView(f)
            | DataType::LargeListView(f)
            | DataType::FixedSizeList(f, _)
            | DataType::Map(f, _)
            | DataType::RunEndEncoded(_, f) => stack.push((f.data_type(), child_depth)),
            DataType::Struct(fields) => {
                for field in fields {
                    stack.push((field.data_type(), child_depth));
                }
            }
            DataType::Union(fields, _) => {
                for (_, field) in fields.iter() {
                    stack.push((field.data_type(), child_depth));
                }
            }
            _ => {}
        }
    }

    false
}

/// Rejects a schema with any column nested deeper than
/// [`crate::MAX_NESTING_DEPTH`], before any recursive walk touches it.
fn check_depths(schema: &Schema) -> Result<(), TableDiffError> {
    for field in schema.fields() {
        if depth_exceeds(field.data_type(), crate::MAX_NESTING_DEPTH) {
            return Err(TableDiffError::MaxDepthExceeded {
                column: field.name().clone(),
                max_depth: crate::MAX_NESTING_DEPTH,
            });
        }
    }

    Ok(())
}

/// Indexes a schema's fields by name, rejecting a side with a duplicate name.
fn index_by_name(schema: &Schema, side: Side) -> Result<HashMap<&str, &FieldRef>, TableDiffError> {
    let mut map = HashMap::with_capacity(schema.fields().len());

    for field in schema.fields() {
        if map.insert(field.name().as_str(), field).is_some() {
            return Err(TableDiffError::DuplicateColumn {
                column: field.name().clone(),
                side,
            });
        }
    }

    Ok(map)
}

/// Compares two Arrow schemas and returns every column that was added,
/// removed, or changed type, in a deterministic order: the left schema's
/// columns in their own order first (each either removed or type-changed),
/// then the columns present only on the right, in the right schema's order
/// (each added).
///
/// # Errors
///
/// - [`TableDiffError::MaxDepthExceeded`] if a column's type is nested past
///   [`crate::MAX_NESTING_DEPTH`] (checked first, before any recursive walk).
/// - [`TableDiffError::DuplicateColumn`] if either side has two columns with
///   the same name, naming the column and the side.
pub fn diff_schemas(left: &Schema, right: &Schema) -> Result<Vec<SchemaChange>, TableDiffError> {
    check_depths(left)?;
    check_depths(right)?;

    let left_by_name = index_by_name(left, Side::Left)?;
    let right_by_name = index_by_name(right, Side::Right)?;
    let mut changes = Vec::new();

    for left_field in left.fields() {
        match right_by_name.get(left_field.name().as_str()) {
            Some(right_field) => {
                if !types_equal(left_field.data_type(), right_field.data_type()) {
                    changes.push(SchemaChange {
                        column: left_field.name().clone(),
                        change: ChangeKind::TypeChanged,
                        left_type: Some(left_field.data_type().to_string()),
                        right_type: Some(right_field.data_type().to_string()),
                        left_nullable: Some(left_field.is_nullable()),
                        right_nullable: Some(right_field.is_nullable()),
                    });
                }
            }
            None => changes.push(SchemaChange {
                column: left_field.name().clone(),
                change: ChangeKind::Removed,
                left_type: Some(left_field.data_type().to_string()),
                right_type: None,
                left_nullable: Some(left_field.is_nullable()),
                right_nullable: None,
            }),
        }
    }

    for right_field in right.fields() {
        if !left_by_name.contains_key(right_field.name().as_str()) {
            changes.push(SchemaChange {
                column: right_field.name().clone(),
                change: ChangeKind::Added,
                left_type: None,
                right_type: Some(right_field.data_type().to_string()),
                left_nullable: None,
                right_nullable: Some(right_field.is_nullable()),
            });
        }
    }

    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::diff_schemas;
    use crate::error::{Side, TableDiffError};
    use crate::schema::ChangeKind;
    use arrow_schema::{DataType, Field, Schema, TimeUnit};

    fn schema(fields: Vec<Field>) -> Schema {
        Schema::new(fields)
    }

    fn list_of(data_type: DataType) -> DataType {
        DataType::List(std::sync::Arc::new(Field::new("item", data_type, true)))
    }

    fn dict_string() -> DataType {
        DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8))
    }

    #[test]
    fn identical_schemas_produce_no_changes() {
        let left = schema(vec![Field::new("id", DataType::Int64, false)]);
        let right = schema(vec![Field::new("id", DataType::Int64, false)]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn added_and_removed_columns_are_reported_in_order() {
        let left = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("gone", DataType::Utf8, true),
        ]);
        let right = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("fresh", DataType::Float64, true),
        ]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].column, "gone");
        assert_eq!(changes[0].change, ChangeKind::Removed);
        assert_eq!(changes[0].left_type.as_deref(), Some("Utf8"));
        assert_eq!(changes[0].right_type, None);
        assert_eq!(changes[1].column, "fresh");
        assert_eq!(changes[1].change, ChangeKind::Added);
        assert_eq!(changes[1].left_type, None);
        assert_eq!(changes[1].right_type.as_deref(), Some("Float64"));
    }

    #[test]
    fn type_change_reports_both_types_and_nullability() {
        let left = schema(vec![Field::new("age", DataType::Int32, false)]);
        let right = schema(vec![Field::new("age", DataType::Int64, true)]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
        assert_eq!(changes[0].left_type.as_deref(), Some("Int32"));
        assert_eq!(changes[0].right_type.as_deref(), Some("Int64"));
        assert_eq!(changes[0].left_nullable, Some(false));
        assert_eq!(changes[0].right_nullable, Some(true));
    }

    #[test]
    fn nullability_only_difference_is_not_a_change() {
        let left = schema(vec![Field::new("id", DataType::Int64, false)]);
        let right = schema(vec![Field::new("id", DataType::Int64, true)]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn timestamp_timezone_change_is_a_type_change() {
        let left = schema(vec![Field::new(
            "t",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            true,
        )]);
        let right = schema(vec![Field::new(
            "t",
            DataType::Timestamp(TimeUnit::Microsecond, Some("America/New_York".into())),
            true,
        )]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
        assert!(changes[0].left_type.as_deref().unwrap().contains("UTC"));
        assert!(
            changes[0]
                .right_type
                .as_deref()
                .unwrap()
                .contains("America/New_York")
        );
    }

    #[test]
    fn decimal_scale_change_is_a_type_change() {
        let left = schema(vec![Field::new(
            "amount",
            DataType::Decimal128(10, 2),
            true,
        )]);
        let right = schema(vec![Field::new(
            "amount",
            DataType::Decimal128(10, 4),
            true,
        )]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn dictionary_encoded_string_equals_plain_string() {
        let left = schema(vec![Field::new("name", dict_string(), true)]);
        let right = schema(vec![Field::new("name", DataType::Utf8, true)]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn dictionary_index_type_does_not_matter() {
        let left = schema(vec![Field::new(
            "name",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            true,
        )]);
        let right = schema(vec![Field::new("name", dict_string(), true)]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn dictionary_value_type_change_is_a_type_change() {
        let left = schema(vec![Field::new("name", dict_string(), true)]);
        let right = schema(vec![Field::new("name", DataType::Int64, true)]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn string_view_and_large_utf8_equal_plain_utf8() {
        let left = schema(vec![
            Field::new("a", DataType::Utf8View, true),
            Field::new("b", DataType::LargeUtf8, true),
        ]);
        let right = schema(vec![
            Field::new("a", DataType::Utf8, true),
            Field::new("b", DataType::Utf8, true),
        ]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn large_binary_and_binary_view_equal_plain_binary() {
        let left = schema(vec![
            Field::new("a", DataType::LargeBinary, true),
            Field::new("b", DataType::BinaryView, true),
        ]);
        let right = schema(vec![
            Field::new("a", DataType::Binary, true),
            Field::new("b", DataType::Binary, true),
        ]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn list_of_dictionary_equals_list_of_string() {
        let left = schema(vec![Field::new("tags", list_of(dict_string()), true)]);
        let right = schema(vec![Field::new(
            "tags",
            DataType::LargeList(std::sync::Arc::new(Field::new(
                "item",
                DataType::Utf8,
                true,
            ))),
            true,
        )]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn list_element_type_change_is_a_type_change() {
        let left = schema(vec![Field::new("nums", list_of(DataType::Int32), true)]);
        let right = schema(vec![Field::new("nums", list_of(DataType::Int64), true)]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn struct_member_dictionary_equals_plain() {
        let left = schema(vec![Field::new(
            "s",
            DataType::Struct(vec![Field::new("name", dict_string(), true)].into()),
            true,
        )]);
        let right = schema(vec![Field::new(
            "s",
            DataType::Struct(vec![Field::new("name", DataType::Utf8, true)].into()),
            true,
        )]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn list_view_equals_list() {
        let left = schema(vec![Field::new(
            "nums",
            DataType::ListView(std::sync::Arc::new(Field::new(
                "item",
                DataType::Int64,
                true,
            ))),
            true,
        )]);
        let right = schema(vec![Field::new("nums", list_of(DataType::Int64), true)]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    fn map_with(value: DataType) -> DataType {
        let entries = DataType::Struct(
            vec![
                Field::new("keys", DataType::Utf8, false),
                Field::new("values", value, true),
            ]
            .into(),
        );
        DataType::Map(
            std::sync::Arc::new(Field::new("entries", entries, false)),
            false,
        )
    }

    #[test]
    fn map_normalizes_its_value_encoding() {
        let left = schema(vec![Field::new("m", map_with(dict_string()), true)]);
        let right = schema(vec![Field::new("m", map_with(DataType::Utf8), true)]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn map_value_type_change_is_a_type_change() {
        let left = schema(vec![Field::new("m", map_with(DataType::Int32), true)]);
        let right = schema(vec![Field::new("m", map_with(DataType::Int64), true)]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn list_element_field_name_and_nullability_are_ignored() {
        // DuckDB names the list element `l` and pyarrow `item`; a non-null vs
        // nullable element must not matter either.
        let left = schema(vec![Field::new(
            "xs",
            DataType::List(std::sync::Arc::new(Field::new("l", DataType::Int64, false))),
            true,
        )]);
        let right = schema(vec![Field::new(
            "xs",
            DataType::List(std::sync::Arc::new(Field::new(
                "item",
                DataType::Int64,
                true,
            ))),
            true,
        )]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn arrow_map_equals_polars_large_list_of_key_value_struct() {
        let arrow_map = map_with(DataType::Int64);
        let polars_style = DataType::LargeList(std::sync::Arc::new(Field::new(
            "item",
            DataType::Struct(
                vec![
                    // polars re-exports the key as nullable Utf8View
                    Field::new("key", DataType::Utf8View, true),
                    Field::new("value", DataType::Int64, true),
                ]
                .into(),
            ),
            true,
        )));
        let left = schema(vec![Field::new("m", arrow_map, true)]);
        let right = schema(vec![Field::new("m", polars_style, true)]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn list_of_non_map_struct_is_not_treated_as_map() {
        // A list of a two-field struct NOT named key/value stays a list; it
        // must not collapse to the map canonical form.
        let point = DataType::Struct(
            vec![
                Field::new("x", DataType::Int64, true),
                Field::new("y", DataType::Int64, true),
            ]
            .into(),
        );
        let left = schema(vec![Field::new("pts", list_of(point), true)]);
        let right = schema(vec![Field::new("pts", map_with(DataType::Int64), true)]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1, "a list of points is not a map");
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn list_and_large_list_of_key_value_struct_both_read_as_a_map() {
        // polars re-exports both a real map and a plain list of key/value
        // structs as the same LargeList<Struct<key,value>>, so a List (pyarrow)
        // and a LargeList (polars) of that struct, and a real Map, must all
        // normalize to the same type — an accepted false negative (a real map
        // and a list of key/value structs are not distinguished).
        let kv = || {
            DataType::Struct(
                vec![
                    Field::new("key", DataType::Utf8, true),
                    Field::new("value", DataType::Int32, true),
                ]
                .into(),
            )
        };
        let as_list = schema(vec![Field::new("m", list_of(kv()), true)]);
        let as_large_list = schema(vec![Field::new(
            "m",
            DataType::LargeList(std::sync::Arc::new(Field::new("item", kv(), true))),
            true,
        )]);
        let as_map = schema(vec![Field::new("m", map_with(DataType::Int32), true)]);

        assert!(diff_schemas(&as_list, &as_map).unwrap().is_empty());
        assert!(diff_schemas(&as_large_list, &as_map).unwrap().is_empty());
        assert!(diff_schemas(&as_list, &as_large_list).unwrap().is_empty());
    }

    #[test]
    fn large_list_with_non_null_key_struct_is_not_a_map() {
        // polars emits the map key as nullable; a LargeList of a key/value
        // struct with a NON-null key is not polars' map export and stays a list.
        let kv_non_null = DataType::Struct(
            vec![
                Field::new("key", DataType::Utf8, false),
                Field::new("value", DataType::Int64, true),
            ]
            .into(),
        );
        let large_list =
            DataType::LargeList(std::sync::Arc::new(Field::new("item", kv_non_null, true)));
        let left = schema(vec![Field::new("m", large_list, true)]);
        let right = schema(vec![Field::new("m", map_with(DataType::Int64), true)]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(
            changes.len(),
            1,
            "a non-null-keyed large list is not polars' map"
        );
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn fixed_size_list_normalizes_element_encoding_and_keeps_width() {
        let left = schema(vec![Field::new(
            "v",
            DataType::FixedSizeList(
                std::sync::Arc::new(Field::new("item", DataType::Utf8View, true)),
                2,
            ),
            true,
        )]);
        let right = schema(vec![Field::new(
            "v",
            DataType::FixedSizeList(
                std::sync::Arc::new(Field::new("l", DataType::Utf8, false)),
                2,
            ),
            true,
        )]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn fixed_size_list_width_change_is_a_type_change() {
        let left = schema(vec![Field::new(
            "v",
            DataType::FixedSizeList(
                std::sync::Arc::new(Field::new("item", DataType::Int64, true)),
                2,
            ),
            true,
        )]);
        let right = schema(vec![Field::new(
            "v",
            DataType::FixedSizeList(
                std::sync::Arc::new(Field::new("item", DataType::Int64, true)),
                3,
            ),
            true,
        )]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    fn nested_struct(depth: usize) -> DataType {
        let mut ty = DataType::Int64;

        for _ in 0..depth {
            ty = DataType::Struct(vec![Field::new("f", ty, true)].into());
        }

        ty
    }

    #[test]
    fn depth_at_the_limit_is_accepted() {
        // A column exactly at the bound compares normally (no MaxDepthExceeded).
        let ty = nested_struct(crate::MAX_NESTING_DEPTH);
        let left = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("x", ty.clone(), true),
        ]);
        let right = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("x", ty, true),
        ]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn depth_past_the_limit_is_a_typed_error_not_a_crash() {
        // Control: depth_exceeds itself is iterative, so this construction and
        // check cannot overflow the stack even far past the bound.
        let ty = nested_struct(crate::MAX_NESTING_DEPTH + 1);
        assert!(super::depth_exceeds(&ty, crate::MAX_NESTING_DEPTH));

        let left = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("deep", ty, true),
        ]);
        let right = schema(vec![Field::new("id", DataType::Int64, false)]);
        assert_eq!(
            diff_schemas(&left, &right),
            Err(TableDiffError::MaxDepthExceeded {
                column: "deep".to_string(),
                max_depth: crate::MAX_NESTING_DEPTH,
            })
        );
    }

    #[test]
    fn deeply_nested_dictionary_is_rejected() {
        // Each dictionary wrapper counts as one level, so a dictionary nested
        // past the bound is refused; if the depth check skipped dictionary
        // wrappers this would slip through.
        let mut ty = DataType::Int64;
        for _ in 0..(crate::MAX_NESTING_DEPTH + 5) {
            ty = DataType::Dictionary(Box::new(DataType::Int32), Box::new(ty));
        }
        let left = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("deep", ty, true),
        ]);
        let right = schema(vec![Field::new("id", DataType::Int64, false)]);
        assert!(matches!(
            diff_schemas(&left, &right),
            Err(TableDiffError::MaxDepthExceeded { .. })
        ));
    }

    #[test]
    fn deeply_nested_union_is_rejected() {
        use arrow_schema::{UnionFields, UnionMode};

        let mut ty = DataType::Int64;
        for _ in 0..(crate::MAX_NESTING_DEPTH + 5) {
            let union_fields: UnionFields = [(0i8, std::sync::Arc::new(Field::new("f", ty, true)))]
                .into_iter()
                .collect();
            ty = DataType::Union(union_fields, UnionMode::Sparse);
        }
        let left = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("deep", ty, true),
        ]);
        let right = schema(vec![Field::new("id", DataType::Int64, false)]);
        assert!(matches!(
            diff_schemas(&left, &right),
            Err(TableDiffError::MaxDepthExceeded { .. })
        ));
    }

    #[test]
    fn depth_check_covers_list_nesting_without_native_recursion() {
        // A list nested far past what a recursive walk (or `DataType`'s own
        // recursive `Drop`) could survive is rejected cleanly, proving the
        // check is iterative. The fixture is built iteratively and `forget`en
        // rather than dropped, so its recursive teardown cannot crash this
        // (non-subprocess) test harness and mask the result — the crashing
        // path itself is covered end to end in the Python subprocess tests.
        let mut ty = DataType::Int64;
        for _ in 0..50_000 {
            ty = DataType::List(std::sync::Arc::new(Field::new("item", ty, true)));
        }
        let left = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("deep", ty, true),
        ]);
        let right = schema(vec![Field::new("id", DataType::Int64, false)]);
        assert!(matches!(
            diff_schemas(&left, &right),
            Err(TableDiffError::MaxDepthExceeded { .. })
        ));

        std::mem::forget(left);
    }

    #[test]
    fn depth_check_traverses_every_nested_variant() {
        use arrow_schema::{UnionFields, UnionMode};

        let item = || std::sync::Arc::new(Field::new("item", DataType::Int64, true));
        let union_fields: UnionFields = [
            (
                0i8,
                std::sync::Arc::new(Field::new("a", DataType::Int64, true)),
            ),
            (
                1i8,
                std::sync::Arc::new(Field::new("b", DataType::Utf8, true)),
            ),
        ]
        .into_iter()
        .collect();
        let union = DataType::Union(union_fields, UnionMode::Sparse);
        let run_end = DataType::RunEndEncoded(
            std::sync::Arc::new(Field::new("run_ends", DataType::Int32, false)),
            item(),
        );
        let fields = vec![
            Field::new("id", DataType::Int64, false),
            Field::new("fsl", DataType::FixedSizeList(item(), 3), true),
            Field::new("llv", DataType::LargeListView(item()), true),
            Field::new("u", union, true),
            Field::new("ree", run_end, true),
        ];
        let left = schema(fields.clone());
        let right = schema(fields);
        // Identical schemas including every nested variant: the depth check
        // walks each arm, and nothing is reported.
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn map_with_non_two_field_entries_is_left_as_is() {
        // A structurally-malformed Map (entries struct without exactly two
        // fields) is not treated as a canonical map; identical ones still
        // compare equal.
        let malformed = DataType::Map(
            std::sync::Arc::new(Field::new(
                "entries",
                DataType::Struct(vec![Field::new("only", DataType::Int64, true)].into()),
                false,
            )),
            false,
        );
        let left = schema(vec![Field::new("m", malformed.clone(), true)]);
        let right = schema(vec![Field::new("m", malformed, true)]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn unicode_column_names_are_matched() {
        let left = schema(vec![Field::new("café", DataType::Int64, false)]);
        let right = schema(vec![Field::new("café", DataType::Utf8, false)]);
        let changes = diff_schemas(&left, &right).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].column, "café");
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn empty_schemas_produce_no_changes() {
        let left = schema(Vec::new());
        let right = schema(Vec::new());
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn metadata_does_not_affect_matching() {
        let left = Schema::new(vec![Field::new("id", DataType::Int64, false)]);
        let mut meta = std::collections::HashMap::new();
        meta.insert("note".to_string(), "x".to_string());
        let right = Schema::new(vec![
            Field::new("id", DataType::Int64, false).with_metadata(meta),
        ]);
        assert!(diff_schemas(&left, &right).unwrap().is_empty());
    }

    #[test]
    fn duplicate_column_on_left_is_rejected() {
        let left = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("x", DataType::Int64, true),
            Field::new("x", DataType::Utf8, true),
        ]);
        let right = schema(vec![Field::new("id", DataType::Int64, false)]);

        assert_eq!(
            diff_schemas(&left, &right),
            Err(TableDiffError::DuplicateColumn {
                column: "x".to_string(),
                side: Side::Left,
            })
        );
    }

    #[test]
    fn duplicate_column_on_right_is_rejected() {
        let left = schema(vec![Field::new("id", DataType::Int64, false)]);
        let right = schema(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("x", DataType::Int64, true),
            Field::new("x", DataType::Utf8, true),
        ]);

        assert_eq!(
            diff_schemas(&left, &right),
            Err(TableDiffError::DuplicateColumn {
                column: "x".to_string(),
                side: Side::Right,
            })
        );
    }

    #[test]
    fn change_kind_as_str_covers_every_variant() {
        assert_eq!(ChangeKind::Added.as_str(), "added");
        assert_eq!(ChangeKind::Removed.as_str(), "removed");
        assert_eq!(ChangeKind::TypeChanged.as_str(), "type_changed");
    }
}
