//! Per-pass wall-time and peak-RSS profile of one keyed row diff, the committed
//! harness every row-diff performance change posts a before/after table from.
//!
//! Builds only with the `profile` feature (which compiles the per-pass
//! instrumentation into `onix-arrow`; it is off by default and absent from the
//! release wheel). Each generated side is spooled to an anonymous Arrow IPC file
//! first, so every pass re-reads the spool through a rewound handle exactly as
//! the Python bindings' input spool does -- the re-read layer this profile
//! measures. The proxy shapes stand in for a real fixture path so the harness
//! needs no parquet reader (no new dependency); the real narrow/wide parquet
//! pairs are profiled through the same instrumentation from the Python side (the
//! wheel built with `--features profile`), documented in `perf/arrow/README.md`.
//!
//! ```sh
//! cargo build -p onix-arrow --release --features profile --example row_diff_profile
//! # narrow proxy: id + value int64, ~2% of rows changed
//! target/release/examples/row_diff_profile 1000000 linear 18
//! # wide proxy: id + 34 string columns, every row changed, one differing cell
//! target/release/examples/row_diff_profile 1000000 manycols 18 34 64
//! # every row changed, one wide string cell (render-heavy)
//! target/release/examples/row_diff_profile 1000000 wide 18 512
//! ```
//!
//! Args: `<rows> <shape> [threads] [shape params...]`. `threads` defaults to the
//! machine's available parallelism (or `ROW_DIFF_THREADS`); `key` is always
//! `id`. Shapes:
//!
//! - `linear`: `id`/`value` int64; the right side shifts the key range by 1% (1%
//!   added, 1% removed) and perturbs every 50th value (~2% changed) -- the
//!   narrow fixture's shape, few changed rows.
//! - `allchange`: `id`/`value` int64, every shared row changed.
//! - `wide`: `id` int64, `value` a `width`-byte string differing on every row
//!   (default 512) -- the render-heavy shape.
//! - `manycols`: `id` int64 plus `ncols` `width`-byte string columns (defaults
//!   34, 64) of which only the first differs -- the wide fixture's shape, every
//!   row changed but one cell each, so the spill carries all columns.

use std::fs::File;
use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, RecordBatchReader, StringArray};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use onix_arrow::{TableDiffError, TableDiffOptions, TableInput, diff_tables, profile, spool};

/// Rows per generated batch (the streamed batch size the spool is written in).
const BATCH: i64 = 65_536;

/// The generated table shape and its per-side parameters.
#[derive(Clone)]
enum Shape {
    /// `id`/`value` int64; `id_offset` shifts the key range, `change_every`
    /// perturbs every nth value.
    Linear { id_offset: i64, change_every: i64 },
    /// `id` int64 and one `width`-byte string `value` filled with `fill`.
    Wide { width: usize, fill: u8 },
    /// `id` int64 and `ncols` `width`-byte string columns; only the first is
    /// filled with `first_fill` (the rest are constant across sides).
    ManyCols {
        ncols: usize,
        width: usize,
        first_fill: u8,
    },
}

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
            shape: self.shape.clone(),
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
        let columns: Vec<ArrayRef> = match &self.shape {
            Shape::Linear {
                id_offset,
                change_every,
            } => {
                let ids: Int64Array = (self.next..end).map(|i| Some(i + id_offset)).collect();
                let values: Int64Array = (self.next..end)
                    .map(|i| {
                        let id = i + id_offset;
                        Some(if id % change_every == 0 { id + 1 } else { id })
                    })
                    .collect();
                vec![Arc::new(ids), Arc::new(values)]
            }
            Shape::Wide { width, fill } => {
                let ids: Int64Array = (self.next..end).map(Some).collect();
                let cell = String::from_utf8(vec![*fill; *width]).unwrap();
                let values: StringArray = (self.next..end).map(|_| Some(cell.as_str())).collect();
                vec![Arc::new(ids), Arc::new(values)]
            }
            Shape::ManyCols {
                ncols,
                width,
                first_fill,
            } => {
                let ids: Int64Array = (self.next..end).map(Some).collect();
                let mut columns: Vec<ArrayRef> = vec![Arc::new(ids)];
                for c in 0..*ncols {
                    let fill = if c == 0 { *first_fill } else { b'a' };
                    let cell = String::from_utf8(vec![fill; *width]).unwrap();
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

/// A side spooled to an anonymous Arrow IPC file, re-read on every `open` — the
/// same re-openable spool the Python bindings hand the row diff.
struct Spooled {
    file: File,
    schema: SchemaRef,
}

impl TableInput for Spooled {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
    fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError> {
        Ok(Box::new(spool::reopen(&self.file)?))
    }
}

fn spool_side(side: &Generated) -> Spooled {
    let (file, mut writer) = spool::open(&side.schema).expect("open spool");
    let reader = side.open().expect("open generator");
    for batch in reader {
        writer
            .write(&batch.expect("generate batch"))
            .expect("spool write");
    }
    writer.finish().expect("spool finish");
    Spooled {
        file,
        schema: side.schema.clone(),
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn build_case(shape: &str, params: &[i64], rows: i64) -> (SchemaRef, Shape, Shape) {
    match shape {
        "wide" => {
            let width = params.first().copied().unwrap_or(512).max(1) as usize;
            let schema = Arc::new(Schema::new(vec![
                Field::new("id", DataType::Int64, false),
                Field::new("value", DataType::Utf8, false),
            ]));
            (
                schema,
                Shape::Wide { width, fill: b'a' },
                Shape::Wide { width, fill: b'b' },
            )
        }
        "manycols" => {
            let ncols = params.first().copied().unwrap_or(34).max(1) as usize;
            let width = params.get(1).copied().unwrap_or(64).max(1) as usize;
            let mut fields = vec![Field::new("id", DataType::Int64, false)];
            for c in 0..ncols {
                fields.push(Field::new(format!("value{c}"), DataType::Utf8, false));
            }
            let schema = Arc::new(Schema::new(fields));
            (
                schema,
                Shape::ManyCols {
                    ncols,
                    width,
                    first_fill: b'a',
                },
                Shape::ManyCols {
                    ncols,
                    width,
                    first_fill: b'b',
                },
            )
        }
        "allchange" => int_case(1, rows),
        _ => int_case(50, rows),
    }
}

fn int_case(change_every: i64, rows: i64) -> (SchemaRef, Shape, Shape) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("value", DataType::Int64, false),
    ]));
    // The mostly-matching `linear` shape shifts the right key range by 1% (1%
    // added, 1% removed); `allchange` (change_every == 1) shares every key.
    let id_offset = if change_every == 1 {
        0
    } else {
        (rows / 100).max(1)
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
    )
}

fn threads_arg(explicit: Option<i64>) -> Option<std::num::NonZeroUsize> {
    explicit
        .filter(|&n| n > 0)
        .and_then(|n| usize::try_from(n).ok())
        .or_else(|| {
            std::env::var("ROW_DIFF_THREADS")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .and_then(std::num::NonZeroUsize::new)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rows: i64 = args
        .get(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let shape = args.get(2).map_or("linear", String::as_str);
    let threads = args.get(3).and_then(|a| a.parse().ok());
    let params: Vec<i64> = args.iter().skip(4).filter_map(|a| a.parse().ok()).collect();

    let (schema, left_shape, right_shape) = build_case(shape, &params, rows);

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

    let left = spool_side(&left);
    let right = spool_side(&right);

    let mut options = TableDiffOptions::new(vec!["id".to_string()]);
    if let Some(t) = threads_arg(threads) {
        options = options.with_threads(t).expect("threads within MAX_THREADS");
    }

    profile::begin();
    let start = std::time::Instant::now();
    let diff = diff_tables(&left, &right, &options).expect("diff succeeds");
    let elapsed = start.elapsed();
    let passes = profile::finish();
    let summary = diff.summary();

    println!("rows per side: {rows} (shape={shape})");
    println!("threads: {}", options.threads());
    println!("total wall: {:.3} s", elapsed.as_secs_f64());
    println!(
        "rows_added={} rows_removed={} rows_changed={} duplicate_keys={} cells_changed={}",
        summary.rows_added,
        summary.rows_removed,
        summary.rows_changed,
        summary.duplicate_keys,
        summary.cells_changed
    );
    println!();
    println!("{:<40} {:>10} {:>12}", "pass", "wall (s)", "peak RSS (MB)");
    for pass in &passes {
        match pass.peak_rss_mib {
            Some(rss) => println!("{:<40} {:>10.3} {:>12.1}", pass.label, pass.wall_secs, rss),
            None => println!("{:<40} {:>10.3} {:>12}", pass.label, pass.wall_secs, "-"),
        }
    }
}
