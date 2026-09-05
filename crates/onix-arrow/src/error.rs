//! Error type returned by [`crate::diff_tables`] and the row-level members
//! of [`crate::TableDiff`].

use std::fmt;

/// Which of the two inputs a problem was found on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The `left` input to [`crate::diff_tables`].
    Left,
    /// The `right` input to [`crate::diff_tables`].
    Right,
}

impl Side {
    /// The lowercase word used for this side in error messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors that can occur while diffing two tables.
///
/// Marked `#[non_exhaustive]` so future work can add variants without a
/// breaking change; matching on it must keep a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableDiffError {
    /// The key column set was empty. A table diff is keyed on a primary key
    /// the way a database table is, so at least one key column is required.
    EmptyKey,
    /// A requested key column is absent from one of the inputs' schemas.
    KeyColumnMissing {
        /// The name of the key column that was not found.
        column: String,
        /// The side whose schema lacks the column.
        side: Side,
    },
    /// One of the inputs has two or more columns with the same name. A table
    /// diff needs column names to be unique on each side: columns are matched
    /// across the two sides by name, and the later row diff keys on unique
    /// column names too.
    DuplicateColumn {
        /// The duplicated column name.
        column: String,
        /// The side whose schema contains the duplicate.
        side: Side,
    },
    /// A column's Arrow type is nested more deeply than the diff will walk.
    /// Arrow nesting depth is attacker-controlled and unbounded, and every
    /// recursive walk over a [`arrow_schema::DataType`] (the comparison here,
    /// plus the type's own `Display`, `Clone`, and `Drop`) is a native-stack
    /// sink; this bound turns a would-be stack overflow into a recoverable
    /// error. The bound is far above any real schema — see
    /// [`crate::MAX_NESTING_DEPTH`].
    MaxDepthExceeded {
        /// The column whose type is too deeply nested.
        column: String,
        /// The maximum nesting depth allowed.
        max_depth: usize,
    },
    /// A column the row diff must hash has an Arrow type it cannot hash by
    /// value: a key column of any such type, or a non-key *scalar* column the
    /// crate does not handle (for example `RunEndEncoded`). A *nested* non-key
    /// column (list, struct, map, union) is out of scope and skipped, not
    /// refused; only a nested *key* column reaches this error.
    UnsupportedRowType {
        /// The column whose type is unsupported.
        column: String,
        /// The unsupported Arrow type, rendered.
        data_type: String,
    },
    /// A key column has a different (normalized) Arrow type on each side. This
    /// is the deliberate conservative choice: a primary key that changed type is
    /// refused rather than guessed — the row diff will not coerce one side to
    /// the other's type to decide row identity.
    KeyTypeMismatch {
        /// The key column whose type differs across the two inputs.
        column: String,
    },
    /// A batch could not be read from an input reader, or a temporary spool
    /// file could not be written or re-read, while diffing rows.
    Read {
        /// The underlying error's message.
        message: String,
    },
    /// A cell value could not be rendered to its canonical string for the
    /// per-cell diff — for example a temporal value outside the range the
    /// formatter can format. Reported as a typed error rather than written into
    /// the output as error prose (which a real string cell could not be told
    /// apart from).
    Render {
        /// The column whose cell could not be rendered.
        column: String,
        /// The underlying formatting error's message.
        message: String,
    },
    /// The per-cell diff would need more than `u32::MAX` changed rows on one
    /// side, which its row-index arrays cannot address. Bound the changed-row
    /// count of untrusted input.
    TooManyChangedRows {
        /// The number of changed rows that overflowed.
        rows: usize,
    },
    /// A `value_changed` cell rendered identically on both sides — a broken
    /// invariant, not a caller error. The per-cell diff renders each side in a
    /// common comparison form so a value change always shows two different
    /// strings; this variant guards that guarantee and cannot fire for any real
    /// input.
    EqualRenderings {
        /// The column whose two renderings were equal.
        column: String,
    },
    /// [`crate::TableDiff::to_json`] would embed more row objects than
    /// [`crate::MAX_JSON_ROWS`] allows (see its own doc for what that caps
    /// and why). Use the Arrow-returning members instead, or export the
    /// batches directly, for a diff this large.
    TooManyJsonRows {
        /// The number of row objects `to_json()` would have embedded.
        rows: usize,
        /// The cap that was exceeded.
        max: usize,
    },
    /// [`crate::TableDiff::to_json`]'s `serde_json` serialization failed.
    /// Its input is a fixed set of already-rendered strings, numbers, and
    /// nested objects/arrays, so this does not happen in practice; it is a
    /// typed error rather than a panic because a public API must return,
    /// not abort, on an unexpected failure.
    Json {
        /// The underlying `serde_json` error's message.
        message: String,
    },
}

impl fmt::Display for TableDiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableDiffError::EmptyKey => f.write_str(
                "the key column set is empty; a table diff requires at least one key column",
            ),
            TableDiffError::KeyColumnMissing { column, side } => write!(
                f,
                "key column {column:?} is missing from the {side} table's schema"
            ),
            TableDiffError::DuplicateColumn { column, side } => write!(
                f,
                "the {side} table has more than one column named {column:?}; \
                 column names must be unique on each side"
            ),
            TableDiffError::MaxDepthExceeded { column, max_depth } => write!(
                f,
                "column {column:?} has an Arrow type nested deeper than the maximum of \
                 {max_depth}; diffing it could overflow the native stack, so it is refused"
            ),
            TableDiffError::UnsupportedRowType { column, data_type } => write!(
                f,
                "column {column:?} has Arrow type {data_type}, which the row diff cannot \
                 compare by value; a key column and a non-key scalar column must be a \
                 supported scalar type (nested non-key columns are skipped instead)"
            ),
            TableDiffError::KeyTypeMismatch { column } => write!(
                f,
                "key column {column:?} has a different type on each side; a keyed row diff \
                 needs the key to be the same type on both sides"
            ),
            TableDiffError::Read { message } => write!(f, "failed to read table data: {message}"),
            TableDiffError::Render { column, message } => write!(
                f,
                "could not render a value of column {column:?} to its canonical string: {message}"
            ),
            TableDiffError::TooManyChangedRows { rows } => write!(
                f,
                "the per-cell diff has {rows} changed rows on one side, more than the \
                 {} its row-index arrays can address; bound the changed-row count",
                u32::MAX
            ),
            TableDiffError::EqualRenderings { column } => write!(
                f,
                "internal invariant: a value change in column {column:?} rendered identically \
                 on both sides, which the common-form rendering is designed to prevent"
            ),
            TableDiffError::TooManyJsonRows { rows, max } => write!(
                f,
                "to_json() would embed {rows} row objects, more than the {max}-row cap; use \
                 the Arrow-returning members (rows_added(), rows_removed(), cells_changed(), \
                 duplicate_keys()) or export the batches directly for a diff this large"
            ),
            TableDiffError::Json { message } => {
                write!(f, "failed to serialize the table diff to JSON: {message}")
            }
        }
    }
}

impl std::error::Error for TableDiffError {}

#[cfg(test)]
mod tests {
    use super::{Side, TableDiffError};

    #[test]
    fn side_renders_lowercase() {
        assert_eq!(Side::Left.to_string(), "left");
        assert_eq!(Side::Right.to_string(), "right");
    }

    #[test]
    fn empty_key_message_names_the_requirement() {
        let message = TableDiffError::EmptyKey.to_string();
        assert!(message.contains("at least one key column"));
    }

    #[test]
    fn key_column_missing_message_names_column_and_side() {
        let error = TableDiffError::KeyColumnMissing {
            column: "id".to_string(),
            side: Side::Right,
        };
        let message = error.to_string();
        assert!(message.contains("\"id\""));
        assert!(message.contains("right"));
    }

    #[test]
    fn duplicate_column_message_names_column_and_side() {
        let error = TableDiffError::DuplicateColumn {
            column: "x".to_string(),
            side: Side::Left,
        };
        let message = error.to_string();
        assert!(message.contains("\"x\""));
        assert!(message.contains("left"));
        assert!(message.contains("unique"));
    }

    #[test]
    fn max_depth_exceeded_message_names_column_and_bound() {
        let error = TableDiffError::MaxDepthExceeded {
            column: "deep".to_string(),
            max_depth: 128,
        };
        let message = error.to_string();
        assert!(message.contains("\"deep\""));
        assert!(message.contains("128"));
    }

    #[test]
    fn unsupported_row_type_message_names_column_and_type() {
        let error = TableDiffError::UnsupportedRowType {
            column: "xs".to_string(),
            data_type: "RunEndEncoded".to_string(),
        };
        let message = error.to_string();
        assert!(message.contains("\"xs\""));
        assert!(message.contains("RunEndEncoded"));
    }

    #[test]
    fn key_type_mismatch_message_names_the_column() {
        let error = TableDiffError::KeyTypeMismatch {
            column: "id".to_string(),
        };
        let message = error.to_string();
        assert!(message.contains("\"id\""));
        assert!(message.contains("same type on both sides"));
    }

    #[test]
    fn render_message_names_the_column() {
        let error = TableDiffError::Render {
            column: "ts".to_string(),
            message: "Cast error".to_string(),
        };
        let message = error.to_string();
        assert!(message.contains("\"ts\""));
        assert!(message.contains("Cast error"));
    }

    #[test]
    fn equal_renderings_message_reads_as_an_invariant() {
        let error = TableDiffError::EqualRenderings {
            column: "d".to_string(),
        };
        let message = error.to_string();
        assert!(message.contains("\"d\""));
        assert!(message.contains("invariant"));
    }

    #[test]
    fn too_many_changed_rows_message_names_the_count() {
        let error = TableDiffError::TooManyChangedRows {
            rows: 5_000_000_000,
        };
        let message = error.to_string();
        assert!(message.contains("5000000000"));
        assert!(message.contains("changed-row count"));
    }

    #[test]
    fn too_many_json_rows_message_names_count_and_cap() {
        let error = TableDiffError::TooManyJsonRows {
            rows: 20_000,
            max: 10_000,
        };
        let message = error.to_string();
        assert!(message.contains("20000"));
        assert!(message.contains("10000-row cap"));
        assert!(message.contains("rows_added()"));
    }

    #[test]
    fn json_message_names_the_underlying_error() {
        let error = TableDiffError::Json {
            message: "unexpected end of input".to_string(),
        };
        let message = error.to_string();
        assert!(message.contains("unexpected end of input"));
    }

    #[test]
    fn error_implements_std_error() {
        let error = TableDiffError::EmptyKey;
        let _: &dyn std::error::Error = &error;
    }
}
