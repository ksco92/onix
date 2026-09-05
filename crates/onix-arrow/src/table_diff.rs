//! The [`TableDiff`] result object: the schema diff and the keyed row diff.

use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, RecordBatch, StringArray};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use serde::Serialize;

use crate::error::TableDiffError;
use crate::row_diff::RowDiff;
use crate::schema::SchemaChange;

/// Counts of each kind of schema and row change, returned by
/// [`TableDiff::summary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TableDiffSummary {
    /// Columns present on the right but not the left.
    pub columns_added: usize,
    /// Columns present on the left but not the right.
    pub columns_removed: usize,
    /// Columns present on both sides whose type changed.
    pub columns_type_changed: usize,
    /// Rows present only on the right (excluding duplicate keys).
    pub rows_added: usize,
    /// Rows present only on the left (excluding duplicate keys).
    pub rows_removed: usize,
    /// Rows present on both sides whose non-key values differ (excluding
    /// duplicate keys).
    pub rows_changed: usize,
    /// Keys appearing more than once on either side; these are reported
    /// separately and excluded from the added/removed/changed counts above.
    pub duplicate_keys: usize,
    /// Distinct keys with a null in any key column, counted across both sides;
    /// a null key still matches its counterpart, so this is an informational
    /// count, not an exclusion.
    pub null_keys: usize,
    /// Total number of changed cells across all changed rows — the row count of
    /// [`TableDiff::cells_changed`].
    pub cells_changed: usize,
}

/// The result of [`crate::diff_tables`].
///
/// Carries the schema diff and the keyed row diff. [`TableDiff::rows_added`],
/// [`TableDiff::rows_removed`], [`TableDiff::duplicate_keys`], and
/// [`TableDiff::cells_changed`] each return an Arrow batch.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDiff {
    schema: Vec<SchemaChange>,
    rows: RowDiff,
}

/// Serialization shape of [`TableDiff::to_json`].
#[derive(Serialize)]
struct TableDiffJson<'a> {
    schema: &'a [SchemaChange],
    summary: TableDiffSummary,
}

impl TableDiff {
    /// Builds a result from a finished schema diff and row diff. Internal to
    /// the crate; callers use [`crate::diff_tables`].
    pub(crate) fn new(schema: Vec<SchemaChange>, rows: RowDiff) -> Self {
        Self { schema, rows }
    }

    /// The schema changes, in [`crate::schema::diff_schemas`]'s deterministic
    /// order.
    #[must_use]
    pub fn schema(&self) -> &[SchemaChange] {
        &self.schema
    }

    /// Counts of each kind of schema and row change.
    #[must_use]
    pub fn summary(&self) -> TableDiffSummary {
        let mut summary = TableDiffSummary {
            columns_added: 0,
            columns_removed: 0,
            columns_type_changed: 0,
            rows_added: self.rows.counts.rows_added,
            rows_removed: self.rows.counts.rows_removed,
            rows_changed: self.rows.counts.rows_changed,
            duplicate_keys: self.rows.counts.duplicate_keys,
            null_keys: self.rows.counts.null_keys,
            cells_changed: self.rows.counts.cells_changed,
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

    /// Rows present only on the right (added), in the right table's schema and
    /// excluding duplicate keys.
    ///
    /// # Errors
    ///
    /// Never fails; the [`Result`] keeps the signature uniform with the other
    /// row-level members.
    pub fn rows_added(&self) -> Result<RecordBatch, TableDiffError> {
        Ok(self.rows.rows_added.clone())
    }

    /// Rows present only on the left (removed), in the left table's schema and
    /// excluding duplicate keys.
    ///
    /// # Errors
    ///
    /// Never fails; the [`Result`] keeps the signature uniform with the other
    /// row-level members.
    pub fn rows_removed(&self) -> Result<RecordBatch, TableDiffError> {
        Ok(self.rows.rows_removed.clone())
    }

    /// Per-cell changes for rows present on both sides with differing non-key
    /// values: the key columns, then `column`, `old_value`, `new_value`, and
    /// `change`. One row per changed cell, ordered by the canonical string
    /// rendering of the key columns, then left-schema column order.
    ///
    /// # Errors
    ///
    /// Never fails; the [`Result`] keeps the signature uniform with the other
    /// row-level members.
    pub fn cells_changed(&self) -> Result<RecordBatch, TableDiffError> {
        Ok(self.rows.cells_changed.clone())
    }

    /// Keys appearing more than once on either side: the key columns, then
    /// `left_count` and `right_count`.
    ///
    /// # Errors
    ///
    /// Never fails; the [`Result`] keeps the signature uniform with the other
    /// row-level members.
    pub fn duplicate_keys(&self) -> Result<RecordBatch, TableDiffError> {
        Ok(self.rows.duplicate_keys.clone())
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
    use crate::row_diff::{RowCounts, RowDiff};
    use crate::schema::{ChangeKind, SchemaChange};
    use arrow_array::{Array, Int64Array, RecordBatch};
    use arrow_schema::{DataType, Field, Schema};
    use std::sync::Arc;

    fn empty_batch() -> RecordBatch {
        RecordBatch::new_empty(Arc::new(Schema::new(vec![Field::new(
            "id",
            DataType::Int64,
            false,
        )])))
    }

    fn row_diff(counts: RowCounts) -> RowDiff {
        RowDiff {
            rows_added: empty_batch(),
            rows_removed: empty_batch(),
            duplicate_keys: empty_batch(),
            cells_changed: empty_batch(),
            counts,
        }
    }

    fn no_rows() -> RowCounts {
        RowCounts {
            rows_added: 0,
            rows_removed: 0,
            rows_changed: 0,
            duplicate_keys: 0,
            null_keys: 0,
            cells_changed: 0,
        }
    }

    fn sample() -> TableDiff {
        TableDiff::new(
            vec![
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
            ],
            row_diff(no_rows()),
        )
    }

    #[test]
    fn summary_counts_each_kind() {
        let summary = sample().summary();
        assert_eq!(summary.columns_added, 1);
        assert_eq!(summary.columns_removed, 1);
        assert_eq!(summary.columns_type_changed, 1);
    }

    #[test]
    fn summary_carries_the_row_counts() {
        let counts = RowCounts {
            rows_added: 2,
            rows_removed: 3,
            rows_changed: 4,
            duplicate_keys: 5,
            null_keys: 6,
            cells_changed: 7,
        };
        let diff = TableDiff::new(Vec::new(), row_diff(counts));
        let summary = diff.summary();
        assert_eq!(summary.rows_added, 2);
        assert_eq!(summary.rows_removed, 3);
        assert_eq!(summary.rows_changed, 4);
        assert_eq!(summary.duplicate_keys, 5);
        assert_eq!(summary.null_keys, 6);
        assert_eq!(summary.cells_changed, 7);
    }

    #[test]
    fn empty_diff_summary_is_all_zero() {
        let summary = TableDiff::new(Vec::new(), row_diff(no_rows())).summary();
        assert_eq!(summary.columns_added, 0);
        assert_eq!(summary.columns_removed, 0);
        assert_eq!(summary.columns_type_changed, 0);
        assert_eq!(summary.rows_added, 0);
    }

    #[test]
    fn to_json_has_schema_and_summary() {
        let json: serde_json::Value = serde_json::from_str(&sample().to_json().unwrap()).unwrap();
        assert_eq!(json["summary"]["columns_added"], 1);
        assert_eq!(json["summary"]["columns_removed"], 1);
        assert_eq!(json["summary"]["columns_type_changed"], 1);
        assert_eq!(json["summary"]["rows_added"], 0);
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
        let batch = TableDiff::new(Vec::new(), row_diff(no_rows()))
            .schema_record_batch()
            .unwrap();
        assert_eq!(batch.num_rows(), 0);
        assert_eq!(batch.num_columns(), 6);
    }

    #[test]
    fn row_members_return_their_batches() {
        let counts = no_rows();
        let mut rows = row_diff(counts);
        rows.rows_added = RecordBatch::try_new(
            Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)])),
            vec![Arc::new(Int64Array::from(vec![7])) as _],
        )
        .unwrap();
        let diff = TableDiff::new(Vec::new(), rows);

        assert_eq!(diff.rows_added().unwrap().num_rows(), 1);
        assert_eq!(diff.rows_removed().unwrap().num_rows(), 0);
        assert_eq!(diff.duplicate_keys().unwrap().num_rows(), 0);
    }

    #[test]
    fn cells_changed_returns_its_batch() {
        let counts = no_rows();
        let mut rows = row_diff(counts);
        rows.cells_changed = RecordBatch::try_new(
            Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)])),
            vec![Arc::new(Int64Array::from(vec![1, 2])) as _],
        )
        .unwrap();
        let diff = TableDiff::new(Vec::new(), rows);

        assert_eq!(diff.cells_changed().unwrap().num_rows(), 2);
    }
}
