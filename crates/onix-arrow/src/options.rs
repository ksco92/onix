//! Options controlling a table diff.

/// Options for [`crate::diff_tables`].
///
/// The key columns are the table's primary key: rows are matched across the
/// two inputs by their values (in the later row-diff versions), and the key
/// must be non-empty. Later versions add more fields (value-comparison
/// tolerances and the like); construct this through [`TableDiffOptions::new`]
/// rather than a struct literal so those additions stay backward compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDiffOptions {
    /// The key columns, in the order given. Required and non-empty; an empty
    /// key makes [`crate::diff_tables`] return
    /// [`crate::TableDiffError::EmptyKey`].
    key: Vec<String>,
}

impl TableDiffOptions {
    /// Creates options keyed on `key`.
    ///
    /// No validation happens here — an empty `key` is reported by
    /// [`crate::diff_tables`] so every misuse surfaces through one error
    /// channel.
    #[must_use]
    pub fn new(key: Vec<String>) -> Self {
        Self { key }
    }

    /// The key columns, in the order supplied.
    #[must_use]
    pub fn key(&self) -> &[String] {
        &self.key
    }
}

#[cfg(test)]
mod tests {
    use super::TableDiffOptions;

    #[test]
    fn new_preserves_key_order() {
        let options = TableDiffOptions::new(vec!["b".to_string(), "a".to_string()]);
        assert_eq!(options.key(), &["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn empty_key_is_accepted_by_the_constructor() {
        let options = TableDiffOptions::new(Vec::new());
        assert!(options.key().is_empty());
    }
}
