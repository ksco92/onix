"""Generate the Arrow table-diff benchmark fixture pair.

Streams a base parquet table (`a.parquet`) and its mutated counterpart
(`b.parquet`) row-group by row-group, in one pass, from a seeded
`random.Random` -- never holding the full table in memory. `a.parquet`
has five columns:

    id int64 (unique, ascending), ts timestamp[us, UTC],
    category string (20 distinct values), amount decimal(18,4),
    payload string (20-200 chars)

`b.parquet` applies a fixed mutation mix on top, in the same streaming pass:

    * 2% of surviving rows modified: half get a new `amount`, half a new
      `payload` (both guaranteed different from the original -- see
      `_random_amount`/`_random_payload`'s disjoint ranges/prefixes).
    * 1% of rows deleted (excluded from `b.parquet` entirely).
    * 1% new rows appended with fresh, higher ids (ascending continues).
    * `category` re-typed to `dictionary<int32, string>` (values unchanged).
    * `ts` cast from `timestamp[us, UTC]` to `timestamp[ms, UTC]`
      (lossless here: every `ts` is generated on a whole-second step).
    * a new `note` column: `null` for every carried-over row, `"added"`
      for the 1% of new rows.

No duplicate `id` values are introduced on either side by construction
(each side's ids are strictly unique and ascending); `perf/arrow/README.md`
records this as a deliberate scope decision -- duplicate-key handling is
exercised by #39's own synthetic/property tests, not by this fixture.

Every exact count (deleted, added, modified per column, unchanged, and the
three schema changes) is written to a sidecar `manifest.json` next to the
two parquet files, so `oracle_duckdb.py`'s counts can be checked against a
ground truth that isn't derived from the oracle itself.

**Determinism is the whole point of this file**, same as
`perf/generate_fixtures.py`: a single `random.Random(seed)` instance drives
every draw in a fixed order, and row order is always construction order (see
`generate`). Two runs of this script with the same `--rows`/`--seed` must
produce byte-identical
`a.parquet`/`b.parquet`/`manifest.json`. To prove it:

    cd perf/arrow
    uv run --group perf generate_fixtures.py --rows 100000 --out /tmp/run1
    uv run --group perf generate_fixtures.py --rows 100000 --out /tmp/run2
    diff <(shasum -a 256 /tmp/run1/* | cut -d' ' -f1) <(shasum -a 256 /tmp/run2/* | cut -d' ' -f1)

Usage::

    cd perf/arrow
    uv sync --group perf
    uv run --group perf generate_fixtures.py --rows 1000 --out fixtures/1k

# The `wide` kind (`--kind wide`, #84)

`wide` trades the five columns above for one of every scalar type `onix-arrow`'s row diff
hashes, cast-normalizes, or renders (see `crates/onix-arrow/src/row_diff.rs`'s "Value
semantics"/"Per-cell changes" and `schema.rs`'s normalization rules), at the same 5 GB-per-side
target, so fewer, much wider rows (see `_wide_column_specs` for the exact list and
`WIDE_DEFAULT_ROWS`'s comment for the row-count derivation). Nested types are out (the row diff
skips a nested non-key column entirely -- see row_diff.rs's "Which column types are hashed,
refused, or skipped").

Four gaps follow from what pyarrow and Parquet can represent, verified empirically against this
repo's pinned pyarrow, plus one deliberate omission:

* `month_day_nano_interval` has no Parquet representation (`ArrowNotImplementedError` on write) and
  `date64` is silently downcast to `date32` on write (Parquet's DATE logical type is a 32-bit day
  count only). Both are stored as raw integer components instead -- `interval` as
  `interval_months`/`interval_days`/`interval_nanos` (int32/int32/int64), `date64` as
  `date64_millis` (int64, whole-day-aligned per Arrow's Date64 contract) -- and every tool,
  `onix` included, reads them as plain integers rather than paying to rebuild the real type
  (`bench_tables.py`'s module docstring measures that rebuild's own cost as prohibitive at this
  size). Neither is ever mutated between `a` and `b`, so every tool's cells-changed count for them
  is zero regardless of which type it reads.
* `decimal256` above precision 38 is a second, more severe gap: DuckDB's parquet reader silently
  decodes it to the wrong number instead of erroring, and polars' parquet and IPC readers both
  fail outright rather than raise a catchable error. `dec256` is kept at precision 38 -- distinct
  from `decimal128` at the Arrow-type level, which is what the row diff's `Decimal256` hashing arm
  needs, but numerically representable by both baselines -- and is never mutated.
* `Interval(YearMonth)` and `Interval(DayTime)` -- two of the three interval variants
  `row_diff.rs` hashes -- have no pyarrow constructor at all (only `month_day_nano_interval`
  exists), so neither is in this fixture; only the interval cross-variant `type_changed` path is
  therefore untested here, and stays covered by `row_diff.rs`'s own unit tests.
* `DataType::Null` -- `row_diff.rs` hashes it (every row a null) -- has no column here, deliberately:
  an all-null column has no value variance to hash or render beyond the null branch, which every
  other nullable column in this fixture already exercises.

The `ts_cast` column is `wide`'s "one unit cast": nanosecond, zone-aware on `a`; microsecond,
zone-naive on `b`. Dropping the zone alongside the unit is deliberate -- a zone-aware/naive pair is
always `type_changed` regardless of whether the instant value differs, so this column gives every
surviving row a `type_changed` cell with an exact, derived count (`rows - rows_deleted`), the only
way to get that change kind at all (the other three schema changes -- dictionary retype, decimal
scale, and this column's unit half -- are lossless normalizations reported `value_changed` only
when the value genuinely differs, never `type_changed`).

Every other non-key column is independently nullable at `WIDE_NULL_RATE`. Two columns (`i16`,
`large_utf8_col`) carry both null-to-value and value-to-null transitions with their own manifest
counts, covering `became_non_null`/`became_null`; a further set of columns carries a
`value_changed` mutation, each with its own manifest count, covering a representative type per
domain: int (`i8`, `i32`, `i64`), uint (`u8`), float (`f32`, `f64`, `float16_col`), decimal
(`dec_scale4`, `decimal32_col`, `decimal64_col`), string (`utf8_col`, `utf8_view_col`), binary
(`binary_col`, `binary_view_col`, `fixed_size_binary_col`), boolean, date, time, and duration. A
changed value is always drawn from a range disjoint from the original's, the same
guaranteed-different convention as the narrow fixture's `amount`/`payload`; a float's replacement
is always a plain finite number, never NaN or signed zero, so a `value_changed` float record's
rendering is never ambiguous with the unmutated NaN/-0.0 cells this fixture also carries.
"""

from __future__ import annotations

import argparse
import json
import random
import string
from collections.abc import Callable
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import TYPE_CHECKING, Final

if TYPE_CHECKING:
    from typing_extensions import Self

import pyarrow as pa
import pyarrow.parquet as pq

##############################################
##############################################
##############################################
##############################################
# Configuration

# Recorded default seed and row count. `--rows`' default is tuned so the
# default invocation lands near 5 GB compressed on the machine that
# generated it -- see README.md's "Sizes" section for the measured figure
# and the row count this constant was set to after that measurement.
DEFAULT_SEED: Final[int] = 20260904
DEFAULT_ROWS: Final[int] = 37_000_000

# `b.parquet`'s one new column, on top of `a.parquet`'s five.
ADDED_COLUMN: Final[str] = "note"

# Row-group size: bounds the number of rows held as in-memory Python/Arrow
# objects at once, independent of `--rows`.
ROW_GROUP_SIZE: Final[int] = 200_000

# Mutation mix (fractions of the base row count).
DELETE_RATE: Final[float] = 0.01
ADD_RATE: Final[float] = 0.01
MODIFY_RATE: Final[float] = 0.02

CATEGORY_VALUES: Final[tuple[str, ...]] = tuple(f"category_{i:02d}" for i in range(20))
PAYLOAD_ALPHABET: Final[str] = string.ascii_lowercase + string.digits
PAYLOAD_MIN_LEN: Final[int] = 20
PAYLOAD_MAX_LEN: Final[int] = 200

# ts starts at this UTC microsecond epoch and steps forward one whole second
# per row -- ascending, unique, and exactly ms-representable (no precision
# lost when `b.parquet` casts to timestamp[ms, UTC]).
BASE_EPOCH_US: Final[int] = 1_704_067_200_000_000  # 2024-01-01T00:00:00Z
TS_STEP_US: Final[int] = 1_000_000

# Disjoint unit ranges (amount stored as integer 1e-4 units, decimal(18,4))
# so a "changed"/"added" amount can never coincide with an original one --
# same disjoint-range convention as perf/generate_fixtures.py's
# `_CHANGED_INT_RANGE`/`_ADDED_INT_RANGE`.
_ORIGINAL_AMOUNT_UNITS: Final[tuple[int, int]] = (0, 1_000_000_000)  # 0.0000 - 100000.0000
_CHANGED_AMOUNT_UNITS: Final[tuple[int, int]] = (2_000_000_000, 3_000_000_000)
_ADDED_AMOUNT_UNITS: Final[tuple[int, int]] = (4_000_000_000, 5_000_000_000)

# A changed/added payload is prefixed with a marker character sequence that
# `_random_payload`'s own alphabet (lowercase letters + digits, no
# underscore) can never produce, guaranteeing it differs from any original
# payload rather than relying on coincidence.
_CHANGED_PAYLOAD_PREFIX: Final[str] = "chg_"
_ADDED_PAYLOAD_PREFIX: Final[str] = "new_"


##############################################
##############################################
##############################################
##############################################
# Value generation


def _random_amount(rng: random.Random, unit_range: tuple[int, int]) -> Decimal:
    """
    Draw a `decimal(18,4)` amount from `unit_range`, expressed as 1e-4 units.

    :param rng: Seeded random source (mutated in place, as `Random` always is).
    :param unit_range: `(low, high)` bounds, inclusive, in 1e-4 units.
    :return: The drawn amount, exact to 4 decimal places.
    """
    return Decimal(rng.randint(*unit_range)).scaleb(-4)


def _random_payload(rng: random.Random, prefix: str = "") -> str:
    """
    Draw a random `payload` string of 20-200 chars, optionally marker-prefixed.

    :param rng: Seeded random source.
    :param prefix: Prepended to the drawn characters; must not itself use
        `PAYLOAD_ALPHABET`'s character set, so a prefixed string can never
        collide with an unprefixed one.
    :return: The generated string, `prefix` included in its length budget.
    """
    length = rng.randint(PAYLOAD_MIN_LEN, PAYLOAD_MAX_LEN)
    body_len = max(0, length - len(prefix))

    return prefix + "".join(rng.choices(PAYLOAD_ALPHABET, k=body_len))


##############################################
##############################################
##############################################
##############################################
# Row-group assembly


def _select_mutated_indices(
    rows: int,
    rng: random.Random,
) -> tuple[set[int], set[int], set[int]]:
    """
    Choose which of the `rows` original row positions are deleted or
    modified in `b.parquet`, and split "modified" into an amount half and
    a payload half.

    :param rows: Number of original rows (`a.parquet`'s row count).
    :param rng: Seeded random source.
    :return: `(delete_indices, modify_amount_indices, modify_payload_indices)`,
        three pairwise-disjoint sets of row positions in `[0, rows)`.
    """
    delete_n = round(rows * DELETE_RATE)
    modify_n = round(rows * MODIFY_RATE)
    chosen = rng.sample(range(rows), delete_n + modify_n)
    delete_indices = set(chosen[:delete_n])
    modify_indices = chosen[delete_n:]
    amount_split = len(modify_indices) // 2
    modify_amount_indices = set(modify_indices[:amount_split])
    modify_payload_indices = set(modify_indices[amount_split:])

    return delete_indices, modify_amount_indices, modify_payload_indices


class _Counters:
    """Running exact counts for the sidecar manifest, updated row by row."""

    def __init__(self: Self) -> None:
        """Zero-initialize every counter."""
        self.deleted = 0
        self.modified_amount = 0
        self.modified_payload = 0
        self.unchanged = 0
        self.added = 0


def _build_original_chunk(
    start: int,
    end: int,
    rng: random.Random,
) -> tuple[pa.RecordBatch, list[tuple[str, Decimal, str]]]:
    """
    Build one `a.parquet` row-group batch for original row positions
    `[start, end)`, plus the per-row `(category, amount, payload)` values
    `b`'s row-group builder needs to derive its own row from.

    :param start: First row position in this chunk (inclusive).
    :param end: Last row position in this chunk (exclusive).
    :param rng: Seeded random source, shared with every other chunk.
    :return: The `a.parquet` batch, and the raw values for `[start, end)`.
    """
    ids = list(range(start, end))
    ts_values = [BASE_EPOCH_US + i * TS_STEP_US for i in ids]
    raw_values = [
        (CATEGORY_VALUES[rng.randrange(len(CATEGORY_VALUES))], _random_amount(rng, _ORIGINAL_AMOUNT_UNITS), _random_payload(rng))
        for _ in ids
    ]
    categories = [v[0] for v in raw_values]
    amounts = [v[1] for v in raw_values]
    payloads = [v[2] for v in raw_values]

    batch = pa.record_batch(
        [
            pa.array(ids, type=pa.int64()),
            pa.array(ts_values, type=pa.int64()).cast(pa.timestamp("us", tz="UTC")),
            pa.array(categories, type=pa.string()),
            pa.array(amounts, type=pa.decimal128(18, 4)),
            pa.array(payloads, type=pa.string()),
        ],
        names=["id", "ts", "category", "amount", "payload"],
    )

    return batch, raw_values


def _build_changed_chunk(
    start: int,
    end: int,
    raw_values: list[tuple[str, Decimal, str]],
    delete_indices: set[int],
    modify_amount_indices: set[int],
    modify_payload_indices: set[int],
    counters: _Counters,
    rng: random.Random,
) -> pa.RecordBatch:
    """
    Build one `b.parquet` row-group batch for original row positions
    `[start, end)`, applying deletions and modifications and updating
    `counters` in place.

    :param start: First row position in this chunk (inclusive).
    :param end: Last row position in this chunk (exclusive).
    :param raw_values: This chunk's `(category, amount, payload)` values,
        aligned to `range(start, end)`, as built for `a.parquet`.
    :param delete_indices: Row positions excluded from `b.parquet`.
    :param modify_amount_indices: Row positions whose `amount` changes.
    :param modify_payload_indices: Row positions whose `payload` changes.
    :param counters: Running sidecar counters, updated in place.
    :param rng: Seeded random source, shared with every other chunk.
    :return: The `b.parquet` batch (fewer rows than `a.parquet`'s when this
        chunk contains a deletion).
    """
    ids: list[int] = []
    ts_values: list[int] = []
    categories: list[str] = []
    amounts: list[Decimal] = []
    payloads: list[str] = []

    for i in range(start, end):
        if i in delete_indices:
            counters.deleted += 1
            continue

        category, amount, payload = raw_values[i - start]

        if i in modify_amount_indices:
            amount = _random_amount(rng, _CHANGED_AMOUNT_UNITS)
            counters.modified_amount += 1
        elif i in modify_payload_indices:
            payload = _random_payload(rng, prefix=_CHANGED_PAYLOAD_PREFIX)
            counters.modified_payload += 1
        else:
            counters.unchanged += 1

        ids.append(i)
        ts_values.append((BASE_EPOCH_US + i * TS_STEP_US) // 1000)
        categories.append(category)
        amounts.append(amount)
        payloads.append(payload)

    category_array = pa.array(categories, type=pa.string()).cast(pa.dictionary(pa.int32(), pa.string()))

    return pa.record_batch(
        [
            pa.array(ids, type=pa.int64()),
            pa.array(ts_values, type=pa.int64()).cast(pa.timestamp("ms", tz="UTC")),
            category_array,
            pa.array(amounts, type=pa.decimal128(18, 4)),
            pa.array(payloads, type=pa.string()),
            pa.array([None] * len(ids), type=pa.string()),
        ],
        names=["id", "ts", "category", "amount", "payload", ADDED_COLUMN],
    )


def _build_added_chunk(start_id: int, count: int, rng: random.Random) -> pa.RecordBatch:
    """
    Build one `b.parquet`-only row-group batch of brand-new rows, appended
    after every original row, with fresh ascending ids and `note="added"`.

    :param start_id: First id to assign (must be greater than every
        original id, so `id` stays ascending across the two segments).
    :param count: Number of new rows in this chunk.
    :param rng: Seeded random source, shared with every other chunk.
    :return: The batch, using `b.parquet`'s schema.
    """
    ids = list(range(start_id, start_id + count))
    ts_values = [(BASE_EPOCH_US + i * TS_STEP_US) // 1000 for i in ids]
    categories = [CATEGORY_VALUES[rng.randrange(len(CATEGORY_VALUES))] for _ in ids]
    amounts = [_random_amount(rng, _ADDED_AMOUNT_UNITS) for _ in ids]
    payloads = [_random_payload(rng, prefix=_ADDED_PAYLOAD_PREFIX) for _ in ids]
    category_array = pa.array(categories, type=pa.string()).cast(pa.dictionary(pa.int32(), pa.string()))

    return pa.record_batch(
        [
            pa.array(ids, type=pa.int64()),
            pa.array(ts_values, type=pa.int64()).cast(pa.timestamp("ms", tz="UTC")),
            category_array,
            pa.array(amounts, type=pa.decimal128(18, 4)),
            pa.array(payloads, type=pa.string()),
            pa.array(["added"] * count, type=pa.string()),
        ],
        names=["id", "ts", "category", "amount", "payload", ADDED_COLUMN],
    )


##############################################
##############################################
##############################################
##############################################
# Top-level generation + manifest


def _schema_a() -> pa.Schema:
    """:return: `a.parquet`'s schema."""
    return pa.schema(
        [
            ("id", pa.int64()),
            ("ts", pa.timestamp("us", tz="UTC")),
            ("category", pa.string()),
            ("amount", pa.decimal128(18, 4)),
            ("payload", pa.string()),
        ],
    )


def _schema_b() -> pa.Schema:
    """:return: `b.parquet`'s schema."""
    return pa.schema(
        [
            ("id", pa.int64()),
            ("ts", pa.timestamp("ms", tz="UTC")),
            ("category", pa.dictionary(pa.int32(), pa.string())),
            ("amount", pa.decimal128(18, 4)),
            ("payload", pa.string()),
            (ADDED_COLUMN, pa.string()),
        ],
    )


def _stream_fixture_pair(
    rows: int,
    row_group_size: int,
    added_rows: int,
    out_dir: Path,
    schema_a: pa.Schema,
    schema_b: pa.Schema,
    build_chunk_pair: Callable[[int, int], tuple[pa.RecordBatch, pa.RecordBatch]],
    build_added_chunk: Callable[[int, int], pa.RecordBatch],
) -> None:
    """
    Stream `rows` original rows, row-group by row-group, then `added_rows`
    brand-new rows, to `out_dir/a.parquet`/`out_dir/b.parquet` -- the
    skeleton both `generate_narrow` and `generate_wide` share (mkdir, both
    `ParquetWriter`s, the two loops, the `try`/`finally` close). Callers
    supply the per-chunk builders; this function makes no RNG draws of its
    own, so a caller's determinism is unaffected by using it.

    :param rows: Number of original rows.
    :param row_group_size: Rows per written batch.
    :param added_rows: Number of `b`-only new rows to append after `rows`.
    :param out_dir: Directory to write into (created if missing).
    :param schema_a: `a.parquet`'s schema.
    :param schema_b: `b.parquet`'s schema.
    :param build_chunk_pair: `(start, end)` -> `(batch_a, batch_b)` for one
        row-group of original rows.
    :param build_added_chunk: `(start_id, count)` -> one `b`-only batch of
        brand-new rows.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    writer_a = pq.ParquetWriter(out_dir / "a.parquet", schema_a)
    writer_b = pq.ParquetWriter(out_dir / "b.parquet", schema_b)
    try:
        for start in range(0, rows, row_group_size):
            end = min(start + row_group_size, rows)
            batch_a, batch_b = build_chunk_pair(start, end)
            writer_a.write_batch(batch_a)
            writer_b.write_batch(batch_b)

        for start in range(0, added_rows, row_group_size):
            count = min(row_group_size, added_rows - start)
            writer_b.write_batch(build_added_chunk(rows + start, count))
    finally:
        writer_a.close()
        writer_b.close()


def generate_narrow(rows: int, seed: int, out_dir: Path) -> dict[str, object]:
    """
    Stream the fixture pair to `out_dir/a.parquet` and `out_dir/b.parquet`,
    write `out_dir/manifest.json`, and return the manifest document.

    :param rows: Number of rows in `a.parquet` before any mutation.
    :param seed: RNG seed; the same seed always produces byte-identical output.
    :param out_dir: Directory to write into (created if missing).
    :return: The manifest document (also written to `manifest.json`).
    """
    rng = random.Random(seed)
    delete_indices, modify_amount_indices, modify_payload_indices = _select_mutated_indices(rows, rng)
    counters = _Counters()
    added_n = round(rows * ADD_RATE)
    counters.added = added_n

    def build_chunk_pair(start: int, end: int) -> tuple[pa.RecordBatch, pa.RecordBatch]:
        batch_a, raw_values = _build_original_chunk(start, end, rng)
        batch_b = _build_changed_chunk(
            start, end, raw_values, delete_indices, modify_amount_indices, modify_payload_indices, counters, rng,
        )
        return batch_a, batch_b

    _stream_fixture_pair(
        rows, ROW_GROUP_SIZE, added_n, out_dir, _schema_a(), _schema_b(),
        build_chunk_pair, lambda start_id, count: _build_added_chunk(start_id, count, rng),
    )

    manifest: dict[str, object] = {
        "seed": seed,
        "rows": rows,
        "rows_deleted": counters.deleted,
        "rows_added": counters.added,
        "rows_modified_amount": counters.modified_amount,
        "rows_modified_payload": counters.modified_payload,
        "rows_unchanged": counters.unchanged,
        "duplicate_keys": 0,
        "schema_changes": [
            {"column": "category", "change": "type_changed", "left_type": "string", "right_type": "dictionary<int32, string>"},
            {"column": "ts", "change": "type_changed", "left_type": "timestamp[us, UTC]", "right_type": "timestamp[ms, UTC]"},
            {"column": ADDED_COLUMN, "change": "added", "left_type": None, "right_type": "string"},
        ],
    }
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n", encoding="utf-8")

    return manifest


##############################################
##############################################
##############################################
##############################################
# Wide-kind fixture (#84): full cell-type surface

WIDE_DEFAULT_SEED: Final[int] = 20260905
# Row density (~296 bytes/row for `a.parquet`, measured at 200,000 rows, after
# adding the float16/decimal32/decimal64/view/fixed-size-binary columns) is
# linear, the same convention `DEFAULT_ROWS` above was tuned with -- see
# README.md's "Sizes" section for the measurement this constant solves for.
WIDE_DEFAULT_ROWS: Final[int] = 16_875_000

WIDE_DELETE_RATE: Final[float] = 0.01
WIDE_ADD_RATE: Final[float] = 0.01
WIDE_MODIFY_RATE: Final[float] = 0.01
WIDE_NULL_RATE: Final[float] = 0.03
WIDE_BECOME_NON_NULL_RATE: Final[float] = 0.3  # fraction of a column's null rows
WIDE_BECOME_NULL_RATE: Final[float] = 0.01  # fraction of a column's non-null rows

WIDE_ROW_GROUP_SIZE: Final[int] = 50_000
WIDE_ADDED_COLUMN: Final[str] = "extra"
WIDE_CATEGORY_VALUES: Final[tuple[str, ...]] = tuple(f"wcat_{i:02d}" for i in range(20))

_WIDE_STR_ALPHABET: Final[str] = string.ascii_lowercase + string.digits
_WIDE_CHANGED_PREFIX: Final[str] = "chg_"
_WIDE_NONNULL_PREFIX: Final[str] = "fromnull_"
_WIDE_FIXED_BINARY_LEN: Final[int] = 4


def _wide_random_str(rng: random.Random, min_len: int, max_len: int, prefix: str = "") -> str:
    """
    Draw a random string of `min_len`-`max_len` chars, optionally marker-prefixed.

    :param rng: Seeded random source.
    :param min_len: Minimum total length, prefix included.
    :param max_len: Maximum total length, prefix included.
    :param prefix: Prepended marker; must not reuse `_WIDE_STR_ALPHABET`'s characters.
    :return: The generated string.
    """
    length = rng.randint(min_len, max_len)
    body_len = max(0, length - len(prefix))
    return prefix + "".join(rng.choices(_WIDE_STR_ALPHABET, k=body_len))


def _wide_fixed_binary(rng: random.Random, first_byte_range: tuple[int, int]) -> bytes:
    """
    Draw a `_WIDE_FIXED_BINARY_LEN`-byte value; the first byte comes from
    `first_byte_range`, so an original and a changed draw can never collide
    by construction (disjoint first-byte ranges), the rest is arbitrary.

    :param rng: Seeded random source.
    :param first_byte_range: `(low, high)` bounds, inclusive, for the first byte.
    :return: The generated fixed-size byte string.
    """
    first = rng.randint(*first_byte_range)
    rest = [rng.randint(0, 255) for _ in range(_WIDE_FIXED_BINARY_LEN - 1)]
    return bytes([first, *rest])


@dataclass(frozen=True)
class _ColumnSpec:
    """
    One `wide` non-key column: its on-disk type(s), value generation, and
    which per-cell mutations it participates in.

    :param name: Column name.
    :param a_type: The Arrow type written to `a.parquet`.
    :param b_type: The Arrow type written to `b.parquet` (a schema change
        from `a_type` for the three retyped/rescaled/cast columns; equal to
        `a_type` for every other column).
    :param generate: Draws one fresh, non-null raw value.
    :param nullable: Whether this column carries `WIDE_NULL_RATE` nulls.
    :param changed: Draws a replacement value guaranteed different from the
        original, for the `WIDE_MODIFY_RATE` subset of rows tracked as this
        column's `value_changed` count. `None` if this column is never
        independently modified.
    :param non_null: Draws a fresh non-null value for the subset of
        originally-null rows tracked as this column's `became_non_null`
        count. `None` if this column carries no null-transition mutation.
    """

    name: str
    a_type: pa.DataType
    b_type: pa.DataType
    generate: Callable[[random.Random], object]
    nullable: bool
    changed: Callable[[random.Random, object], object] | None = None
    non_null: Callable[[random.Random], object] | None = None


def _wide_column_specs() -> list[_ColumnSpec]:
    """:return: Every `wide` non-key column, in schema order."""
    return [
        _ColumnSpec(
            "i8", pa.int8(), pa.int8(),
            lambda r: r.randint(-100, 100), True,
            changed=lambda r, _v: r.randint(101, 127),
        ),
        _ColumnSpec(
            "i16", pa.int16(), pa.int16(),
            lambda r: r.randint(-30_000, 30_000), True,
            non_null=lambda r: r.randint(-30_000, 30_000),
        ),
        _ColumnSpec(
            "i32", pa.int32(), pa.int32(),
            lambda r: r.randint(-2_000_000_000, 2_000_000_000), True,
            changed=lambda r, _v: r.randint(2_000_000_001, 2_147_483_647),
        ),
        _ColumnSpec(
            "i64", pa.int64(), pa.int64(),
            lambda r: r.randint(-(2**62), 2**62), True,
            changed=lambda r, _v: r.randint(2**62 + 1, 2**63 - 1),
        ),
        _ColumnSpec(
            "u8", pa.uint8(), pa.uint8(),
            lambda r: r.randint(0, 200), True,
            changed=lambda r, _v: r.randint(201, 255),
        ),
        _ColumnSpec("u16", pa.uint16(), pa.uint16(), lambda r: r.randint(0, 65_535), True),
        _ColumnSpec(
            "u32", pa.uint32(), pa.uint32(),
            lambda r: r.randint(0, 4_000_000_000), True,
            changed=lambda r, _v: r.randint(4_000_000_001, 4_294_967_295),
        ),
        _ColumnSpec("u64", pa.uint64(), pa.uint64(), lambda r: r.randint(0, 2**63 - 1), True),
        _ColumnSpec(
            "f32", pa.float32(), pa.float32(),
            lambda r: _wide_special_float(r), True,
            changed=lambda r, _v: r.uniform(2_000.0, 3_000.0),
        ),
        _ColumnSpec(
            "f64", pa.float64(), pa.float64(),
            lambda r: _wide_special_float(r), True,
            changed=lambda r, _v: r.uniform(2_000.0, 3_000.0),
        ),
        _ColumnSpec(
            "float16_col", pa.float16(), pa.float16(),
            lambda r: r.uniform(-1_000.0, 1_000.0), True,
            changed=lambda r, _v: r.uniform(2_000.0, 3_000.0),
        ),
        _ColumnSpec(
            "dec_scale4", pa.decimal128(18, 4), pa.decimal128(18, 6),
            lambda r: Decimal(r.randint(0, 1_000_000_000)).scaleb(-4), True,
            changed=lambda r, _v: Decimal(r.randint(2_000_000_000, 3_000_000_000)).scaleb(-4),
        ),
        _ColumnSpec("dec_scale10", pa.decimal128(38, 10), pa.decimal128(38, 10),
                    lambda r: Decimal(r.randint(0, 1_000_000_000)).scaleb(-10), True),
        _ColumnSpec("dec256", pa.decimal256(38, 10), pa.decimal256(38, 10),
                    lambda r: Decimal(r.randint(0, 1_000_000_000)).scaleb(-10), True),
        _ColumnSpec(
            "decimal32_col", pa.decimal32(5, 2), pa.decimal32(5, 2),
            lambda r: Decimal(r.randint(0, 50_000)).scaleb(-2), True,
            changed=lambda r, _v: Decimal(r.randint(60_000, 99_999)).scaleb(-2),
        ),
        _ColumnSpec(
            "decimal64_col", pa.decimal64(10, 2), pa.decimal64(10, 2),
            lambda r: Decimal(r.randint(0, 1_000_000_000)).scaleb(-2), True,
            changed=lambda r, _v: Decimal(r.randint(2_000_000_000, 3_000_000_000)).scaleb(-2),
        ),
        _ColumnSpec(
            "utf8_col", pa.string(), pa.string(),
            lambda r: _wide_random_str(r, 5, 40), True,
            changed=lambda r, _v: _wide_random_str(r, 5, 40, prefix=_WIDE_CHANGED_PREFIX),
        ),
        _ColumnSpec(
            "large_utf8_col", pa.large_string(), pa.large_string(),
            lambda r: _wide_random_str(r, 5, 40), True,
            non_null=lambda r: _wide_random_str(r, 5, 40, prefix=_WIDE_NONNULL_PREFIX),
        ),
        _ColumnSpec(
            "utf8_view_col", pa.string_view(), pa.string_view(),
            lambda r: _wide_random_str(r, 5, 40), True,
            changed=lambda r, _v: _wide_random_str(r, 5, 40, prefix=_WIDE_CHANGED_PREFIX),
        ),
        _ColumnSpec(
            "binary_col", pa.binary(), pa.binary(),
            lambda r: _wide_random_str(r, 5, 40).encode("ascii"), True,
            changed=lambda r, _v: _wide_random_str(r, 5, 40, prefix=_WIDE_CHANGED_PREFIX).encode("ascii"),
        ),
        _ColumnSpec(
            "binary_view_col", pa.binary_view(), pa.binary_view(),
            lambda r: _wide_random_str(r, 5, 40).encode("ascii"), True,
            changed=lambda r, _v: _wide_random_str(r, 5, 40, prefix=_WIDE_CHANGED_PREFIX).encode("ascii"),
        ),
        _ColumnSpec(
            "fixed_size_binary_col", pa.binary(_WIDE_FIXED_BINARY_LEN), pa.binary(_WIDE_FIXED_BINARY_LEN),
            lambda r: _wide_fixed_binary(r, (0, 99)), True,
            changed=lambda r, _v: _wide_fixed_binary(r, (200, 255)),
        ),
        _ColumnSpec(
            "bool_col", pa.bool_(), pa.bool_(),
            lambda r: r.random() < 0.5, True,
            changed=lambda _r, v: not v,
        ),
        _ColumnSpec(
            "date32_col", pa.date32(), pa.date32(),
            lambda r: r.randint(0, 10_000), True,
            changed=lambda r, _v: r.randint(20_000, 30_000),
        ),
        _ColumnSpec("date64_millis", pa.int64(), pa.int64(),
                    lambda r: r.randint(0, 10_000) * 86_400_000, True),
        _ColumnSpec("time32_col", pa.time32("ms"), pa.time32("ms"),
                    lambda r: r.randint(0, 86_399_999), True),
        _ColumnSpec(
            "time64_col", pa.time64("us"), pa.time64("us"),
            lambda r: r.randint(0, 40_000_000_000), True,
            changed=lambda r, _v: r.randint(50_000_000_000, 86_399_999_999),
        ),
        _ColumnSpec("dur_a", pa.duration("s"), pa.duration("s"), lambda r: r.randint(0, 100_000), True),
        _ColumnSpec(
            "dur_b", pa.duration("ns"), pa.duration("ns"),
            lambda r: r.randint(0, 1_000_000_000), True,
            changed=lambda r, _v: r.randint(2_000_000_000, 3_000_000_000),
        ),
        _ColumnSpec("interval_months", pa.int32(), pa.int32(), lambda r: r.randint(0, 24), False),
        _ColumnSpec("interval_days", pa.int32(), pa.int32(), lambda r: r.randint(0, 28), False),
        _ColumnSpec("interval_nanos", pa.int64(), pa.int64(), lambda r: r.randint(0, 86_400_000_000_000), False),
        _ColumnSpec("category", pa.string(), pa.dictionary(pa.int32(), pa.string()),
                    lambda r: WIDE_CATEGORY_VALUES[r.randrange(len(WIDE_CATEGORY_VALUES))], True),
        _ColumnSpec(
            "ts_cast",
            pa.timestamp("ns", tz="UTC"),
            pa.timestamp("us"),
            lambda r: r.randint(1_700_000_000, 1_800_000_000) * 1_000_000_000,
            False,
        ),
    ]


def _wide_special_float(rng: random.Random) -> float:
    """
    Draw a plain float in `[-1000, 1000)`, occasionally replaced by NaN or
    signed zero -- see the module docstring's "wide" section.

    :param rng: Seeded random source.
    :return: The drawn value.
    """
    draw = rng.random()
    if draw < 0.02:
        return float("nan")
    if draw < 0.04:
        return -0.0
    return rng.uniform(-1000.0, 1000.0)


# Interval's three raw components (see the module docstring) are one
# logical value: their null status must move together, which is why they
# are not three independent `_ColumnSpec`s.
_INTERVAL_COLUMNS: Final[tuple[str, ...]] = ("interval_months", "interval_days", "interval_nanos")
_NULL_TRANSITION_COLUMNS: Final[tuple[str, ...]] = ("i16", "large_utf8_col")


class _WideRowPlan:
    """
    Every row-position index set the `wide` generator needs, sampled once
    from the shared RNG in a fixed, documented order so a re-run with the
    same seed reproduces the same plan.
    """

    def __init__(self: Self, rows: int, rng: random.Random, specs: list[_ColumnSpec]) -> None:
        """
        :param rows: Row count in `a.parquet`.
        :param rng: Seeded random source (mutated in place).
        :param specs: Every non-key column spec, in schema order.
        """
        delete_n = round(rows * WIDE_DELETE_RATE)
        self.delete_indices: set[int] = set(rng.sample(range(rows), delete_n))
        self.modify_indices: dict[str, set[int]] = {}
        self.null_indices: dict[str, set[int]] = {}
        self.null_to_value_indices: dict[str, set[int]] = {}
        self.value_to_null_indices: dict[str, set[int]] = {}

        for spec in specs:
            if spec.nullable:
                null_n = round(rows * WIDE_NULL_RATE)
                self.null_indices[spec.name] = set(rng.sample(range(rows), null_n))
            if spec.changed is not None:
                modify_n = round(rows * WIDE_MODIFY_RATE)
                self.modify_indices[spec.name] = set(rng.sample(range(rows), modify_n))
            if spec.non_null is not None:
                null_pool = sorted(self.null_indices[spec.name])
                become_non_null_n = round(len(null_pool) * WIDE_BECOME_NON_NULL_RATE)
                self.null_to_value_indices[spec.name] = set(rng.sample(null_pool, become_non_null_n))
                become_null_n = round(rows * WIDE_BECOME_NULL_RATE)
                self.value_to_null_indices[spec.name] = set(rng.sample(range(rows), become_null_n))

        # Interval's three components share one null mask, sampled after
        # every per-spec draw above so the fixed draw order stays stable
        # regardless of `specs`' contents.
        interval_null_n = round(rows * WIDE_NULL_RATE)
        interval_null = set(rng.sample(range(rows), interval_null_n))
        for name in _INTERVAL_COLUMNS:
            self.null_indices[name] = interval_null


class _WideCounters:
    """Running exact per-column mutation counts for the `wide` sidecar manifest."""

    def __init__(self: Self, specs: list[_ColumnSpec]) -> None:
        """:param specs: Every non-key column spec, used to zero-initialize its counters."""
        self.deleted = 0
        self.added = 0
        self.value_changed: dict[str, int] = {s.name: 0 for s in specs if s.changed is not None}
        self.became_non_null: dict[str, int] = {s.name: 0 for s in specs if s.non_null is not None}
        self.became_null: dict[str, int] = {s.name: 0 for s in specs if s.non_null is not None}


def _wide_row_values(
    start: int,
    end: int,
    specs: list[_ColumnSpec],
    plan: _WideRowPlan,
    counters: _WideCounters,
    rng: random.Random,
) -> tuple[dict[str, list[object]], dict[str, list[object]]]:
    """
    Build one row-group's raw per-column values for both sides, updating
    `counters` in place.

    :param start: First row position in this chunk (inclusive).
    :param end: Last row position in this chunk (exclusive).
    :param specs: Every non-key column spec, in schema order.
    :param plan: The precomputed index sets driving every row's outcome.
    :param counters: Running sidecar counters, updated in place.
    :param rng: Seeded random source, shared with every other chunk.
    :return: `(a_values, b_values)`, each column name mapped to its list of
        raw values (`b_values` has one entry per surviving row only).
    """
    a_values: dict[str, list[object]] = {s.name: [] for s in specs}
    b_values: dict[str, list[object]] = {s.name: [] for s in specs}

    for i in range(start, end):
        row_a: dict[str, object] = {}
        for spec in specs:
            null_set = plan.null_indices.get(spec.name)
            value = None if null_set is not None and i in null_set else spec.generate(rng)
            row_a[spec.name] = value
            a_values[spec.name].append(value)

        if i in plan.delete_indices:
            counters.deleted += 1
            continue

        for spec in specs:
            a_val = row_a[spec.name]
            if spec.name == "ts_cast":
                b_val = a_val
            elif a_val is None:
                if spec.name in plan.null_to_value_indices and i in plan.null_to_value_indices[spec.name]:
                    b_val = spec.non_null(rng)  # type: ignore[misc]
                    counters.became_non_null[spec.name] += 1
                else:
                    b_val = None
            elif spec.changed is not None and i in plan.modify_indices.get(spec.name, ()):
                b_val = spec.changed(rng, a_val)
                counters.value_changed[spec.name] += 1
            elif spec.name in plan.value_to_null_indices and i in plan.value_to_null_indices[spec.name]:
                b_val = None
                counters.became_null[spec.name] += 1
            else:
                b_val = a_val
            b_values[spec.name].append(b_val)

    return a_values, b_values


def _wide_build_array(spec: _ColumnSpec, values: list[object], target_type: pa.DataType) -> pa.Array:
    """
    Build `values` as `spec.a_type`, casting to `target_type` when it differs
    (a schema-level retype/rescale/unit-and-zone change applies to the whole
    column, never per value -- see the module docstring).

    :param spec: The column spec `values` belongs to.
    :param values: Raw per-row values, `spec.a_type`-shaped.
    :param target_type: `spec.a_type` (the `a.parquet` array) or `spec.b_type`
        (the `b.parquet` array).
    :return: The built (and possibly cast) array.
    """
    array = pa.array(values, type=spec.a_type)
    if target_type != spec.a_type:
        array = array.cast(target_type)
    return array


def _wide_schema(specs: list[_ColumnSpec], side: str) -> pa.Schema:
    """
    :param specs: Every non-key column spec.
    :param side: `"a"` or `"b"`.
    :return: The side's schema (key column first, `extra` last on `"b"`).
    """
    fields = [("id", pa.int64())]
    fields += [(s.name, s.a_type if side == "a" else s.b_type) for s in specs]
    if side == "b":
        fields.append((WIDE_ADDED_COLUMN, pa.string()))
    return pa.schema(fields)


def _wide_added_chunk(specs: list[_ColumnSpec], start_id: int, count: int, rng: random.Random) -> pa.RecordBatch:
    """
    Build one `b.parquet`-only row-group of brand-new rows: fresh values for
    every column (never null, never a tracked mutation), `extra="added"`.

    :param specs: Every non-key column spec.
    :param start_id: First id to assign.
    :param count: Number of new rows in this chunk.
    :param rng: Seeded random source, shared with every other chunk.
    :return: The batch, using `b.parquet`'s schema.
    """
    columns: list[pa.Array] = [pa.array(range(start_id, start_id + count), type=pa.int64())]
    for spec in specs:
        values = [spec.generate(rng) for _ in range(count)]
        columns.append(_wide_build_array(spec, values, spec.b_type))
    columns.append(pa.array(["added"] * count, type=pa.string()))
    return pa.record_batch(columns, schema=_wide_schema(specs, "b"))


def generate_wide(rows: int, seed: int, out_dir: Path) -> dict[str, object]:
    """
    Stream the `wide`-kind fixture pair to `out_dir`, the same contract as
    `generate` (streaming, seeded, byte-identical on re-run) -- see the
    module docstring's "wide" section for the column set and mutation mix.

    :param rows: Number of rows in `a.parquet` before any mutation.
    :param seed: RNG seed.
    :param out_dir: Directory to write into (created if missing).
    :return: The manifest document (also written to `manifest.json`).
    """
    rng = random.Random(seed)
    specs = _wide_column_specs()
    plan = _WideRowPlan(rows, rng, specs)
    counters = _WideCounters(specs)
    added_n = round(rows * WIDE_ADD_RATE)
    counters.added = added_n

    def build_chunk_pair(start: int, end: int) -> tuple[pa.RecordBatch, pa.RecordBatch]:
        a_values, b_values = _wide_row_values(start, end, specs, plan, counters, rng)

        a_columns = [pa.array(range(start, end), type=pa.int64())]
        a_columns += [_wide_build_array(s, a_values[s.name], s.a_type) for s in specs]
        batch_a = pa.record_batch(a_columns, schema=_wide_schema(specs, "a"))

        surviving_ids = [i for i in range(start, end) if i not in plan.delete_indices]
        b_columns = [pa.array(surviving_ids, type=pa.int64())]
        b_columns += [_wide_build_array(s, b_values[s.name], s.b_type) for s in specs]
        b_columns.append(pa.array([None] * len(surviving_ids), type=pa.string()))
        batch_b = pa.record_batch(b_columns, schema=_wide_schema(specs, "b"))

        return batch_a, batch_b

    _stream_fixture_pair(
        rows, WIDE_ROW_GROUP_SIZE, added_n, out_dir, _wide_schema(specs, "a"), _wide_schema(specs, "b"),
        build_chunk_pair, lambda start_id, count: _wide_added_chunk(specs, start_id, count, rng),
    )

    manifest = _wide_manifest(rows, seed, specs, counters)
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n", encoding="utf-8")
    return manifest


def _wide_manifest(rows: int, seed: int, specs: list[_ColumnSpec], counters: _WideCounters) -> dict[str, object]:
    """
    :param rows: Row count in `a.parquet`.
    :param seed: RNG seed.
    :param specs: Every non-key column spec.
    :param counters: The run's realized mutation counts.
    :return: The manifest document.
    """
    schema_changes = [
        {"column": "category", "change": "type_changed", "left_type": "string", "right_type": "dictionary<int32, string>"},
        {"column": "dec_scale4", "change": "type_changed", "left_type": "decimal128(18, 4)", "right_type": "decimal128(18, 6)"},
        {"column": "ts_cast", "change": "type_changed", "left_type": "timestamp[ns, UTC]", "right_type": "timestamp[us]"},
        {"column": WIDE_ADDED_COLUMN, "change": "added", "left_type": None, "right_type": "string"},
    ]
    return {
        "kind": "wide",
        "seed": seed,
        "rows": rows,
        "rows_deleted": counters.deleted,
        "rows_added": counters.added,
        "duplicate_keys": 0,
        "value_changed_per_column": dict(sorted(counters.value_changed.items())),
        "became_non_null_per_column": dict(sorted(counters.became_non_null.items())),
        "became_null_per_column": dict(sorted(counters.became_null.items())),
        "cells_value_changed": sum(counters.value_changed.values()),
        "cells_became_non_null": sum(counters.became_non_null.values()),
        "cells_became_null": sum(counters.became_null.values()),
        "cells_type_changed": rows - counters.deleted,
        "schema_changes": schema_changes,
    }


def generate(rows: int, seed: int, out_dir: Path, kind: str = "narrow") -> dict[str, object]:
    """
    Dispatch to `generate_narrow` or `generate_wide` by `kind`.

    :param rows: Number of rows in `a.parquet` before any mutation.
    :param seed: RNG seed; the same seed always produces byte-identical output.
    :param out_dir: Directory to write into (created if missing).
    :param kind: `"narrow"` (the original five-column fixture) or `"wide"`
        (#84's full cell-type-surface fixture).
    :return: The manifest document (also written to `manifest.json`).
    """
    if kind == "wide":
        return generate_wide(rows, seed, out_dir)
    return generate_narrow(rows, seed, out_dir)


def main() -> None:
    """Parse CLI arguments and generate one fixture pair."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--kind", choices=("narrow", "wide"), default="narrow", help="Fixture kind.")
    parser.add_argument("--rows", type=int, default=None, help="Row count for a.parquet (defaults per --kind).")
    parser.add_argument("--out", type=Path, required=True, help="Output directory.")
    parser.add_argument("--seed", type=int, default=None, help="RNG seed (defaults per --kind).")
    args = parser.parse_args()

    default_rows = WIDE_DEFAULT_ROWS if args.kind == "wide" else DEFAULT_ROWS
    default_seed = WIDE_DEFAULT_SEED if args.kind == "wide" else DEFAULT_SEED
    rows = args.rows if args.rows is not None else default_rows
    seed = args.seed if args.seed is not None else default_seed

    manifest = generate(rows, seed, args.out, kind=args.kind)
    a_bytes = (args.out / "a.parquet").stat().st_size
    b_bytes = (args.out / "b.parquet").stat().st_size
    print(f"Wrote {rows:,} base rows to {args.out} (a={a_bytes / 1_000_000:.1f} MB, b={b_bytes / 1_000_000:.1f} MB)")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
