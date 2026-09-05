//! Schema comparison: which columns were added, removed, or changed type
//! between two Arrow schemas.

use arrow_schema::{DataType, Schema};
use serde::Serialize;

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

/// Unwraps a dictionary-encoded type to the type it encodes, so a
/// dictionary-encoded column compares equal to a plainly-encoded column of the
/// same value type. Applied recursively for the (rare) nested-dictionary
/// case. Every other type is returned unchanged.
fn value_type(data_type: &DataType) -> &DataType {
    match data_type {
        DataType::Dictionary(_, value) => value_type(value),
        other => other,
    }
}

/// Whether two column types are considered equal: equal after unwrapping any
/// dictionary encoding, and including every other type parameter (timestamp
/// unit and timezone, decimal precision and scale). Nullability is not part
/// of this comparison.
fn types_equal(left: &DataType, right: &DataType) -> bool {
    value_type(left) == value_type(right)
}

/// Compares two Arrow schemas and returns every column that was added,
/// removed, or changed type, in a deterministic order: the left schema's
/// columns in their own order first (each either removed or type-changed),
/// then the columns present only on the right, in the right schema's order
/// (each added).
#[must_use]
pub fn diff_schemas(left: &Schema, right: &Schema) -> Vec<SchemaChange> {
    let mut changes = Vec::new();

    for left_field in left.fields() {
        match right.fields().find(left_field.name()) {
            Some((_, right_field)) => {
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
        if left.fields().find(right_field.name()).is_none() {
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

    changes
}

#[cfg(test)]
mod tests {
    use super::{ChangeKind, diff_schemas};
    use arrow_schema::{DataType, Field, Schema, TimeUnit};
    use std::sync::Arc;

    fn schema(fields: Vec<Field>) -> Schema {
        Schema::new(fields)
    }

    #[test]
    fn identical_schemas_produce_no_changes() {
        let left = schema(vec![Field::new("id", DataType::Int64, false)]);
        let right = schema(vec![Field::new("id", DataType::Int64, false)]);
        assert!(diff_schemas(&left, &right).is_empty());
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
        let changes = diff_schemas(&left, &right);

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
        let changes = diff_schemas(&left, &right);

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
        assert!(diff_schemas(&left, &right).is_empty());
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
        let changes = diff_schemas(&left, &right);

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
        let changes = diff_schemas(&left, &right);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn dictionary_encoded_string_equals_plain_string() {
        let left = schema(vec![Field::new(
            "name",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        )]);
        let right = schema(vec![Field::new("name", DataType::Utf8, true)]);
        assert!(diff_schemas(&left, &right).is_empty());
    }

    #[test]
    fn dictionary_index_type_does_not_matter() {
        let left = schema(vec![Field::new(
            "name",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            true,
        )]);
        let right = schema(vec![Field::new(
            "name",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        )]);
        assert!(diff_schemas(&left, &right).is_empty());
    }

    #[test]
    fn dictionary_value_type_change_is_a_type_change() {
        let left = schema(vec![Field::new(
            "name",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            true,
        )]);
        let right = schema(vec![Field::new("name", DataType::Int64, true)]);
        let changes = diff_schemas(&left, &right);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn unicode_column_names_are_matched() {
        let left = schema(vec![Field::new("café", DataType::Int64, false)]);
        let right = schema(vec![Field::new("café", DataType::Utf8, false)]);
        let changes = diff_schemas(&left, &right);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].column, "café");
        assert_eq!(changes[0].change, ChangeKind::TypeChanged);
    }

    #[test]
    fn empty_schemas_produce_no_changes() {
        let left = schema(Vec::new());
        let right = schema(Vec::new());
        assert!(diff_schemas(&left, &right).is_empty());
    }

    #[test]
    fn metadata_is_carried_but_does_not_affect_matching() {
        let left = Schema::new(vec![Field::new("id", DataType::Int64, false)]);
        let mut meta = std::collections::HashMap::new();
        meta.insert("note".to_string(), "x".to_string());
        let right = Schema::new(vec![
            Field::new("id", DataType::Int64, false).with_metadata(meta),
        ])
        .with_metadata({
            let mut m = std::collections::HashMap::new();
            m.insert("k".to_string(), "v".to_string());
            m
        });
        assert!(diff_schemas(&left, &right).is_empty());
        let _ = Arc::new(right);
    }

    #[test]
    fn change_kind_as_str_covers_every_variant() {
        assert_eq!(ChangeKind::Added.as_str(), "added");
        assert_eq!(ChangeKind::Removed.as_str(), "removed");
        assert_eq!(ChangeKind::TypeChanged.as_str(), "type_changed");
    }
}
