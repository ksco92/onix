//! Per-pass wall-time and peak-RSS profile of one keyed row diff. Builds only
//! with the `profile` feature, which the release wheel never enables.
//!
//! Each invocation runs a discarded warm-up diff, a timed uninstrumented diff
//! (the `uninstrumented wall` line), and an instrumented diff whose passes make
//! up the table. The closure line compares the passes' sum with the instrumented
//! wall minus the profiler's own boundary `ps` reads; `share` is each row's wall
//! over that net wall. Peak RSS is the process's, third diff in the process.
//!
//! - **file**: reads each side from an uncompressed Arrow IPC file, re-opened
//!   by each pass that reads it. Convert a parquet fixture once with
//!   `python -c "import pyarrow.parquet as p, pyarrow.feather as f;
//!   f.write_feather(p.read_table('a.parquet'), 'a.arrow', compression='uncompressed')"`.
//! - **generated**: spools both sides of a deterministic proxy shape to anonymous
//!   Arrow IPC files (the `spool write` line times the IPC writer alone).
//!
//! ```sh
//! cargo build -p onix-arrow --release --features profile --example row_diff_profile
//! target/release/examples/row_diff_profile file a.arrow b.arrow --key id --threads 18
//! target/release/examples/row_diff_profile 1000000 linear 18
//! target/release/examples/row_diff_profile 1000000 manycols 18 34 64
//! ```
//!
//! Generated args: `[rows [shape [threads [shape params...]]]]`, key `id`,
//! `threads` defaulting to `ROW_DIFF_THREADS` or available parallelism. Shapes:
//!
//! - `linear`: `id`/`value` int64; 1% of keys added, 1% removed, ~2% changed.
//! - `allchange`: `id`/`value` int64, every row changed.
//! - `wide [width=512]`: `id` plus one `width`-byte string, every row changed.
//! - `manycols [ncols=34 [width=64]]`: `id` plus `ncols` `width`-byte strings,
//!   only the first differing, so the spill carries every value column.

use std::fs::File;
use std::io::BufReader;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use arrow_array::RecordBatchReader;
use arrow_ipc::reader::FileReader;
use arrow_schema::SchemaRef;
use onix_arrow::{TableDiffError, TableDiffOptions, TableInput, diff_tables, profile, spool};

#[path = "shared/gen_shapes.rs"]
mod gen_shapes;
use gen_shapes::{Case, Generated, batch_rows};

/// A table read from an Arrow IPC file, re-opened on every `open`.
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

/// Spools one generated side, returning it and the time spent in the IPC
/// writer (generation excluded).
fn spool_side(side: &Generated) -> (Spooled, Duration) {
    let (file, mut writer) = spool::open(&side.schema).expect("open spool");
    let mut writing = Duration::ZERO;
    for batch in side.open().expect("open generator") {
        let batch = batch.expect("generate batch");
        let start = Instant::now();
        writer.write(&batch).expect("spool write");
        writing += start.elapsed();
    }
    let start = Instant::now();
    writer.finish().expect("spool finish");
    writing += start.elapsed();
    let spooled = Spooled {
        file,
        schema: side.schema.clone(),
    };
    (spooled, writing)
}

/// Runs one discarded warm-up diff, one timed uninstrumented diff, and one
/// instrumented diff, then prints both walls, the closure of the instrumented
/// wall over its passes, and the per-pass table.
fn run(left: &impl TableInput, right: &impl TableInput, options: &TableDiffOptions, label: &str) {
    let _ = diff_tables(left, right, options).expect("diff succeeds");
    let start = Instant::now();
    let diff = diff_tables(left, right, options).expect("diff succeeds");
    let uninstrumented = start.elapsed().as_secs_f64();
    let summary = diff.summary();

    let session = profile::begin();
    let start = Instant::now();
    let _ = diff_tables(left, right, options).expect("diff succeeds");
    let instrumented = start.elapsed().as_secs_f64();
    let report = session.finish();

    let boundary: f64 = report
        .iter()
        .filter(|row| row.label == profile::BOUNDARY_LABEL)
        .map(|row| row.wall_secs)
        .sum();
    let passes: f64 = report
        .iter()
        .filter(|row| row.peak_rss_mib.is_some())
        .map(|row| row.wall_secs)
        .sum();
    let closed = instrumented - boundary;

    println!("{label}");
    println!("threads: {}", options.threads());
    println!("uninstrumented wall: {uninstrumented:.3} s");
    println!("instrumented wall: {instrumented:.3} s");
    println!("boundary ps reads: {boundary:.3} s");
    println!(
        "passes sum: {passes:.3} s of {closed:.3} s (instrumented minus boundary); residual {:.3} s ({:.1}%)",
        closed - passes,
        100.0 * (closed - passes) / closed
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
    println!(
        "{:<44} {:>10} {:>8} {:>14}",
        "pass", "wall (s)", "share", "peak RSS (MB)"
    );
    for row in report
        .iter()
        .filter(|row| row.label != profile::BOUNDARY_LABEL)
    {
        let share = 100.0 * row.wall_secs / closed;
        match row.peak_rss_mib {
            Some(rss) => println!(
                "{:<44} {:>10.3} {:>7.1}% {:>14.1}",
                row.label, row.wall_secs, share, rss
            ),
            None => println!(
                "{:<44} {:>10.3} {:>7.1}% {:>14}",
                format!("  {}", row.label),
                row.wall_secs,
                share,
                "-"
            ),
        }
    }
}

const USAGE: &str =
    "usage: row_diff_profile file <left.arrow> <right.arrow> [--key col[,col...]] [--threads N]
       row_diff_profile [rows [linear|allchange|wide|manycols [threads [shape params...]]]]";

fn usage_error(message: &str) -> ! {
    eprintln!("{message}\n{USAGE}");
    std::process::exit(2);
}

/// Parses a positive integer argument, exiting with a usage error otherwise.
fn positive(what: &str, value: &str) -> NonZeroUsize {
    value.parse().unwrap_or_else(|_| {
        usage_error(&format!("{what} must be a positive integer, got {value:?}"))
    })
}

fn options_for(keys: Vec<String>, threads: Option<NonZeroUsize>) -> TableDiffOptions {
    let threads = threads.or_else(|| {
        std::env::var("ROW_DIFF_THREADS")
            .ok()
            .map(|v| positive("ROW_DIFF_THREADS", &v))
    });
    let options = TableDiffOptions::new(keys);
    match threads {
        Some(t) => options
            .with_threads(t)
            .unwrap_or_else(|e| usage_error(&e.to_string())),
        None => options,
    }
}

fn file_mode(args: &[String]) {
    let [left_path, right_path, flags @ ..] = args else {
        usage_error("file mode needs <left.arrow> <right.arrow>");
    };
    let mut keys = Vec::new();
    let mut threads = None;
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let Some(value) = flags.next() else {
            usage_error(&format!("{flag} needs a value"));
        };
        match flag.as_str() {
            "--key" => keys.extend(value.split(',').map(str::to_string)),
            "--threads" => threads = Some(positive("--threads", value)),
            _ => usage_error(&format!("unknown flag {flag:?}")),
        }
    }
    if keys.is_empty() {
        keys.push("id".to_string());
    }
    let left = FileInput::load(left_path);
    let right = FileInput::load(right_path);
    run(
        &left,
        &right,
        &options_for(keys, threads),
        &format!("file: {left_path} vs {right_path}"),
    );
}

fn generated_mode(args: &[String]) {
    let rows = args.first().map_or(1_000_000, |a| {
        i64::try_from(positive("rows", a).get()).unwrap_or_else(|_| usage_error("rows too large"))
    });
    let shape = args.get(1).map_or("linear", String::as_str);
    let threads = args.get(2).map(|a| positive("threads", a));
    let params: Vec<usize> = args
        .iter()
        .skip(3)
        .map(|a| positive("shape parameter", a).get())
        .collect();
    let param = |i: usize, default: usize| params.get(i).copied().unwrap_or(default);
    let (case, arity) = match shape {
        "linear" => (Case::Linear, 0),
        "allchange" => (Case::AllChange, 0),
        "wide" => (Case::Wide(param(0, 512)), 1),
        "manycols" => (
            Case::ManyCols {
                ncols: param(0, 34),
                width: param(1, 64),
            },
            2,
        ),
        _ => usage_error(&format!("unknown shape {shape:?}")),
    };
    if params.len() > arity {
        usage_error(&format!("shape {shape} takes at most {arity} parameter(s)"));
    }

    let (schema, left_shape, right_shape, key) = case.build(rows);
    let options = options_for(vec![key.to_string()], threads);
    let batch = batch_rows();
    let (left, left_write) = spool_side(&Generated {
        schema: schema.clone(),
        rows,
        shape: left_shape,
        batch,
    });
    let (right, right_write) = spool_side(&Generated {
        schema,
        rows,
        shape: right_shape,
        batch,
    });
    println!(
        "spool write (both sides, before the diff): {:.3} s",
        (left_write + right_write).as_secs_f64()
    );
    run(
        &left,
        &right,
        &options,
        &format!("rows per side: {rows} (shape={shape})"),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        Some((mode, rest)) if mode == "file" => file_mode(rest),
        _ => generated_mode(&args),
    }
}
