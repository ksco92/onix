//! The `diff_tables` entry point and its result objects.
//!
//! `diff_tables` accepts any Python object implementing the Arrow `PyCapsule`
//! interface — `__arrow_c_stream__` (pyarrow `Table`/`RecordBatchReader`,
//! polars `DataFrame`, `DuckDB` relation) or `__arrow_c_array__` (pyarrow
//! `RecordBatch`/`StructArray`) — imports it into native Arrow record batches
//! with no Python round trip (via `pyo3-arrow`'s C Data Interface bridge),
//! and diffs the two tables — schema and keyed rows — with [`onix_arrow`].
//!
//! `pyarrow` is not needed to *call* `diff_tables`: polars and `DuckDB` expose
//! the same protocol, so `import deepdiff_rs` and diffing their tables work
//! with no pyarrow installed. It is needed only by [`ArrowTable::to_pyarrow`],
//! which raises a clear `ImportError` naming the optional extra when pyarrow
//! is absent. Every Arrow result also exports itself through
//! `__arrow_c_stream__`, so polars and pandas can consume it without pyarrow
//! either.

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch, RecordBatchIterator, RecordBatchReader, StructArray};
use arrow_schema::{Field, FieldRef, SchemaRef};
use pyo3::exceptions::{
    PyImportError, PyModuleNotFoundError, PyNotImplementedError, PyTypeError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::{PyCapsule, PyDict, PyList, PyString};
use pyo3_arrow::ffi::{ArrayIterator, ArrayReader, to_schema_pycapsule, to_stream_pycapsule};
use pyo3_arrow::input::AnyRecordBatch;

use onix_arrow::{
    SchemaChange, TableDiff as CoreTableDiff, TableDiffError, TableDiffOptions, TableInput,
    diff_tables as core_diff_tables,
};

/// A record batch reader over one imported input, with its schema attached.
type ImportedReader = RecordBatchIterator<Box<dyn RecordBatchReader + Send>>;

/// One diff input, spooled to an anonymous temporary Arrow IPC stream file so
/// the core's two-pass row diff can re-read it. The temp file is created with
/// [`tempfile::tempfile`] — unlinked the instant it is opened, mode 0600, and
/// never given a predictable name — so no other user can read the plaintext
/// copy or pre-plant a symlink at its path, and nothing is left on disk even if
/// the process is killed. Each input is imported and fully drained before the
/// next, so two one-shot Python streams (a pair of `DuckDB` relations sharing
/// one connection, say) are never open at once.
struct SpooledInput {
    file: File,
    schema: SchemaRef,
}

impl TableInput for SpooledInput {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError> {
        // Re-read the anonymous file from the start through a fresh handle; the
        // row diff opens each side sequentially, so rewinding here is safe.
        let mut file = self.file.try_clone().map_err(|e| TableDiffError::Read {
            message: e.to_string(),
        })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| TableDiffError::Read {
                message: e.to_string(),
            })?;
        let reader =
            arrow_ipc::reader::StreamReader::try_new_buffered(file, None).map_err(|e| {
                TableDiffError::Read {
                    message: e.to_string(),
                }
            })?;

        Ok(Box::new(reader))
    }
}

/// Turns a spool I/O failure into a `ValueError` that names what ran out and
/// where — a full temporary filesystem is the likely cause, and it is set by
/// `TMPDIR`.
fn spool_io_error(context: &str, error: &dyn std::fmt::Display) -> PyErr {
    PyValueError::new_err(format!(
        "{context} the temporary directory ({}, overridable with TMPDIR); \
         it may be out of space: {error}",
        std::env::temp_dir().display()
    ))
}

/// Imports one Python Arrow input and spools every batch to an anonymous
/// temporary IPC stream file, draining and closing its (possibly one-shot)
/// source before returning.
fn spool_input(obj: &Bound<'_, PyAny>) -> PyResult<SpooledInput> {
    let reader = import_reader(obj)?;
    let schema = reader.schema();

    let file =
        tempfile::tempfile().map_err(|e| spool_io_error("could not create a spool file in", &e))?;
    let write_handle = file
        .try_clone()
        .map_err(|e| spool_io_error("could not open a spool file in", &e))?;
    let mut writer = arrow_ipc::writer::StreamWriter::try_new_buffered(write_handle, &schema)
        .map_err(|e| spool_io_error("could not write to a spool file in", &e))?;
    for batch in reader {
        let batch = batch.map_err(|e| PyValueError::new_err(e.to_string()))?;
        writer
            .write(&batch)
            .map_err(|e| spool_io_error("could not write table data to a spool file in", &e))?;
    }
    writer
        .finish()
        .map_err(|e| spool_io_error("could not finish writing a spool file in", &e))?;

    Ok(SpooledInput { file, schema })
}

/// Diffs two Arrow tables.
///
/// `left` and `right` are any objects implementing the Arrow `PyCapsule`
/// interface (see the module docs). `key` is the list of primary-key column
/// names; it is required and must be non-empty, and every key column must
/// exist on both sides. The result is a [`TableDiff`].
#[pyfunction]
#[pyo3(signature = (left, right, *, key))]
pub(crate) fn diff_tables(
    py: Python<'_>,
    left: &Bound<'_, PyAny>,
    right: &Bound<'_, PyAny>,
    key: &Bound<'_, PyAny>,
) -> PyResult<TableDiff> {
    let key = extract_key(key)?;
    // Import, diff, and drop all run on the stack-sized worker (re-acquiring the
    // GIL there) because the recursive Arrow FFI import and the imported types'
    // recursive drop are native-stack sinks on deep nesting, and — unlike the
    // JSON path, which measures depth cheaply first and only spawns the worker
    // past a threshold — depth here can only be measured after the import that
    // is itself at risk, so the worker is unconditional. Its fixed per-call
    // cost (tens of microseconds) is negligible for a whole-table diff.
    // `onix_arrow::MAX_NESTING_DEPTH` then bounds the comparison; see its doc.
    let left = left.clone().unbind();
    let right = right.clone().unbind();

    crate::guard::run_on_worker(py, move || {
        Python::attach(|py| {
            // Each input is imported and fully spooled before the next, so two
            // one-shot Python streams are never open at the same time.
            let left_input = spool_input(left.bind(py))?;
            let right_input = spool_input(right.bind(py))?;
            let options = TableDiffOptions::new(key);
            let core = core_diff_tables(&left_input, &right_input, &options)
                .map_err(|e| map_table_error(&e))?;

            TableDiff::from_core(core)
        })
    })?
}

/// Extracts the key column list, rejecting a bare string (which would
/// otherwise be silently read as a list of one-character column names).
fn extract_key(key: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    if key.is_instance_of::<PyString>() {
        return Err(PyTypeError::new_err(
            "key must be a list of column names, e.g. key=[\"id\"], not a single string",
        ));
    }

    key.extract()
}

/// Imports one input into a record batch reader, or raises a `TypeError`
/// naming the protocol if the object implements neither Arrow capsule method.
fn import_reader(obj: &Bound<'_, PyAny>) -> PyResult<ImportedReader> {
    let any = if obj.hasattr("__arrow_c_stream__")? {
        AnyRecordBatch::Stream(obj.extract()?)
    } else if obj.hasattr("__arrow_c_array__")? {
        AnyRecordBatch::RecordBatch(obj.extract()?)
    } else {
        return Err(PyTypeError::new_err(format!(
            "diff_tables input of type '{}' does not implement the Arrow PyCapsule interface; \
             it must provide __arrow_c_stream__ (a pyarrow Table or RecordBatchReader, a polars \
             DataFrame, a DuckDB relation) or __arrow_c_array__ (a pyarrow RecordBatch or \
             StructArray)",
            obj.get_type().name()?
        )));
    };

    let schema = any.schema()?;
    let reader = any.into_reader()?;

    Ok(RecordBatchIterator::new(reader, schema))
}

/// Maps an [`onix_arrow`] error to the Python exception a caller sees. A
/// wildcard arm is required because [`TableDiffError`] is `#[non_exhaustive]`.
fn map_table_error(error: &TableDiffError) -> PyErr {
    let message = error.to_string();
    match error {
        TableDiffError::NotImplemented { .. } => PyNotImplementedError::new_err(message),
        TableDiffError::MaxDepthExceeded { .. } => crate::errors::MaxDepthError::new_err(message),
        _ => PyValueError::new_err(message),
    }
}

/// The result of [`diff_tables`]: the schema diff, plus the still-unbuilt
/// row-level members.
#[pyclass(module = "deepdiff_rs", name = "TableDiff", frozen)]
pub(crate) struct TableDiff {
    core: CoreTableDiff,
    /// The JSON rendering and the Arrow record batch, both built once here
    /// because they cost real work; the schema list and summary are derived
    /// from `core` on demand instead, since they are cheap.
    json: String,
    schema_batch: RecordBatch,
}

impl TableDiff {
    /// Builds the Python result from a finished core diff, taking it by value so
    /// the (potentially large) changed-key set is moved, not cloned.
    fn from_core(core: CoreTableDiff) -> PyResult<Self> {
        let json = core
            .to_json()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let schema_batch = core
            .schema_record_batch()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;

        Ok(Self {
            core,
            json,
            schema_batch,
        })
    }
}

#[pymethods]
impl TableDiff {
    /// The schema changes as a list of dicts, one per changed column, each
    /// with `column`, `change` (`added`/`removed`/`type_changed`),
    /// `left_type`, `right_type`, `left_nullable`, `right_nullable`. The type
    /// and nullability fields are `None` on the side a column is absent from.
    #[getter]
    fn schema<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let rows = self
            .core
            .schema()
            .iter()
            .map(|change| schema_change_dict(py, change))
            .collect::<PyResult<Vec<_>>>()?;

        PyList::new(py, rows)
    }

    /// The schema diff as an Arrow-exportable table: it implements
    /// `__arrow_c_stream__` (so polars and pandas can consume it without
    /// pyarrow) and offers [`ArrowTable::to_pyarrow`].
    #[getter]
    fn schema_arrow(&self) -> ArrowTable {
        ArrowTable::new(self.schema_batch.clone())
    }

    /// Counts of each kind of change: the schema counts (`columns_added`,
    /// `columns_removed`, `columns_type_changed`) and the row counts
    /// (`rows_added`, `rows_removed`, `rows_changed`, `duplicate_keys`,
    /// `null_keys`). Duplicate keys are reported separately and excluded from
    /// the added/removed/changed row counts; `null_keys` is an informational
    /// count of distinct keys with a null component.
    fn summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let summary = self.core.summary();
        let dict = PyDict::new(py);
        dict.set_item("columns_added", summary.columns_added)?;
        dict.set_item("columns_removed", summary.columns_removed)?;
        dict.set_item("columns_type_changed", summary.columns_type_changed)?;
        dict.set_item("rows_added", summary.rows_added)?;
        dict.set_item("rows_removed", summary.rows_removed)?;
        dict.set_item("rows_changed", summary.rows_changed)?;
        dict.set_item("duplicate_keys", summary.duplicate_keys)?;
        dict.set_item("null_keys", summary.null_keys)?;

        Ok(dict)
    }

    /// The schema diff and its summary as a JSON string.
    fn to_json(&self) -> &str {
        &self.json
    }

    /// Rows present only on the right (added), in the right table's schema and
    /// excluding duplicate keys.
    fn rows_added(&self) -> PyResult<ArrowTable> {
        self.core
            .rows_added()
            .map(ArrowTable::new)
            .map_err(|e| map_table_error(&e))
    }

    /// Rows present only on the left (removed), in the left table's schema and
    /// excluding duplicate keys.
    fn rows_removed(&self) -> PyResult<ArrowTable> {
        self.core
            .rows_removed()
            .map(ArrowTable::new)
            .map_err(|e| map_table_error(&e))
    }

    /// Per-cell changes for rows on both sides. Raises `NotImplementedError`
    /// until a later version fills it in.
    fn cells_changed(&self) -> PyResult<ArrowTable> {
        self.core
            .cells_changed()
            .map(ArrowTable::new)
            .map_err(|e| map_table_error(&e))
    }

    /// Keys appearing more than once on either side: the key columns, then
    /// `left_count` and `right_count`.
    fn duplicate_keys(&self) -> PyResult<ArrowTable> {
        self.core
            .duplicate_keys()
            .map(ArrowTable::new)
            .map_err(|e| map_table_error(&e))
    }

    fn __repr__(&self) -> String {
        let summary = self.core.summary();

        format!(
            "TableDiff(columns_added={}, columns_removed={}, columns_type_changed={}, \
             rows_added={}, rows_removed={}, rows_changed={}, duplicate_keys={})",
            summary.columns_added,
            summary.columns_removed,
            summary.columns_type_changed,
            summary.rows_added,
            summary.rows_removed,
            summary.rows_changed,
            summary.duplicate_keys,
        )
    }
}

/// Renders one [`SchemaChange`] as a Python dict.
fn schema_change_dict<'py>(py: Python<'py>, change: &SchemaChange) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("column", &change.column)?;
    dict.set_item("change", change.change.as_str())?;
    dict.set_item("left_type", &change.left_type)?;
    dict.set_item("right_type", &change.right_type)?;
    dict.set_item("left_nullable", change.left_nullable)?;
    dict.set_item("right_nullable", change.right_nullable)?;

    Ok(dict)
}

/// An Arrow record batch exposed to Python through the Arrow `PyCapsule`
/// interface. Every table-shaped result of a diff is one of these, so polars,
/// pandas, and pyarrow can all consume it: `__arrow_c_stream__` needs no
/// third-party package, and [`ArrowTable::to_pyarrow`] is a convenience for
/// when pyarrow is present.
#[pyclass(module = "deepdiff_rs", name = "ArrowTable", frozen)]
pub(crate) struct ArrowTable {
    batch: RecordBatch,
}

impl ArrowTable {
    /// Wraps a record batch. Not exposed to Python; the diff builds these.
    fn new(batch: RecordBatch) -> Self {
        Self { batch }
    }
}

#[pymethods]
impl ArrowTable {
    /// Exports this table as an Arrow C stream (one record batch) in a
    /// `PyCapsule`, the standard zero-copy hand-off polars, pandas, and pyarrow
    /// all understand.
    #[pyo3(signature = (requested_schema=None))]
    fn __arrow_c_stream__<'py>(
        &self,
        py: Python<'py>,
        requested_schema: Option<Bound<'py, PyCapsule>>,
    ) -> PyResult<Bound<'py, PyCapsule>> {
        let schema = self.batch.schema();
        let field: FieldRef = Arc::new(
            Field::new_struct("", schema.fields().clone(), false)
                .with_metadata(schema.metadata.clone()),
        );
        let array: ArrayRef = Arc::new(StructArray::from(self.batch.clone()));
        let reader: Box<dyn ArrayReader + Send> =
            Box::new(ArrayIterator::new(std::iter::once(Ok(array)), field));

        // A malformed `requested_schema` capsule (e.g. one carrying a zeroed
        // ArrowSchema) makes the arrow importer panic; catch it here and raise
        // a catchable `ValueError` instead of letting a `PanicException`
        // escape, matching the never-crash posture of the other entry points.
        let capsule = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            to_stream_pycapsule(py, reader, requested_schema)
        }))
        .map_err(|_| {
            PyValueError::new_err(
                "the requested_schema passed to __arrow_c_stream__ is not a valid Arrow C schema",
            )
        })?;

        Ok(capsule?)
    }

    /// Exports the schema of this table as an Arrow C schema in a `PyCapsule`.
    fn __arrow_c_schema__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyCapsule>> {
        Ok(to_schema_pycapsule(py, self.batch.schema_ref().as_ref())?)
    }

    /// This table as a `pyarrow.Table`.
    ///
    /// Requires pyarrow (`pip install deepdiff-rs[arrow]`); raises
    /// `ImportError` naming that extra if pyarrow is not installed. Consuming
    /// the table with polars or pandas needs no pyarrow — use
    /// `__arrow_c_stream__` (which those libraries call for you) instead.
    fn to_pyarrow<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let pyarrow = match py.import("pyarrow") {
            Ok(module) => module,
            // Only a genuinely-absent pyarrow becomes the install-the-extra
            // hint; any other import failure (a broken or partial pyarrow) is
            // propagated with its own message, kept as the cause so the real
            // error is not hidden.
            Err(error) if error.is_instance_of::<PyModuleNotFoundError>(py) => {
                let hint = PyImportError::new_err(
                    "pyarrow is required for to_pyarrow(); install it with \
                     'pip install deepdiff-rs[arrow]'. Consuming this result with polars or \
                     pandas needs no pyarrow.",
                );
                hint.set_cause(py, Some(error));
                return Err(hint);
            }
            Err(error) => return Err(error),
        };

        pyarrow.getattr("table")?.call1((slf,))
    }

    /// The number of rows in this table.
    fn __len__(&self) -> usize {
        self.batch.num_rows()
    }

    fn __repr__(&self) -> String {
        format!(
            "ArrowTable({} rows x {} columns)",
            self.batch.num_rows(),
            self.batch.num_columns(),
        )
    }
}
