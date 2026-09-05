//! Measures the keyed row diff's peak memory and wall time at a given row
//! count, to check the "peak RSS proportional to rows, not data" claim.
//!
//! Run under the OS's max-RSS reporter:
//!
//! ```sh
//! cargo build -p onix-arrow --release --example row_diff_rss
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000
//! /usr/bin/time -l target/release/examples/row_diff_rss 10000000
//! ```
//!
//! Each side is generated on the fly, batch by batch, and nothing is retained
//! between batches, so the process's peak RSS is the diff's own state (the
//! per-row hash vectors and the classification sets), not the table data. The
//! left is ids `0..n` and the right is ids `step..n + step` with `step = n /
//! 100`, so 1% of rows are removed, 1% added, and about 2% of the shared rows
//! changed — a realistic, materialization-exercising mix.

use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, RecordBatchReader};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use onix_arrow::{TableDiffError, TableDiffOptions, TableInput, diff_tables};

const BATCH: i64 = 65_536;

/// A table of `(id, value)` rows generated on demand, retaining nothing.
struct Generated {
    schema: SchemaRef,
    rows: i64,
    id_offset: i64,
    /// Every `change_every`-th row gets a perturbed value, so a fraction of the
    /// shared keys count as changed.
    change_every: i64,
}

impl TableInput for Generated {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError> {
        Ok(Box::new(GenReader {
            schema: self.schema.clone(),
            rows: self.rows,
            id_offset: self.id_offset,
            change_every: self.change_every,
            next: 0,
        }))
    }
}

struct GenReader {
    schema: SchemaRef,
    rows: i64,
    id_offset: i64,
    change_every: i64,
    next: i64,
}

impl Iterator for GenReader {
    type Item = Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.rows {
            return None;
        }
        let end = (self.next + BATCH).min(self.rows);
        let ids: Int64Array = (self.next..end).map(|i| Some(i + self.id_offset)).collect();
        // The value is a function of the id, so the same key holds the same
        // value on both sides — except every `change_every`-th id, perturbed, so
        // a fixed fraction of the shared keys count as changed.
        let values: Int64Array = (self.next..end)
            .map(|i| {
                let id = i + self.id_offset;
                Some(if id % self.change_every == 0 {
                    id + 1
                } else {
                    id
                })
            })
            .collect();
        self.next = end;

        let columns: Vec<ArrayRef> = vec![Arc::new(ids), Arc::new(values)];
        Some(RecordBatch::try_new(self.schema.clone(), columns))
    }
}

impl RecordBatchReader for GenReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
}

fn main() {
    let rows: i64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let step = (rows / 100).max(1);

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Int64, false),
    ]));

    // Left ids 0..rows unchanged; right ids step..rows+step with a perturbed
    // value on ~2% of the shared rows.
    let left = Generated {
        schema: schema.clone(),
        rows,
        id_offset: 0,
        change_every: i64::MAX,
    };
    let right = Generated {
        schema,
        rows,
        id_offset: step,
        change_every: 50,
    };

    let start = std::time::Instant::now();
    let diff = diff_tables(
        &left,
        &right,
        &TableDiffOptions::new(vec!["id".to_string()]),
    )
    .expect("diff succeeds");
    let elapsed = start.elapsed();
    let summary = diff.summary();

    println!("rows per side: {rows}");
    println!("wall: {:.2}s", elapsed.as_secs_f64());
    println!(
        "rows_added={} rows_removed={} rows_changed={} duplicate_keys={}",
        summary.rows_added, summary.rows_removed, summary.rows_changed, summary.duplicate_keys
    );
}
