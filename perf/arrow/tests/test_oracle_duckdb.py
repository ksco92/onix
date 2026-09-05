"""Tests for `oracle_duckdb.py`: parity with the fixture sidecar, plus the
value-comparison semantics its module docstring documents (null handling,
duplicate keys, schema-diff classification) exercised on small synthetic
tables the shared 5%-mutation fixture never produces on its own.
"""

from datetime import datetime, timezone
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest

from generate_fixtures import generate
from oracle_duckdb import run

SEED = 67890


# Parity against the fixture sidecar
@pytest.mark.parametrize(
    "n_rows",
    [1_000, pytest.param(100_000, marks=pytest.mark.slow), pytest.param(1_000_000, marks=pytest.mark.slow)],
)
def test_oracle_counts_match_sidecar(n_rows: int, tmp_path: Path) -> None:
    """Every oracle count equals the sidecar's corresponding count, at every tested fixture size."""
    fixture_dir = tmp_path / "fixture"
    manifest = generate(n_rows, SEED, fixture_dir)

    summary = run(fixture_dir / "a.parquet", fixture_dir / "b.parquet", ["id"], tmp_path / "oracle")

    assert summary["rows_added"] == manifest["rows_added"]
    assert summary["rows_removed"] == manifest["rows_deleted"]
    assert summary["duplicate_keys"] == manifest["duplicate_keys"]
    assert summary["cells_changed"] == manifest["rows_modified_amount"] + manifest["rows_modified_payload"]
    # `category`'s dictionary retype is invisible to a SQL-only oracle (see the
    # module docstring); only `ts` and `note` show up in `schema_changes`.
    assert summary["schema_changes"] == len(manifest["schema_changes"]) - 1


def test_oracle_output_files_have_expected_columns(tmp_path: Path) -> None:
    """Each written parquet file has the documented long-format columns."""
    fixture_dir = tmp_path / "fixture"
    generate(1000, SEED, fixture_dir)
    out_dir = tmp_path / "oracle"
    run(fixture_dir / "a.parquet", fixture_dir / "b.parquet", ["id"], out_dir)

    cells_changed = pq.read_schema(out_dir / "cells_changed.parquet")
    assert cells_changed.names == ["id", "column", "old_value", "new_value", "change"]

    duplicate_keys = pq.read_schema(out_dir / "duplicate_keys.parquet")
    assert duplicate_keys.names == ["id", "left_count", "right_count"]

    schema_diff = pq.read_schema(out_dir / "schema_diff.parquet")
    assert schema_diff.names == ["column", "left_type", "right_type", "change"]


# Synthetic small-table tests: semantics the shared fixture never exercises
def _write_pair(tmp_path: Path, a_rows: list[dict], b_rows: list[dict]) -> tuple[Path, Path]:
    """
    Write two small ad hoc parquet files for a synthetic oracle test.

    :param tmp_path: pytest's per-test temp directory.
    :param a_rows: Records for the base side.
    :param b_rows: Records for the changed side.
    :return: `(a_path, b_path)`.
    """
    a_path, b_path = tmp_path / "synthetic_a.parquet", tmp_path / "synthetic_b.parquet"
    pq.write_table(pa.Table.from_pylist(a_rows), a_path)
    pq.write_table(pa.Table.from_pylist(b_rows), b_path)

    return a_path, b_path


def test_null_becomes_non_null_is_reported_as_a_changed_cell(tmp_path: Path) -> None:
    """A cell that goes from NULL to a value is a changed cell, per IS DISTINCT FROM."""
    a_rows = [{"id": 1, "value": None}, {"id": 2, "value": "x"}]
    b_rows = [{"id": 1, "value": "now set"}, {"id": 2, "value": "x"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    changed = pq.read_table(tmp_path / "oracle" / "cells_changed.parquet").to_pylist()

    assert summary["cells_changed"] == 1
    assert changed == [{"id": 1, "column": "value", "old_value": None, "new_value": "now set", "change": "became_non_null"}]


def test_two_nulls_compare_equal_and_are_not_reported(tmp_path: Path) -> None:
    """Two NULL cells for the same key/column are unchanged, not a false positive."""
    a_rows = [{"id": 1, "value": None}]
    b_rows = [{"id": 1, "value": None}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    assert summary["cells_changed"] == 0


def test_duplicate_key_on_the_left_is_reported_and_excluded_from_changed_removed(tmp_path: Path) -> None:
    """A key appearing twice on the left is a duplicate, not a false removal/change."""
    a_rows = [{"id": 1, "value": "x"}, {"id": 1, "value": "y"}, {"id": 2, "value": "z"}]
    b_rows = [{"id": 2, "value": "z"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    duplicates = pq.read_table(tmp_path / "oracle" / "duplicate_keys.parquet").to_pylist()

    assert summary["duplicate_keys"] == 1
    assert summary["rows_removed"] == 0
    assert summary["cells_changed"] == 0
    assert duplicates == [{"id": 1, "left_count": 2, "right_count": 0}]


def test_duplicate_key_on_both_sides_reports_both_counts(tmp_path: Path) -> None:
    """A key duplicated on both sides reports its exact left/right counts."""
    a_rows = [{"id": 1, "value": "x"}, {"id": 1, "value": "y"}]
    b_rows = [{"id": 1, "value": "x"}, {"id": 1, "value": "y"}, {"id": 1, "value": "z"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    duplicates = pq.read_table(tmp_path / "oracle" / "duplicate_keys.parquet").to_pylist()

    assert summary["duplicate_keys"] == 1
    assert duplicates == [{"id": 1, "left_count": 2, "right_count": 3}]


# Null-key tests: NULL in a key column must equal itself across sides (#39)
def test_null_keyed_row_is_matched_across_sides_when_value_is_equal(tmp_path: Path) -> None:
    """A NULL key present on both sides is matched, not reported as added+removed."""
    a_rows = [{"id": None, "value": "x"}, {"id": 2, "value": "y"}]
    b_rows = [{"id": None, "value": "x"}, {"id": 2, "value": "y"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")

    assert summary["rows_added"] == 0
    assert summary["rows_removed"] == 0
    assert summary["cells_changed"] == 0
    assert summary["null_keys"] == 1


def test_null_keyed_row_with_a_different_value_yields_one_cell_change(tmp_path: Path) -> None:
    """A NULL key matched across sides still detects a genuine cell change."""
    a_rows = [{"id": None, "value": "x"}]
    b_rows = [{"id": None, "value": "y"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    changed = pq.read_table(tmp_path / "oracle" / "cells_changed.parquet").to_pylist()

    assert summary["rows_added"] == 0
    assert summary["rows_removed"] == 0
    assert summary["cells_changed"] == 1
    assert changed == [{"id": None, "column": "value", "old_value": "x", "new_value": "y", "change": "value_changed"}]


def test_null_key_duplicated_on_both_sides_combines_counts(tmp_path: Path) -> None:
    """A NULL key duplicated on both sides is one duplicate_keys row with combined counts."""
    a_rows = [{"id": None, "value": "x"}, {"id": None, "value": "y"}]
    b_rows = [{"id": None, "value": "x"}, {"id": None, "value": "y"}, {"id": None, "value": "z"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    duplicates = pq.read_table(tmp_path / "oracle" / "duplicate_keys.parquet").to_pylist()

    assert summary["duplicate_keys"] == 1
    assert duplicates == [{"id": None, "left_count": 2, "right_count": 3}]


def test_composite_key_with_one_null_component_matches_component_wise(tmp_path: Path) -> None:
    """A composite key null-matches only when every component (including NULLs) agrees."""
    a_rows = [{"k1": 1, "k2": None, "value": "x"}, {"k1": 2, "k2": None, "value": "z"}]
    b_rows = [{"k1": 1, "k2": None, "value": "x"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["k1", "k2"], tmp_path / "oracle")
    removed = pq.read_table(tmp_path / "oracle" / "rows_removed.parquet").to_pylist()

    assert summary["rows_added"] == 0
    assert summary["rows_removed"] == 1
    assert summary["cells_changed"] == 0
    assert removed == [{"k1": 2, "k2": None, "value": "z"}]


# Cross-unit timestamp tests: a us-precision left side vs. an ms-precision right side
def test_same_instant_across_timestamp_units_is_not_a_changed_cell(tmp_path: Path) -> None:
    """A timestamp exactly representable at ms precision is not reported as changed."""
    instant = datetime(2024, 1, 1, 12, 0, 0, tzinfo=timezone.utc)
    a_path, b_path = tmp_path / "ts_a.parquet", tmp_path / "ts_b.parquet"
    pq.write_table(
        pa.table({"id": pa.array([1], type=pa.int64()), "ts": pa.array([instant], type=pa.timestamp("us", tz="UTC"))}), a_path,
    )
    pq.write_table(
        pa.table({"id": pa.array([1], type=pa.int64()), "ts": pa.array([instant], type=pa.timestamp("ms", tz="UTC"))}), b_path,
    )

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    assert summary["cells_changed"] == 0


def test_different_sub_millisecond_instant_is_a_changed_cell_rendered_correctly(tmp_path: Path) -> None:
    """A genuine sub-millisecond difference is one changed cell, rendered in UTC."""
    a_instant = datetime(2024, 1, 1, 12, 0, 0, 500, tzinfo=timezone.utc)  # .0005s
    b_instant = datetime(2024, 1, 1, 12, 0, 0, 1000, tzinfo=timezone.utc)  # .001s
    a_path, b_path = tmp_path / "ts_a.parquet", tmp_path / "ts_b.parquet"
    pq.write_table(
        pa.table({"id": pa.array([1], type=pa.int64()), "ts": pa.array([a_instant], type=pa.timestamp("us", tz="UTC"))}), a_path,
    )
    pq.write_table(
        pa.table({"id": pa.array([1], type=pa.int64()), "ts": pa.array([b_instant], type=pa.timestamp("ms", tz="UTC"))}), b_path,
    )

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    changed = pq.read_table(tmp_path / "oracle" / "cells_changed.parquet").to_pylist()

    assert summary["cells_changed"] == 1
    assert changed == [
        {"id": 1, "column": "ts", "old_value": "2024-01-01 12:00:00.0005+00", "new_value": "2024-01-01 12:00:00.001+00", "change": "value_changed"},
    ]


# Identifier quoting: column names that could break unquoted SQL text
def test_unusual_column_names_on_key_and_non_key_columns(tmp_path: Path) -> None:
    """A quote, a space, a semicolon, and a reserved word in column names don't break the query."""
    key_col = 'weird "key"; col'
    value_col = "select"  # a reserved word
    a_rows = [{key_col: 1, value_col: "x"}, {key_col: 2, value_col: "y"}]
    b_rows = [{key_col: 1, value_col: "x"}, {key_col: 2, value_col: "changed"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, [key_col], tmp_path / "oracle")
    changed = pq.read_table(tmp_path / "oracle" / "cells_changed.parquet").to_pylist()

    assert summary["rows_added"] == 0
    assert summary["rows_removed"] == 0
    assert summary["cells_changed"] == 1
    assert changed == [{key_col: 2, "column": value_col, "old_value": "y", "new_value": "changed", "change": "value_changed"}]


# Path quoting: file/directory names that could break an unquoted SQL literal
def test_path_with_single_quote_and_space_does_not_break_the_query(tmp_path: Path) -> None:
    """A directory name containing a single quote and a space doesn't break the SQL."""
    weird_dir = tmp_path / "o'brien's data 2024"
    weird_dir.mkdir()
    a_rows = [{"id": 1, "value": "x"}, {"id": 2, "value": "y"}]
    b_rows = [{"id": 1, "value": "x"}, {"id": 2, "value": "changed"}]
    a_path, b_path = weird_dir / "a.parquet", weird_dir / "b.parquet"
    pq.write_table(pa.Table.from_pylist(a_rows), a_path)
    pq.write_table(pa.Table.from_pylist(b_rows), b_path)
    out_dir = weird_dir / "oracle's output"

    summary = run(a_path, b_path, ["id"], out_dir)
    changed = pq.read_table(out_dir / "cells_changed.parquet").to_pylist()

    assert summary["cells_changed"] == 1
    assert changed == [{"id": 2, "column": "value", "old_value": "y", "new_value": "changed", "change": "value_changed"}]


def test_schema_diff_reports_removed_column(tmp_path: Path) -> None:
    """A column present only on the left is reported as `removed`."""
    a_rows = [{"id": 1, "value": "x", "extra": "gone"}]
    b_rows = [{"id": 1, "value": "x"}]
    a_path, b_path = _write_pair(tmp_path, a_rows, b_rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    schema_diff = pq.read_table(tmp_path / "oracle" / "schema_diff.parquet").to_pylist()

    assert summary["schema_changes"] == 1
    assert schema_diff == [{"column": "extra", "left_type": "VARCHAR", "right_type": None, "change": "removed"}]


def test_schema_diff_reports_type_changed_column(tmp_path: Path) -> None:
    """A column present on both sides with a different DuckDB type is `type_changed`."""
    a_path = tmp_path / "typed_a.parquet"
    b_path = tmp_path / "typed_b.parquet"
    pq.write_table(pa.table({"id": pa.array([1], type=pa.int64()), "value": pa.array([1], type=pa.int32())}), a_path)
    pq.write_table(pa.table({"id": pa.array([1], type=pa.int64()), "value": pa.array([1], type=pa.int64())}), b_path)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")
    schema_diff = pq.read_table(tmp_path / "oracle" / "schema_diff.parquet").to_pylist()

    assert summary["schema_changes"] == 1
    assert schema_diff[0]["change"] == "type_changed"


def test_category_dictionary_retype_is_invisible_to_the_sql_schema_diff(tmp_path: Path) -> None:
    """
    Document (and pin) the known limitation: a plain-string-to-dictionary
    retype has zero footprint in Parquet's own schema, so the SQL-only
    oracle cannot see it -- `pyarrow` can, by re-reading the Arrow-side type.
    """
    fixture_dir = tmp_path / "fixture"
    generate(1000, SEED, fixture_dir)

    summary = run(fixture_dir / "a.parquet", fixture_dir / "b.parquet", ["id"], tmp_path / "oracle")
    schema_diff_columns = {
        row["column"] for row in pq.read_table(tmp_path / "oracle" / "schema_diff.parquet").to_pylist()
    }
    assert "category" not in schema_diff_columns
    assert summary["schema_changes"] == 2

    a_category_type = pq.read_schema(fixture_dir / "a.parquet").field("category").type
    b_category_type = pq.read_schema(fixture_dir / "b.parquet").field("category").type
    assert str(a_category_type) == "string"
    assert str(b_category_type) == "dictionary<values=string, indices=int32, ordered=0>"


# Boundary tests
def test_run_with_no_changes_at_all(tmp_path: Path) -> None:
    """Two byte-identical tables produce zero of every count."""
    rows = [{"id": 1, "value": "x"}, {"id": 2, "value": "y"}]
    a_path, b_path = _write_pair(tmp_path, rows, rows)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")

    assert summary == {
        "rows_added": 0,
        "rows_removed": 0,
        "duplicate_keys": 0,
        "null_keys": 0,
        "cells_changed": 0,
        "schema_changes": 0,
    }


def test_run_with_empty_tables(tmp_path: Path) -> None:
    """Two empty (schema-only) tables produce zero of every count, no crash."""
    a_path = tmp_path / "empty_a.parquet"
    b_path = tmp_path / "empty_b.parquet"
    pq.write_table(pa.table({"id": pa.array([], type=pa.int64()), "value": pa.array([], type=pa.string())}), a_path)
    pq.write_table(pa.table({"id": pa.array([], type=pa.int64()), "value": pa.array([], type=pa.string())}), b_path)

    summary = run(a_path, b_path, ["id"], tmp_path / "oracle")

    assert summary["rows_added"] == 0
    assert summary["rows_removed"] == 0
    assert summary["cells_changed"] == 0
