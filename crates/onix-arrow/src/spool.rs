//! Anonymous temporary Arrow IPC spool files, shared by the row diff's cell-pass
//! partition spill and the Python bindings' input spool. Each file is a
//! [`tempfile::tempfile`] — unlinked at creation, mode 0600, never given a path,
//! so nothing is left on disk on abnormal exit — re-read through a rewound
//! `try_clone`.

use std::fs::File;
use std::io::{BufReader, BufWriter, Seek, SeekFrom};

use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::SchemaRef;

use crate::error::TableDiffError;

/// The buffered IPC writer over an anonymous spool file.
pub type SpoolWriter = StreamWriter<BufWriter<File>>;

/// The buffered IPC reader over a reopened spool file.
pub type SpoolReader = StreamReader<BufReader<File>>;

/// Maps a spool failure to a [`TableDiffError::Read`] naming the temporary
/// directory and `TMPDIR`, so a full or unwritable temp filesystem points the
/// caller at what to change. `context` is a verb phrase like `"write to"`.
///
/// # Errors
///
/// This constructs the error; it does not fail.
pub fn error(context: &str, error: &dyn std::fmt::Display) -> TableDiffError {
    TableDiffError::Read {
        message: format!(
            "could not {context} a spool file in the temporary directory ({}, overridable with \
             TMPDIR); it may be out of space: {error}",
            std::env::temp_dir().display()
        ),
    }
}

/// Creates an anonymous temporary file and an Arrow IPC stream writer over it,
/// returning the retained file handle (reopen it for reading with [`reopen`])
/// and the writer.
///
/// # Errors
///
/// Returns [`TableDiffError::Read`] if the temporary file cannot be created or
/// cloned or the IPC stream cannot be started (typically a full or unwritable
/// temporary filesystem).
pub fn open(schema: &SchemaRef) -> Result<(File, SpoolWriter), TableDiffError> {
    let file = tempfile::tempfile().map_err(|e| error("create", &e))?;
    let handle = file.try_clone().map_err(|e| error("open", &e))?;
    let writer = StreamWriter::try_new_buffered(handle, schema)
        .map_err(|e| error("start writing to", &e))?;
    Ok((file, writer))
}

/// Reopens a spool file for reading from the start through a fresh, rewound
/// handle. The caller opens each spool sequentially, so rewinding is safe.
///
/// # Errors
///
/// Returns [`TableDiffError::Read`] if the file cannot be cloned or rewound or
/// the IPC stream cannot be opened.
pub fn reopen(file: &File) -> Result<SpoolReader, TableDiffError> {
    let mut handle = file.try_clone().map_err(|e| error("re-open", &e))?;
    handle
        .seek(SeekFrom::Start(0))
        .map_err(|e| error("rewind", &e))?;
    StreamReader::try_new_buffered(handle, None).map_err(|e| error("read", &e))
}

#[cfg(test)]
mod tests {
    use super::error;
    use crate::error::TableDiffError;

    #[test]
    fn spool_error_names_the_temp_dir_and_tmpdir() {
        let TableDiffError::Read { message } = error("write to", &"No space left on device") else {
            panic!("spool::error must map to TableDiffError::Read");
        };
        let temp = std::env::temp_dir();
        assert!(
            message.contains("write to"),
            "names the operation: {message}"
        );
        assert!(
            message.contains(&temp.display().to_string()),
            "names the temporary directory: {message}"
        );
        assert!(message.contains("TMPDIR"), "names TMPDIR: {message}");
        assert!(
            message.contains("No space left on device"),
            "carries the underlying error: {message}"
        );
    }
}
