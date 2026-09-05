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
every draw in a fixed order, row order is always construction order, and
parquet is written with fixed writer options (see `_write_pair`). Two runs
of this script with the same `--rows`/`--seed` must produce byte-identical
`a.parquet`/`b.parquet`/`manifest.json`. To prove it:

    cd perf/arrow
    uv run --group perf generate_fixtures.py --rows 100000 --out /tmp/run1
    uv run --group perf generate_fixtures.py --rows 100000 --out /tmp/run2
    diff <(shasum -a 256 /tmp/run1/* | cut -d' ' -f1) <(shasum -a 256 /tmp/run2/* | cut -d' ' -f1)

Usage::

    cd perf/arrow
    uv sync --group perf
    uv run --group perf generate_fixtures.py --rows 1000 --out fixtures/1k
"""

import argparse
import json
import random
import string
from decimal import Decimal
from pathlib import Path
from typing import Final

import pyarrow as pa
import pyarrow.parquet as pq

from _common import (
    ADDED_COLUMN,
    SIDECAR_DUPLICATE_KEYS,
    SIDECAR_ROWS,
    SIDECAR_ROWS_ADDED,
    SIDECAR_ROWS_DELETED,
    SIDECAR_ROWS_MODIFIED_AMOUNT,
    SIDECAR_ROWS_MODIFIED_PAYLOAD,
    SIDECAR_ROWS_UNCHANGED,
    SIDECAR_SCHEMA_CHANGES,
    SIDECAR_SEED,
)

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

    def __init__(self: "_Counters") -> None:
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


def generate(rows: int, seed: int, out_dir: Path) -> dict[str, object]:
    """
    Stream the fixture pair to `out_dir/a.parquet` and `out_dir/b.parquet`,
    write `out_dir/manifest.json`, and return the manifest document.

    :param rows: Number of rows in `a.parquet` before any mutation.
    :param seed: RNG seed; the same seed always produces byte-identical output.
    :param out_dir: Directory to write into (created if missing).
    :return: The manifest document (also written to `manifest.json`).
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    rng = random.Random(seed)
    delete_indices, modify_amount_indices, modify_payload_indices = _select_mutated_indices(rows, rng)
    counters = _Counters()

    writer_a = pq.ParquetWriter(out_dir / "a.parquet", _schema_a())
    writer_b = pq.ParquetWriter(out_dir / "b.parquet", _schema_b())

    try:
        for start in range(0, rows, ROW_GROUP_SIZE):
            end = min(start + ROW_GROUP_SIZE, rows)
            batch_a, raw_values = _build_original_chunk(start, end, rng)
            writer_a.write_batch(batch_a)
            batch_b = _build_changed_chunk(
                start,
                end,
                raw_values,
                delete_indices,
                modify_amount_indices,
                modify_payload_indices,
                counters,
                rng,
            )
            writer_b.write_batch(batch_b)

        added_n = round(rows * ADD_RATE)
        counters.added = added_n

        for start in range(0, added_n, ROW_GROUP_SIZE):
            count = min(ROW_GROUP_SIZE, added_n - start)
            writer_b.write_batch(_build_added_chunk(rows + start, count, rng))
    finally:
        writer_a.close()
        writer_b.close()

    manifest: dict[str, object] = {
        SIDECAR_SEED: seed,
        SIDECAR_ROWS: rows,
        SIDECAR_ROWS_DELETED: counters.deleted,
        SIDECAR_ROWS_ADDED: counters.added,
        SIDECAR_ROWS_MODIFIED_AMOUNT: counters.modified_amount,
        SIDECAR_ROWS_MODIFIED_PAYLOAD: counters.modified_payload,
        SIDECAR_ROWS_UNCHANGED: counters.unchanged,
        SIDECAR_DUPLICATE_KEYS: 0,
        SIDECAR_SCHEMA_CHANGES: [
            {"column": "category", "change": "type_changed", "left_type": "string", "right_type": "dictionary<int32, string>"},
            {"column": "ts", "change": "type_changed", "left_type": "timestamp[us, UTC]", "right_type": "timestamp[ms, UTC]"},
            {"column": ADDED_COLUMN, "change": "added", "left_type": None, "right_type": "string"},
        ],
    }
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n", encoding="utf-8")

    return manifest


def main() -> None:
    """Parse CLI arguments and generate one fixture pair."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--rows", type=int, default=DEFAULT_ROWS, help="Row count for a.parquet.")
    parser.add_argument("--out", type=Path, required=True, help="Output directory.")
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED, help="RNG seed.")
    args = parser.parse_args()

    manifest = generate(args.rows, args.seed, args.out)
    a_bytes = (args.out / "a.parquet").stat().st_size
    b_bytes = (args.out / "b.parquet").stat().st_size
    print(f"Wrote {args.rows:,} base rows to {args.out} (a={a_bytes / 1_000_000:.1f} MB, b={b_bytes / 1_000_000:.1f} MB)")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
