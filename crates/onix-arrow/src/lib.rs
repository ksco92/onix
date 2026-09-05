//! Table diffing for onix, over Apache Arrow.
//!
//! [`diff_tables`] compares two tables presented as [`TableInput`]s and returns
//! a [`TableDiff`] carrying the **schema** diff — which columns were added,
//! removed, or changed type — the keyed **row** diff — which rows were added,
//! removed, or changed, and which keys are duplicated — and the per-cell diff
//! (`cells_changed`), reporting which columns changed in each changed row and
//! how.
//!
//! The two tables are matched on a required, non-empty set of key columns (the
//! table's primary key), carried in [`TableDiffOptions`]. Every key column must
//! exist on both sides — a missing one is a [`TableDiffError::KeyColumnMissing`].
//! Column names must be unique on each side; a repeated name is a
//! [`TableDiffError::DuplicateColumn`].
//!
//! # Inputs
//!
//! The row diff reads each side more than once (to hash every row, to
//! materialize the added/removed rows, and to materialize the changed rows for
//! the per-cell diff), so [`diff_tables`] takes a re-openable [`TableInput`]
//! rather than a single-use `RecordBatchReader`.
//! In-memory tables use [`MemoryInput`]; a caller whose data is a one-shot
//! stream spools it to a temporary Arrow IPC file first and implements
//! [`TableInput`] over that file (as the Python bindings do).
//!
//! # Row diff
//!
//! Rows are matched by a keyed 128-bit hash of the key columns; a row present
//! only on one side is added or removed, a row on both sides whose non-key
//! columns differ is changed, and a key appearing more than once on either side
//! is a duplicate — reported with its per-side counts and excluded from the
//! added/removed/changed sets. Only the non-key columns present on *both* sides
//! take part in change detection (a column on one side only is a schema change,
//! not a cell change), and only scalar columns are compared; a nested non-key
//! column is skipped, while a nested key column is a
//! [`TableDiffError::UnsupportedRowType`]. See `src/row_diff.rs` for the exact
//! value semantics.
//!
//! # Type comparison
//!
//! Two columns of the same name are "changed type" when their Arrow
//! [`arrow_schema::DataType`]s differ, comparing every logical parameter
//! (timestamp unit and timezone, decimal precision and scale). Nullability is
//! ignored (but reported). Physical encodings that carry the same logical type
//! are treated as equal — recursively — so a column keeps the same type when a
//! producer picks a different encoding of it: dictionary encoding, string and
//! binary views, and the several list variants all normalize together
//! (`diff_tables(pl.DataFrame, pa.Table)` does not flag every string column, for
//! instance); a `FixedSizeList` keeps its width; and a list of structs named
//! exactly `key`/`value` with a nullable key is read as a map, on every list
//! variant, so a real map and such a list are not distinguished. See
//! `normalized_type` in `src/schema.rs` for the exact, enumerated rule list.
//!
//! The reported `left_type`/`right_type` strings show the actual (un-normalized)
//! Arrow type, so a real change reports exactly what each side holds.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//!
//! use arrow_array::{Int64Array, RecordBatch};
//! use arrow_schema::{DataType, Field, Schema};
//! use onix_arrow::{diff_tables, MemoryInput, TableDiffOptions};
//!
//! let left_schema = Arc::new(Schema::new(vec![
//!     Field::new("id", DataType::Int64, false),
//!     Field::new("amount", DataType::Int64, true),
//! ]));
//! let right_schema = left_schema.clone();
//!
//! let left = MemoryInput::new(
//!     left_schema.clone(),
//!     vec![RecordBatch::try_new(
//!         left_schema.clone(),
//!         vec![
//!             Arc::new(Int64Array::from(vec![1, 2])),
//!             Arc::new(Int64Array::from(vec![10, 20])),
//!         ],
//!     )
//!     .unwrap()],
//! );
//! let right = MemoryInput::new(
//!     right_schema.clone(),
//!     vec![RecordBatch::try_new(
//!         right_schema,
//!         vec![
//!             Arc::new(Int64Array::from(vec![2, 3])),
//!             Arc::new(Int64Array::from(vec![20, 30])),
//!         ],
//!     )
//!     .unwrap()],
//! );
//!
//! let options = TableDiffOptions::new(vec!["id".to_string()]);
//! let diff = diff_tables(&left, &right, &options).unwrap();
//!
//! // id 1 is only on the left (removed), id 3 only on the right (added).
//! assert_eq!(diff.summary().rows_removed, 1);
//! assert_eq!(diff.summary().rows_added, 1);
//! assert_eq!(diff.rows_added().unwrap().num_rows(), 1);
//! ```

mod error;
mod json_rows;
mod options;
mod row_diff;
mod schema;
mod table_diff;

pub use error::{Side, TableDiffError};
pub use json_rows::MAX_JSON_ROWS;
pub use options::TableDiffOptions;
pub use row_diff::{MemoryInput, TableInput};
pub use schema::{ChangeKind, SchemaChange, diff_schemas};
pub use table_diff::{TableDiff, TableDiffSummary};

/// The maximum column-type nesting depth [`diff_tables`] will compare; a
/// column nested deeper is refused with [`TableDiffError::MaxDepthExceeded`].
///
/// Arrow `DataType` nesting depth is attacker-controlled and unbounded, and
/// every recursive walk over one — the comparison, the type's `Display` (which
/// renders the report), and its own `Clone`/`Drop` — is a native-stack sink
/// that would abort the process with an uncatchable overflow, not a Python
/// exception (pyarrow's own recursive `str()` survives depths where these
/// die, so the producer is not a backstop). This bound is checked iteratively,
/// before any recursive walk runs, and converts that hazard into a recoverable
/// error.
///
/// The value, 128, is far above any real Arrow schema (nesting beyond a
/// handful of levels is exotic; a hundred is unheard of). The per-level native
/// stack cost of those recursive walks is measured by a committed example,
/// `crates/onix-arrow/examples/type_stack_cost.rs` (`cargo run -p onix-arrow
/// --example type_stack_cost`, and `--release`), which builds a type nested to
/// a given depth and binary-searches the deepest one that survives a
/// clone + `Display` + drop on a fixed-size stack. The worst case is nested
/// structs in a debug build (the profile `cargo test` uses) at roughly
/// 5.0 KiB per level; release is roughly 0.7 KiB. So 128 levels costs on the
/// order of 640 KiB debug / 90 KiB release. The Python bindings run the whole
/// operation — the recursive FFI import and the drop of the imported types
/// included — on the large stack-sized worker thread `crate::guard` sizes for
/// the JSON path (hundreds of MiB), which clears the debug worst case by
/// roughly 500x, so this bound is the clean-error ceiling rather than the sole
/// backstop.
pub const MAX_NESTING_DEPTH: usize = 128;

/// Diffs two tables presented as re-openable [`TableInput`]s.
///
/// See the [crate-level docs](crate) for the type-comparison rules, the
/// key-column contract, and the row-diff semantics.
///
/// # Errors
///
/// - [`TableDiffError::EmptyKey`] if `options` has no key columns.
/// - [`TableDiffError::MaxDepthExceeded`] if a column's type is nested past
///   [`MAX_NESTING_DEPTH`].
/// - [`TableDiffError::DuplicateColumn`] if either input has two columns with
///   the same name.
/// - [`TableDiffError::KeyColumnMissing`] if a key column is absent from
///   either input's schema, naming the column and the side.
/// - [`TableDiffError::KeyTypeMismatch`] if a key column's normalized type
///   differs across the two inputs (a primary key that changed type is refused,
///   not coerced).
/// - [`TableDiffError::UnsupportedRowType`] if a key column's type, or any
///   non-nested column's type on either side, cannot be hashed by value — a
///   nested key, a run-end-encoded column, or a type combination Arrow cannot
///   build.
/// - [`TableDiffError::Read`] if a batch cannot be read from either input.
/// - [`TableDiffError::Render`] if a changed cell's value cannot be rendered to
///   its canonical string — for example an out-of-range temporal value the
///   formatter cannot format.
/// - [`TableDiffError::TooManyChangedRows`] if one side has more than
///   `u32::MAX` changed rows, which the per-cell diff's row-index arrays cannot
///   address.
/// - [`TableDiffError::EqualRenderings`] never fires for real input: it guards
///   the internal invariant that a `value_changed` cell always renders two
///   different strings.
pub fn diff_tables(
    left: &impl TableInput,
    right: &impl TableInput,
    options: &TableDiffOptions,
) -> Result<TableDiff, TableDiffError> {
    if options.key().is_empty() {
        return Err(TableDiffError::EmptyKey);
    }

    let left_schema = left.schema();
    let right_schema = right.schema();

    // Runs the schema diff first: it rejects duplicate column names and
    // over-deep nesting, after which the key lookups below are unambiguous and
    // the row hash walks only bounded, scalar columns.
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

        // The key's own hashability (a nested or otherwise unhashable key) is
        // checked, along with every other column, by `row_diff::diff_rows`; only
        // key existence and the type-mismatch below live here.

        // A key whose normalized type differs across sides is a schema type
        // change; refuse it rather than guess row identity across a changed key
        // type (the conservative choice). The schema diff above already computed
        // this.
        if changes
            .iter()
            .any(|change| &change.column == key && change.change == ChangeKind::TypeChanged)
        {
            return Err(TableDiffError::KeyTypeMismatch {
                column: key.clone(),
            });
        }
    }

    let rows = row_diff::diff_rows(left, right, &left_schema, &right_schema, options.key())?;

    Ok(TableDiff::new(changes, rows))
}

#[cfg(test)]
mod tests {
    use super::{MemoryInput, Side, TableDiffError, TableDiffOptions, diff_tables};
    use arrow_schema::{DataType, Field, Schema};
    use std::sync::Arc;

    fn reader(fields: Vec<Field>) -> MemoryInput {
        let schema = Arc::new(Schema::new(fields));
        MemoryInput::new(schema, Vec::new())
    }

    #[test]
    fn empty_key_is_rejected() {
        let left = reader(vec![Field::new("id", DataType::Int64, false)]);
        let right = reader(vec![Field::new("id", DataType::Int64, false)]);
        let options = TableDiffOptions::new(Vec::new());
        assert_eq!(
            diff_tables(&left, &right, &options),
            Err(TableDiffError::EmptyKey)
        );
    }

    #[test]
    fn missing_key_on_left_is_named() {
        let left = reader(vec![Field::new("other", DataType::Int64, false)]);
        let right = reader(vec![Field::new("id", DataType::Int64, false)]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert_eq!(
            diff_tables(&left, &right, &options),
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
            diff_tables(&left, &right, &options),
            Err(TableDiffError::KeyColumnMissing {
                column: "id".to_string(),
                side: Side::Right,
            })
        );
    }

    #[test]
    fn key_type_mismatch_int_versus_float_is_rejected() {
        let left = reader(vec![Field::new("id", DataType::Int64, false)]);
        let right = reader(vec![Field::new("id", DataType::Float64, false)]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert_eq!(
            diff_tables(&left, &right, &options),
            Err(TableDiffError::KeyTypeMismatch {
                column: "id".to_string(),
            })
        );
    }

    #[test]
    fn key_type_mismatch_int_widths_is_rejected() {
        let left = reader(vec![Field::new("id", DataType::Int32, false)]);
        let right = reader(vec![Field::new("id", DataType::Int64, false)]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert_eq!(
            diff_tables(&left, &right, &options),
            Err(TableDiffError::KeyTypeMismatch {
                column: "id".to_string(),
            })
        );
    }

    #[test]
    fn dictionary_key_column_is_accepted() {
        // A dictionary key is a scalar encoding, so it passes the up-front
        // hashable-key check (covering the dictionary arm of `is_hashable`).
        let dict = DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8));
        let left = reader(vec![Field::new("id", dict.clone(), false)]);
        let right = reader(vec![Field::new("id", dict, false)]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert!(diff_tables(&left, &right, &options).is_ok());
    }

    #[test]
    fn nested_key_column_is_rejected_up_front() {
        // An empty table with a list key errors before any row is read, rather
        // than diffing to an empty result.
        let list = DataType::List(Arc::new(Field::new("item", DataType::Int64, true)));
        let left = reader(vec![Field::new("k", list.clone(), true)]);
        let right = reader(vec![Field::new("k", list, true)]);
        let options = TableDiffOptions::new(vec!["k".to_string()]);
        assert!(matches!(
            diff_tables(&left, &right, &options),
            Err(TableDiffError::UnsupportedRowType { column, .. }) if column == "k"
        ));
    }

    #[test]
    fn unhashable_and_type_mismatched_key_reports_type_mismatch() {
        // A key that is both unhashable (a nested list on the left) and
        // type-changed across sides: the type-mismatch refusal is checked first,
        // so it wins over the unhashable-column refusal. Pins that priority.
        let list = DataType::List(Arc::new(Field::new("item", DataType::Int64, true)));
        let left = reader(vec![Field::new("id", list, true)]);
        let right = reader(vec![Field::new("id", DataType::Int64, false)]);
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert_eq!(
            diff_tables(&left, &right, &options),
            Err(TableDiffError::KeyTypeMismatch {
                column: "id".to_string(),
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
        let diff = diff_tables(&left, &right, &options).unwrap();

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
            diff_tables(&left, &right, &options),
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
            diff_tables(&left, &right, &options),
            Err(TableDiffError::KeyColumnMissing {
                column: "b".to_string(),
                side: Side::Right,
            })
        );
    }
}
