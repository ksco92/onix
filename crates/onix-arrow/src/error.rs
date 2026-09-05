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
/// Marked `#[non_exhaustive]` because the later row-diff slices add their own
/// variants (streaming/read failures and the like); matching on it must keep
/// a wildcard arm.
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
    /// A member of [`crate::TableDiff`] that a later version fills in was
    /// asked for before that version exists. Schema diffing is complete in
    /// this version; row-level results (`rows_added`, `rows_removed`,
    /// `cells_changed`, `duplicate_keys`) are not.
    NotImplemented {
        /// The name of the member that is not implemented yet.
        feature: &'static str,
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
            TableDiffError::NotImplemented { feature } => write!(
                f,
                "{feature} is not implemented in this version; \
                 only the schema diff is available yet"
            ),
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
    fn not_implemented_message_names_the_feature() {
        let error = TableDiffError::NotImplemented {
            feature: "rows_added",
        };
        let message = error.to_string();
        assert!(message.contains("rows_added"));
        assert!(message.contains("not implemented"));
    }

    #[test]
    fn error_implements_std_error() {
        let error = TableDiffError::EmptyKey;
        let _: &dyn std::error::Error = &error;
    }
}
