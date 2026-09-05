//! Measures the keyed row diff's peak memory and wall time, to check the memory
//! bounds the README states.
//!
//! Run under the OS's max-RSS reporter:
//!
//! ```sh
//! cargo build -p onix-arrow --release --example row_diff_rss
//! # linear shape (default): mostly-matching rows, 1% added/removed, ~2% changed
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000
//! /usr/bin/time -l target/release/examples/row_diff_rss 10000000
//! # same shape with no changed rows: the cell pass materializes nothing, the
//! # pass-one baseline the ~2%-changed run is measured against
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 nochange
//! # duplicate-heavy shape: every key duplicated, wide string key
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 dup 16
//! /usr/bin/time -l target/release/examples/row_diff_rss 200000 dup 1024
//! ```
//!
//! Each side is generated on the fly, batch by batch, and nothing is retained
//! between batches, so the process's peak RSS is the diff's own state, not the
//! table data. The **linear** shape (`id`, `value` int64 columns) exercises the
//! per-row hash vectors: the left is ids `0..n`, the right `step..n + step` with
//! `step = n / 100`, so 1% removed, 1% added, ~2% changed — and the cell pass
//! materializes those ~2% changed rows on both sides. The **nochange** variant
//! keeps the 1% added/removed but makes every shared row equal, so the cell pass
//! materializes nothing: the difference in peak RSS between it and the default
//! run is the cell pass's cost. The **dup** shape (`key` Utf8 of the given width,
//! `value` int64) makes every key appear twice on each side, so every distinct
//! key is a duplicate and the whole `duplicate_keys` report is materialized —
//! the term that scales with distinct duplicated keys times the key width.

use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, RecordBatchReader, StringArray};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use onix_arrow::{TableDiffError, TableDiffOptions, TableInput, diff_tables};

const BATCH: i64 = 65_536;

/// The generated table shape.
#[derive(Clone, Copy)]
enum Shape {
    /// `(id, value)` int64 columns; `id_offset` shifts the key range and
    /// `change_every` perturbs a fraction of values.
    Linear { id_offset: i64, change_every: i64 },
    /// `(key, value)`; `key` is a `key_width`-byte string and each key value
    /// appears twice, so every key is a duplicate.
    Dup { key_width: usize },
}

/// A table generated on demand, retaining nothing between batches.
struct Generated {
    schema: SchemaRef,
    rows: i64,
    shape: Shape,
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
            next: 0,
        }))
    }
}

struct GenReader {
    schema: SchemaRef,
    rows: i64,
    shape: Shape,
    next: i64,
}

impl Iterator for GenReader {
    type Item = Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.rows {
            return None;
        }
        let end = (self.next + BATCH).min(self.rows);
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rows: i64 = args
        .get(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let dup = args.get(2).is_some_and(|a| a == "dup");
    // `nochange`: same 1% added / 1% removed as the default linear shape but no
    // changed rows, so the cell pass materializes nothing — its peak RSS is the
    // pass-one baseline the cell pass is measured against.
    let nochange = args.get(2).is_some_and(|a| a == "nochange");
    let key_width: usize = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(16);

    let (schema, left_shape, right_shape, key) = if dup {
        let schema = Arc::new(Schema::new(vec![
            Field::new("key", DataType::Utf8, false),
            Field::new("value", DataType::Int64, false),
        ]));
        (
            schema,
            Shape::Dup { key_width },
            Shape::Dup { key_width },
            "key",
        )
    } else {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("value", DataType::Int64, false),
        ]));
        let step = (rows / 100).max(1);
        (
            schema,
            Shape::Linear {
                id_offset: 0,
                change_every: i64::MAX,
            },
            Shape::Linear {
                id_offset: step,
                change_every: if nochange { i64::MAX } else { 50 },
            },
            "id",
        )
    };

    let left = Generated {
        schema: schema.clone(),
        rows,
        shape: left_shape,
    };
    let right = Generated {
        schema,
        rows,
        shape: right_shape,
    };

    let start = std::time::Instant::now();
    let diff = diff_tables(&left, &right, &TableDiffOptions::new(vec![key.to_string()]))
        .expect("diff succeeds");
    let elapsed = start.elapsed();
    let summary = diff.summary();

    println!(
        "rows per side: {rows}{}",
        if dup {
            format!(" (dup, key_width={key_width})")
        } else {
            String::new()
        }
    );
    println!("wall: {:.2}s", elapsed.as_secs_f64());
    println!(
        "rows_added={} rows_removed={} rows_changed={} duplicate_keys={} cells_changed={}",
        summary.rows_added,
        summary.rows_removed,
        summary.rows_changed,
        summary.duplicate_keys,
        summary.cells_changed
    );
}
