"""Tests for the keyed row diff: added/removed/changed/duplicate/null rows, oracle parity, and a property test."""

import decimal
import random
from collections import defaultdict
from datetime import datetime, time, timedelta, timezone
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


def _cells(table: pa.Table, key: tuple[str, ...] = ("id",)) -> set:
    """A ``cells_changed`` table as a set of ``(*key, column, old, new, change)`` tuples."""
    rows = table.to_pylist()
    return {(*(r[k] for k in key), r["column"], r["old_value"], r["new_value"], r["change"]) for r in rows}


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
    """A differing non-key cell of each supported scalar type is a row change and one reported cell, never silently skipped."""
    left = pa.table({"id": pa.array([1], pa.int64()), "v": pa.array([left_value], column_type)})
    right = pa.table({"id": pa.array([1], pa.int64()), "v": pa.array([right_value], column_type)})
    diff = diff_tables(left, right, key=["id"])

    assert diff.summary()["rows_changed"] == 1
    assert diff.summary()["cells_changed"] == 1
    cell = _table(diff.cells_changed()).to_pylist()[0]
    assert (cell["id"], cell["column"], cell["change"]) == (1, "v", "value_changed")
    assert cell["old_value"] is not None
    assert cell["new_value"] is not None


# Per-cell diff: labels, rendering, and ordering


def test_changed_cell_reports_key_column_old_new_and_label() -> None:
    """A value change lists the key, the column, both rendered values, and value_changed."""
    left = pa.table({"id": pa.array([1, 2], pa.int64()), "v": pa.array([10, 20], pa.int64())})
    right = pa.table({"id": pa.array([1, 2], pa.int64()), "v": pa.array([10, 25], pa.int64())})
    cells = _table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()

    assert cells == [{"id": 2, "column": "v", "old_value": "20", "new_value": "25", "change": "value_changed"}]


def test_became_null_and_became_non_null_labels() -> None:
    """A cell going to/from null is labelled and renders the present side only."""
    left = pa.table({"id": pa.array([1, 2], pa.int64()), "v": pa.array([10, None], pa.int64())})
    right = pa.table({"id": pa.array([1, 2], pa.int64()), "v": pa.array([None, 20], pa.int64())})
    cells = _cells(_table(diff_tables(left, right, key=["id"]).cells_changed()))

    assert cells == {
        (1, "v", "10", None, "became_null"),
        (2, "v", None, "20", "became_non_null"),
    }


def test_value_domain_mismatch_is_type_changed() -> None:
    """A column that is a string on one side and an int on the other is reported type_changed, not value_changed."""
    left = pa.table({"id": pa.array([1], pa.int64()), "c": pa.array(["5"], pa.string())})
    right = pa.table({"id": pa.array([1], pa.int64()), "c": pa.array([5], pa.int64())})
    cells = _table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()

    assert cells == [{"id": 1, "column": "c", "old_value": "5", "new_value": "5", "change": "type_changed"}]


def test_lossless_type_change_at_equal_value_reports_no_cell() -> None:
    """An int32->int64 column with equal values is a schema change but no cell change."""
    left = pa.table({"id": pa.array([1], pa.int64()), "v": pa.array([7], pa.int32())})
    right = pa.table({"id": pa.array([1], pa.int64()), "v": pa.array([7], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    assert any(c["column"] == "v" and c["change"] == "type_changed" for c in diff.schema)
    assert diff.summary()["rows_changed"] == 0
    assert diff.summary()["cells_changed"] == 0


def test_decimal_cell_renders_at_native_scale() -> None:
    """A changed decimal cell renders old/new at the column's scale, matching a fixed-point form."""
    dtype = pa.decimal128(18, 4)
    left = pa.table({"id": pa.array([1], pa.int64()), "amount": pa.array([decimal.Decimal("20.0000")], dtype)})
    right = pa.table({"id": pa.array([1], pa.int64()), "amount": pa.array([decimal.Decimal("20.5000")], dtype)})
    cell = _table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()[0]

    assert (cell["old_value"], cell["new_value"]) == ("20.0000", "20.5000")


def test_negative_time32_cell_raises_a_render_error() -> None:
    """A value Arrow accepts but the formatter cannot render surfaces as a typed error naming the column, not error prose in the output."""
    left = pa.table({"id": pa.array([1], pa.int64()), "t": pa.array([-1], pa.time32("s"))})
    right = pa.table({"id": pa.array([1], pa.int64()), "t": pa.array([0], pa.time32("s"))})
    with pytest.raises(ValueError, match='could not render a value of column "t"'):
        diff_tables(left, right, key=["id"])


def test_out_of_range_timestamp_cell_raises_a_render_error() -> None:
    """An out-of-range timestamp renders to a typed error naming the column."""
    left = pa.table({"id": pa.array([1], pa.int64()), "ts": pa.array([2**63 - 1], pa.timestamp("us"))})
    right = pa.table({"id": pa.array([1], pa.int64()), "ts": pa.array([0], pa.timestamp("us"))})
    with pytest.raises(ValueError, match='could not render a value of column "ts"'):
        diff_tables(left, right, key=["id"])


# Same-domain, different-type facets: labels and equal-after-cast pairs


def _one_cell(left_type: pa.DataType, left_val: object, right_type: pa.DataType, right_val: object) -> list:
    """Diff a single (id=1, c) row pair whose c column differs in type across sides."""
    left = pa.table({"id": pa.array([1], pa.int64()), "c": pa.array([left_val], left_type)})
    right = pa.table({"id": pa.array([1], pa.int64()), "c": pa.array([right_val], right_type)})
    return _table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()


def test_float_width_difference_is_value_changed_with_distinct_renderings() -> None:
    """f32 0.1 versus f64 0.1 is a value change rendered at the wider type, so the two renderings differ."""
    cells = _one_cell(pa.float32(), 0.1, pa.float64(), 0.1)
    assert len(cells) == 1
    assert cells[0]["change"] == "value_changed"
    assert cells[0]["old_value"] == "0.10000000149011612"
    assert cells[0]["new_value"] == "0.1"


def test_equal_value_across_float_width_emits_no_record() -> None:
    """f32 2.0 equals f64 2.0 after widening, so no cell is reported."""
    assert _one_cell(pa.float32(), 2.0, pa.float64(), 2.0) == []


def test_timestamp_zone_awareness_difference_is_type_changed() -> None:
    """A zone-aware and a naive timestamp at the same instant are a type change with distinct renderings."""
    cells = _one_cell(
        pa.timestamp("us", tz="UTC"),
        datetime(2024, 1, 1, tzinfo=timezone.utc),
        pa.timestamp("us"),
        datetime(2024, 1, 1),
    )
    assert len(cells) == 1
    assert cells[0]["change"] == "type_changed"
    assert cells[0]["old_value"] != cells[0]["new_value"]


def test_time_unit_change_at_the_same_clock_emits_no_record() -> None:
    """time32('s') and time32('ms') at the same clock time normalize equal, so no cell is reported."""
    assert _one_cell(pa.time32("s"), time(1, 0, 0), pa.time32("ms"), time(1, 0, 0)) == []


def test_duration_unit_change_at_the_same_span_emits_no_record() -> None:
    """duration('s') and duration('ms') of the same span normalize equal, so no cell is reported."""
    assert _one_cell(pa.duration("s"), timedelta(seconds=1), pa.duration("ms"), timedelta(seconds=1)) == []


def test_extreme_duration_renders_without_a_sentinel() -> None:
    """A duration past chrono's range renders as a real value, never arrow's '<invalid>' sentinel."""
    left = pa.table({"id": pa.array([1], pa.int64()), "d": pa.array([9_300_000_000_000_000], pa.duration("s"))})
    right = pa.table({"id": pa.array([1], pa.int64()), "d": pa.array([9_400_000_000_000_000], pa.duration("s"))})
    cells = _table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()
    assert len(cells) == 1
    assert cells[0]["change"] == "value_changed"
    assert cells[0]["old_value"] != cells[0]["new_value"]
    assert "invalid" not in cells[0]["old_value"]
    assert "invalid" not in cells[0]["new_value"]


def test_duration_key_column_at_an_extreme_value_diffs_cleanly() -> None:
    """A Duration key column at an extreme value diffs cleanly: its sort-key rendering is sentinel-free."""
    # Built by casting int64: the value is past timedelta's range (as any value
    # that triggers the formatter's sentinel must be), so it is never converted
    # back to Python — the key column below is read only through Arrow.
    key = pa.array([9_300_000_000_000_000], pa.int64()).cast(pa.duration("s"))
    left = pa.table({"k": key, "v": pa.array([1], pa.int64())})
    right = pa.table({"k": key, "v": pa.array([2], pa.int64())})
    diff = diff_tables(left, right, key=["k"])
    assert diff.summary()["cells_changed"] == 1
    table = _table(diff.cells_changed())
    assert table.column("column").to_pylist() == ["v"]
    assert table.column("old_value").to_pylist() == ["1"]
    assert table.column("new_value").to_pylist() == ["2"]
    assert table.column("change").to_pylist() == ["value_changed"]


def test_decimal_scale_change_is_no_record_when_equal_and_value_changed_when_not() -> None:
    """A decimal scale change at an equal value emits nothing; a differing value is value_changed."""
    assert _one_cell(pa.decimal128(10, 2), decimal.Decimal("1.00"), pa.decimal128(10, 4), decimal.Decimal("1.0000")) == []
    cells = _one_cell(pa.decimal128(10, 2), decimal.Decimal("1.00"), pa.decimal128(10, 4), decimal.Decimal("2.0000"))
    assert len(cells) == 1
    assert cells[0]["change"] == "value_changed"


def test_cells_ordered_by_rendered_key_then_left_schema_column() -> None:
    """Output rows sort by the canonical key rendering, then left-schema column order."""
    left = pa.table(
        {
            "id": pa.array([2, 10], pa.int64()),
            "b": pa.array([1, 3], pa.int64()),
            "a": pa.array([1, 3], pa.int64()),
        },
    )
    right = pa.table(
        {
            "id": pa.array([2, 10], pa.int64()),
            "b": pa.array([9, 8], pa.int64()),
            "a": pa.array([9, 8], pa.int64()),
        },
    )
    cells = _table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()
    order = [(c["id"], c["column"]) for c in cells]

    # id renders "10" < "2" as strings; within a key, b (index 1) before a (index 2).
    assert order == [(10, "b"), (10, "a"), (2, "b"), (2, "a")]


def test_composite_key_cells_changed_carries_every_key_column() -> None:
    """cells_changed carries all key columns for a composite key."""
    left = pa.table(
        {
            "region": pa.array(["us", "eu"], pa.string()),
            "id": pa.array([1, 1], pa.int64()),
            "v": pa.array([10, 20], pa.int64()),
        },
    )
    right = pa.table(
        {
            "region": pa.array(["us", "eu"], pa.string()),
            "id": pa.array([1, 1], pa.int64()),
            "v": pa.array([11, 20], pa.int64()),
        },
    )
    cells = _cells(_table(diff_tables(left, right, key=["region", "id"]).cells_changed()), key=("region", "id"))

    assert cells == {("us", 1, "v", "10", "11", "value_changed")}


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

    # Every changed cell (key, column, rendered old/new, change label) matches
    # the oracle's, so the per-cell diff agrees on the decimal and string
    # renderings on real data.
    assert summary["cells_changed"] == oracle["cells_changed"]
    onix_cells = _cells(_table(diff.cells_changed()))
    oracle_cells = _cells(pq.read_table(tmp_path / "oracle" / "cells_changed.parquet"))
    assert onix_cells == oracle_cells


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

    onix_cells = _cells(_table(diff.cells_changed()))
    oracle_cells = _cells(pq.read_table(tmp_path / "oracle" / "cells_changed.parquet"))
    assert onix_cells == oracle_cells


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
    cells = set()
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
            # `v` is a non-null int64 on both sides, so a change is value_changed
            # rendered in base 10; the id key column stays typed (int64), null for
            # a null key.
            cells.add((key, "v", str(left_groups[key][0]), str(right_groups[key][0]), "value_changed"))

    return {
        "rows_added": added,
        "rows_removed": removed,
        "rows_changed": changed,
        "duplicate_keys": dup,
        "null_keys": null,
        "cells": cells,
    }


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
        diff = diff_tables(left, right, key=["id"])
        summary = diff.summary()
        expected = _naive(left_rows, right_rows)

        counts = {k: v for k, v in expected.items() if k != "cells"}
        assert {k: summary[k] for k in counts} == counts, (left_rows, right_rows)
        assert summary["cells_changed"] == len(expected["cells"]), (left_rows, right_rows)
        assert _cells(_table(diff.cells_changed())) == expected["cells"], (left_rows, right_rows)


# Per-cell change detection and labels across every hashed value domain


def _naive_cell_labels(left_rows: list, right_rows: list) -> set:
    """The expected ``(id, "v", change)`` set for a single-typed nullable value column.

    Because both sides share one column type, a change is a plain value
    difference (nulls handled explicitly), so this reference needs no
    type-specific comparison beyond Python equality.
    """
    left_groups: dict = defaultdict(list)
    right_groups: dict = defaultdict(list)
    for key, value in left_rows:
        left_groups[key].append(value)
    for key, value in right_rows:
        right_groups[key].append(value)

    labels = set()
    for key in set(left_groups) | set(right_groups):
        if len(left_groups.get(key, [])) != 1 or len(right_groups.get(key, [])) != 1:
            continue
        old, new = left_groups[key][0], right_groups[key][0]
        if old is None and new is None:
            continue
        if old is None:
            labels.add((key, "v", "became_non_null"))
        elif new is None:
            labels.add((key, "v", "became_null"))
        elif old != new:
            labels.add((key, "v", "value_changed"))

    return labels


def test_cell_labels_match_a_naive_reference_across_every_hashed_type() -> None:
    """For every hashed value domain, onix's changed cells and their labels equal a naive reference."""
    ts0 = datetime(2024, 1, 1, tzinfo=timezone.utc)
    ts1 = datetime(2024, 6, 1, 12, 30, tzinfo=timezone.utc)
    pools = {
        pa.int64(): [None, 0, 1, 2],
        pa.int32(): [None, -1, 0, 7],
        pa.uint16(): [None, 0, 3, 9],
        pa.float64(): [None, 0.0, -0.0, 1.5, 2.0],
        pa.bool_(): [None, True, False],
        pa.decimal128(12, 3): [None, decimal.Decimal("1.000"), decimal.Decimal("1.500"), decimal.Decimal("2.250")],
        pa.string(): [None, "a", "b", "café"],
        pa.large_string(): [None, "x", "y"],
        pa.binary(): [None, b"x", b"yy"],
        pa.timestamp("us", tz="UTC"): [None, ts0, ts1],
        pa.date32(): [None, ts0.date(), ts1.date()],
    }
    rng = random.Random(20260906)

    for column_type, pool in pools.items():
        for _ in range(40):
            left_rows = [(rng.randint(0, 3), rng.choice(pool)) for _ in range(rng.randint(0, 8))]
            right_rows = [(rng.randint(0, 3), rng.choice(pool)) for _ in range(rng.randint(0, 8))]
            left = pa.table(
                {
                    "id": pa.array([k for k, _ in left_rows], pa.int64()),
                    "v": pa.array([v for _, v in left_rows], column_type),
                },
            )
            right = pa.table(
                {
                    "id": pa.array([k for k, _ in right_rows], pa.int64()),
                    "v": pa.array([v for _, v in right_rows], column_type),
                },
            )
            cells = _table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()
            got = {(c["id"], c["column"], c["change"]) for c in cells}
            expected = _naive_cell_labels(left_rows, right_rows)
            assert got == expected, (column_type, left_rows, right_rows)


def test_cross_type_cell_labels_match_a_naive_reference() -> None:
    """With two types per column, onix's changed cells and labels equal a naive reference read from pyarrow's own values.

    Each pair is a lossless same-domain cast (int/float widening, a time unit
    change), so the label is value_changed and equality read back through
    ``to_pylist`` reflects onix's own normalization (an f32 0.1 widens to a value
    distinct from f64 0.1; time32 s/ms at the same clock read equal).
    """
    pairs = [
        (pa.int32(), pa.int64(), [None, -1, 0, 7]),
        (pa.float32(), pa.float64(), [None, 0.0, 0.1, 2.0]),
        (pa.time32("s"), pa.time32("ms"), [None, time(0, 0, 1), time(1, 0, 0)]),
    ]
    rng = random.Random(20260907)

    for left_type, right_type, pool in pairs:
        for _ in range(40):
            left_rows = [(rng.randint(0, 3), rng.choice(pool)) for _ in range(rng.randint(0, 8))]
            right_rows = [(rng.randint(0, 3), rng.choice(pool)) for _ in range(rng.randint(0, 8))]
            left = pa.table(
                {
                    "id": pa.array([k for k, _ in left_rows], pa.int64()),
                    "v": pa.array([v for _, v in left_rows], left_type),
                },
            )
            right = pa.table(
                {
                    "id": pa.array([k for k, _ in right_rows], pa.int64()),
                    "v": pa.array([v for _, v in right_rows], right_type),
                },
            )
            # Read the values back through pyarrow so the naive reference sees
            # the same normalized values onix hashes (f32 widening, time units).
            left_read = list(zip((k for k, _ in left_rows), left.column("v").to_pylist()))
            right_read = list(zip((k for k, _ in right_rows), right.column("v").to_pylist()))
            cells = _table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()
            got = {(c["id"], c["column"], c["change"]) for c in cells}
            expected = _naive_cell_labels(left_read, right_read)
            assert got == expected, (left_type, right_type, left_rows, right_rows)
