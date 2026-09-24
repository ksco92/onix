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
//! # wide rows, few changed cells: id + 8 512-byte columns, only one differing
//! ROW_DIFF_THREADS=18 /usr/bin/time -l target/release/examples/row_diff_rss 150000 manycols 8 512
//! # two 1 KB `Utf8View` columns: every row removed, every row added, every
//! # 10,000th row removed; the right repeating left keys, repeating keys the
//! # left lacks, and repeating one key the left lacks once per 10,000 rows
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 viewremoved 1024
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 viewadded 1024
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 viewaddedbyvalue 1024
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 viewsparse 1024 10000
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 duprightonce 1024
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 duprightabsent 1024
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 repeatabsent 1024 10000
//! # a right side of keys the left lacks, each batch repeating the last one's
//! ROW_DIFF_BATCH=16 target/release/examples/row_diff_rss 1000000 chain 64
//! # duplicate-heavy shape: every key duplicated, wide string key
//! /usr/bin/time -l target/release/examples/row_diff_rss 1000000 dup 16
//! /usr/bin/time -l target/release/examples/row_diff_rss 200000 dup 1024
//! # size-gate peek: identical wide-cell sides (zero changes); ROW_DIFF_BATCH
//! # sets the producer's batch size, ROW_DIFF_THREADS the worker count
//! ROW_DIFF_BATCH=100 ROW_DIFF_THREADS=18 /usr/bin/time -l target/release/examples/row_diff_rss 49999 widesame 8192
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

use onix_arrow::{TableDiffOptions, diff_tables};

#[path = "shared/gen_shapes.rs"]
mod gen_shapes;
use gen_shapes::{Case, Generated, batch_rows};

/// Options for the diff, honoring a `ROW_DIFF_THREADS` override so the parallel
/// path's peak RSS can be compared against the single-threaded baseline; unset
/// uses the default (available parallelism).
fn options_from_env(key: &str) -> TableDiffOptions {
    let mut options = TableDiffOptions::new(vec![key.to_string()]);
    if let Some(threads) = std::env::var("ROW_DIFF_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .and_then(std::num::NonZeroUsize::new)
    {
        options = options
            .with_threads(threads)
            .expect("ROW_DIFF_THREADS within MAX_THREADS");
    }
    options
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rows: i64 = args
        .get(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let mode = args.get(2).map_or("", String::as_str);
    let width: usize = args
        .get(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or(if mode == "wide" { 1024 } else { 16 });

    let (case, label) = match mode {
        "" | "linear" => (Case::Linear, String::new()),
        "nochange" => (Case::NoChange, " (nochange baseline)".to_string()),
        "allchange" => (Case::AllChange, " (all changed)".to_string()),
        "wide" => (Case::Wide(width), format!(" (wide, value_width={width})")),
        "widesame" => (
            Case::WideSame(width),
            format!(" (widesame, value_width={width})"),
        ),
        "manycols" => {
            let ncols: usize = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(8);
            let width: usize = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(512);
            (
                Case::ManyCols { ncols, width },
                format!(" (manycols, ncols={ncols}, width={width})"),
            )
        }
        "dup" => (Case::Dup(width), format!(" (dup, key_width={width})")),
        "viewremoved" | "viewadded" | "viewaddedbyvalue" | "viewsparse" | "duprightonce"
        | "duprightabsent" | "repeatabsent" | "chain" => {
            let width: usize = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1024);
            let every: i64 = args.get(4).and_then(|a| a.parse().ok()).unwrap_or(10_000);
            let case = match mode {
                "viewremoved" => Case::ViewRemoved(width),
                "viewadded" => Case::ViewAdded(width),
                "viewaddedbyvalue" => Case::ViewAddedByValue(width),
                "viewsparse" => Case::ViewSparse { width, every },
                "duprightonce" => Case::DupRightOnce(width),
                "duprightabsent" => Case::DupRightAbsent(width),
                "chain" => Case::Chain(width),
                _ => Case::RepeatAbsent { width, every },
            };
            (case, format!(" ({mode}, width={width}, every={every})"))
        }
        other => {
            eprintln!(
                "unknown mode {other:?}; expected linear (the default), nochange, allchange, wide, widesame, manycols, dup, viewremoved, viewadded, viewaddedbyvalue, viewsparse, duprightonce, duprightabsent, repeatabsent or chain"
            );
            std::process::exit(2);
        }
    };
    let (schema, left_shape, right_shape, key) = case.build(rows);

    let batch = batch_rows();
    let left = Generated {
        schema: schema.clone(),
        rows,
        shape: left_shape,
        batch,
    };
    let right = Generated {
        schema,
        rows,
        shape: right_shape,
        batch,
    };

    let options = options_from_env(key);
    let start = std::time::Instant::now();
    let diff = diff_tables(&left, &right, &options).expect("diff succeeds");
    let elapsed = start.elapsed();
    let summary = diff.summary();

    println!("rows per side: {rows}{label}");
    println!("threads: {}", options.threads());
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
