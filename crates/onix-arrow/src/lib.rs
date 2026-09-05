//! Table diffing for onix, over Apache Arrow.
//!
//! [`diff_tables`] compares two tables presented as Arrow
//! [`RecordBatchReader`]s and returns a [`TableDiff`]. This version computes
//! the **schema** diff — which columns were added, removed, or changed type —
//! in full; the row-level members of [`TableDiff`] exist but return
//! [`TableDiffError::NotImplemented`] until a later version fills them in.
//!
//! The two tables are matched on a required, non-empty set of key columns
//! (the table's primary key), carried in [`TableDiffOptions`]. The key is not
//! used by the schema diff itself, but every key column must exist on both
//! sides — a missing one is a [`TableDiffError::KeyColumnMissing`] — so the
//! later row diff has a valid key to match on. Column names must be unique on
//! each side; a repeated name is a [`TableDiffError::DuplicateColumn`].
//!
//! # Type comparison
//!
//! Two columns of the same name are "changed type" when their Arrow
//! [`arrow_schema::DataType`]s differ, comparing every logical parameter
//! (timestamp unit and timezone, decimal precision and scale). Nullability is
//! ignored (but reported). Physical encodings that carry the same logical type
//! are treated as equal, so a column keeps the same type when a producer picks
//! a different encoding of it (`diff_tables(pl.DataFrame, pa.Table)` does not
//! flag every string column, for instance). The normalized-away encodings,
//! applied recursively through `List`/`LargeList`/`ListView`/`LargeListView`,
//! `Struct`, and `Map` children:
//!
//! - a dictionary-encoded type compares as its value type (dictionary-encoded
//!   string == plain string; `list<dictionary<int32, string>>` ==
//!   `list<string>`);
//! - `Utf8View` and `LargeUtf8` compare as `Utf8`;
//! - `BinaryView` and `LargeBinary` compare as `Binary`;
//! - `LargeList`, `ListView`, and `LargeListView` compare as `List`.
//!
//! The reported `left_type`/`right_type` strings show the actual (un-normalized)
//! Arrow type, so a real change reports exactly what each side holds.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//!
//! use arrow_array::{RecordBatch, RecordBatchIterator};
//! use arrow_schema::{ArrowError, DataType, Field, Schema};
//! use onix_arrow::{diff_tables, TableDiffOptions};
//!
//! let left_schema = Arc::new(Schema::new(vec![
//!     Field::new("id", DataType::Int64, false),
//!     Field::new("amount", DataType::Decimal128(10, 2), true),
//! ]));
//! let right_schema = Arc::new(Schema::new(vec![
//!     Field::new("id", DataType::Int64, false),
//!     Field::new("amount", DataType::Decimal128(10, 4), true),
//! ]));
//!
//! // The schema diff reads only each reader's schema, so the readers can be
//! // empty here.
//! let no_batches: Vec<Result<RecordBatch, ArrowError>> = vec![];
//! let left = RecordBatchIterator::new(no_batches.into_iter(), left_schema);
//! let no_batches: Vec<Result<RecordBatch, ArrowError>> = vec![];
//! let right = RecordBatchIterator::new(no_batches.into_iter(), right_schema);
//!
//! let options = TableDiffOptions::new(vec!["id".to_string()]);
//! let diff = diff_tables(left, right, &options).unwrap();
//!
//! assert_eq!(diff.summary().columns_type_changed, 1);
//! assert_eq!(diff.schema()[0].column, "amount");
//! ```

mod error;
mod options;
mod schema;
mod table_diff;

use arrow_array::RecordBatchReader;

pub use error::{Side, TableDiffError};
pub use options::TableDiffOptions;
pub use schema::{ChangeKind, SchemaChange, diff_schemas};
pub use table_diff::{TableDiff, TableDiffSummary};

/// Diffs two tables presented as Arrow [`RecordBatchReader`]s.
///
/// Only the readers' schemas are read in this version, so their batches are
/// left untouched. See the [crate-level docs](crate) for the type-comparison
/// rules and the key-column contract.
///
/// # Errors
///
/// - [`TableDiffError::EmptyKey`] if `options` has no key columns.
/// - [`TableDiffError::DuplicateColumn`] if either input has two columns with
///   the same name.
/// - [`TableDiffError::KeyColumnMissing`] if a key column is absent from
///   either input's schema, naming the column and the side.
// The readers are taken by value because the row-diff versions consume them
// (they iterate every batch); this version reads only their schemas, so the
// owned readers are dropped here, but the by-value signature is the stable
// contract those versions build on.
#[allow(clippy::needless_pass_by_value)]
pub fn diff_tables(
    left: impl RecordBatchReader,
    right: impl RecordBatchReader,
    options: &TableDiffOptions,
) -> Result<TableDiff, TableDiffError> {
    if options.key().is_empty() {
        return Err(TableDiffError::EmptyKey);
    }

    let left_schema = left.schema();
    let right_schema = right.schema();

    // Runs the schema diff first: it also rejects duplicate column names, after
    // which the key lookups below are unambiguous.
    let changes = diff_schemas(&left_schema, &right_schema)?;

    for key in options.key() {
        if left_schema.field_with_name(key).is_err() {
            return Err(TableDiffError::KeyColumnMissing {
                column: key.clone(),
                side: Side::Left,
            });
        }
        if right_schema.field_with_name(key).is_err() {
            return Err(TableDiffError::KeyColumnMissing {
                column: key.clone(),
                side: Side::Right,
            });
        }
    }

    Ok(TableDiff::new(changes))
}

#[cfg(test)]
mod tests {
    use super::{Side, TableDiffError, TableDiffOptions, diff_tables};
    use arrow_array::{RecordBatch, RecordBatchIterator};
    use arrow_schema::{ArrowError, DataType, Field, Schema};
    use std::sync::Arc;

    type EmptyReader = RecordBatchIterator<std::vec::IntoIter<Result<RecordBatch, ArrowError>>>;

    fn reader(fields: Vec<Field>) -> EmptyReader {
        let schema = Arc::new(Schema::new(fields));
        let batches: Vec<Result<RecordBatch, ArrowError>> = vec![];
        RecordBatchIterator::new(batches.into_iter(), schema)
    }

    #[test]
    fn empty_key_is_rejected() {
        let left = reader(vec![Field::new("id", DataType::Int64, false)]);
        let right = reader(vec![Field::new("id", DataType::Int64, false)]);
        let options = TableDiffOptions::new(Vec::new());
        assert_eq!(
            diff_tables(left, right, &options),
            Err(TableDiffError::EmptyKey)
        );
    }

    #[test]
    fn missing_key_on_left_is_named() {
        let left = reader(vec![Field::new("other", DataType::Int64, false)]);
        let right = reader(vec![Field::new("id", DataType::Int64, false)]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert_eq!(
            diff_tables(left, right, &options),
            Err(TableDiffError::KeyColumnMissing {
                column: "id".to_string(),
                side: Side::Left,
            })
        );
    }

    #[test]
    fn missing_key_on_right_is_named() {
        let left = reader(vec![Field::new("id", DataType::Int64, false)]);
        let right = reader(vec![Field::new("other", DataType::Int64, false)]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert_eq!(
            diff_tables(left, right, &options),
            Err(TableDiffError::KeyColumnMissing {
                column: "id".to_string(),
                side: Side::Right,
            })
        );
    }

    #[test]
    fn present_key_is_not_reported_when_unchanged() {
        let left = reader(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
        ]);
        let right = reader(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Int64, true),
        ]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        let diff = diff_tables(left, right, &options).unwrap();

        assert_eq!(diff.schema().len(), 1);
        assert_eq!(diff.schema()[0].column, "name");
    }

    #[test]
    fn duplicate_column_is_rejected() {
        let left = reader(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("x", DataType::Int64, true),
            Field::new("x", DataType::Utf8, true),
        ]);
        let right = reader(vec![Field::new("id", DataType::Int64, false)]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert_eq!(
            diff_tables(left, right, &options),
            Err(TableDiffError::DuplicateColumn {
                column: "x".to_string(),
                side: Side::Left,
            })
        );
    }

    #[test]
    fn composite_key_all_columns_must_exist() {
        let left = reader(vec![
            Field::new("a", DataType::Int64, false),
            Field::new("b", DataType::Int64, false),
        ]);
        let right = reader(vec![Field::new("a", DataType::Int64, false)]);
        let options = TableDiffOptions::new(vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            diff_tables(left, right, &options),
            Err(TableDiffError::KeyColumnMissing {
                column: "b".to_string(),
                side: Side::Right,
            })
        );
    }
}
