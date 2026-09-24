//! The profiled passes of a row diff and the spool decode time under each. A
//! binary of its own, so no other test's diff records into the session.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use arrow_array::{Int64Array, RecordBatch, RecordBatchIterator, RecordBatchReader};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use onix_arrow::{TableDiffError, TableDiffOptions, TableInput, diff_tables, profile};

const DECODE: &str = "spool decode (reader.next)";
const DELAY: Duration = Duration::from_millis(3);

/// A side whose reader sleeps [`DELAY`] before yielding each batch, so every
/// decode it records is at least that long.
struct SlowInput {
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
}

impl SlowInput {
    fn new(batches: i64, rows: i64, id_offset: i64) -> SlowInput {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("v", DataType::Int64, false),
        ]));
        let batches = (0..batches)
            .map(|b| {
                let ids: Int64Array = (0..rows).map(|i| b * rows + i + id_offset).collect();
                // With the right side offset by one, every shared key changes.
                let values: Int64Array = ids.iter().map(|id| id.map(|id| id + id_offset)).collect();
                RecordBatch::try_new(schema.clone(), vec![Arc::new(ids), Arc::new(values)]).unwrap()
            })
            .collect();
        SlowInput { schema, batches }
    }
}

impl TableInput for SlowInput {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
    fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError> {
        let slow = self.batches.clone().into_iter().map(|batch| {
            std::thread::sleep(DELAY);
            Ok::<_, ArrowError>(batch)
        });
        Ok(Box::new(RecordBatchIterator::new(
            slow,
            self.schema.clone(),
        )))
    }
}

/// Each pass of a diff over two `batches`-batch sides at `threads`, with its
/// decode seconds.
fn profiled(threads: usize, batches: i64, rows: i64) -> Vec<(&'static str, f64)> {
    let left = SlowInput::new(batches, rows, 0);
    let right = SlowInput::new(batches, rows, 1);
    let options = TableDiffOptions::new(vec!["id".to_string()])
        .with_threads(NonZeroUsize::new(threads).unwrap())
        .unwrap();
    let session = profile::begin();
    diff_tables(&left, &right, &options).unwrap();
    let mut passes: Vec<(&'static str, f64)> = Vec::new();
    for row in session.finish() {
        match (row.peak_rss_mib, passes.last_mut()) {
            (Some(_), _) => passes.push((row.label, 0.0)),
            (None, Some(last)) if row.label == DECODE => {
                last.1 = row.wall_secs;
            }
            _ => {}
        }
    }
    passes
}

/// Asserts `passes` are `expected` in order and that each `(pass, sides)` in
/// `reads` decoded `sides` sides' `batches` batches.
fn assert_passes(passes: &[(&str, f64)], expected: &[&str], reads: &[(usize, u32)], batches: u32) {
    let labels: Vec<&str> = passes.iter().map(|&(label, _)| label).collect();
    assert_eq!(labels, expected);
    for &(pass, sides) in reads {
        let (label, decode) = passes[pass];
        let floor = (DELAY * sides * batches).as_secs_f64();
        assert!(
            decode >= floor,
            "{label}: {decode} s decoding, under {floor} s"
        );
    }
}

#[test]
fn each_pass_records_the_decode_of_every_batch_it_reads() {
    let sequential = [
        "set-up",
        "hash and classify",
        "materialize",
        "cell (sequential)",
    ];
    let both_sides_each_pass = [(1, 2), (2, 2), (3, 2)];
    // One thread: the plain sequential scans.
    assert_passes(&profiled(1, 4, 100), &sequential, &both_sides_each_pass, 4);
    // Two threads under the size gate: the peek decodes every batch.
    assert_passes(&profiled(2, 4, 100), &sequential, &both_sides_each_pass, 4);
    // Two threads over the size gate: the peek, then the parallel scans, which
    // read the right once and the left twice.
    let parallel = [
        "set-up",
        "hash and classify",
        "materialize and cell spill (left re-read)",
        "materialize added (right candidates)",
        "cell: render sort keys",
        "cell: partition read-back and render",
        "cell: sort and interleave",
    ];
    assert_passes(&profiled(2, 8, 10_000), &parallel, &[(1, 2), (2, 1)], 8);
}
