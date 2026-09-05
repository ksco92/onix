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
//! # every row changed (narrow int cells): the cell pass at full width
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 allchange
//! # every row changed with a wide (1 KB) string cell: the rendering worst case
//! /usr/bin/time -l target/release/examples/row_diff_rss 100000 wide 1024
//! /usr/bin/time -l target/release/examples/row_diff_rss 200000 wide 1024
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
//! run is the cell pass's cost. The **allchange** variant drops the offset and
//! changes every shared row (no added/removed), so the cell pass materializes
//! and renders every row. The **wide** shape (`id` int64, `value` a
//! `value_width`-byte Utf8 that differs between the sides) changes every row too
//! and renders `value_width` bytes per changed cell — the rendering worst case,
//! whose peak RSS scales with changed cells times cell width. The **dup** shape
//! (`key` Utf8 of the given width, `value` int64) makes every key appear twice
//! on each side, so every distinct
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
    /// `(id, value)` where `value` is a `value_width`-byte string filled with
    /// `fill`; the two sides share every id but differ in `fill`, so every row
    /// is changed and every changed cell renders `value_width` bytes — the
    /// wide-cell worst case for the per-cell diff's rendering memory.
    Wide { value_width: usize, fill: u8 },
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
            Shape::Wide { value_width, fill } => {
                let ids: Int64Array = (self.next..end).map(Some).collect();
                let cell = String::from_utf8(vec![fill; value_width]).unwrap();
                let values: StringArray = (self.next..end).map(|_| Some(cell.as_str())).collect();
                vec![Arc::new(ids), Arc::new(values)]
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
    // Mode selects the shape: "dup" (duplicate-heavy), "wide" (all rows changed,
    // wide string value column), "nochange"/"allchange"/default (linear int).
    let mode = args.get(2).map_or("", String::as_str);
    let width: usize = args
        .get(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or(if mode == "wide" { 1024 } else { 16 });

    let (schema, left_shape, right_shape, key, label) = match mode {
        "dup" => {
            let schema = Arc::new(Schema::new(vec![
                Field::new("key", DataType::Utf8, false),
                Field::new("value", DataType::Int64, false),
            ]));
            let shape = Shape::Dup { key_width: width };
            (
                schema,
                shape,
                shape,
                "key",
                format!(" (dup, key_width={width})"),
            )
        }
        "wide" => {
            let schema = Arc::new(Schema::new(vec![
                Field::new("id", DataType::Int64, false),
                Field::new("value", DataType::Utf8, false),
            ]));
            (
                schema,
                Shape::Wide {
                    value_width: width,
                    fill: b'a',
                },
                Shape::Wide {
                    value_width: width,
                    fill: b'b',
                },
                "id",
                format!(" (wide, value_width={width}, all changed)"),
            )
        }
        _ => {
            let schema = Arc::new(Schema::new(vec![
                Field::new("id", DataType::Int64, false),
                Field::new("value", DataType::Int64, false),
            ]));
            let step = (rows / 100).max(1);
            // `nochange`: 1% added/removed but no changed rows (the pass-one
            // baseline). `allchange`: no added/removed, every row changed.
            // Default: 1% added/removed, ~2% changed.
            let (id_offset, change_every, label) = match mode {
                "nochange" => (step, i64::MAX, " (nochange baseline)"),
                "allchange" => (0, 1, " (all changed)"),
                _ => (step, 50, ""),
            };
            (
                schema,
                Shape::Linear {
                    id_offset: 0,
                    change_every: i64::MAX,
                },
                Shape::Linear {
                    id_offset,
                    change_every,
                },
                "id",
                label.to_string(),
            )
        }
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

    println!("rows per side: {rows}{label}");
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
