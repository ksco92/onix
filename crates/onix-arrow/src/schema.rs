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
/// recursively through `List`/`LargeList`/`ListView`/`LargeListView`,
/// `Struct`, and `Map` children:
///
/// - a dictionary-encoded type becomes its value type (so a dictionary-encoded
///   string equals a plain string, and `list<dictionary<int32, string>>`
///   equals `list<string>` — polars and `DuckDB` emit dictionary/categorical
///   encodings routinely);
/// - `Utf8View` and `LargeUtf8` become `Utf8` (polars exports strings as
///   `Utf8View`, pyarrow and `DuckDB` as `Utf8`);
/// - `BinaryView` and `LargeBinary` become `Binary`;
/// - `LargeList`, `ListView`, and `LargeListView` become `List`.
///
/// Every other parameter — timestamp unit and timezone, decimal precision and
/// scale — is preserved and does count as a difference. Nullability is handled
/// separately (it never counts) and is not touched here.
fn normalized_type(data_type: &DataType) -> DataType {
    match data_type {
        DataType::Dictionary(_, value) => normalized_type(value),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => DataType::Utf8,
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => DataType::Binary,
        DataType::List(field)
        | DataType::LargeList(field)
        | DataType::ListView(field)
        | DataType::LargeListView(field) => DataType::List(normalized_field(field)),
        DataType::Struct(fields) => DataType::Struct(fields.iter().map(normalized_field).collect()),
        DataType::Map(field, sorted) => DataType::Map(normalized_field(field), *sorted),
        other => other.clone(),
    }
}

/// Normalizes a field's data type (see [`normalized_type`]), keeping its name
/// and nullability so nested structs still compare by member names.
fn normalized_field(field: &FieldRef) -> FieldRef {
    Arc::new(Field::new(
        field.name(),
        normalized_type(field.data_type()),
        field.is_nullable(),
    ))
}

/// Whether two column types are considered equal: equal after
/// [`normalized_type`] canonicalizes both. Nullability is not part of this
/// comparison.
fn types_equal(left: &DataType, right: &DataType) -> bool {
    normalized_type(left) == normalized_type(right)
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
/// [`TableDiffError::DuplicateColumn`] if either side has two columns with the
/// same name, naming the column and the side.
pub fn diff_schemas(left: &Schema, right: &Schema) -> Result<Vec<SchemaChange>, TableDiffError> {
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
