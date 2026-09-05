//! Keyed row diff: which rows were added, removed, or changed between two
//! tables, matched by a required primary key, in memory proportional to the
//! row count rather than the data size.
//!
//! # Algorithm
//!
//! Two passes, so the full decoded tables never sit in memory at once:
//!
//! 1. **Hash pass.** Each side is opened and streamed batch by batch. Every row
//!    yields a keyed 128-bit hash of its key columns and a keyed 128-bit hash of
//!    its non-key columns (the value semantics are in [`hash_cell`]). The pairs
//!    are collected into one `(key_hash, row_hash)` vector per side — 32 bytes
//!    per row, the only per-row state that grows with the input — while the
//!    batches themselves are dropped as they are consumed. Set arithmetic on the
//!    two sorted vectors then classifies every key: only on the left (removed),
//!    only on the right (added), on both with different row hashes (changed), on
//!    both with equal row hashes (unchanged, never materialized), or appearing
//!    more than once on either side (a duplicate key, excluded from the other
//!    three and reported with its per-side counts).
//! 2. **Materialize pass.** Each side is opened again (a [`TableInput`] is
//!    re-openable) and filtered to the rows whose keys landed in the added /
//!    removed sets, plus one row per duplicate key for the duplicate-key report.
//!    Only the differing rows are ever built into an output batch.
//!
//! Memory beyond the per-row hash vectors: the duplicate-key report holds the
//! actual key values of every *distinct duplicated* key, so a duplicate-heavy
//! input adds a term proportional to the number of distinct duplicated keys
//! times the key width (see the README's Known-limitations bullet for measured
//! figures).
//!
//! # Hashing
//!
//! Row identity is a single keyed 128-bit SipHash-1-3 ([`siphasher`]), keyed
//! from 16 bytes of OS randomness ([`getrandom`]) drawn once per diff. Both
//! sides of one diff share the key, so their hashes are comparable; a different
//! diff draws a fresh key. Because the key is secret and random per run, the
//! row-matching table cannot be forced into collisions by chosen input, and no
//! unkeyed content hash table is used on this default (no-flag) path. Two
//! distinct keys colliding to the same 128-bit hash — the only way this can
//! misclassify — has probability on the order of `n² / 2¹²⁸`, negligible for
//! any real table.
//!
//! # Value semantics
//!
//! Cell hashing matches how onix's core compares scalars: integers and integral
//! floats within `±2⁵³` fold to one integer form (so `1`, `1.0`, `-0.0`, and a
//! dictionary-encoded `1` all hash equal), other floats hash by their bit
//! pattern, decimals (128- and 256-bit) hash by their exact value with trailing
//! zeros removed (so `1.00` equals `1.0000`), timestamps hash by their UTC
//! instant in nanoseconds (so the same instant at microsecond and millisecond
//! precision hashes equal), and a null is a distinct value that equals only
//! another null — the `IS DISTINCT FROM` semantics the `DuckDB` oracle uses.
//!
//! # Which column types are hashed, refused, or skipped
//!
//! - **Hashed** (compared): `Null` (every row a null); boolean; every signed
//!   and unsigned integer width; `Float16`/`Float32`/`Float64`; `Decimal32`,
//!   `Decimal64`, `Decimal128`, and `Decimal256` (all by exact value, so equal
//!   values of different widths or scales hash equal); `Utf8`, `LargeUtf8`,
//!   `Utf8View`; `Binary`, `LargeBinary`, `BinaryView`, `FixedSizeBinary`;
//!   `Timestamp` (any unit, with or without a zone); `Date32` and `Date64` (both
//!   by day count, so a `Date32` and a whole-day `Date64` of the same calendar
//!   day hash equal; a non-whole-day `Date64`, which Arrow's whole-day contract
//!   forbids, keeps its raw value distinctly); `Time32` (second, millisecond),
//!   `Time64` (microsecond, nanosecond), and `Duration` (by raw value tagged
//!   with their unit, so a unit change reads as a value change); `Interval` (by
//!   its per-variant fields); and a `Dictionary` of any of these (decoded
//!   first).
//! - **Refused** with [`TableDiffError::UnsupportedRowType`], key or non-key:
//!   `RunEndEncoded`, and the `Time32`/`Time64` unit and `FixedSizeBinary` width
//!   combinations arrow-rs has no array type for (e.g. `Time32(Nanosecond)`, a
//!   negative fixed-size width). A scalar column is always hashed or refused,
//!   never silently skipped.
//! - **Skipped** (not compared, so a change in it is not reported): a *nested*
//!   non-key column (`List` and its variants, `FixedSizeList`, `Struct`, `Map`,
//!   `Union`), which is out of scope for the row diff. A nested *key* column is
//!   refused.
//!
//! [`hash_cell`] itself is non-recursive; the one recursive walk here,
//! [`is_hashable`] over a dictionary value type, is bounded by the
//! [`crate::MAX_NESTING_DEPTH`] depth check [`crate::diff_schemas`] runs before
//! any row is read.

use std::collections::{HashMap, HashSet};
use std::hash::Hasher;

use siphasher::sip128::{Hasher128, SipHasher13};

use arrow_array::cast::AsArray;
use arrow_array::types::{
    Date32Type, Date64Type, Decimal32Type, Decimal64Type, Decimal128Type, Decimal256Type,
    DurationMicrosecondType, DurationMillisecondType, DurationNanosecondType, DurationSecondType,
    Float16Type, Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type,
    IntervalDayTimeType, IntervalMonthDayNanoType, IntervalYearMonthType, Time32MillisecondType,
    Time32SecondType, Time64MicrosecondType, Time64NanosecondType, TimestampMicrosecondType,
    TimestampMillisecondType, TimestampNanosecondType, TimestampSecondType, UInt8Type, UInt16Type,
    UInt32Type, UInt64Type,
};
use arrow_array::{Array, ArrayRef, BooleanArray, Int64Array, RecordBatch, RecordBatchReader};
use arrow_buffer::i256;
use arrow_schema::{ArrowError, DataType, Field, IntervalUnit, Schema, SchemaRef, TimeUnit};

/// A re-openable source of one table's record batches.
///
/// The row diff reads each side twice — once to hash every row, once to
/// materialize only the differing rows — so it needs a source it can open more
/// than once rather than a single-use [`RecordBatchReader`]. A caller whose
/// input is a one-shot reader (a Python Arrow stream, say) spools it to an
/// anonymous temporary Arrow IPC file first and re-reads that file through a
/// fresh, rewound handle on each `open`; an in-memory table is re-openable
/// directly (see [`MemoryInput`]).
pub trait TableInput {
    /// The table's schema, without opening a reader.
    fn schema(&self) -> SchemaRef;

    /// A fresh reader over the whole table. Called up to twice per diff.
    ///
    /// # Errors
    ///
    /// [`TableDiffError::Read`] if a reader cannot be opened.
    fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError>;
}

/// A [`TableInput`] backed by record batches already in memory, re-openable by
/// cloning the (cheap, `Arc`-backed) batch handles.
#[derive(Debug, Clone)]
pub struct MemoryInput {
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
}

impl MemoryInput {
    /// Wraps a schema and its batches as a re-openable input.
    #[must_use]
    pub fn new(schema: SchemaRef, batches: Vec<RecordBatch>) -> Self {
        Self { schema, batches }
    }
}

impl TableInput for MemoryInput {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError> {
        let batches: Vec<Result<RecordBatch, ArrowError>> =
            self.batches.iter().cloned().map(Ok).collect();
        Ok(Box::new(arrow_array::RecordBatchIterator::new(
            batches.into_iter(),
            self.schema.clone(),
        )))
    }
}

use crate::error::TableDiffError;

/// The largest exact integer a binary64 float can hold (`2^53`); a float
/// integral and within `±` this folds to the integer hash form. `onix-core`
/// carries the identical bound and rationale as `MAX_EXACT_F64_INT` in
/// `crates/onix-core/src/lcs.rs`; the two crates are decoupled by design, so
/// this is a deliberate copy that must move with it.
const MAX_EXACT_F64_INT: f64 = 9_007_199_254_740_992.0;

/// Domain-separation tag mixed into a key-column hash, so a key hash and a row
/// hash of the same bytes cannot collide.
const DOMAIN_KEY: u8 = 1;
/// Domain-separation tag mixed into a non-key-column (row) hash.
const DOMAIN_ROW: u8 = 2;

// Per-cell type tags, written before the value so two values of different kinds
// (a string `"1"` and an integer `1`) never hash equal.
const TAG_NULL: u8 = 0;
const TAG_INT: u8 = 1;
const TAG_FLOAT: u8 = 2;
const TAG_DECIMAL: u8 = 3;
const TAG_STR: u8 = 4;
const TAG_BIN: u8 = 5;
const TAG_TS: u8 = 6;
const TAG_DATE: u8 = 7;
const TAG_TIME: u8 = 8;
const TAG_DURATION: u8 = 9;
const TAG_INTERVAL: u8 = 10;

/// The per-diff SipHash-1-3 key (`k0`, `k1`), 16 bytes of OS randomness. Both
/// sides of one diff share it so their hashes are comparable; a different diff
/// draws a fresh key, so a chosen input cannot precompute collisions.
struct RowHasher {
    k0: u64,
    k1: u64,
}

impl RowHasher {
    /// Draws a fresh random key for this diff from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`TableDiffError::Read`] if the OS random source is unavailable.
    fn new() -> Result<Self, TableDiffError> {
        // Draw each half into its own fixed-size array, so there is no fallible
        // slice-to-array conversion to discard and no way to end up with a
        // zeroed key half.
        let mut k0 = [0u8; 8];
        let mut k1 = [0u8; 8];
        getrandom::fill(&mut k0).map_err(random_key_error)?;
        getrandom::fill(&mut k1).map_err(random_key_error)?;
        Ok(Self {
            k0: u64::from_le_bytes(k0),
            k1: u64::from_le_bytes(k1),
        })
    }

    /// A fresh keyed hasher primed with this diff's key and the domain tag, so a
    /// key hash and a row hash of the same cells cannot collide.
    fn start(&self, domain: u8) -> CellHasher {
        let mut sip = SipHasher13::new_with_keys(self.k0, self.k1);
        sip.write(&[domain]);
        CellHasher(sip)
    }
}

/// One row's keyed 128-bit hash in progress. Values are written little-endian so
/// the byte stream is stable within a run (cross-run stability is not needed —
/// the key is per-run).
struct CellHasher(SipHasher13);

impl CellHasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.write(bytes);
    }

    fn tag(&mut self, tag: u8) {
        self.write(&[tag]);
    }

    fn write_i128(&mut self, value: i128) {
        self.write(&value.to_le_bytes());
    }

    fn write_i64(&mut self, value: i64) {
        self.write(&value.to_le_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    fn finish(self) -> u128 {
        self.0.finish128().as_u128()
    }
}

/// The row-level result of a table diff.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RowDiff {
    /// Rows present only on the right, in the right table's schema.
    pub(crate) rows_added: RecordBatch,
    /// Rows present only on the left, in the left table's schema.
    pub(crate) rows_removed: RecordBatch,
    /// One row per key appearing more than once on either side: the key
    /// columns, then `left_count` and `right_count`.
    pub(crate) duplicate_keys: RecordBatch,
    /// Key hashes of rows present on both sides with a differing non-key hash.
    /// Not yet exposed to callers; the per-cell diff (a later version) consumes
    /// it.
    pub(crate) changed_keys: Vec<u128>,
    /// Counts derived from the classification.
    pub(crate) counts: RowCounts,
}

/// Counts of each row-level outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowCounts {
    pub(crate) rows_added: usize,
    pub(crate) rows_removed: usize,
    pub(crate) rows_changed: usize,
    pub(crate) duplicate_keys: usize,
    pub(crate) null_keys: usize,
}

/// Turns an Arrow error from the streaming machinery into a
/// [`TableDiffError`].
fn read_error(error: &ArrowError) -> TableDiffError {
    TableDiffError::Read {
        message: error.to_string(),
    }
}

/// Turns an OS-randomness failure (drawing the per-diff hash key) into a
/// [`TableDiffError`].
fn random_key_error(error: getrandom::Error) -> TableDiffError {
    TableDiffError::Read {
        message: format!("could not obtain a random hash key from the OS: {error}"),
    }
}

/// The column indices, on one side, of the key columns (in the caller's key
/// order) and the common non-key columns (sorted by name so both sides agree).
struct SideColumns {
    key: Vec<usize>,
    value: Vec<usize>,
}

/// Resolves, for one schema, the key columns in the caller's order and the
/// common non-key columns (present on both sides) in sorted-name order.
fn side_columns(schema: &Schema, key: &[String], common_values: &[String]) -> SideColumns {
    // Both sets are already known to exist on this side (the key by
    // `diff_tables`'s check, the common columns by construction), so
    // `index_of` never returns `None` on the reachable path.
    let key = key
        .iter()
        .filter_map(|name| schema.index_of(name).ok())
        .collect();
    let value = common_values
        .iter()
        .filter_map(|name| schema.index_of(name).ok())
        .collect();
    SideColumns { key, value }
}

/// The non-key columns compared for row changes: the columns present on both
/// schemas that are *not* nested on either side, by name, sorted. A column on
/// only one side is a schema change, never a cell change; a nested column is out
/// of scope for the row diff and is skipped. A non-nested (scalar) column is
/// kept even if [`hash_cell`] cannot hash it, so it is refused there rather than
/// silently skipped. Sorting makes both sides agree on the hashing order.
fn common_value_columns(left: &Schema, right: &Schema, key: &[String]) -> Vec<String> {
    let mut names: Vec<String> = left
        .fields()
        .iter()
        .filter(|field| {
            let name = field.name().as_str();
            !key.iter().any(|k| k == name)
                && !is_nested(field.data_type())
                && right
                    .field_with_name(name)
                    .is_ok_and(|right_field| !is_nested(right_field.data_type()))
        })
        .map(|field| field.name().clone())
        .collect();
    names.sort();
    names
}

/// Refuses any non-nested column of `schema` that [`is_hashable`] cannot hash,
/// so an invalid scalar type (including one present on only one side) fails
/// before `RecordBatch::new_empty` would panic building its empty array. Nested
/// columns are left to the per-column skip/refuse rules.
fn reject_unhashable_columns(schema: &Schema) -> Result<(), TableDiffError> {
    for field in schema.fields() {
        if !is_nested(field.data_type()) && !is_hashable(field.data_type()) {
            return Err(TableDiffError::UnsupportedRowType {
                column: field.name().clone(),
                data_type: field.data_type().to_string(),
            });
        }
    }

    Ok(())
}

/// Whether a type is a nested (container) type — the types the row diff is out
/// of scope for, skipped when non-key and refused when key. A `Dictionary` is
/// *not* nested: it is a scalar encoding, decoded before hashing.
fn is_nested(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::List(_)
            | DataType::LargeList(_)
            | DataType::ListView(_)
            | DataType::LargeListView(_)
            | DataType::FixedSizeList(_, _)
            | DataType::Struct(_)
            | DataType::Map(_, _)
            | DataType::Union(_, _)
    )
}

/// Whether a column of this type can be hashed by value (see [`hash_cell`]): a
/// scalar type this crate handles, or a dictionary of one. Used to validate key
/// columns up front — a key of a non-hashable type (nested, or a scalar the
/// crate refuses such as `RunEndEncoded`) is a
/// [`TableDiffError::UnsupportedRowType`] before any row is read. The
/// enumeration matches [`hash_cell`]'s.
///
/// Recurses through the dictionary value type, but only after
/// [`crate::diff_schemas`]'s `check_depths` has rejected any column nested past
/// [`crate::MAX_NESTING_DEPTH`], so the recursion is bounded and cannot overflow
/// the native stack.
pub(crate) fn is_hashable(data_type: &DataType) -> bool {
    match data_type {
        DataType::Null
        | DataType::Boolean
        | DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64
        | DataType::Float16
        | DataType::Float32
        | DataType::Float64
        | DataType::Decimal32(_, _)
        | DataType::Decimal64(_, _)
        | DataType::Decimal128(_, _)
        | DataType::Decimal256(_, _)
        | DataType::Utf8
        | DataType::LargeUtf8
        | DataType::Utf8View
        | DataType::Binary
        | DataType::LargeBinary
        | DataType::BinaryView
        | DataType::Timestamp(_, _)
        | DataType::Date32
        | DataType::Date64
        | DataType::Duration(_)
        | DataType::Interval(_) => true,
        // Only the width/unit combinations arrow-rs has a concrete array type
        // for; the others (e.g. `Time32(Nanosecond)`) have no array and would
        // panic in `RecordBatch::new_empty`, so they fall through to refused.
        DataType::FixedSizeBinary(width) => *width >= 0,
        DataType::Time32(unit) => {
            matches!(unit, TimeUnit::Second | TimeUnit::Millisecond)
        }
        DataType::Time64(unit) => {
            matches!(unit, TimeUnit::Microsecond | TimeUnit::Nanosecond)
        }
        DataType::Dictionary(_, value) => is_hashable(value),
        _ => false,
    }
}

/// Decodes a dictionary-encoded column to its value type; leaves any other
/// column untouched. Called before hashing so the hasher only ever sees plain
/// value arrays.
fn decoded(column: &ArrayRef) -> Result<ArrayRef, TableDiffError> {
    match column.data_type() {
        DataType::Dictionary(_, value) => {
            arrow_cast::cast(column, value).map_err(|e| read_error(&e))
        }
        _ => Ok(column.clone()),
    }
}

/// The integer value of a boolean or any-width integer cell, folded to `i128`;
/// `None` for any other type. Booleans map to `0`/`1`, so `true` hashes like the
/// integer `1`.
fn integer_value(array: &ArrayRef, row: usize) -> Option<i128> {
    Some(match array.data_type() {
        DataType::Boolean => i128::from(array.as_boolean().value(row)),
        DataType::Int8 => i128::from(array.as_primitive::<Int8Type>().value(row)),
        DataType::Int16 => i128::from(array.as_primitive::<Int16Type>().value(row)),
        DataType::Int32 => i128::from(array.as_primitive::<Int32Type>().value(row)),
        DataType::Int64 => i128::from(array.as_primitive::<Int64Type>().value(row)),
        DataType::UInt8 => i128::from(array.as_primitive::<UInt8Type>().value(row)),
        DataType::UInt16 => i128::from(array.as_primitive::<UInt16Type>().value(row)),
        DataType::UInt32 => i128::from(array.as_primitive::<UInt32Type>().value(row)),
        DataType::UInt64 => i128::from(array.as_primitive::<UInt64Type>().value(row)),
        _ => return None,
    })
}

/// The `(value, scale)` of a decimal cell, every width widened to `i256`, so a
/// decimal of any width and scale hashes by its exact value; `None` for any
/// non-decimal type.
fn decimal_value(array: &ArrayRef, row: usize) -> Option<(i256, i8)> {
    Some(match array.data_type() {
        DataType::Decimal32(_, scale) => (
            i256::from_i128(i128::from(array.as_primitive::<Decimal32Type>().value(row))),
            *scale,
        ),
        DataType::Decimal64(_, scale) => (
            i256::from_i128(i128::from(array.as_primitive::<Decimal64Type>().value(row))),
            *scale,
        ),
        DataType::Decimal128(_, scale) => (
            i256::from_i128(array.as_primitive::<Decimal128Type>().value(row)),
            *scale,
        ),
        DataType::Decimal256(_, scale) => {
            (array.as_primitive::<Decimal256Type>().value(row), *scale)
        }
        _ => return None,
    })
}

/// Hashes one cell into `hasher`, tagged by kind so unlike kinds never collide.
/// A null writes only [`TAG_NULL`]; every other kind writes its tag then a
/// canonical form of the value (see the module docs' value semantics).
fn hash_cell(
    hasher: &mut CellHasher,
    array: &ArrayRef,
    column: &str,
    row: usize,
) -> Result<(), TableDiffError> {
    // A Null-typed column is all-null by definition and carries no validity
    // buffer, so `is_null` cannot be trusted for it; hash every row as a null.
    if matches!(array.data_type(), DataType::Null) {
        hasher.tag(TAG_NULL);
        return Ok(());
    }

    if array.is_null(row) {
        hasher.tag(TAG_NULL);
        return Ok(());
    }

    // Bool and every integer width fold to one integer form (`1`, `1.0`, `true`
    // all hash equal); decimals fold to a common exact value; both handled first
    // so the match below stays short.
    if let Some(value) = integer_value(array, row) {
        hash_int(hasher, value);
        return Ok(());
    }
    if let Some((value, scale)) = decimal_value(array, row) {
        hash_decimal(hasher, value, scale);
        return Ok(());
    }

    match array.data_type() {
        DataType::Float16 => hash_float(
            hasher,
            array.as_primitive::<Float16Type>().value(row).to_f64(),
        ),
        DataType::Float32 => hash_float(
            hasher,
            f64::from(array.as_primitive::<Float32Type>().value(row)),
        ),
        DataType::Float64 => hash_float(hasher, array.as_primitive::<Float64Type>().value(row)),
        DataType::Utf8 => hash_bytes(
            hasher,
            TAG_STR,
            array.as_string::<i32>().value(row).as_bytes(),
        ),
        DataType::LargeUtf8 => {
            hash_bytes(
                hasher,
                TAG_STR,
                array.as_string::<i64>().value(row).as_bytes(),
            );
        }
        DataType::Utf8View => hash_bytes(
            hasher,
            TAG_STR,
            array.as_string_view().value(row).as_bytes(),
        ),
        DataType::Binary => hash_bytes(hasher, TAG_BIN, array.as_binary::<i32>().value(row)),
        DataType::LargeBinary => hash_bytes(hasher, TAG_BIN, array.as_binary::<i64>().value(row)),
        DataType::BinaryView => hash_bytes(hasher, TAG_BIN, array.as_binary_view().value(row)),
        DataType::FixedSizeBinary(_) => {
            hash_bytes(hasher, TAG_BIN, array.as_fixed_size_binary().value(row));
        }
        DataType::Timestamp(unit, tz) => {
            let raw = timestamp_value(array, *unit, row);
            hash_timestamp(hasher, raw, *unit, tz.is_some());
        }
        DataType::Date32 => {
            hash_day(
                hasher,
                i64::from(array.as_primitive::<Date32Type>().value(row)),
            );
        }
        DataType::Date64 => hash_date64(hasher, array.as_primitive::<Date64Type>().value(row)),
        DataType::Time32(unit) => hash_time32(hasher, array, *unit, row),
        DataType::Time64(unit) => hash_time64(hasher, array, *unit, row),
        DataType::Duration(unit) => {
            hash_time_like(
                hasher,
                TAG_DURATION,
                time_unit_discriminant(*unit),
                duration_value(array, *unit, row),
            );
        }
        DataType::Interval(unit) => hash_interval(hasher, array, *unit, row),
        other => {
            return Err(TableDiffError::UnsupportedRowType {
                column: column.to_string(),
                data_type: other.to_string(),
            });
        }
    }

    Ok(())
}

/// Number of milliseconds in a whole day, the `Date64` unit.
const MILLIS_PER_DAY: i64 = 86_400_000;

/// Writes a whole-day date by its day count (discriminant `0`), so a `Date32`
/// and a whole-day `Date64` of the same calendar day hash equal.
fn hash_day(hasher: &mut CellHasher, days: i64) {
    hasher.tag(TAG_DATE);
    hasher.write(&[0]);
    hasher.write_i64(days);
}

/// Writes a `Date64` (milliseconds since epoch). Arrow's contract is a whole
/// number of days: a whole-day value folds to its day count (via *exact*
/// division, so it hashes equal to the matching `Date32`), and a
/// non-whole-day value — which the contract forbids but the hash must still
/// distinguish — keeps its raw millisecond value under a separate discriminant,
/// so it neither collides with a day count nor is truncated.
fn hash_date64(hasher: &mut CellHasher, ms: i64) {
    if ms % MILLIS_PER_DAY == 0 {
        hash_day(hasher, ms / MILLIS_PER_DAY);
    } else {
        hasher.tag(TAG_DATE);
        hasher.write(&[1]);
        hasher.write_i64(ms);
    }
}

/// A stable per-`TimeUnit` byte, so the same raw value at different units never
/// collides.
fn time_unit_discriminant(unit: TimeUnit) -> u8 {
    match unit {
        TimeUnit::Second => 0,
        TimeUnit::Millisecond => 1,
        TimeUnit::Microsecond => 2,
        TimeUnit::Nanosecond => 3,
    }
}

/// Writes an integer-backed time or duration by its raw value tagged with a
/// unit discriminant (raw value, not normalized across units, so a unit change
/// reads as a value change).
fn hash_time_like(hasher: &mut CellHasher, tag: u8, unit_discriminant: u8, raw: i64) {
    hasher.tag(tag);
    hasher.write(&[unit_discriminant]);
    hasher.write_i64(raw);
}

/// Hashes a `Time32` cell. Only `Second` and `Millisecond` have a `Time32`
/// array type in arrow-rs; [`is_hashable`] refuses the other two units before
/// any row is read, so those arms are unreachable.
fn hash_time32(hasher: &mut CellHasher, array: &ArrayRef, unit: TimeUnit, row: usize) {
    let raw = match unit {
        TimeUnit::Second => i64::from(array.as_primitive::<Time32SecondType>().value(row)),
        TimeUnit::Millisecond => {
            i64::from(array.as_primitive::<Time32MillisecondType>().value(row))
        }
        TimeUnit::Microsecond | TimeUnit::Nanosecond => {
            unreachable!("is_hashable refuses this Time32 unit before any row is read")
        }
    };
    hash_time_like(hasher, TAG_TIME, time_unit_discriminant(unit), raw);
}

/// Hashes a `Time64` cell. Only `Microsecond` and `Nanosecond` have a `Time64`
/// array type in arrow-rs; [`is_hashable`] refuses the other two units before
/// any row is read, so those arms are unreachable.
fn hash_time64(hasher: &mut CellHasher, array: &ArrayRef, unit: TimeUnit, row: usize) {
    let raw = match unit {
        TimeUnit::Microsecond => array.as_primitive::<Time64MicrosecondType>().value(row),
        TimeUnit::Nanosecond => array.as_primitive::<Time64NanosecondType>().value(row),
        TimeUnit::Second | TimeUnit::Millisecond => {
            unreachable!("is_hashable refuses this Time64 unit before any row is read")
        }
    };
    hash_time_like(hasher, TAG_TIME, time_unit_discriminant(unit), raw);
}

/// Reads a `Duration` cell's raw integer in `unit`.
fn duration_value(array: &ArrayRef, unit: TimeUnit, row: usize) -> i64 {
    match unit {
        TimeUnit::Second => array.as_primitive::<DurationSecondType>().value(row),
        TimeUnit::Millisecond => array.as_primitive::<DurationMillisecondType>().value(row),
        TimeUnit::Microsecond => array.as_primitive::<DurationMicrosecondType>().value(row),
        TimeUnit::Nanosecond => array.as_primitive::<DurationNanosecondType>().value(row),
    }
}

/// Writes an interval by its exact per-variant fields, tagged with the variant
/// so the three interval kinds never collide.
fn hash_interval(hasher: &mut CellHasher, array: &ArrayRef, unit: IntervalUnit, row: usize) {
    hasher.tag(TAG_INTERVAL);
    match unit {
        IntervalUnit::YearMonth => {
            hasher.write(&[0]);
            hasher.write(
                &array
                    .as_primitive::<IntervalYearMonthType>()
                    .value(row)
                    .to_le_bytes(),
            );
        }
        IntervalUnit::DayTime => {
            hasher.write(&[1]);
            let v = array.as_primitive::<IntervalDayTimeType>().value(row);
            hasher.write(&v.days.to_le_bytes());
            hasher.write(&v.milliseconds.to_le_bytes());
        }
        IntervalUnit::MonthDayNano => {
            hasher.write(&[2]);
            let v = array.as_primitive::<IntervalMonthDayNanoType>().value(row);
            hasher.write(&v.months.to_le_bytes());
            hasher.write(&v.days.to_le_bytes());
            hasher.write(&v.nanoseconds.to_le_bytes());
        }
    }
}

/// Reads a timestamp cell as its raw integer in `unit`, regardless of the
/// concrete timestamp array type.
fn timestamp_value(array: &ArrayRef, unit: TimeUnit, row: usize) -> i64 {
    match unit {
        TimeUnit::Second => array.as_primitive::<TimestampSecondType>().value(row),
        TimeUnit::Millisecond => array.as_primitive::<TimestampMillisecondType>().value(row),
        TimeUnit::Microsecond => array.as_primitive::<TimestampMicrosecondType>().value(row),
        TimeUnit::Nanosecond => array.as_primitive::<TimestampNanosecondType>().value(row),
    }
}

/// Writes an integer in the shared integer form: `1`, `1.0`, and a
/// dictionary-encoded `1` all reach this.
fn hash_int(hasher: &mut CellHasher, value: i128) {
    hasher.tag(TAG_INT);
    hasher.write_i128(value);
}

/// Writes a float using the same exact-integral fold `scalar_key` applies in
/// `crates/onix-core/src/lcs.rs` (its doc carries the full rationale; the
/// predicate and the `±2⁵³` bound are duplicated here on purpose because the
/// crates are decoupled, and both sites move together): an integral value in
/// range folds to the integer form (so `-0.0` and `0.0` both become `0`);
/// anything else keeps its raw bit pattern, so two NaNs hash equal only when
/// bit-identical.
fn hash_float(hasher: &mut CellHasher, value: f64) {
    if value.fract() == 0.0 && value.abs() <= MAX_EXACT_F64_INT {
        // `fract() == 0.0` and the magnitude bound guarantee an exact cast.
        #[allow(clippy::cast_possible_truncation)]
        hash_int(hasher, value as i128);
    } else {
        hasher.tag(TAG_FLOAT);
        hasher.write_u64(value.to_bits());
    }
}

/// Writes a decimal by its exact value with trailing decimal zeros removed, so
/// `1.00` (scale 2) and `1.0000` (scale 4) hash equal. `Decimal128` cells are
/// widened to `i256` first, so a 128-bit and a 256-bit decimal of the same value
/// hash equal.
fn hash_decimal(hasher: &mut CellHasher, mut value: i256, scale: i8) {
    let ten = i256::from_i128(10);
    let mut scale = i32::from(scale);
    if value == i256::ZERO {
        scale = 0;
    } else {
        while value.wrapping_rem(ten) == i256::ZERO {
            value = value.wrapping_div(ten);
            scale -= 1;
        }
    }
    hasher.tag(TAG_DECIMAL);
    hasher.write(&value.to_le_bytes());
    hasher.write(&scale.to_le_bytes());
}

/// Writes a byte string (UTF-8 or binary) under the given tag.
fn hash_bytes(hasher: &mut CellHasher, tag: u8, bytes: &[u8]) {
    hasher.tag(tag);
    hasher.write_u64(bytes.len() as u64);
    hasher.write(bytes);
}

/// Writes a timestamp as its UTC instant in nanoseconds plus a flag for whether
/// it carries a timezone, so the same instant at different precisions hashes
/// equal while a naive timestamp stays distinct from an aware one. This mirrors
/// `onix-core`'s `ScalarKey::DateTime { aware, instant }` in
/// `crates/onix-core/src/lcs.rs` (instant plus an aware flag); the two are kept
/// consistent by hand, the crates being decoupled by design.
fn hash_timestamp(hasher: &mut CellHasher, raw: i64, unit: TimeUnit, has_tz: bool) {
    let nanos = i128::from(raw)
        * match unit {
            TimeUnit::Second => 1_000_000_000,
            TimeUnit::Millisecond => 1_000_000,
            TimeUnit::Microsecond => 1_000,
            TimeUnit::Nanosecond => 1,
        };
    hasher.tag(TAG_TS);
    hasher.write(&[u8::from(has_tz)]);
    hasher.write_i128(nanos);
}

/// Hashes one row's selected columns under a domain tag into a single 128-bit
/// value.
fn hash_row(
    hasher: &RowHasher,
    domain: u8,
    arrays: &[ArrayRef],
    names: &[&str],
    row: usize,
) -> Result<u128, TableDiffError> {
    let mut dual = hasher.start(domain);
    for (array, name) in arrays.iter().zip(names) {
        hash_cell(&mut dual, array, name, row)?;
    }
    Ok(dual.finish())
}

/// Prepares (decodes dictionaries in) the selected columns of a batch for
/// hashing.
fn prepared_columns(
    batch: &RecordBatch,
    indices: &[usize],
) -> Result<Vec<ArrayRef>, TableDiffError> {
    indices.iter().map(|&i| decoded(batch.column(i))).collect()
}

/// One side's per-row hashes, produced by streaming the reader once.
struct SidePass {
    entries: Vec<(u128, u128)>,
    null_keys: HashSet<u128>,
}

/// Streams one reader once: hashes every row's key and non-key columns and
/// records which keys carry a null component. Only the fixed-size hashes are
/// retained, so memory grows with the row count, not the data size.
fn hash_side(
    reader: Box<dyn RecordBatchReader + Send>,
    columns: &SideColumns,
    key_names: &[&str],
    value_names: &[&str],
    hasher: &RowHasher,
) -> Result<SidePass, TableDiffError> {
    let mut entries = Vec::new();
    let mut null_keys = HashSet::new();

    for batch in reader {
        let batch = batch.map_err(|e| read_error(&e))?;

        let key_arrays = prepared_columns(&batch, &columns.key)?;
        let value_arrays = prepared_columns(&batch, &columns.value)?;

        for row in 0..batch.num_rows() {
            let key_hash = hash_row(hasher, DOMAIN_KEY, &key_arrays, key_names, row)?;
            let row_hash = hash_row(hasher, DOMAIN_ROW, &value_arrays, value_names, row)?;
            if key_arrays.iter().any(|array| array.is_null(row)) {
                null_keys.insert(key_hash);
            }
            entries.push((key_hash, row_hash));
        }
    }

    Ok(SidePass { entries, null_keys })
}

/// The classification of every key from the two sorted hash vectors.
struct Classified {
    added: HashSet<u128>,
    removed: HashSet<u128>,
    changed: Vec<u128>,
    /// Duplicate key hash -> (`left_count`, `right_count`).
    duplicates: HashMap<u128, (usize, usize)>,
}

/// Merge-joins the two sorted `(key_hash, row_hash)` vectors and classifies
/// each distinct key into added / removed / changed / unchanged / duplicate.
fn classify(mut left: Vec<(u128, u128)>, mut right: Vec<(u128, u128)>) -> Classified {
    left.sort_unstable_by_key(|&(key, _)| key);
    right.sort_unstable_by_key(|&(key, _)| key);

    let mut result = Classified {
        added: HashSet::new(),
        removed: HashSet::new(),
        changed: Vec::new(),
        duplicates: HashMap::new(),
    };

    let mut li = 0;
    let mut ri = 0;
    loop {
        // Both cursors exhausted ends the merge; otherwise the next key is the
        // smaller of the two cursors' keys (or the only remaining one).
        let key = match (left.get(li), right.get(ri)) {
            (Some(&(lk, _)), Some(&(rk, _))) => lk.min(rk),
            (Some(&(lk, _)), None) => lk,
            (None, Some(&(rk, _))) => rk,
            (None, None) => break,
        };

        let l_start = li;
        while li < left.len() && left[li].0 == key {
            li += 1;
        }
        let r_start = ri;
        while ri < right.len() && right[ri].0 == key {
            ri += 1;
        }

        let left_count = li - l_start;
        let right_count = ri - r_start;

        if left_count > 1 || right_count > 1 {
            result.duplicates.insert(key, (left_count, right_count));
        } else if left_count == 1 && right_count == 0 {
            result.removed.insert(key);
        } else if left_count == 0 && right_count == 1 {
            result.added.insert(key);
        } else if left[l_start].1 != right[r_start].1 {
            result.changed.push(key);
        }
    }

    result
}

/// A batch of captured key columns, in duplicate-report order, with the key
/// hashes captured so the per-side counts can be attached afterwards.
struct DupCapture {
    key_batches: Vec<RecordBatch>,
    order: Vec<u128>,
}

/// The read-only context shared by both materialize-pass calls: how to find and
/// hash the key columns, and the shape of the duplicate-key report.
struct Materialize<'a> {
    columns: &'a SideColumns,
    key_names: &'a [&'a str],
    hasher: &'a RowHasher,
    /// The key columns alone (no count columns): the shape of one captured
    /// duplicate-key row.
    key_only_schema: &'a SchemaRef,
    key_output_types: &'a [DataType],
}

/// Re-reads one side and, in a single scan, filters it to the rows whose key is
/// in `select` and captures one key-columns row per still-pending duplicate key.
fn materialize_side(
    source: &impl TableInput,
    ctx: &Materialize<'_>,
    select: &HashSet<u128>,
    pending_dups: &mut HashSet<u128>,
) -> Result<(RecordBatch, DupCapture), TableDiffError> {
    let full_schema = source.schema();
    let reader = source.open()?;

    let mut selected_batches = Vec::new();
    let mut capture = DupCapture {
        key_batches: Vec::new(),
        order: Vec::new(),
    };

    for batch in reader {
        let batch = batch.map_err(|e| read_error(&e))?;
        let key_arrays = prepared_columns(&batch, &ctx.columns.key)?;

        let mut select_mask = Vec::with_capacity(batch.num_rows());
        let mut dup_mask = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            let key_hash = hash_row(ctx.hasher, DOMAIN_KEY, &key_arrays, ctx.key_names, row)?;
            select_mask.push(select.contains(&key_hash));
            // `remove` returns true only the first time a given duplicate key is
            // seen, so exactly one row per duplicate key is captured.
            let capture_this = pending_dups.remove(&key_hash);
            dup_mask.push(capture_this);
            if capture_this {
                capture.order.push(key_hash);
            }
        }

        let select_predicate = BooleanArray::from(select_mask);
        let selected = arrow_select::filter::filter_record_batch(&batch, &select_predicate)
            .map_err(|e| read_error(&e))?;
        if selected.num_rows() > 0 {
            selected_batches.push(selected);
        }

        if dup_mask.iter().any(|&keep| keep) {
            let dup_predicate = BooleanArray::from(dup_mask);
            let key_batch = dup_key_batch(&batch, ctx, &dup_predicate)?;
            capture.key_batches.push(key_batch);
        }
    }

    let selected = concat_or_empty(&full_schema, &selected_batches)?;
    Ok((selected, capture))
}

/// Builds the key-columns-only batch for the duplicate rows selected by
/// `predicate`, decoding dictionaries and casting to the shared output key
/// types.
fn dup_key_batch(
    batch: &RecordBatch,
    ctx: &Materialize<'_>,
    predicate: &BooleanArray,
) -> Result<RecordBatch, TableDiffError> {
    let mut key_columns = Vec::with_capacity(ctx.columns.key.len());
    for (position, &index) in ctx.columns.key.iter().enumerate() {
        let decoded_column = decoded(batch.column(index))?;
        let filtered =
            arrow_select::filter::filter(&decoded_column, predicate).map_err(|e| read_error(&e))?;
        let cast = arrow_cast::cast(&filtered, &ctx.key_output_types[position])
            .map_err(|e| read_error(&e))?;
        key_columns.push(cast);
    }
    RecordBatch::try_new(ctx.key_only_schema.clone(), key_columns).map_err(|e| read_error(&e))
}

/// Concatenates batches into one, or returns an empty batch of `schema` when
/// there are none.
fn concat_or_empty(
    schema: &SchemaRef,
    batches: &[RecordBatch],
) -> Result<RecordBatch, TableDiffError> {
    if batches.is_empty() {
        return Ok(RecordBatch::new_empty(schema.clone()));
    }
    arrow_select::concat::concat_batches(schema, batches).map_err(|e| read_error(&e))
}

/// The output schema of the duplicate-key report: each key column (decoded to
/// its value type, nullable) followed by `left_count` and `right_count`.
fn dup_key_schema(left: &Schema, key: &[String]) -> (SchemaRef, Vec<DataType>) {
    let mut fields = Vec::with_capacity(key.len() + 2);
    let mut output_types = Vec::with_capacity(key.len());
    for name in key {
        // The key exists on the left (checked by `diff_tables`), so this
        // lookup succeeds on the reachable path.
        let data_type = left
            .index_of(name)
            .ok()
            .map_or(DataType::Null, |i| decoded_type(left.field(i).data_type()));
        fields.push(Field::new(name, data_type.clone(), true));
        output_types.push(data_type);
    }
    fields.push(Field::new("left_count", DataType::Int64, false));
    fields.push(Field::new("right_count", DataType::Int64, false));
    (SchemaRef::new(Schema::new(fields)), output_types)
}

/// A dictionary type's value type; any other type unchanged. The duplicate
/// report shows a key column by value, never by its physical encoding.
fn decoded_type(data_type: &DataType) -> DataType {
    match data_type {
        DataType::Dictionary(_, value) => value.as_ref().clone(),
        other => other.clone(),
    }
}

/// Assembles the duplicate-key report: the captured key columns from both
/// sides, then the per-side counts in the same order.
fn build_duplicate_keys(
    dup_key_schema: &SchemaRef,
    left_capture: DupCapture,
    right_capture: DupCapture,
    duplicates: &HashMap<u128, (usize, usize)>,
) -> Result<RecordBatch, TableDiffError> {
    let key_field_count = dup_key_schema.fields().len() - 2;
    let key_only_schema = SchemaRef::new(Schema::new(
        dup_key_schema.fields()[..key_field_count].to_vec(),
    ));

    let mut key_batches = left_capture.key_batches;
    key_batches.extend(right_capture.key_batches);
    let key_columns = concat_or_empty(&key_only_schema, &key_batches)?;

    let mut order = left_capture.order;
    order.extend(right_capture.order);
    let left_counts: Int64Array = order
        .iter()
        .map(|key| i64::try_from(duplicates[key].0).unwrap_or(i64::MAX))
        .collect();
    let right_counts: Int64Array = order
        .iter()
        .map(|key| i64::try_from(duplicates[key].1).unwrap_or(i64::MAX))
        .collect();

    let mut columns: Vec<ArrayRef> = key_columns.columns().to_vec();
    columns.push(std::sync::Arc::new(left_counts));
    columns.push(std::sync::Arc::new(right_counts));
    RecordBatch::try_new(dup_key_schema.clone(), columns).map_err(|e| read_error(&e))
}

/// Diffs the rows of two tables matched by `key`. See the module docs for the
/// algorithm and value semantics.
pub(crate) fn diff_rows(
    left: &impl TableInput,
    right: &impl TableInput,
    left_schema: &Schema,
    right_schema: &Schema,
    key: &[String],
) -> Result<RowDiff, TableDiffError> {
    let hasher = RowHasher::new()?;

    // Refuse every non-nested-but-unhashable column of *either* full schema up
    // front (a `RunEndEncoded`, or a `Time32`/`Time64` unit or `FixedSizeBinary`
    // width arrow-rs has no array type for), including a column present on only
    // one side: such a column reaches `RecordBatch::new_empty` through the
    // added/removed output even when it is never compared, and building its
    // empty array would panic. Nested columns are fine (their empty array
    // builds), so only non-nested columns are checked here.
    reject_unhashable_columns(left_schema)?;
    reject_unhashable_columns(right_schema)?;

    let common_values = common_value_columns(left_schema, right_schema, key);
    let key_names: Vec<&str> = key.iter().map(String::as_str).collect();
    let value_names: Vec<&str> = common_values.iter().map(String::as_str).collect();

    let left_columns = side_columns(left_schema, key, &common_values);
    let right_columns = side_columns(right_schema, key, &common_values);

    let left_pass = hash_side(
        left.open()?,
        &left_columns,
        &key_names,
        &value_names,
        &hasher,
    )?;
    let right_pass = hash_side(
        right.open()?,
        &right_columns,
        &key_names,
        &value_names,
        &hasher,
    )?;

    let mut null_keys = left_pass.null_keys;
    null_keys.extend(&right_pass.null_keys);
    let null_key_count = null_keys.len();

    let classified = classify(left_pass.entries, right_pass.entries);

    let (dup_schema, key_output_types) = dup_key_schema(left_schema, key);
    let key_only_schema = SchemaRef::new(Schema::new(dup_schema.fields()[..key.len()].to_vec()));
    let mut pending_dups: HashSet<u128> = classified.duplicates.keys().copied().collect();

    let left_ctx = Materialize {
        columns: &left_columns,
        key_names: &key_names,
        hasher: &hasher,
        key_only_schema: &key_only_schema,
        key_output_types: &key_output_types,
    };
    let right_ctx = Materialize {
        columns: &right_columns,
        key_names: &key_names,
        hasher: &hasher,
        key_only_schema: &key_only_schema,
        key_output_types: &key_output_types,
    };

    let (rows_removed, left_dup_capture) =
        materialize_side(left, &left_ctx, &classified.removed, &mut pending_dups)?;
    let (rows_added, right_dup_capture) =
        materialize_side(right, &right_ctx, &classified.added, &mut pending_dups)?;

    let duplicate_keys = build_duplicate_keys(
        &dup_schema,
        left_dup_capture,
        right_dup_capture,
        &classified.duplicates,
    )?;

    let counts = RowCounts {
        rows_added: classified.added.len(),
        rows_removed: classified.removed.len(),
        rows_changed: classified.changed.len(),
        duplicate_keys: classified.duplicates.len(),
        null_keys: null_key_count,
    };

    Ok(RowDiff {
        rows_added,
        rows_removed,
        duplicate_keys,
        changed_keys: classified.changed,
        counts,
    })
}

#[cfg(test)]
mod tests {
    use super::{MemoryInput, RowDiff, TableInput, diff_rows};
    use crate::error::TableDiffError;
    use arrow_array::cast::AsArray;
    use arrow_array::types::Int64Type;
    use arrow_array::{
        Array, ArrayRef, BinaryArray, BinaryViewArray, Date32Array, Date64Array, Decimal32Array,
        Decimal64Array, Decimal128Array, Decimal256Array, DurationNanosecondArray,
        DurationSecondArray, Float64Array, Int32Array, Int64Array, IntervalDayTimeArray,
        IntervalMonthDayNanoArray, IntervalYearMonthArray, ListArray, NullArray, RecordBatch,
        RecordBatchReader, StringArray, Time32MillisecondArray, Time32SecondArray,
        Time64MicrosecondArray, Time64NanosecondArray, TimestampMicrosecondArray,
        TimestampMillisecondArray,
    };
    use arrow_buffer::{IntervalDayTime, IntervalMonthDayNano, i256};
    use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef, TimeUnit};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    /// A one-batch input over the given schema and columns.
    fn reader(schema: &SchemaRef, columns: Vec<ArrayRef>) -> MemoryInput {
        let batch = RecordBatch::try_new(schema.clone(), columns).unwrap();
        MemoryInput::new(schema.clone(), vec![batch])
    }

    /// An input over several batches sharing one schema.
    fn multi_reader(schema: &SchemaRef, batches: Vec<RecordBatch>) -> MemoryInput {
        MemoryInput::new(schema.clone(), batches)
    }

    /// A [`TableInput`] whose reader fails on its first batch, to exercise the
    /// read-error path.
    struct FailingInput {
        schema: SchemaRef,
    }

    impl TableInput for FailingInput {
        fn schema(&self) -> SchemaRef {
            self.schema.clone()
        }

        fn open(&self) -> Result<Box<dyn RecordBatchReader + Send>, TableDiffError> {
            let batches: Vec<Result<RecordBatch, ArrowError>> =
                vec![Err(ArrowError::ComputeError("boom".to_string()))];
            Ok(Box::new(arrow_array::RecordBatchIterator::new(
                batches.into_iter(),
                self.schema.clone(),
            )))
        }
    }

    fn schema(fields: Vec<Field>) -> SchemaRef {
        Arc::new(Schema::new(fields))
    }

    fn id_field() -> Field {
        Field::new("id", DataType::Int64, true)
    }

    /// The `id` column of a result batch as nullable values.
    fn ids(batch: &RecordBatch) -> BTreeSet<Option<i64>> {
        let column = batch
            .column_by_name("id")
            .unwrap()
            .as_primitive::<Int64Type>();
        (0..batch.num_rows())
            .map(|i| (!column.is_null(i)).then(|| column.value(i)))
            .collect()
    }

    fn key() -> Vec<String> {
        vec!["id".to_string()]
    }

    /// Runs a diff of two `(id, v)` int tables and returns the result.
    fn diff_int_tables(
        left_ids: Vec<Option<i64>>,
        left_v: Vec<i64>,
        right_ids: Vec<Option<i64>>,
        right_v: Vec<i64>,
    ) -> RowDiff {
        let sch = schema(vec![id_field(), Field::new("v", DataType::Int64, false)]);
        let left = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(left_ids)),
                Arc::new(Int64Array::from(left_v)),
            ],
        );
        let right = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(right_ids)),
                Arc::new(Int64Array::from(right_v)),
            ],
        );
        diff_rows(&left, &right, &sch, &sch, &key()).unwrap()
    }

    #[test]
    fn added_removed_changed_unchanged() {
        // left ids 1,2,3 (v 10,20,30); right ids 2,3,4 (v 20,31,40).
        // 1 removed, 4 added, 3 changed (30 -> 31), 2 unchanged.
        let diff = diff_int_tables(
            vec![Some(1), Some(2), Some(3)],
            vec![10, 20, 30],
            vec![Some(2), Some(3), Some(4)],
            vec![20, 31, 40],
        );
        assert_eq!(diff.counts.rows_added, 1);
        assert_eq!(diff.counts.rows_removed, 1);
        assert_eq!(diff.counts.rows_changed, 1);
        assert_eq!(diff.counts.duplicate_keys, 0);
        assert_eq!(ids(&diff.rows_added), BTreeSet::from([Some(4)]));
        assert_eq!(ids(&diff.rows_removed), BTreeSet::from([Some(1)]));
        assert_eq!(diff.changed_keys.len(), 1);
    }

    #[test]
    fn identical_tables_have_no_row_changes() {
        let diff = diff_int_tables(
            vec![Some(1), Some(2)],
            vec![10, 20],
            vec![Some(1), Some(2)],
            vec![10, 20],
        );
        assert_eq!(diff.counts.rows_added, 0);
        assert_eq!(diff.counts.rows_removed, 0);
        assert_eq!(diff.counts.rows_changed, 0);
        assert_eq!(diff.rows_added.num_rows(), 0);
        assert_eq!(diff.rows_removed.num_rows(), 0);
    }

    #[test]
    fn duplicate_key_on_left_only_is_reported_and_excluded() {
        // left ids 1,1,2; right ids 2,3. Key 1 is a left duplicate (2,0).
        let diff = diff_int_tables(
            vec![Some(1), Some(1), Some(2)],
            vec![10, 11, 20],
            vec![Some(2), Some(3)],
            vec![20, 30],
        );
        assert_eq!(diff.counts.duplicate_keys, 1);
        assert_eq!(
            diff.counts.rows_removed, 0,
            "the duplicate key is not removed"
        );
        assert_eq!(diff.counts.rows_added, 1);
        assert_eq!(ids(&diff.rows_added), BTreeSet::from([Some(3)]));

        let dup = &diff.duplicate_keys;
        assert_eq!(dup.num_rows(), 1);
        assert_eq!(ids(dup), BTreeSet::from([Some(1)]));
        let left_count = dup
            .column_by_name("left_count")
            .unwrap()
            .as_primitive::<Int64Type>();
        let right_count = dup
            .column_by_name("right_count")
            .unwrap()
            .as_primitive::<Int64Type>();
        assert_eq!(left_count.value(0), 2);
        assert_eq!(right_count.value(0), 0);
    }

    #[test]
    fn duplicate_key_on_right_only_is_captured_from_the_right() {
        // left id 1; right ids 2,2. Key 2 is a right-only duplicate (0,2).
        let diff = diff_int_tables(
            vec![Some(1)],
            vec![10],
            vec![Some(2), Some(2)],
            vec![20, 21],
        );
        assert_eq!(diff.counts.duplicate_keys, 1);
        assert_eq!(diff.counts.rows_added, 0, "the duplicate key is not added");
        assert_eq!(diff.counts.rows_removed, 1);

        let dup = &diff.duplicate_keys;
        assert_eq!(ids(dup), BTreeSet::from([Some(2)]));
        let left_count = dup
            .column_by_name("left_count")
            .unwrap()
            .as_primitive::<Int64Type>();
        let right_count = dup
            .column_by_name("right_count")
            .unwrap()
            .as_primitive::<Int64Type>();
        assert_eq!(left_count.value(0), 0);
        assert_eq!(right_count.value(0), 2);
    }

    #[test]
    fn duplicate_key_on_both_sides_combines_counts() {
        let diff = diff_int_tables(
            vec![Some(1), Some(1)],
            vec![10, 11],
            vec![Some(1), Some(1), Some(1)],
            vec![10, 11, 12],
        );
        assert_eq!(diff.counts.duplicate_keys, 1);
        let dup = &diff.duplicate_keys;
        let left_count = dup
            .column_by_name("left_count")
            .unwrap()
            .as_primitive::<Int64Type>();
        let right_count = dup
            .column_by_name("right_count")
            .unwrap()
            .as_primitive::<Int64Type>();
        assert_eq!(left_count.value(0), 2);
        assert_eq!(right_count.value(0), 3);
    }

    #[test]
    fn null_key_matches_itself_and_is_counted() {
        // Both sides have a null-keyed row; its v differs, so it is one changed
        // row, and the null key is counted once.
        let diff = diff_int_tables(
            vec![None, Some(1)],
            vec![10, 20],
            vec![None, Some(1)],
            vec![11, 20],
        );
        assert_eq!(diff.counts.null_keys, 1);
        assert_eq!(diff.counts.rows_changed, 1);
        assert_eq!(diff.counts.rows_added, 0);
        assert_eq!(diff.counts.rows_removed, 0);
    }

    #[test]
    fn null_key_present_on_one_side_is_removed() {
        let diff = diff_int_tables(vec![None], vec![10], vec![Some(1)], vec![20]);
        assert_eq!(diff.counts.null_keys, 1);
        assert_eq!(diff.counts.rows_removed, 1);
        assert_eq!(diff.counts.rows_added, 1);
        assert_eq!(ids(&diff.rows_removed), BTreeSet::from([None]));
    }

    #[test]
    fn int_width_change_with_equal_value_is_unchanged() {
        // Left v is Int32, right v is Int64; the same numeric value is unchanged.
        let left_schema = schema(vec![id_field(), Field::new("v", DataType::Int32, false)]);
        let right_schema = schema(vec![id_field(), Field::new("v", DataType::Int64, false)]);
        let left = reader(
            &left_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Int32Array::from(vec![5])),
            ],
        );
        let right = reader(
            &right_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Int64Array::from(vec![5])),
            ],
        );
        let diff = diff_rows(&left, &right, &left_schema, &right_schema, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn signed_zero_floats_are_equal() {
        let sch = schema(vec![id_field(), Field::new("v", DataType::Float64, false)]);
        let left = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Float64Array::from(vec![0.0])),
            ],
        );
        let right = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Float64Array::from(vec![-0.0])),
            ],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn distinct_floats_are_a_change() {
        let sch = schema(vec![id_field(), Field::new("v", DataType::Float64, false)]);
        let left = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Float64Array::from(vec![1.5])),
            ],
        );
        let right = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Float64Array::from(vec![2.5])),
            ],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 1);
    }

    #[test]
    fn decimal_scale_difference_with_equal_value_is_unchanged() {
        let left_schema = schema(vec![
            id_field(),
            Field::new("v", DataType::Decimal128(10, 2), false),
        ]);
        let right_schema = schema(vec![
            id_field(),
            Field::new("v", DataType::Decimal128(10, 4), false),
        ]);
        // 1.00 (scale 2) == 1.0000 (scale 4).
        let left = reader(
            &left_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(
                    Decimal128Array::from(vec![100])
                        .with_precision_and_scale(10, 2)
                        .unwrap(),
                ),
            ],
        );
        let right = reader(
            &right_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(
                    Decimal128Array::from(vec![10_000])
                        .with_precision_and_scale(10, 4)
                        .unwrap(),
                ),
            ],
        );
        let diff = diff_rows(&left, &right, &left_schema, &right_schema, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn timestamp_unit_difference_at_same_instant_is_unchanged() {
        let us = DataType::Timestamp(arrow_schema::TimeUnit::Microsecond, Some("UTC".into()));
        let ms = DataType::Timestamp(arrow_schema::TimeUnit::Millisecond, Some("UTC".into()));
        let left_schema = schema(vec![id_field(), Field::new("t", us, false)]);
        let right_schema = schema(vec![id_field(), Field::new("t", ms, false)]);
        // 2_000_000 us == 2_000 ms == the same instant.
        let left = reader(
            &left_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(TimestampMicrosecondArray::from(vec![2_000_000]).with_timezone("UTC")),
            ],
        );
        let right = reader(
            &right_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(TimestampMillisecondArray::from(vec![2_000]).with_timezone("UTC")),
            ],
        );
        let diff = diff_rows(&left, &right, &left_schema, &right_schema, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn date32_and_date64_same_day_are_unchanged() {
        // A Date32 and a whole-day Date64 of the same calendar day compare equal
        // (day 3 == 3 * 86_400_000 ms), so a schema migration between the two
        // types is not reported as a row change.
        let left_schema = schema(vec![id_field(), Field::new("d", DataType::Date32, false)]);
        let right_schema = schema(vec![id_field(), Field::new("d", DataType::Date64, false)]);
        let left = reader(
            &left_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Date32Array::from(vec![3])),
            ],
        );
        let right = reader(
            &right_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Date64Array::from(vec![3 * 86_400_000])),
            ],
        );
        let diff = diff_rows(&left, &right, &left_schema, &right_schema, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn date64_distinguishes_distinct_and_non_whole_day_values() {
        // Equal values are unchanged; a whole-day value and a sub-day value
        // within the same calendar day (which Arrow's whole-day contract forbids
        // but the hash still distinguishes) are a change.
        let sch = schema(vec![id_field(), Field::new("d", DataType::Date64, false)]);
        let with = |v: i64| {
            reader(
                &sch,
                vec![
                    Arc::new(Int64Array::from(vec![Some(1)])),
                    Arc::new(Date64Array::from(vec![v])),
                ],
            )
        };
        assert_eq!(
            diff_rows(&with(0), &with(0), &sch, &sch, &key())
                .unwrap()
                .counts
                .rows_changed,
            0
        );
        assert_eq!(
            diff_rows(&with(0), &with(86_399_999), &sch, &sch, &key())
                .unwrap()
                .counts
                .rows_changed,
            1
        );
    }

    /// Diffs two one-row tables whose single non-key column `v` differs, and
    /// asserts the row is reported changed — proving the type is hashed, not
    /// skipped.
    fn assert_value_change_detected(value_type: DataType, left: ArrayRef, right: ArrayRef) {
        let sch = schema(vec![id_field(), Field::new("v", value_type, false)]);
        let left = reader(&sch, vec![Arc::new(Int64Array::from(vec![Some(1)])), left]);
        let right = reader(&sch, vec![Arc::new(Int64Array::from(vec![Some(1)])), right]);
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 1);
    }

    #[test]
    fn time32_value_change_is_detected() {
        assert_value_change_detected(
            DataType::Time32(TimeUnit::Second),
            Arc::new(Time32SecondArray::from(vec![1])),
            Arc::new(Time32SecondArray::from(vec![2])),
        );
    }

    #[test]
    fn time64_value_change_is_detected() {
        assert_value_change_detected(
            DataType::Time64(TimeUnit::Microsecond),
            Arc::new(Time64MicrosecondArray::from(vec![1])),
            Arc::new(Time64MicrosecondArray::from(vec![2])),
        );
    }

    #[test]
    fn duration_value_change_is_detected() {
        assert_value_change_detected(
            DataType::Duration(TimeUnit::Second),
            Arc::new(DurationSecondArray::from(vec![1])),
            Arc::new(DurationSecondArray::from(vec![2])),
        );
    }

    #[test]
    fn interval_value_change_is_detected() {
        assert_value_change_detected(
            DataType::Interval(arrow_schema::IntervalUnit::YearMonth),
            Arc::new(IntervalYearMonthArray::from(vec![1])),
            Arc::new(IntervalYearMonthArray::from(vec![2])),
        );
    }

    #[test]
    fn decimal32_value_change_is_detected() {
        let d = |v: i32| {
            Decimal32Array::from(vec![v])
                .with_precision_and_scale(8, 2)
                .unwrap()
        };
        assert_value_change_detected(
            DataType::Decimal32(8, 2),
            Arc::new(d(100)),
            Arc::new(d(200)),
        );
    }

    #[test]
    fn decimal64_value_change_is_detected() {
        let d = |v: i64| {
            Decimal64Array::from(vec![v])
                .with_precision_and_scale(16, 2)
                .unwrap()
        };
        assert_value_change_detected(
            DataType::Decimal64(16, 2),
            Arc::new(d(100)),
            Arc::new(d(200)),
        );
    }

    #[test]
    fn null_column_hashes_and_is_unchanged() {
        // An all-Null column is hashable (every row a null), so two such columns
        // compare unchanged rather than being refused.
        let sch = schema(vec![id_field(), Field::new("n", DataType::Null, true)]);
        let make = || {
            reader(
                &sch,
                vec![
                    Arc::new(Int64Array::from(vec![Some(1)])),
                    Arc::new(NullArray::new(1)) as ArrayRef,
                ],
            )
        };
        let diff = diff_rows(&make(), &make(), &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn decimal256_value_change_is_detected() {
        let d = |v: i128| {
            Decimal256Array::from(vec![i256::from_i128(v)])
                .with_precision_and_scale(40, 2)
                .unwrap()
        };
        assert_value_change_detected(
            DataType::Decimal256(40, 2),
            Arc::new(d(100)),
            Arc::new(d(200)),
        );
    }

    #[test]
    fn decimal256_equal_value_across_scales_is_unchanged() {
        let left_schema = schema(vec![
            id_field(),
            Field::new("v", DataType::Decimal256(40, 2), false),
        ]);
        let right_schema = schema(vec![
            id_field(),
            Field::new("v", DataType::Decimal256(40, 4), false),
        ]);
        let left = reader(
            &left_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(
                    Decimal256Array::from(vec![i256::from_i128(100)])
                        .with_precision_and_scale(40, 2)
                        .unwrap(),
                ),
            ],
        );
        let right = reader(
            &right_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(
                    Decimal256Array::from(vec![i256::from_i128(10_000)])
                        .with_precision_and_scale(40, 4)
                        .unwrap(),
                ),
            ],
        );
        // Value 1.00 == 1.0000; the column is not the key, so the scale change is
        // a schema change but the row is unchanged.
        let diff = diff_rows(&left, &right, &left_schema, &right_schema, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn run_end_encoded_non_key_column_is_refused() {
        use arrow_array::RunArray;
        let run_ends = Int32Array::from(vec![1]);
        let values = Int64Array::from(vec![10]);
        let run: RunArray<arrow_array::types::Int32Type> =
            RunArray::try_new(&run_ends, &values).unwrap();
        let sch = schema(vec![
            id_field(),
            Field::new("v", run.data_type().clone(), true),
        ]);
        let make = || {
            reader(
                &sch,
                vec![
                    Arc::new(Int64Array::from(vec![Some(1)])),
                    Arc::new(run.clone()),
                ],
            )
        };
        let error = diff_rows(&make(), &make(), &sch, &sch, &key()).unwrap_err();
        assert!(
            matches!(error, TableDiffError::UnsupportedRowType { column, .. } if column == "v")
        );
    }

    #[test]
    fn adjacent_string_cells_are_not_confused_by_framing() {
        // Pins the per-string length prefix in `hash_bytes`. The two rows are
        // ("x", "\x04y") and ("x\x04", "y"), where `\x04` is `TAG_STR` itself:
        // each string writes TAG_STR then its bytes, so without the length
        // prefix both rows flatten to the identical byte stream
        // `04 78 04 04 79` and would hash equal. With the prefix (each string's
        // length between the tag and the bytes) they differ. Removing the prefix
        // at `hash_bytes` makes this test go red.
        let tag = char::from(super::TAG_STR); // U+0004
        let sch = schema(vec![
            id_field(),
            Field::new("p", DataType::Utf8, false),
            Field::new("q", DataType::Utf8, false),
        ]);
        let left = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(StringArray::from(vec!["x".to_string()])),
                Arc::new(StringArray::from(vec![format!("{tag}y")])),
            ],
        );
        let right = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(StringArray::from(vec![format!("x{tag}")])),
                Arc::new(StringArray::from(vec!["y".to_string()])),
            ],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 1);
    }

    #[test]
    fn null_versus_empty_string_is_a_change() {
        let sch = schema(vec![id_field(), Field::new("v", DataType::Utf8, true)]);
        let left = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(StringArray::from(vec![None::<&str>])),
            ],
        );
        let right = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(StringArray::from(vec![Some("")])),
            ],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 1);
    }

    #[test]
    fn dictionary_encoded_key_and_value_decode_before_hashing() {
        // A dictionary-encoded string value equals the plain string; used as a
        // value column here so decoding is exercised on both key and value paths
        // via the string key test below.
        let plain = DataType::Utf8;
        let dict = DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8));
        let left_schema = schema(vec![id_field(), Field::new("s", dict, false)]);
        let right_schema = schema(vec![id_field(), Field::new("s", plain, false)]);
        let dict_array = StringArray::from(vec!["x"])
            .into_iter()
            .collect::<arrow_array::DictionaryArray<arrow_array::types::Int32Type>>();
        let left = reader(
            &left_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(dict_array),
            ],
        );
        let right = reader(
            &right_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(StringArray::from(vec!["x"])),
            ],
        );
        let diff = diff_rows(&left, &right, &left_schema, &right_schema, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn string_key_matches_across_sides() {
        let sch = schema(vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("v", DataType::Int64, false),
        ]);
        let left = reader(
            &sch,
            vec![
                Arc::new(StringArray::from(vec!["a", "b"])),
                Arc::new(Int64Array::from(vec![1, 2])),
            ],
        );
        let right = reader(
            &sch,
            vec![
                Arc::new(StringArray::from(vec!["b", "c"])),
                Arc::new(Int64Array::from(vec![2, 3])),
            ],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &["name".to_string()]).unwrap();
        assert_eq!(diff.counts.rows_removed, 1);
        assert_eq!(diff.counts.rows_added, 1);
        let removed = diff
            .rows_removed
            .column_by_name("name")
            .unwrap()
            .as_string::<i32>()
            .value(0);
        assert_eq!(removed, "a");
    }

    #[test]
    fn columns_only_on_one_side_do_not_cause_row_changes() {
        // only_left / only_right are not common columns, so matched rows with
        // equal common values are unchanged.
        let left_schema = schema(vec![
            id_field(),
            Field::new("keep", DataType::Int64, false),
            Field::new("only_left", DataType::Int64, false),
        ]);
        let right_schema = schema(vec![
            id_field(),
            Field::new("keep", DataType::Int64, false),
            Field::new("only_right", DataType::Int64, false),
        ]);
        let left = reader(
            &left_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Int64Array::from(vec![10])),
                Arc::new(Int64Array::from(vec![99])),
            ],
        );
        let right = reader(
            &right_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Int64Array::from(vec![10])),
                Arc::new(Int64Array::from(vec![77])),
            ],
        );
        let diff = diff_rows(&left, &right, &left_schema, &right_schema, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn key_only_tables_match_on_the_key() {
        let sch = schema(vec![id_field()]);
        let left = reader(
            &sch,
            vec![Arc::new(Int64Array::from(vec![Some(1), Some(2)]))],
        );
        let right = reader(
            &sch,
            vec![Arc::new(Int64Array::from(vec![Some(2), Some(3)]))],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_removed, 1);
        assert_eq!(diff.counts.rows_added, 1);
        assert_eq!(diff.counts.rows_changed, 0);
    }

    #[test]
    fn empty_tables_produce_empty_results() {
        let sch = schema(vec![id_field(), Field::new("v", DataType::Int64, false)]);
        let empty: Vec<RecordBatch> = vec![];
        let left = multi_reader(&sch, empty.clone());
        let right = multi_reader(&sch, empty);
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_added, 0);
        assert_eq!(diff.counts.rows_removed, 0);
        assert_eq!(diff.rows_added.num_rows(), 0);
        assert_eq!(diff.duplicate_keys.num_rows(), 0);
    }

    #[test]
    fn rows_are_diffed_across_multiple_batches() {
        let sch = schema(vec![id_field(), Field::new("v", DataType::Int64, false)]);
        let batch = |ids: Vec<i64>, vs: Vec<i64>| {
            RecordBatch::try_new(
                sch.clone(),
                vec![
                    Arc::new(Int64Array::from(ids)) as ArrayRef,
                    Arc::new(Int64Array::from(vs)),
                ],
            )
            .unwrap()
        };
        let left = multi_reader(
            &sch,
            vec![batch(vec![1, 2], vec![10, 20]), batch(vec![3], vec![30])],
        );
        let right = multi_reader(
            &sch,
            vec![batch(vec![2], vec![20]), batch(vec![3, 4], vec![31, 40])],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_removed, 1);
        assert_eq!(diff.counts.rows_added, 1);
        assert_eq!(diff.counts.rows_changed, 1);
        assert_eq!(ids(&diff.rows_removed), BTreeSet::from([Some(1)]));
        assert_eq!(ids(&diff.rows_added), BTreeSet::from([Some(4)]));
    }

    #[test]
    fn nested_non_key_column_is_skipped_not_compared() {
        // A list non-key column is out of scope for the row diff: it is skipped,
        // so two rows with the same key are unchanged even though the lists
        // differ, and the diff still succeeds.
        let list = DataType::List(Arc::new(Field::new("item", DataType::Int64, true)));
        let sch = schema(vec![id_field(), Field::new("xs", list, true)]);
        let left_xs = ListArray::from_iter_primitive::<Int64Type, _, _>(vec![Some(vec![Some(1)])]);
        let right_xs = ListArray::from_iter_primitive::<Int64Type, _, _>(vec![Some(vec![Some(9)])]);
        let left = reader(
            &sch,
            vec![Arc::new(Int64Array::from(vec![Some(1)])), Arc::new(left_xs)],
        );
        let right = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(right_xs),
            ],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
        assert_eq!(diff.counts.rows_added, 0);
        assert_eq!(diff.counts.rows_removed, 0);
    }

    #[test]
    fn nested_key_column_is_rejected() {
        // A nested key column cannot be hashed by value and is refused.
        let list = DataType::List(Arc::new(Field::new("item", DataType::Int64, true)));
        let sch = schema(vec![
            Field::new("k", list, true),
            Field::new("v", DataType::Int64, false),
        ]);
        let xs = ListArray::from_iter_primitive::<Int64Type, _, _>(vec![Some(vec![Some(1)])]);
        let make = || {
            MemoryInput::new(
                sch.clone(),
                vec![
                    RecordBatch::try_new(
                        sch.clone(),
                        vec![
                            Arc::new(xs.clone()) as ArrayRef,
                            Arc::new(Int64Array::from(vec![1])),
                        ],
                    )
                    .unwrap(),
                ],
            )
        };
        let error = diff_rows(&make(), &make(), &sch, &sch, &["k".to_string()]).unwrap_err();
        assert!(
            matches!(error, TableDiffError::UnsupportedRowType { column, .. } if column == "k")
        );
    }

    /// Hashes a single-cell row under the row domain, for the hash-sensitivity
    /// tests below.
    fn cell_hash(hasher: &super::RowHasher, array: ArrayRef) -> u128 {
        super::hash_row(hasher, super::DOMAIN_ROW, &[array], &["c"], 0).unwrap()
    }

    #[test]
    fn hash_distinguishes_and_equates_scalar_values() {
        let hasher = super::RowHasher::new().unwrap();

        // Decimals: distinct values differ; the same value at different scales
        // (trailing zeros removed) is equal.
        let dec = |v: i128, s: i8| -> ArrayRef {
            Arc::new(
                Decimal128Array::from(vec![v])
                    .with_precision_and_scale(20, s)
                    .unwrap(),
            )
        };
        assert_ne!(
            cell_hash(&hasher, dec(100, 2)),
            cell_hash(&hasher, dec(200, 2))
        );
        assert_eq!(
            cell_hash(&hasher, dec(100, 2)),
            cell_hash(&hasher, dec(10_000, 4))
        );

        // Timestamps: distinct instants differ; the same instant across units is
        // equal.
        let micros: ArrayRef = Arc::new(TimestampMicrosecondArray::from(vec![2_000_000]));
        let other_micros: ArrayRef = Arc::new(TimestampMicrosecondArray::from(vec![3_000_000]));
        let millis: ArrayRef = Arc::new(TimestampMillisecondArray::from(vec![2_000]));
        assert_ne!(
            cell_hash(&hasher, micros.clone()),
            cell_hash(&hasher, other_micros)
        );
        assert_eq!(cell_hash(&hasher, micros), cell_hash(&hasher, millis));

        // Dates: distinct days differ; the same raw value is equal.
        let day_three: ArrayRef = Arc::new(Date32Array::from(vec![3]));
        let day_four: ArrayRef = Arc::new(Date32Array::from(vec![4]));
        let day_three_again: ArrayRef = Arc::new(Date32Array::from(vec![3]));
        assert_ne!(
            cell_hash(&hasher, day_three.clone()),
            cell_hash(&hasher, day_four)
        );
        assert_eq!(
            cell_hash(&hasher, day_three),
            cell_hash(&hasher, day_three_again)
        );

        // Integers of different widths but the same value are equal; different
        // values differ.
        let i32v: ArrayRef = Arc::new(Int32Array::from(vec![7]));
        let seven_wide: ArrayRef = Arc::new(Int64Array::from(vec![7]));
        let eight: ArrayRef = Arc::new(Int64Array::from(vec![8]));
        assert_eq!(
            cell_hash(&hasher, i32v),
            cell_hash(&hasher, seven_wide.clone())
        );
        assert_ne!(cell_hash(&hasher, seven_wide), cell_hash(&hasher, eight));

        // Strings.
        let sa: ArrayRef = Arc::new(StringArray::from(vec!["a"]));
        let sb: ArrayRef = Arc::new(StringArray::from(vec!["b"]));
        assert_ne!(cell_hash(&hasher, sa), cell_hash(&hasher, sb));
    }

    #[test]
    fn hash_of_large_integral_floats_stays_distinct() {
        // Two distinct integral floats past 2^53 keep their bit patterns (they do
        // not fold to an integer): folding them would saturate both to the same
        // i128 and collide.
        let hasher = super::RowHasher::new().unwrap();
        let f1: ArrayRef = Arc::new(Float64Array::from(vec![1e300]));
        let f2: ArrayRef = Arc::new(Float64Array::from(vec![2e300]));
        assert_ne!(cell_hash(&hasher, f1), cell_hash(&hasher, f2));
    }

    #[test]
    fn hash_uses_both_128_bit_halves() {
        // SipHash-1-3's 128-bit output must populate both halves: across a run of
        // inputs, at least one has a non-zero top half and at least one has the
        // two halves unequal. A hash that collapsed to 64 bits (top half always
        // zero, or the halves mirrored) would fail this.
        let keyed = super::RowHasher::new().unwrap();
        let outputs: Vec<u128> = (0..16)
            .map(|v| cell_hash(&keyed, Arc::new(Int64Array::from(vec![v])) as ArrayRef))
            .collect();
        assert!(
            outputs.iter().any(|h| h >> 64 != 0),
            "the top 64 bits must be used"
        );
        assert!(
            outputs
                .iter()
                .any(|h| (h >> 64) != (h & u128::from(u64::MAX))),
            "the two 64-bit halves must differ"
        );
    }

    #[test]
    fn key_and_row_domains_hash_differently() {
        // The domain tag separates a key hash from a row hash of the same cell.
        let hasher = super::RowHasher::new().unwrap();
        let cell: ArrayRef = Arc::new(Int64Array::from(vec![5]));
        let as_key = super::hash_row(
            &hasher,
            super::DOMAIN_KEY,
            std::slice::from_ref(&cell),
            &["c"],
            0,
        )
        .unwrap();
        let as_row = super::hash_row(&hasher, super::DOMAIN_ROW, &[cell], &["c"], 0).unwrap();
        assert_ne!(as_key, as_row);
    }

    #[test]
    fn three_column_composite_key_duplicate_report_keeps_every_key_column() {
        // A 3-column key exercises the key-field count in the duplicate report's
        // schema (key columns are all fields but the last two counts).
        let sch = schema(vec![
            Field::new("a", DataType::Int64, true),
            Field::new("b", DataType::Int64, true),
            Field::new("c", DataType::Int64, true),
            Field::new("v", DataType::Int64, false),
        ]);
        // (1,2,3) appears twice on the left: a duplicate key.
        let left = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![1, 1])),
                Arc::new(Int64Array::from(vec![2, 2])),
                Arc::new(Int64Array::from(vec![3, 3])),
                Arc::new(Int64Array::from(vec![10, 11])),
            ],
        );
        let right = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![9])),
                Arc::new(Int64Array::from(vec![9])),
                Arc::new(Int64Array::from(vec![9])),
                Arc::new(Int64Array::from(vec![90])),
            ],
        );
        let diff = diff_rows(
            &left,
            &right,
            &sch,
            &sch,
            &["a".to_string(), "b".to_string(), "c".to_string()],
        )
        .unwrap();
        assert_eq!(diff.counts.duplicate_keys, 1);

        let dup = &diff.duplicate_keys;
        assert_eq!(
            dup.schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c", "left_count", "right_count"]
        );
        assert_eq!(dup.column(0).as_primitive::<Int64Type>().value(0), 1);
        assert_eq!(dup.column(1).as_primitive::<Int64Type>().value(0), 2);
        assert_eq!(dup.column(2).as_primitive::<Int64Type>().value(0), 3);
    }

    #[test]
    fn dictionary_non_key_value_change_is_detected() {
        // A dictionary-encoded non-key column is hashable, so a changed value is
        // a row change (if it were treated as unhashable it would be skipped).
        let dict_type = DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8));
        let sch = schema(vec![id_field(), Field::new("s", dict_type, false)]);
        let left_s: arrow_array::DictionaryArray<arrow_array::types::Int32Type> =
            StringArray::from(vec!["a"]).into_iter().collect();
        let right_s: arrow_array::DictionaryArray<arrow_array::types::Int32Type> =
            StringArray::from(vec!["b"]).into_iter().collect();
        let left = reader(
            &sch,
            vec![Arc::new(Int64Array::from(vec![Some(1)])), Arc::new(left_s)],
        );
        let right = reader(
            &sch,
            vec![Arc::new(Int64Array::from(vec![Some(1)])), Arc::new(right_s)],
        );
        let diff = diff_rows(&left, &right, &sch, &sch, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 1);
    }

    #[test]
    fn string_and_binary_of_the_same_bytes_hash_differently() {
        // The per-cell type tag disambiguates kinds: a Utf8 "x" and a Binary
        // b"x" carry the same length and bytes, so only the tag separates them.
        let hasher = super::RowHasher::new().unwrap();
        let as_str: ArrayRef = Arc::new(StringArray::from(vec!["x"]));
        let as_bin: ArrayRef = Arc::new(BinaryArray::from(vec![&b"x"[..]]));
        assert_ne!(cell_hash(&hasher, as_str), cell_hash(&hasher, as_bin));
    }

    #[test]
    fn time_unit_change_is_detected() {
        // 1 second and 1 millisecond are different times; the unit discriminant
        // keeps them from colliding.
        let left_schema = schema(vec![
            id_field(),
            Field::new("t", DataType::Time32(TimeUnit::Second), false),
        ]);
        let right_schema = schema(vec![
            id_field(),
            Field::new("t", DataType::Time32(TimeUnit::Millisecond), false),
        ]);
        let left = reader(
            &left_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Time32SecondArray::from(vec![1])),
            ],
        );
        let right = reader(
            &right_schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Time32MillisecondArray::from(vec![1])),
            ],
        );
        let diff = diff_rows(&left, &right, &left_schema, &right_schema, &key()).unwrap();
        assert_eq!(diff.counts.rows_changed, 1);
    }

    #[test]
    fn remaining_temporal_and_view_variants_are_hashed() {
        // Cover the time/duration/interval unit variants and the binary view arm
        // not exercised by the tests above; each distinct value must be a change.
        assert_value_change_detected(
            DataType::Time32(TimeUnit::Millisecond),
            Arc::new(Time32MillisecondArray::from(vec![1])),
            Arc::new(Time32MillisecondArray::from(vec![2])),
        );
        assert_value_change_detected(
            DataType::Time64(TimeUnit::Nanosecond),
            Arc::new(Time64NanosecondArray::from(vec![1])),
            Arc::new(Time64NanosecondArray::from(vec![2])),
        );
        assert_value_change_detected(
            DataType::Duration(TimeUnit::Nanosecond),
            Arc::new(DurationNanosecondArray::from(vec![1])),
            Arc::new(DurationNanosecondArray::from(vec![2])),
        );
        assert_value_change_detected(
            DataType::Interval(arrow_schema::IntervalUnit::DayTime),
            Arc::new(IntervalDayTimeArray::from(vec![IntervalDayTime::new(1, 0)])),
            Arc::new(IntervalDayTimeArray::from(vec![IntervalDayTime::new(2, 0)])),
        );
        assert_value_change_detected(
            DataType::Interval(arrow_schema::IntervalUnit::MonthDayNano),
            Arc::new(IntervalMonthDayNanoArray::from(vec![
                IntervalMonthDayNano::new(1, 0, 0),
            ])),
            Arc::new(IntervalMonthDayNanoArray::from(vec![
                IntervalMonthDayNano::new(2, 0, 0),
            ])),
        );
        assert_value_change_detected(
            DataType::BinaryView,
            Arc::new(BinaryViewArray::from(vec![&b"a"[..]])),
            Arc::new(BinaryViewArray::from(vec![&b"b"[..]])),
        );
    }

    #[test]
    fn unhashable_scalar_value_types_are_refused_over_empty_input() {
        // Scalar types arrow-rs has no array for (invalid Time32/Time64 units,
        // a negative-width FixedSizeBinary) are refused up front, so an empty
        // table with such a column errors cleanly instead of panicking when the
        // empty output array is built — whether the column is on both sides or
        // on one side only (a one-sided column still reaches the added/removed
        // output's full schema).
        let invalid = [
            DataType::Time32(TimeUnit::Microsecond),
            DataType::Time32(TimeUnit::Nanosecond),
            DataType::Time64(TimeUnit::Second),
            DataType::Time64(TimeUnit::Millisecond),
            DataType::FixedSizeBinary(-1),
        ];
        for dt in invalid {
            let with = schema(vec![id_field(), Field::new("v", dt.clone(), true)]);
            let without = schema(vec![id_field()]);
            let empty_with = MemoryInput::new(with.clone(), Vec::new());
            let empty_without = MemoryInput::new(without.clone(), Vec::new());

            // Both sides, left-only, and right-only all refuse cleanly.
            for (left_input, left_schema, right_input, right_schema) in [
                (&empty_with, &with, &empty_with, &with),
                (&empty_with, &with, &empty_without, &without),
                (&empty_without, &without, &empty_with, &with),
            ] {
                let error = diff_rows(left_input, right_input, left_schema, right_schema, &key())
                    .unwrap_err();
                assert!(
                    matches!(error, TableDiffError::UnsupportedRowType { ref column, .. } if column == "v"),
                    "{dt:?} should be refused, got {error:?}"
                );
            }
        }
    }

    #[test]
    fn read_error_from_a_failing_reader_is_surfaced() {
        let sch = schema(vec![id_field(), Field::new("v", DataType::Int64, false)]);
        let bad = FailingInput {
            schema: sch.clone(),
        };
        let good = reader(
            &sch,
            vec![
                Arc::new(Int64Array::from(vec![Some(1)])),
                Arc::new(Int64Array::from(vec![1])),
            ],
        );
        let error = diff_rows(&bad, &good, &sch, &sch, &key()).unwrap_err();
        assert!(matches!(error, TableDiffError::Read { .. }));
    }

    /// What the naive in-memory reference computes for a case.
    #[derive(Debug, PartialEq, Eq)]
    struct Naive {
        added: usize,
        removed: usize,
        changed: usize,
        dup: usize,
        null: usize,
        added_ids: BTreeSet<Option<i64>>,
        removed_ids: BTreeSet<Option<i64>>,
    }

    // Property test: the counts and the added/removed key sets equal a naive
    // in-memory reference on random small int tables (with duplicates and null
    // keys forced by a tiny key range).
    fn naive_counts(left: &[(Option<i64>, i64)], right: &[(Option<i64>, i64)]) -> Naive {
        let mut lg: BTreeMap<Option<i64>, Vec<i64>> = BTreeMap::new();
        let mut rg: BTreeMap<Option<i64>, Vec<i64>> = BTreeMap::new();
        for &(k, v) in left {
            lg.entry(k).or_default().push(v);
        }
        for &(k, v) in right {
            rg.entry(k).or_default().push(v);
        }

        let mut keys: BTreeSet<Option<i64>> = BTreeSet::new();
        keys.extend(lg.keys().copied());
        keys.extend(rg.keys().copied());

        let (mut added, mut removed, mut changed, mut dup, mut null) = (0, 0, 0, 0, 0);
        let mut added_ids = BTreeSet::new();
        let mut removed_ids = BTreeSet::new();
        for k in keys {
            if k.is_none() {
                null += 1;
            }
            let lc = lg.get(&k).map_or(0, Vec::len);
            let rc = rg.get(&k).map_or(0, Vec::len);
            if lc > 1 || rc > 1 {
                dup += 1;
            } else if lc == 1 && rc == 0 {
                removed += 1;
                removed_ids.insert(k);
            } else if lc == 0 && rc == 1 {
                added += 1;
                added_ids.insert(k);
            } else if lg[&k][0] != rg[&k][0] {
                changed += 1;
            }
        }
        Naive {
            added,
            removed,
            changed,
            dup,
            null,
            added_ids,
            removed_ids,
        }
    }

    fn run_case(left: &[(Option<i64>, i64)], right: &[(Option<i64>, i64)]) {
        let sch = schema(vec![id_field(), Field::new("v", DataType::Int64, false)]);
        let to_reader = |rows: &[(Option<i64>, i64)]| {
            let id: Int64Array = rows.iter().map(|&(k, _)| k).collect();
            let v: Int64Array = rows.iter().map(|&(_, val)| Some(val)).collect();
            reader(&sch, vec![Arc::new(id), Arc::new(v)])
        };
        let diff = diff_rows(&to_reader(left), &to_reader(right), &sch, &sch, &key()).unwrap();
        let naive = naive_counts(left, right);
        assert_eq!(diff.counts.rows_added, naive.added, "added count");
        assert_eq!(diff.counts.rows_removed, naive.removed, "removed count");
        assert_eq!(diff.counts.rows_changed, naive.changed, "changed count");
        assert_eq!(diff.counts.duplicate_keys, naive.dup, "duplicate count");
        assert_eq!(diff.counts.null_keys, naive.null, "null-key count");
        assert_eq!(ids(&diff.rows_added), naive.added_ids, "added ids");
        assert_eq!(ids(&diff.rows_removed), naive.removed_ids, "removed ids");
    }

    proptest::proptest! {
        #[test]
        fn matches_naive_reference(
            left in proptest::collection::vec(
                (proptest::option::weighted(0.15, 0i64..6), 0i64..4), 0..12usize),
            right in proptest::collection::vec(
                (proptest::option::weighted(0.15, 0i64..6), 0i64..4), 0..12usize),
        ) {
            run_case(&left, &right);
        }
    }
}

#[cfg(test)]
mod type_coverage_tests {
    use super::{MemoryInput, diff_rows};
    use arrow_array::builder::FixedSizeBinaryBuilder;
    use arrow_array::types::Int32Type;
    use arrow_array::{
        ArrayRef, BinaryArray, BooleanArray, Decimal128Array, DictionaryArray, Float16Array,
        Float32Array, Int8Array, Int16Array, Int64Array, LargeBinaryArray, LargeStringArray,
        RecordBatch, StringArray, StringViewArray, TimestampNanosecondArray, TimestampSecondArray,
        UInt8Array, UInt16Array, UInt32Array, UInt64Array,
    };
    use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
    use half::f16;
    use std::sync::Arc;

    /// A one-column-of-each-scalar-type table (two identical rows), diffed
    /// against itself: exercises every arm of `hash_cell` and reports no change.
    #[test]
    fn every_scalar_type_hashes_and_matches_itself() {
        let mut fsb = FixedSizeBinaryBuilder::new(2);
        fsb.append_value(b"ab").unwrap();
        fsb.append_value(b"cd").unwrap();
        let fixed = fsb.finish();

        let dict: DictionaryArray<Int32Type> =
            StringArray::from(vec!["p", "q"]).into_iter().collect();

        let fields = vec![
            Field::new("id", DataType::Int64, false),
            Field::new("b", DataType::Boolean, false),
            Field::new("i8", DataType::Int8, false),
            Field::new("i16", DataType::Int16, false),
            Field::new("u8", DataType::UInt8, false),
            Field::new("u16", DataType::UInt16, false),
            Field::new("u32", DataType::UInt32, false),
            Field::new("u64", DataType::UInt64, false),
            Field::new("f16", DataType::Float16, false),
            Field::new("f32", DataType::Float32, false),
            Field::new("zero_dec", DataType::Decimal128(10, 2), false),
            Field::new("lutf8", DataType::LargeUtf8, false),
            Field::new("sview", DataType::Utf8View, false),
            Field::new("bin", DataType::Binary, false),
            Field::new("lbin", DataType::LargeBinary, false),
            Field::new("fixed", DataType::FixedSizeBinary(2), false),
            Field::new("ts_s", DataType::Timestamp(TimeUnit::Second, None), false),
            Field::new(
                "ts_ns",
                DataType::Timestamp(TimeUnit::Nanosecond, None),
                false,
            ),
            Field::new(
                "s",
                DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
                false,
            ),
        ];
        let schema: SchemaRef = Arc::new(Schema::new(fields));

        let columns = || -> Vec<ArrayRef> {
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(BooleanArray::from(vec![true, false])),
                Arc::new(Int8Array::from(vec![1, 2])),
                Arc::new(Int16Array::from(vec![1, 2])),
                Arc::new(UInt8Array::from(vec![1, 2])),
                Arc::new(UInt16Array::from(vec![1, 2])),
                Arc::new(UInt32Array::from(vec![1, 2])),
                Arc::new(UInt64Array::from(vec![1, 2])),
                Arc::new(Float16Array::from(vec![
                    f16::from_f32(1.5),
                    f16::from_f32(2.5),
                ])),
                Arc::new(Float32Array::from(vec![1.5, 2.5])),
                // Value 0 exercises the decimal zero-scale branch.
                Arc::new(
                    Decimal128Array::from(vec![0, 0])
                        .with_precision_and_scale(10, 2)
                        .unwrap(),
                ),
                Arc::new(LargeStringArray::from(vec!["a", "b"])),
                Arc::new(StringViewArray::from(vec!["a", "b"])),
                Arc::new(BinaryArray::from(vec![&b"a"[..], &b"b"[..]])),
                Arc::new(LargeBinaryArray::from(vec![&b"a"[..], &b"b"[..]])),
                Arc::new(fixed.clone()),
                Arc::new(TimestampSecondArray::from(vec![10, 20])),
                Arc::new(TimestampNanosecondArray::from(vec![10, 20])),
                Arc::new(dict.clone()),
            ]
        };

        let left = MemoryInput::new(
            schema.clone(),
            vec![RecordBatch::try_new(schema.clone(), columns()).unwrap()],
        );
        let right = MemoryInput::new(
            schema.clone(),
            vec![RecordBatch::try_new(schema.clone(), columns()).unwrap()],
        );

        let diff = diff_rows(&left, &right, &schema, &schema, &["id".to_string()]).unwrap();
        assert_eq!(diff.counts.rows_changed, 0);
        assert_eq!(diff.counts.rows_added, 0);
        assert_eq!(diff.counts.rows_removed, 0);
    }

    /// A dictionary-encoded key column is decoded before hashing and in the
    /// duplicate-key report's schema.
    #[test]
    fn dictionary_key_column_is_decoded() {
        let schema: SchemaRef = Arc::new(Schema::new(vec![
            Field::new(
                "k",
                DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
                false,
            ),
            Field::new("v", DataType::Int64, false),
        ]));
        // Left key "a" twice -> a duplicate whose report decodes the key type.
        let left_key: DictionaryArray<Int32Type> =
            StringArray::from(vec!["a", "a"]).into_iter().collect();
        let right_key: DictionaryArray<Int32Type> =
            StringArray::from(vec!["b"]).into_iter().collect();
        let left = MemoryInput::new(
            schema.clone(),
            vec![
                RecordBatch::try_new(
                    schema.clone(),
                    vec![Arc::new(left_key), Arc::new(Int64Array::from(vec![1, 2]))],
                )
                .unwrap(),
            ],
        );
        let right = MemoryInput::new(
            schema.clone(),
            vec![
                RecordBatch::try_new(
                    schema.clone(),
                    vec![Arc::new(right_key), Arc::new(Int64Array::from(vec![3]))],
                )
                .unwrap(),
            ],
        );

        let diff = diff_rows(&left, &right, &schema, &schema, &["k".to_string()]).unwrap();
        assert_eq!(diff.counts.duplicate_keys, 1);
        assert_eq!(
            diff.duplicate_keys.schema().field(0).data_type(),
            &DataType::Utf8
        );
        let dup_key = diff
            .duplicate_keys
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(dup_key.value(0), "a");
    }

    /// The empty-value-column path: `hash_row` over zero non-key columns still
    /// distinguishes rows by key only.
    #[test]
    fn zero_common_value_columns() {
        let schema: SchemaRef =
            Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
        let input = |ids: Vec<i64>| {
            MemoryInput::new(
                schema.clone(),
                vec![
                    RecordBatch::try_new(
                        schema.clone(),
                        vec![Arc::new(Int64Array::from(ids)) as ArrayRef],
                    )
                    .unwrap(),
                ],
            )
        };
        let left = input(vec![1, 2]);
        let right = input(vec![2, 3]);
        let diff = diff_rows(&left, &right, &schema, &schema, &["id".to_string()]).unwrap();
        assert_eq!(diff.counts.rows_added, 1);
        assert_eq!(diff.counts.rows_removed, 1);
    }
}
