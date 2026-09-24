//! Shared streaming table generator and cases for the row-diff examples
//! (`row_diff_rss` and `row_diff_profile`), included with `#[path]` by both.
//! Each shape's data is a deterministic function of the row index, so nothing
//! is retained between batches and two runs at the same size produce
//! byte-identical data.

// Each example includes this module and uses a subset of the shapes, so a shape
// unused by one example is not dead across the pair.
#![allow(dead_code)]

use std::sync::Arc;

use arrow_array::{
    ArrayRef, Int64Array, RecordBatch, RecordBatchReader, StringArray, StringViewArray,
};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
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
    /// `(id, v0, v1)`, two `width`-byte `Utf8View` columns starting with `fill`;
    /// `keys` maps row `i` to its id, or omits it.
    View {
        width: usize,
        fill: u8,
        keys: ViewKeys,
    },
}

/// How a [`Shape::View`] side keys its rows.
#[derive(Clone, Copy)]
pub enum ViewKeys {
    /// Id `i`, omitting every row when `omit_every` is 1, every
    /// `omit_every`-th row when above 1, and none when 0.
    Plain { omit_every: i64 },
    /// Id `offset + i / 2`, so each id appears twice.
    Twice { offset: i64 },
    /// Id `i`, except every `every`-th row, whose id is `-1`.
    RepeatAbsent { every: i64 },
}

impl ViewKeys {
    fn id(self, i: i64) -> Option<i64> {
        match self {
            ViewKeys::Plain { omit_every } => (omit_every == 0 || i % omit_every != 0).then_some(i),
            ViewKeys::Twice { offset } => Some(offset + i / 2),
            ViewKeys::RepeatAbsent { every } => Some(if i % every == 0 { -1 } else { i }),
        }
    }
}

/// A generated two-sided case, with its size parameters already defaulted by
/// the calling example.
#[derive(Clone, Copy)]
pub enum Case {
    /// 1% of keys added and 1% removed, every 50th shared value changed.
    Linear,
    /// [`Case::Linear`]'s added and removed keys with no changed row.
    NoChange,
    /// The same keys on both sides, every row changed.
    AllChange,
    /// Every row changed in a `width`-byte string column.
    Wide(usize),
    /// [`Case::Wide`] with equal sides.
    WideSame(usize),
    /// `ncols` `width`-byte string columns, only the first differing.
    ManyCols { ncols: usize, width: usize },
    /// Every `key_width`-byte string key appearing twice on each side.
    Dup(usize),
    /// Two `width`-byte view columns; every left row removed (right empty).
    ViewRemoved(usize),
    /// [`Case::ViewRemoved`] mirrored: every right row added (left empty).
    ViewAdded(usize),
    /// Two `width`-byte view columns, equal sides except every `every`-th left
    /// row, which the right lacks.
    ViewSparse { width: usize, every: i64 },
    /// Left ids once; the right repeats the first half of them twice each, with
    /// different values, so each is a duplicate key.
    DupRightOnce(usize),
    /// Left ids once; the right holds ids the left lacks, each twice.
    DupRightAbsent(usize),
    /// Equal sides except every `every`-th right row, keyed by one id the left
    /// lacks.
    RepeatAbsent { width: usize, every: i64 },
}

impl Case {
    /// The schema, the left and right shapes, and the key column.
    pub fn build(self, rows: i64) -> (SchemaRef, Shape, Shape, &'static str) {
        let int_schema = || {
            Arc::new(Schema::new(vec![
                Field::new("id", DataType::Int64, false),
                Field::new("value", DataType::Int64, false),
            ]))
        };
        let linear = |id_offset, change_every| {
            let left = Shape::Linear {
                id_offset: 0,
                change_every: i64::MAX,
            };
            let right = Shape::Linear {
                id_offset,
                change_every,
            };
            (int_schema(), left, right, "id")
        };
        let step = (rows / 100).max(1);
        match self {
            Case::Linear => linear(step, 50),
            Case::NoChange => linear(step, i64::MAX),
            Case::AllChange => linear(0, 1),
            Case::Wide(width) | Case::WideSame(width) => {
                let schema = Arc::new(Schema::new(vec![
                    Field::new("id", DataType::Int64, false),
                    Field::new("value", DataType::Utf8, false),
                ]));
                let right_fill = if matches!(self, Case::WideSame(_)) {
                    b'a'
                } else {
                    b'b'
                };
                let shape = |fill| Shape::Wide {
                    value_width: width,
                    fill,
                };
                (schema, shape(b'a'), shape(right_fill), "id")
            }
            Case::ManyCols { ncols, width } => {
                let mut fields = vec![Field::new("id", DataType::Int64, false)];
                for c in 0..ncols {
                    fields.push(Field::new(format!("value{c}"), DataType::Utf8, false));
                }
                let shape = |first_fill| Shape::ManyCols {
                    ncols,
                    width,
                    first_fill,
                };
                (
                    Arc::new(Schema::new(fields)),
                    shape(b'a'),
                    shape(b'b'),
                    "id",
                )
            }
            Case::ViewRemoved(width)
            | Case::ViewAdded(width)
            | Case::ViewSparse { width, .. }
            | Case::DupRightOnce(width)
            | Case::DupRightAbsent(width)
            | Case::RepeatAbsent { width, .. } => {
                let schema = Arc::new(Schema::new(vec![
                    Field::new("id", DataType::Int64, false),
                    Field::new("v0", DataType::Utf8View, false),
                    Field::new("v1", DataType::Utf8View, false),
                ]));
                let view = |fill, keys| Shape::View { width, fill, keys };
                let all = ViewKeys::Plain { omit_every: 0 };
                let (left, right) = match self {
                    Case::ViewRemoved(_) => (all, ViewKeys::Plain { omit_every: 1 }),
                    Case::ViewAdded(_) => (ViewKeys::Plain { omit_every: 1 }, all),
                    Case::ViewSparse { every, .. } => (all, ViewKeys::Plain { omit_every: every }),
                    Case::DupRightOnce(_) => (all, ViewKeys::Twice { offset: 0 }),
                    Case::DupRightAbsent(_) => (all, ViewKeys::Twice { offset: rows }),
                    Case::RepeatAbsent { every, .. } => (all, ViewKeys::RepeatAbsent { every }),
                    _ => unreachable!("only view cases reach this arm"),
                };
                let right_fill = if matches!(self, Case::DupRightOnce(_)) {
                    b'b'
                } else {
                    b'a'
                };
                (schema, view(b'a', left), view(right_fill, right), "id")
            }
            Case::Dup(key_width) => {
                let schema = Arc::new(Schema::new(vec![
                    Field::new("key", DataType::Utf8, false),
                    Field::new("value", DataType::Int64, false),
                ]));
                let shape = Shape::Dup { key_width };
                (schema, shape, shape, "key")
            }
        }
    }
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
            Shape::View { width, fill, keys } => {
                let (ids, rows): (Vec<i64>, Vec<i64>) = (self.next..end)
                    .filter_map(|i| keys.id(i).map(|id| (id, i)))
                    .unzip();
                let cell = |column: u8| {
                    let cells: StringViewArray = rows
                        .iter()
                        .map(|&i| {
                            let pad = width.saturating_sub(2);
                            Some(format!("{}{column}{i:0>pad$}", char::from(fill)))
                        })
                        .collect();
                    Arc::new(cells) as ArrayRef
                };
                vec![Arc::new(Int64Array::from(ids)), cell(0), cell(1)]
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
