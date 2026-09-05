"""Tests for the keyed row diff: added/removed/changed/duplicate/null rows, oracle parity, and a property test."""

import decimal
import random
from collections import defaultdict
from pathlib import Path

import polars as pl
import pyarrow as pa
import pyarrow.parquet as pq
import pytest

from deepdiff_rs import diff_tables
from generate_fixtures import generate
from oracle_duckdb import run as oracle_run

# Helpers


def _table(diff_member: object) -> pa.Table:
    """Materialize a diff's Arrow-table result member as a pyarrow Table."""
    return pa.table(diff_member)


def _ids(table: pa.Table) -> set:
    """The set of ``id`` values in a result table."""
    return set(table.column("id").to_pylist())


# Basic row-diff tests


def test_added_removed_changed_and_unchanged() -> None:
    """A mix of added, removed, changed, and unchanged rows is classified correctly."""
    left = pa.table({"id": pa.array([1, 2, 3], pa.int64()), "v": pa.array([10, 20, 30], pa.int64())})
    right = pa.table({"id": pa.array([2, 3, 4], pa.int64()), "v": pa.array([20, 31, 40], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    summary = diff.summary()
    assert summary["rows_added"] == 1
    assert summary["rows_removed"] == 1
    assert summary["rows_changed"] == 1
    assert summary["duplicate_keys"] == 0
    assert _ids(_table(diff.rows_added())) == {4}
    assert _ids(_table(diff.rows_removed())) == {1}


def test_identical_tables_report_no_row_changes() -> None:
    """Two identical tables report no added, removed, or changed rows."""
    left = pa.table({"id": pa.array([1, 2], pa.int64()), "v": pa.array([10, 20], pa.int64())})
    diff = diff_tables(left, left, key=["id"])

    assert diff.summary()["rows_added"] == 0
    assert diff.summary()["rows_removed"] == 0
    assert diff.summary()["rows_changed"] == 0
    assert len(diff.rows_added()) == 0


def test_rows_added_keeps_the_right_schema() -> None:
    """rows_added carries the right table's columns."""
    left = pa.table({"id": pa.array([1], pa.int64()), "v": pa.array([10], pa.int64())})
    right = pa.table({"id": pa.array([1, 2], pa.int64()), "v": pa.array([10, 20], pa.int64())})
    added = _table(diff_tables(left, right, key=["id"]).rows_added())

    assert added.column_names == ["id", "v"]
    assert added.num_rows == 1
    assert added.column("v").to_pylist() == [20]


def test_duplicate_key_is_reported_and_excluded() -> None:
    """A key repeated on one side is reported with per-side counts and excluded from added/removed."""
    left = pa.table({"id": pa.array([1, 1, 2], pa.int64()), "v": pa.array([10, 11, 20], pa.int64())})
    right = pa.table({"id": pa.array([2, 3], pa.int64()), "v": pa.array([20, 30], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    assert diff.summary()["duplicate_keys"] == 1
    assert diff.summary()["rows_removed"] == 0
    assert _ids(_table(diff.rows_added())) == {3}

    dup = _table(diff.duplicate_keys())
    assert dup.column("id").to_pylist() == [1]
    assert dup.column("left_count").to_pylist() == [2]
    assert dup.column("right_count").to_pylist() == [0]


def test_null_key_matches_itself_and_is_counted() -> None:
    """A null key matches its counterpart and is counted in null_keys."""
    left = pa.table({"id": pa.array([None, 1], pa.int64()), "v": pa.array([10, 20], pa.int64())})
    right = pa.table({"id": pa.array([None, 1], pa.int64()), "v": pa.array([11, 20], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    assert diff.summary()["null_keys"] == 1
    assert diff.summary()["rows_changed"] == 1
    assert diff.summary()["rows_added"] == 0


def test_composite_key_row_diff() -> None:
    """A composite key matches rows on all key columns together."""
    left = pa.table(
        {
            "a": pa.array([1, 1], pa.int64()),
            "b": pa.array([1, 2], pa.int64()),
            "v": pa.array([10, 20], pa.int64()),
        },
    )
    right = pa.table(
        {
            "a": pa.array([1, 1], pa.int64()),
            "b": pa.array([2, 3], pa.int64()),
            "v": pa.array([20, 30], pa.int64()),
        },
    )
    diff = diff_tables(left, right, key=["a", "b"])

    assert diff.summary()["rows_removed"] == 1
    assert diff.summary()["rows_added"] == 1
    assert diff.summary()["rows_changed"] == 0


def test_dictionary_and_timestamp_retype_do_not_flag_unchanged_rows() -> None:
    """A dictionary-retyped column and a cross-unit timestamp with equal values are not row changes."""
    left = pa.table(
        {
            "id": pa.array([1], pa.int64()),
            "c": pa.array(["x"], pa.string()),
            "t": pa.array([1_000_000], pa.timestamp("us", tz="UTC")),
        },
    )
    right = pa.table(
        {
            "id": pa.array([1], pa.int64()),
            "c": pa.array(["x"]).dictionary_encode(),
            "t": pa.array([1_000], pa.timestamp("ms", tz="UTC")),
        },
    )
    diff = diff_tables(left, right, key=["id"])

    assert diff.summary()["rows_changed"] == 0


def test_nested_non_key_column_is_skipped() -> None:
    """A nested (list) non-key column is out of scope: it is skipped, not compared, and the diff still runs."""
    left = pa.table({"id": pa.array([1], pa.int64()), "xs": pa.array([[1, 2]], pa.list_(pa.int64()))})
    right = pa.table({"id": pa.array([1], pa.int64()), "xs": pa.array([[9, 9]], pa.list_(pa.int64()))})
    diff = diff_tables(left, right, key=["id"])

    assert diff.summary()["rows_changed"] == 0


def test_nested_key_column_raises() -> None:
    """A nested key column cannot be hashed by value and is refused."""
    schema = pa.schema([pa.field("k", pa.list_(pa.int64())), pa.field("v", pa.int64())])
    left = pa.table({"k": pa.array([[1, 2]], pa.list_(pa.int64())), "v": pa.array([1], pa.int64())}, schema=schema)

    with pytest.raises(ValueError, match="row diff cannot"):
        diff_tables(left, left, key=["k"])


def test_empty_table_with_list_key_raises() -> None:
    """A nested key is refused up front, so an empty table with a list key errors rather than diffing to empty."""
    schema = pa.schema([pa.field("k", pa.list_(pa.int64())), pa.field("v", pa.int64())])
    empty = schema.empty_table()

    with pytest.raises(ValueError, match="row diff cannot"):
        diff_tables(empty, empty, key=["k"])


@pytest.mark.parametrize(
    ("left_type", "right_type"),
    [(pa.int64(), pa.float64()), (pa.int32(), pa.int64())],
)
def test_key_type_mismatch_across_sides_raises(left_type: pa.DataType, right_type: pa.DataType) -> None:
    """A key column whose type differs across sides is refused rather than coerced."""
    left = pa.table({"id": pa.array([1], left_type)})
    right = pa.table({"id": pa.array([1], right_type)})

    with pytest.raises(ValueError, match="same type on both sides"):
        diff_tables(left, right, key=["id"])


# Scalar-type coverage: a changed value in each supported non-key type is reported


@pytest.mark.parametrize(
    ("column_type", "left_value", "right_value"),
    [
        (pa.time32("s"), 1, 2),
        (pa.time64("us"), 1, 2),
        (pa.duration("s"), 1, 2),
        (pa.decimal32(8, 2), decimal.Decimal("1.00"), decimal.Decimal("2.00")),
        (pa.decimal64(16, 2), decimal.Decimal("1.00"), decimal.Decimal("2.00")),
        (pa.decimal128(20, 2), decimal.Decimal("1.00"), decimal.Decimal("2.00")),
        (pa.decimal256(40, 2), decimal.Decimal("1.00"), decimal.Decimal("2.00")),
        (pa.month_day_nano_interval(), (1, 0, 0), (2, 0, 0)),
    ],
)
def test_changed_value_in_each_scalar_type_is_reported(
    column_type: pa.DataType,
    left_value: object,
    right_value: object,
) -> None:
    """A differing non-key cell of each supported scalar type is a row change, never silently skipped."""
    left = pa.table({"id": pa.array([1], pa.int64()), "v": pa.array([left_value], column_type)})
    right = pa.table({"id": pa.array([1], pa.int64()), "v": pa.array([right_value], column_type)})
    diff = diff_tables(left, right, key=["id"])

    assert diff.summary()["rows_changed"] == 1


def test_pyarrow_all_none_column_compares_as_all_null() -> None:
    """A pyarrow all-None column (inferred as the null type) is compared, not refused: its rows read as all-null."""
    left = pa.table({"id": pa.array([1, 2], pa.int64()), "n": pa.array([None, None])})
    right = pa.table({"id": pa.array([1, 3], pa.int64()), "n": pa.array([None, None])})
    assert left.schema.field("n").type == pa.null()
    diff = diff_tables(left, right, key=["id"])

    assert diff.summary()["rows_added"] == 1
    assert diff.summary()["rows_removed"] == 1
    assert diff.summary()["rows_changed"] == 0


def test_polars_all_null_column_fails_at_import() -> None:
    """A polars all-null (Null-typed) column cannot cross the Arrow C interface; the error is surfaced as ValueError."""
    left = pl.DataFrame({"id": [1, 2], "n": [None, None]})
    right = pl.DataFrame({"id": [1, 3], "n": [None, None]})
    assert left["n"].dtype == pl.Null

    with pytest.raises(ValueError, match='datatype "Null" doesn\'t expect buffer'):
        diff_tables(left, right, key=["id"])


# Oracle parity


def _oracle_summary(left: pa.Table, right: pa.Table, tmp_path: Path) -> dict:
    """Write the pair to parquet and return the DuckDB oracle's summary."""
    left_path = tmp_path / "a.parquet"
    right_path = tmp_path / "b.parquet"
    pq.write_table(left, left_path)
    pq.write_table(right, right_path)

    return oracle_run(left_path, right_path, ["id"], tmp_path / "oracle")


@pytest.mark.parametrize(
    "n_rows",
    [1_000, pytest.param(100_000, marks=pytest.mark.slow), pytest.param(1_000_000, marks=pytest.mark.slow)],
)
def test_oracle_parity_on_the_fixture_pair(n_rows: int, tmp_path: Path) -> None:
    """onix's row counts equal the DuckDB oracle's on the seeded fixture pair."""
    fixture_dir = tmp_path / "fixture"
    generate(n_rows, 4242, fixture_dir)
    left = pq.read_table(fixture_dir / "a.parquet")
    right = pq.read_table(fixture_dir / "b.parquet")

    diff = diff_tables(left, right, key=["id"])
    oracle = oracle_run(fixture_dir / "a.parquet", fixture_dir / "b.parquet", ["id"], tmp_path / "oracle")

    summary = diff.summary()
    assert summary["rows_added"] == oracle["rows_added"]
    assert summary["rows_removed"] == oracle["rows_removed"]
    assert summary["duplicate_keys"] == oracle["duplicate_keys"]
    assert summary["null_keys"] == oracle["null_keys"]
    # Every modified fixture row changes exactly one cell, so changed rows equal
    # the oracle's changed-cell count.
    assert summary["rows_changed"] == oracle["cells_changed"]

    oracle_added = _ids(pq.read_table(tmp_path / "oracle" / "rows_added.parquet"))
    oracle_removed = _ids(pq.read_table(tmp_path / "oracle" / "rows_removed.parquet"))
    assert _ids(_table(diff.rows_added())) == oracle_added
    assert _ids(_table(diff.rows_removed())) == oracle_removed


def test_oracle_parity_with_duplicates_and_null_keys(tmp_path: Path) -> None:
    """onix matches the oracle on a synthetic table with duplicate and null keys (which the fixture never has)."""
    left = pa.table(
        {
            "id": pa.array([1, 1, 2, 3, None], pa.int64()),
            "v": pa.array([10, 11, 20, 30, 50], pa.int64()),
        },
    )
    right = pa.table(
        {
            "id": pa.array([2, 3, 4, 4, None], pa.int64()),
            "v": pa.array([20, 31, 40, 41, 50], pa.int64()),
        },
    )
    diff = diff_tables(left, right, key=["id"])
    oracle = _oracle_summary(left, right, tmp_path)

    summary = diff.summary()
    assert summary["rows_added"] == oracle["rows_added"]
    assert summary["rows_removed"] == oracle["rows_removed"]
    assert summary["rows_changed"] == oracle["cells_changed"]
    assert summary["duplicate_keys"] == oracle["duplicate_keys"]
    assert summary["null_keys"] == oracle["null_keys"]

    dup = _table(diff.duplicate_keys())
    oracle_dup = pq.read_table(tmp_path / "oracle" / "duplicate_keys.parquet")
    assert _ids(dup) == _ids(oracle_dup)


# Property test against a naive in-memory reference


def _naive(left_rows: list, right_rows: list) -> dict:
    """Classify rows the obvious way, by real key/value, as an independent reference."""
    left_groups: dict = defaultdict(list)
    right_groups: dict = defaultdict(list)
    for key, value in left_rows:
        left_groups[key].append(value)
    for key, value in right_rows:
        right_groups[key].append(value)

    added = removed = changed = dup = null = 0
    for key in set(left_groups) | set(right_groups):
        if key is None:
            null += 1

        left_count = len(left_groups.get(key, []))
        right_count = len(right_groups.get(key, []))

        if left_count > 1 or right_count > 1:
            dup += 1
        elif left_count == 1 and right_count == 0:
            removed += 1
        elif left_count == 0 and right_count == 1:
            added += 1
        elif left_groups[key][0] != right_groups[key][0]:
            changed += 1

    return {"rows_added": added, "rows_removed": removed, "rows_changed": changed, "duplicate_keys": dup, "null_keys": null}


def _random_rows(rng: random.Random) -> list:
    """A short list of (id, value) rows with a tiny key range to force duplicates and nulls."""
    rows = []

    for _ in range(rng.randint(0, 12)):
        key = None if rng.random() < 0.15 else rng.randint(0, 5)
        rows.append((key, rng.randint(0, 3)))

    return rows


def test_matches_naive_reference_over_random_tables() -> None:
    """onix's counts equal a naive in-memory reference across many random small tables."""
    rng = random.Random(20260905)

    for _ in range(300):
        left_rows = _random_rows(rng)
        right_rows = _random_rows(rng)
        left = pa.table(
            {
                "id": pa.array([key for key, _ in left_rows], pa.int64()),
                "v": pa.array([value for _, value in left_rows], pa.int64()),
            },
        )
        right = pa.table(
            {
                "id": pa.array([key for key, _ in right_rows], pa.int64()),
                "v": pa.array([value for _, value in right_rows], pa.int64()),
            },
        )
        summary = diff_tables(left, right, key=["id"]).summary()
        expected = _naive(left_rows, right_rows)

        assert {k: summary[k] for k in expected} == expected, (left_rows, right_rows)
