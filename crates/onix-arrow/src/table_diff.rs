//! The [`TableDiff`] result object: the schema diff plus the still-unbuilt
//! row-level members.

use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, RecordBatch, StringArray};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use serde::Serialize;

use crate::error::TableDiffError;
use crate::schema::SchemaChange;

/// Counts of each kind of schema change, returned by [`TableDiff::summary`].
///
/// The row-level counts (rows added/removed, cells changed, duplicate keys)
/// are not present yet; they arrive with the row-diff versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TableDiffSummary {
    /// Columns present on the right but not the left.
    pub columns_added: usize,
    /// Columns present on the left but not the right.
    pub columns_removed: usize,
    /// Columns present on both sides whose type changed.
    pub columns_type_changed: usize,
}

/// The result of [`crate::diff_tables`].
///
/// This version carries the schema diff in full. The row-level members
/// ([`TableDiff::rows_added`], [`TableDiff::rows_removed`],
/// [`TableDiff::cells_changed`], [`TableDiff::duplicate_keys`]) exist so the
/// type and its callers are stable across versions, but each returns
/// [`TableDiffError::NotImplemented`] until a later version fills it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDiff {
    schema: Vec<SchemaChange>,
}

/// Serialization shape of [`TableDiff::to_json`].
#[derive(Serialize)]
struct TableDiffJson<'a> {
    schema: &'a [SchemaChange],
    summary: TableDiffSummary,
}

impl TableDiff {
    /// Builds a result from a finished schema diff. Internal to the crate;
    /// callers use [`crate::diff_tables`].
    pub(crate) fn new(schema: Vec<SchemaChange>) -> Self {
        Self { schema }
    }

    /// The schema changes, in [`crate::schema::diff_schemas`]'s deterministic
    /// order.
    #[must_use]
    pub fn schema(&self) -> &[SchemaChange] {
        &self.schema
    }

    /// Counts of each kind of schema change.
    #[must_use]
    pub fn summary(&self) -> TableDiffSummary {
        let mut summary = TableDiffSummary {
            columns_added: 0,
            columns_removed: 0,
            columns_type_changed: 0,
        };

        for change in &self.schema {
            match change.change {
                crate::schema::ChangeKind::Added => summary.columns_added += 1,
                crate::schema::ChangeKind::Removed => summary.columns_removed += 1,
                crate::schema::ChangeKind::TypeChanged => summary.columns_type_changed += 1,
            }
        }

        summary
    }

    /// The schema diff and its summary as a JSON string.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`serde_json::Error`] if serialization fails,
    /// which does not happen for the value shapes this type holds.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&TableDiffJson {
            schema: &self.schema,
            summary: self.summary(),
        })
    }

    /// The schema diff as an Arrow [`RecordBatch`], one row per changed
    /// column, with columns `column`, `change`, `left_type`, `right_type`,
    /// `left_nullable`, `right_nullable`. This is the zero-Python-copy view
    /// that the bindings hand back to polars, pandas, or pyarrow.
    ///
    /// # Errors
    ///
    /// Returns an [`ArrowError`] only if the fixed, internally-consistent
    /// column arrays fail to assemble into a batch, which does not happen in
    /// practice.
    pub fn schema_record_batch(&self) -> Result<RecordBatch, ArrowError> {
        let columns: StringArray = self
            .schema
            .iter()
            .map(|c| c.column.as_str())
            .collect::<Vec<_>>()
            .into();
        let changes: StringArray = self
            .schema
            .iter()
            .map(|c| c.change.as_str())
            .collect::<Vec<_>>()
            .into();
        let left_type: StringArray = self.schema.iter().map(|c| c.left_type.clone()).collect();
        let right_type: StringArray = self.schema.iter().map(|c| c.right_type.clone()).collect();
        let left_nullable: BooleanArray = self.schema.iter().map(|c| c.left_nullable).collect();
        let right_nullable: BooleanArray = self.schema.iter().map(|c| c.right_nullable).collect();

        let arrays: Vec<ArrayRef> = vec![
            Arc::new(columns),
            Arc::new(changes),
            Arc::new(left_type),
            Arc::new(right_type),
            Arc::new(left_nullable),
            Arc::new(right_nullable),
        ];

        RecordBatch::try_new(schema_batch_schema(), arrays)
    }

    /// Rows present only on the right (added). Not implemented in this
    /// version.
    ///
    /// # Errors
    ///
    /// Always returns [`TableDiffError::NotImplemented`] in this version.
    pub fn rows_added(&self) -> Result<RecordBatch, TableDiffError> {
        Err(TableDiffError::NotImplemented {
            feature: "rows_added",
        })
    }

    /// Rows present only on the left (removed). Not implemented in this
    /// version.
    ///
    /// # Errors
    ///
    /// Always returns [`TableDiffError::NotImplemented`] in this version.
    pub fn rows_removed(&self) -> Result<RecordBatch, TableDiffError> {
        Err(TableDiffError::NotImplemented {
            feature: "rows_removed",
        })
    }

    /// Per-cell changes for rows present on both sides. Not implemented in
    /// this version.
    ///
    /// # Errors
    ///
    /// Always returns [`TableDiffError::NotImplemented`] in this version.
    pub fn cells_changed(&self) -> Result<RecordBatch, TableDiffError> {
        Err(TableDiffError::NotImplemented {
            feature: "cells_changed",
        })
    }

    /// Keys appearing more than once on either side. Not implemented in this
    /// version.
    ///
    /// # Errors
    ///
    /// Always returns [`TableDiffError::NotImplemented`] in this version.
    pub fn duplicate_keys(&self) -> Result<RecordBatch, TableDiffError> {
        Err(TableDiffError::NotImplemented {
            feature: "duplicate_keys",
        })
    }
}

/// The fixed Arrow schema of the batch [`TableDiff::schema_record_batch`]
/// produces.
fn schema_batch_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("column", DataType::Utf8, false),
        Field::new("change", DataType::Utf8, false),
        Field::new("left_type", DataType::Utf8, true),
        Field::new("right_type", DataType::Utf8, true),
        Field::new("left_nullable", DataType::Boolean, true),
        Field::new("right_nullable", DataType::Boolean, true),
    ]))
}

#[cfg(test)]
mod tests {
    use super::TableDiff;
    use crate::error::TableDiffError;
    use crate::schema::{ChangeKind, SchemaChange};
    use arrow_array::Array;

    fn sample() -> TableDiff {
        TableDiff::new(vec![
            SchemaChange {
                column: "gone".to_string(),
                change: ChangeKind::Removed,
                left_type: Some("Utf8".to_string()),
                right_type: None,
                left_nullable: Some(true),
                right_nullable: None,
            },
            SchemaChange {
                column: "age".to_string(),
                change: ChangeKind::TypeChanged,
                left_type: Some("Int32".to_string()),
                right_type: Some("Int64".to_string()),
                left_nullable: Some(false),
                right_nullable: Some(true),
            },
            SchemaChange {
                column: "fresh".to_string(),
                change: ChangeKind::Added,
                left_type: None,
                right_type: Some("Float64".to_string()),
                left_nullable: None,
                right_nullable: Some(true),
            },
        ])
    }

    #[test]
    fn summary_counts_each_kind() {
        let summary = sample().summary();
        assert_eq!(summary.columns_added, 1);
        assert_eq!(summary.columns_removed, 1);
        assert_eq!(summary.columns_type_changed, 1);
    }

    #[test]
    fn empty_diff_summary_is_all_zero() {
        let summary = TableDiff::new(Vec::new()).summary();
        assert_eq!(summary.columns_added, 0);
        assert_eq!(summary.columns_removed, 0);
        assert_eq!(summary.columns_type_changed, 0);
    }

    #[test]
    fn to_json_has_schema_and_summary() {
        let json: serde_json::Value = serde_json::from_str(&sample().to_json().unwrap()).unwrap();
        assert_eq!(json["summary"]["columns_added"], 1);
        assert_eq!(json["summary"]["columns_removed"], 1);
        assert_eq!(json["summary"]["columns_type_changed"], 1);
        assert_eq!(json["schema"][0]["column"], "gone");
        assert_eq!(json["schema"][0]["change"], "removed");
        assert_eq!(json["schema"][0]["right_type"], serde_json::Value::Null);
        assert_eq!(json["schema"][1]["change"], "type_changed");
        assert_eq!(json["schema"][2]["change"], "added");
    }

    #[test]
    fn schema_record_batch_matches_the_changes() {
        let batch = sample().schema_record_batch().unwrap();
        assert_eq!(batch.num_rows(), 3);
        assert_eq!(batch.num_columns(), 6);
        assert_eq!(
            batch.schema().field(0).name(),
            "column",
            "first column is the changed column name"
        );

        let columns = batch
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .unwrap();
        assert_eq!(columns.value(0), "gone");
        assert_eq!(columns.value(2), "fresh");

        let right_type = batch
            .column(3)
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .unwrap();
        assert!(right_type.is_null(0), "removed column has no right_type");
        assert_eq!(right_type.value(2), "Float64");

        let left_nullable = batch
            .column(4)
            .as_any()
            .downcast_ref::<arrow_array::BooleanArray>()
            .unwrap();
        assert!(
            left_nullable.is_null(2),
            "added column has no left_nullable"
        );
        assert!(!left_nullable.value(1), "age is non-null on the left");
    }

    #[test]
    fn empty_diff_yields_an_empty_batch_with_the_full_schema() {
        let batch = TableDiff::new(Vec::new()).schema_record_batch().unwrap();
        assert_eq!(batch.num_rows(), 0);
        assert_eq!(batch.num_columns(), 6);
    }

    #[test]
    fn each_row_level_member_names_itself() {
        assert!(matches!(
            sample().rows_added(),
            Err(TableDiffError::NotImplemented {
                feature: "rows_added"
            })
        ));
        assert!(matches!(
            sample().rows_removed(),
            Err(TableDiffError::NotImplemented {
                feature: "rows_removed"
            })
        ));
        assert!(matches!(
            sample().cells_changed(),
            Err(TableDiffError::NotImplemented {
                feature: "cells_changed"
            })
        ));
        assert!(matches!(
            sample().duplicate_keys(),
            Err(TableDiffError::NotImplemented {
                feature: "duplicate_keys"
            })
        ));
    }
}
