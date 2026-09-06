//! Shared streaming table generator for the row-diff examples (`row_diff_rss`
//! and `row_diff_profile`), included with `#[path]` by both. Each shape's data
//! is a deterministic function of the row index, so nothing is retained between
//! batches and two runs at the same size produce byte-identical data.

// Each example includes this module and uses a subset of the shapes, so a shape
// unused by one example is not dead across the pair.
#![allow(dead_code)]

use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, RecordBatchReader, StringArray};
use arrow_schema::{ArrowError, SchemaRef};
use onix_arrow::{TableDiffError, TableInput};

/// The default rows per generated batch; override with `ROW_DIFF_BATCH` to
/// simulate a streamed input of many small batches.
pub const BATCH: i64 = 65_536;

/// The rows-per-batch used by the generator, from `ROW_DIFF_BATCH` or [`BATCH`].
pub fn batch_rows() -> i64 {
    std::env::var("ROW_DIFF_BATCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(BATCH)
}

/// The generated table shape.
#[derive(Clone, Copy)]
pub enum Shape {
    /// `(id, value)` int64 columns; `id_offset` shifts the key range and
    /// `change_every` perturbs a fraction of values.
    Linear { id_offset: i64, change_every: i64 },
    /// `(key, value)`; `key` is a `key_width`-byte string and each key value
    /// appears twice, so every key is a duplicate.
    Dup { key_width: usize },
    /// `(id, value)` where `value` is a `value_width`-byte string filled with
    /// `fill`; the two sides share every id but differ in `fill`, so every row
    /// is changed and every changed cell renders `value_width` bytes — the
    /// wide-cell worst case for the per-cell diff's rendering memory.
    Wide { value_width: usize, fill: u8 },
    /// `(id, value0..value{ncols})` each a `width`-byte string; only `value0`
    /// differs between the sides (`first_fill`), so every row is changed but only
    /// one cell per row — the wide-rows-few-changed-cells case, where the spill
    /// holds every common value column of every changed row though few change.
    ManyCols {
        ncols: usize,
        width: usize,
        first_fill: u8,
    },
}

/// A table generated on demand, retaining nothing between batches.
pub struct Generated {
    pub schema: SchemaRef,
    pub rows: i64,
    pub shape: Shape,
    pub batch: i64,
}

impl TableInput for Generated {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError> {
        Ok(Box::new(GenReader {
            schema: self.schema.clone(),
            rows: self.rows,
            shape: self.shape,
            batch: self.batch,
            next: 0,
        }))
    }
}

struct GenReader {
    schema: SchemaRef,
    rows: i64,
    shape: Shape,
    batch: i64,
    next: i64,
}

impl Iterator for GenReader {
    type Item = Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.rows {
            return None;
        }
        let end = (self.next + self.batch).min(self.rows);
        let columns: Vec<ArrayRef> = match self.shape {
            Shape::Linear {
                id_offset,
                change_every,
            } => {
                let ids: Int64Array = (self.next..end).map(|i| Some(i + id_offset)).collect();
                // The value is a function of the id, so a shared key holds the
                // same value on both sides except every `change_every`-th id.
                let values: Int64Array = (self.next..end)
                    .map(|i| {
                        let id = i + id_offset;
                        Some(if id % change_every == 0 { id + 1 } else { id })
                    })
                    .collect();
                vec![Arc::new(ids), Arc::new(values)]
            }
            Shape::Dup { key_width } => {
                // Key value `i / 2`, so each distinct key appears twice.
                let keys: StringArray = (self.next..end)
                    .map(|i| Some(format!("{:0>width$}", i / 2, width = key_width)))
                    .collect();
                let values: Int64Array = (self.next..end).map(Some).collect();
                vec![Arc::new(keys), Arc::new(values)]
            }
            Shape::Wide { value_width, fill } => {
                let ids: Int64Array = (self.next..end).map(Some).collect();
                let cell = String::from_utf8(vec![fill; value_width]).unwrap();
                let values: StringArray = (self.next..end).map(|_| Some(cell.as_str())).collect();
                vec![Arc::new(ids), Arc::new(values)]
            }
            Shape::ManyCols {
                ncols,
                width,
                first_fill,
            } => {
                let ids: Int64Array = (self.next..end).map(Some).collect();
                let mut columns: Vec<ArrayRef> = Vec::with_capacity(ncols + 1);
                columns.push(Arc::new(ids));
                for c in 0..ncols {
                    // Only value0 differs between the sides; the rest are equal,
                    // so every row is changed but only one cell per row.
                    let fill = if c == 0 { first_fill } else { b'a' };
                    let cell = String::from_utf8(vec![fill; width]).unwrap();
                    let values: StringArray =
                        (self.next..end).map(|_| Some(cell.as_str())).collect();
                    columns.push(Arc::new(values));
                }
                columns
            }
        };
        self.next = end;

        Some(RecordBatch::try_new(self.schema.clone(), columns))
    }
}

impl RecordBatchReader for GenReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
}
