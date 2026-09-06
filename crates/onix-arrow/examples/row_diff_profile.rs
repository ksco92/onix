//! Per-pass wall-time and peak-RSS profile of one keyed row diff, the committed
//! harness every row-diff performance change posts a before/after table from.
//!
//! Builds only with the `profile` feature (which compiles the per-pass
//! instrumentation into `onix-arrow`; it is off by default and absent from the
//! release wheel). Two modes:
//!
//! - **file**: reads the two sides from uncompressed Arrow IPC files and takes
//!   `--key`. Each pass re-opens and re-decodes the file, the re-read this
//!   profile measures. Convert the parquet fixtures once with pyarrow (no parquet
//!   reader is a dependency of this crate), writing uncompressed IPC so the
//!   reader needs no codec feature: `python -c "import pyarrow.parquet as p,
//!   pyarrow.feather as f; f.write_feather(p.read_table('a.parquet'), 'a.arrow',
//!   compression='uncompressed')"` (and likewise for `b`).
//! - **generated**: builds both sides from a deterministic shape and spools each
//!   to an anonymous Arrow IPC file first, so the re-read layer is exercised with
//!   no external fixture. The shapes are proxies for the real fixtures, not the
//!   fixtures themselves (see the RESULTS.md per-pass section).
//!
//! ```sh
//! cargo build -p onix-arrow --release --features profile --example row_diff_profile
//! # real fixtures (after the pyarrow conversion above):
//! target/release/examples/row_diff_profile file a.arrow b.arrow --key id --threads 18
//! # narrow proxy (id + value int64, ~2% of rows changed):
//! target/release/examples/row_diff_profile 1000000 linear 18
//! # wide proxy (id + 34 64-byte string columns, every row changed, one cell each):
//! target/release/examples/row_diff_profile 1000000 manycols 18 34 64
//! ```
//!
//! Generated args: `<rows> <shape> [threads] [shape params...]`, key always `id`,
//! `threads` defaulting to available parallelism (or `ROW_DIFF_THREADS`). Shapes:
//!
//! - `linear`: `id`/`value` int64; the right side shifts the key range by 1% (1%
//!   added, 1% removed) and perturbs every 50th value (~2% changed).
//! - `allchange`: `id`/`value` int64, every shared row changed.
//! - `wide`: `id` int64, `value` a `width`-byte string differing on every row.
//! - `manycols`: `id` int64 plus `ncols` `width`-byte string columns of which
//!   only the first differs, so every row is changed but one cell each and the
//!   spill carries all value columns.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::RecordBatchReader;
use arrow_ipc::reader::FileReader;
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use onix_arrow::{TableDiffError, TableDiffOptions, TableInput, diff_tables, profile, spool};

#[path = "shared/gen_shapes.rs"]
mod gen_shapes;
use gen_shapes::{Generated, Shape, batch_rows};

/// A table read from an Arrow IPC (`.arrow`/Feather v2) file, re-opened on every
/// `open` so each pass re-reads and re-decodes it, the same re-read the Python
/// bindings' input spool incurs. The real narrow/wide parquet fixtures are
/// profiled through this mode after a one-line pyarrow conversion to Arrow IPC
/// (see `perf/arrow/README.md`).
struct FileInput {
    path: PathBuf,
    schema: SchemaRef,
}

impl FileInput {
    fn load(path: &str) -> FileInput {
        let reader = open_ipc(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
        FileInput {
            path: PathBuf::from(path),
            schema: reader.schema(),
        }
    }
}

fn open_ipc(path: &str) -> Result<FileReader<BufReader<File>>, TableDiffError> {
    let file = File::open(path).map_err(|e| TableDiffError::Read {
        message: format!("open {path}: {e}"),
    })?;
    FileReader::try_new(BufReader::new(file), None).map_err(|e| TableDiffError::Read {
        message: format!("read Arrow IPC {path}: {e}"),
    })
}

impl TableInput for FileInput {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
    fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError> {
        let path = self.path.to_string_lossy();
        Ok(Box::new(open_ipc(&path)?))
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
                Shape::Wide {
                    value_width: width,
                    fill: b'a',
                },
                Shape::Wide {
                    value_width: width,
                    fill: b'b',
                },
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

/// Times one uninstrumented diff (recording off, so no `ps` forks perturb the
/// wall) and one instrumented diff (the per-pass breakdown), then prints both
/// walls and the table. The per-pass walls exclude the boundary `ps` cost, so
/// they sum to about the uninstrumented wall, not the instrumented one.
fn run(left: &impl TableInput, right: &impl TableInput, options: &TableDiffOptions, label: &str) {
    let start = std::time::Instant::now();
    let diff = diff_tables(left, right, options).expect("diff succeeds");
    let uninstrumented = start.elapsed();
    let summary = diff.summary();

    profile::begin();
    let start = std::time::Instant::now();
    let _ = diff_tables(left, right, options).expect("diff succeeds");
    let instrumented = start.elapsed();
    let passes = profile::finish();

    println!("{label}");
    println!("threads: {}", options.threads());
    println!("uninstrumented wall: {:.3} s", uninstrumented.as_secs_f64());
    println!(
        "instrumented wall (with ps sampling): {:.3} s",
        instrumented.as_secs_f64()
    );
    println!(
        "rows_added={} rows_removed={} rows_changed={} duplicate_keys={} cells_changed={}",
        summary.rows_added,
        summary.rows_removed,
        summary.rows_changed,
        summary.duplicate_keys,
        summary.cells_changed
    );
    println!();
    println!("{:<42} {:>10} {:>12}", "pass", "wall (s)", "peak RSS (MB)");
    for pass in &passes {
        match pass.peak_rss_mib {
            Some(rss) => println!("{:<42} {:>10.3} {:>12.1}", pass.label, pass.wall_secs, rss),
            None => println!("{:<42} {:>10.3} {:>12}", pass.label, pass.wall_secs, "-"),
        }
    }
}

fn options_for(keys: Vec<String>, threads: Option<i64>) -> TableDiffOptions {
    let mut options = TableDiffOptions::new(keys);
    if let Some(t) = threads_arg(threads) {
        options = options.with_threads(t).expect("threads within MAX_THREADS");
    }
    options
}

/// Parses `--key col[,col...]` and `--threads N` from a file-mode arg tail.
fn parse_file_flags(tail: &[String]) -> (Vec<String>, Option<i64>) {
    let mut keys = Vec::new();
    let mut threads = None;
    let mut i = 0;
    while i < tail.len() {
        match tail[i].as_str() {
            "--key" => {
                if let Some(v) = tail.get(i + 1) {
                    keys.extend(v.split(',').map(str::to_string));
                }
                i += 2;
            }
            "--threads" => {
                threads = tail.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            _ => i += 1,
        }
    }
    if keys.is_empty() {
        keys.push("id".to_string());
    }
    (keys, threads)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("file") {
        let left_path = args.get(2).expect("file mode: <left.arrow>");
        let right_path = args.get(3).expect("file mode: <right.arrow>");
        let (keys, threads) = parse_file_flags(&args[4..]);
        let left = FileInput::load(left_path);
        let right = FileInput::load(right_path);
        let options = options_for(keys, threads);
        run(
            &left,
            &right,
            &options,
            &format!("file: {left_path} vs {right_path}"),
        );
        return;
    }

    let rows: i64 = args
        .get(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let shape = args.get(2).map_or("linear", String::as_str);
    let threads = args.get(3).and_then(|a| a.parse().ok());
    let params: Vec<i64> = args.iter().skip(4).filter_map(|a| a.parse().ok()).collect();

    let (schema, left_shape, right_shape) = build_case(shape, &params, rows);
    let batch = batch_rows();
    let left = spool_side(&Generated {
        schema: schema.clone(),
        rows,
        shape: left_shape,
        batch,
    });
    let right = spool_side(&Generated {
        schema,
        rows,
        shape: right_shape,
        batch,
    });
    let options = options_for(vec!["id".to_string()], threads);
    run(
        &left,
        &right,
        &options,
        &format!("rows per side: {rows} (shape={shape})"),
    );
}
