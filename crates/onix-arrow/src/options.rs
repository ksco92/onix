//! Options controlling a table diff.

use std::num::NonZeroUsize;

use crate::error::TableDiffError;

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
    /// Worker threads the row diff hashes and classifies rows with. Defaults to
    /// the machine's available parallelism; `1` runs the single-threaded path.
    /// The result is byte-identical at any value.
    threads: NonZeroUsize,
}

impl TableDiffOptions {
    /// Creates options keyed on `key`, with [`threads`](Self::threads)
    /// defaulting to the machine's available parallelism.
    ///
    /// No validation happens here — an empty `key` is reported by
    /// [`crate::diff_tables`] so every misuse surfaces through one error
    /// channel.
    #[must_use]
    pub fn new(key: Vec<String>) -> Self {
        Self {
            key,
            threads: default_threads(),
        }
    }

    /// Sets the number of worker threads the row diff uses. `1` selects the
    /// single-threaded path; higher values partition the hash and classify
    /// work across that many threads. The diff's output is byte-identical at
    /// any value.
    ///
    /// # Errors
    ///
    /// [`TableDiffError::ThreadCountTooLarge`] if `threads` exceeds
    /// [`crate::MAX_THREADS`] — the row diff spawns one worker per thread, so
    /// the count is bounded here, before any thread or buffer is allocated.
    pub fn with_threads(mut self, threads: NonZeroUsize) -> Result<Self, TableDiffError> {
        if threads.get() > crate::MAX_THREADS {
            return Err(TableDiffError::ThreadCountTooLarge {
                threads: threads.get(),
                max: crate::MAX_THREADS,
            });
        }
        self.threads = threads;
        Ok(self)
    }

    /// The key columns, in the order supplied.
    #[must_use]
    pub fn key(&self) -> &[String] {
        &self.key
    }

    /// The number of worker threads the row diff uses.
    #[must_use]
    pub fn threads(&self) -> NonZeroUsize {
        self.threads
    }
}

/// The machine's available parallelism, or `1` when it cannot be queried.
fn default_threads() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

#[cfg(test)]
mod tests {
    use super::{TableDiffOptions, default_threads};
    use std::num::NonZeroUsize;

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

    #[test]
    fn threads_defaults_to_available_parallelism() {
        let options = TableDiffOptions::new(vec!["id".to_string()]);
        assert_eq!(options.threads(), default_threads());
    }

    #[test]
    fn default_threads_is_the_machine_parallelism() {
        let expected = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        assert_eq!(default_threads().get(), expected);
    }

    #[test]
    fn with_threads_overrides_the_default() {
        let options = TableDiffOptions::new(vec!["id".to_string()])
            .with_threads(NonZeroUsize::new(4).unwrap())
            .unwrap();
        assert_eq!(options.threads().get(), 4);
    }

    #[test]
    fn with_threads_at_the_ceiling_is_accepted() {
        let options = TableDiffOptions::new(vec!["id".to_string()])
            .with_threads(NonZeroUsize::new(crate::MAX_THREADS).unwrap())
            .unwrap();
        assert_eq!(options.threads().get(), crate::MAX_THREADS);
    }

    #[test]
    fn with_threads_above_the_ceiling_errors() {
        let result = TableDiffOptions::new(vec!["id".to_string()])
            .with_threads(NonZeroUsize::new(crate::MAX_THREADS + 1).unwrap());
        assert!(matches!(
            result,
            Err(crate::error::TableDiffError::ThreadCountTooLarge { threads, max })
                if threads == crate::MAX_THREADS + 1 && max == crate::MAX_THREADS
        ));
    }
}
