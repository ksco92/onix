"""Arrow table diffing at the bindings boundary: schema diff, ingestion, and export.

Ingestion is exercised through pyarrow, polars, and DuckDB, which must all
produce identical results for the same data; the schema-comparison semantics
(timestamp timezone, decimal scale, dictionary encoding, nullability) are
pinned with pyarrow, which gives the most direct control over Arrow types. The
optional-dependency behaviour (importing and diffing with no pyarrow, and the
ImportError from to_pyarrow when it is absent) runs in a subprocess that blocks
the pyarrow import.
"""

import decimal
import json
import subprocess
import sys
import textwrap

import duckdb
import polars as pl
import pyarrow as pa
import pytest

from deepdiff_rs import diff_tables

# Helpers


def _int_tables() -> tuple[pa.Table, pa.Table]:
    """
    Build a left/right pair with one column each of every schema change.

    Only integer columns are used so the result is identical no matter which
    library re-encodes the data: int32/int64 map to the same Arrow types
    through pyarrow, polars, and DuckDB, whereas string types do not.

    :return: The left and right pyarrow tables.
    """
    left = pa.table(
        {
            "id": pa.array([1, 2], pa.int64()),
            "keep": pa.array([10, 20], pa.int64()),
            "changed": pa.array([1, 2], pa.int32()),
            "only_left": pa.array([5, 6], pa.int64()),
        },
    )
    right = pa.table(
        {
            "id": pa.array([1, 2], pa.int64()),
            "keep": pa.array([10, 20], pa.int64()),
            "changed": pa.array([1, 2], pa.int64()),
            "only_right": pa.array([7, 8], pa.int64()),
        },
    )

    return left, right


def _as(lib: str, table: pa.Table) -> object:
    """
    Re-present a pyarrow table as the given library's table object.

    :param lib: One of ``pyarrow``, ``polars``, ``duckdb``.
    :param table: The source pyarrow table.
    :return: The table as the requested library's object.
    """
    if lib == "pyarrow":
        return table

    if lib == "polars":
        return pl.from_arrow(table)

    return duckdb.from_arrow(table)


def _by_change(diff: object) -> dict:
    """
    Group a diff's schema records by their change kind.

    :param diff: A TableDiff.
    :return: A dict of change kind to the sorted list of column names.
    """
    grouped: dict = {"added": [], "removed": [], "type_changed": []}

    for record in diff.schema:
        grouped[record["change"]].append(record["column"])

    return {kind: sorted(columns) for kind, columns in grouped.items()}


# Cross-library ingestion tests


@pytest.mark.parametrize(
    ("left_lib", "right_lib"),
    [
        ("pyarrow", "pyarrow"),
        ("polars", "polars"),
        ("duckdb", "duckdb"),
        ("polars", "pyarrow"),
        ("duckdb", "polars"),
    ],
)
def test_identical_results_across_input_libraries(left_lib: str, right_lib: str) -> None:
    """
    The schema diff is identical no matter which library supplies each table.

    :param left_lib: The library providing the left table.
    :param right_lib: The library providing the right table.
    """
    left_pa, right_pa = _int_tables()
    diff = diff_tables(_as(left_lib, left_pa), _as(right_lib, right_pa), key=["id"])

    assert _by_change(diff) == {
        "added": ["only_right"],
        "removed": ["only_left"],
        "type_changed": ["changed"],
    }
    assert diff.summary() == {
        "columns_added": 1,
        "columns_removed": 1,
        "columns_type_changed": 1,
    }


def test_record_batch_array_protocol_input() -> None:
    """A pyarrow RecordBatch (the __arrow_c_array__ path) is accepted."""
    left_pa, right_pa = _int_tables()
    left_batch = left_pa.to_batches()[0]
    right_batch = right_pa.to_batches()[0]
    diff = diff_tables(left_batch, right_batch, key=["id"])

    assert _by_change(diff) == {
        "added": ["only_right"],
        "removed": ["only_left"],
        "type_changed": ["changed"],
    }


# Schema-comparison semantics tests


def _keyed(columns: dict) -> pa.Table:
    """
    Build a one-row pyarrow table with an int64 ``id`` key plus the columns.

    :param columns: Column name to pyarrow array.
    :return: The table.
    """
    return pa.table({"id": pa.array([1], pa.int64()), **columns})


def test_identical_schemas_have_no_changes() -> None:
    """Two tables with the same schema report no schema changes."""
    left = _keyed({"a": pa.array([1], pa.int64())})
    right = _keyed({"a": pa.array([9], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    assert diff.schema == []
    assert diff.summary() == {
        "columns_added": 0,
        "columns_removed": 0,
        "columns_type_changed": 0,
    }


def test_timestamp_timezone_change_is_type_changed() -> None:
    """A timestamp column differing only in timezone is a type change."""
    left = _keyed({"t": pa.array([0], pa.timestamp("us", tz="UTC"))})
    right = _keyed({"t": pa.array([0], pa.timestamp("us", tz="America/New_York"))})
    diff = diff_tables(left, right, key=["id"])

    (record,) = diff.schema
    assert record["column"] == "t"
    assert record["change"] == "type_changed"
    assert "UTC" in record["left_type"]
    assert "America/New_York" in record["right_type"]


def test_decimal_scale_change_is_type_changed() -> None:
    """A decimal column differing only in scale is a type change."""
    left = _keyed({"amount": pa.array([decimal.Decimal("1.00")], pa.decimal128(10, 2))})
    right = _keyed({"amount": pa.array([decimal.Decimal("1.0000")], pa.decimal128(10, 4))})
    diff = diff_tables(left, right, key=["id"])

    (record,) = diff.schema
    assert record["change"] == "type_changed"


def test_dictionary_encoded_string_matches_plain_string() -> None:
    """A dictionary-encoded string column equals a plain string column."""
    left = _keyed({"name": pa.array(["a"]).dictionary_encode()})
    right = _keyed({"name": pa.array(["a"], pa.string())})
    diff = diff_tables(left, right, key=["id"])

    assert diff.schema == []


def test_nullability_difference_is_not_a_change() -> None:
    """A column that differs only in nullability is not a schema change."""
    left_schema = pa.schema([pa.field("id", pa.int64()), pa.field("a", pa.int64(), nullable=False)])
    right_schema = pa.schema([pa.field("id", pa.int64()), pa.field("a", pa.int64(), nullable=True)])
    left = pa.table({"id": [1], "a": [2]}, schema=left_schema)
    right = pa.table({"id": [1], "a": [2]}, schema=right_schema)
    diff = diff_tables(left, right, key=["id"])

    assert diff.schema == []


def test_type_change_reports_both_nullabilities() -> None:
    """A type-changed record carries each side's nullability and type string."""
    left_schema = pa.schema([pa.field("id", pa.int64()), pa.field("a", pa.int32(), nullable=False)])
    right_schema = pa.schema([pa.field("id", pa.int64()), pa.field("a", pa.int64(), nullable=True)])
    left = pa.table({"id": [1], "a": [2]}, schema=left_schema)
    right = pa.table({"id": [1], "a": [2]}, schema=right_schema)
    diff = diff_tables(left, right, key=["id"])

    (record,) = diff.schema
    assert record["left_type"] == "Int32"
    assert record["right_type"] == "Int64"
    assert record["left_nullable"] is False
    assert record["right_nullable"] is True


def test_added_column_has_null_left_side() -> None:
    """An added column's left type and nullability are None."""
    left = _keyed({})
    right = _keyed({"fresh": pa.array([1], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    (record,) = diff.schema
    assert record["change"] == "added"
    assert record["left_type"] is None
    assert record["left_nullable"] is None
    assert record["right_type"] == "Int64"


def test_unicode_column_names() -> None:
    """Unicode column names are matched and reported correctly."""
    left = _keyed({"café": pa.array([1], pa.int64())})
    right = _keyed({"café": pa.array(["x"], pa.string())})
    diff = diff_tables(left, right, key=["id"])

    (record,) = diff.schema
    assert record["column"] == "café"
    assert record["change"] == "type_changed"


# Boundary tests


def test_empty_tables_diff_by_schema_only() -> None:
    """Zero-row tables still diff by their schemas."""
    left = pa.table({"id": pa.array([], pa.int64()), "gone": pa.array([], pa.int64())})
    right = pa.table({"id": pa.array([], pa.int64()), "fresh": pa.array([], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    assert _by_change(diff) == {"added": ["fresh"], "removed": ["gone"], "type_changed": []}


def test_key_only_tables_have_no_changes() -> None:
    """Tables whose only column is the key report no changes."""
    left = pa.table({"id": pa.array([1], pa.int64())})
    right = pa.table({"id": pa.array([1], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    assert diff.schema == []


def test_zero_column_table_fails_key_check() -> None:
    """A table with zero columns cannot contain the key, so the key is missing."""
    left = pa.table({})
    right = pa.table({"id": pa.array([1], pa.int64())})

    with pytest.raises(ValueError, match='"id".*left'):
        diff_tables(left, right, key=["id"])


def test_composite_key() -> None:
    """A composite key is accepted when all its columns exist on both sides."""
    left = pa.table(
        {
            "a": pa.array([1], pa.int64()),
            "b": pa.array([1], pa.int64()),
            "v": pa.array([1], pa.int32()),
        },
    )
    right = pa.table(
        {
            "a": pa.array([1], pa.int64()),
            "b": pa.array([1], pa.int64()),
            "v": pa.array([1], pa.int64()),
        },
    )
    diff = diff_tables(left, right, key=["a", "b"])

    assert _by_change(diff)["type_changed"] == ["v"]


# Error tests


def test_missing_key_names_the_column_and_side() -> None:
    """A key absent from one side raises ValueError naming the column."""
    left = pa.table({"other": pa.array([1], pa.int64())})
    right = pa.table({"id": pa.array([1], pa.int64())})

    with pytest.raises(ValueError, match='"id".*left'):
        diff_tables(left, right, key=["id"])


def test_empty_key_is_rejected() -> None:
    """An empty key list raises ValueError."""
    left = _keyed({})
    right = _keyed({})

    with pytest.raises(ValueError, match="at least one key column"):
        diff_tables(left, right, key=[])


def test_string_key_is_rejected() -> None:
    """A bare string key raises TypeError rather than being split into characters."""
    left = _keyed({})
    right = _keyed({})

    with pytest.raises(TypeError, match="list of column names"):
        diff_tables(left, right, key="id")


def test_non_arrow_input_raises_type_error() -> None:
    """An object implementing neither Arrow protocol raises TypeError naming them."""
    right = _keyed({})

    with pytest.raises(TypeError, match="__arrow_c_stream__.*__arrow_c_array__"):
        diff_tables({"id": [1]}, right, key=["id"])


def test_row_level_members_not_implemented() -> None:
    """The row-level members raise NotImplementedError in this version."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])

    for member in ("rows_added", "rows_removed", "cells_changed", "duplicate_keys"):
        with pytest.raises(NotImplementedError):
            getattr(diff, member)()


# Output and export tests


def test_to_json_has_schema_and_summary() -> None:
    """to_json round-trips to an object with schema and summary."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])
    data = json.loads(diff.to_json())

    assert set(data) == {"schema", "summary"}
    assert data["summary"]["columns_type_changed"] == 1
    assert {record["column"] for record in data["schema"]} == {"only_left", "only_right", "changed"}


def test_schema_arrow_exports_to_pyarrow() -> None:
    """schema_arrow is consumable by pyarrow via the Arrow C stream."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])
    table = pa.table(diff.schema_arrow)

    assert table.num_rows == 3
    assert table.column_names == [
        "column",
        "change",
        "left_type",
        "right_type",
        "left_nullable",
        "right_nullable",
    ]
    assert set(table.column("column").to_pylist()) == {"only_left", "only_right", "changed"}


def test_schema_arrow_exports_to_polars() -> None:
    """schema_arrow is consumable by polars with no pyarrow round trip."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])
    frame = pl.DataFrame(diff.schema_arrow)

    assert frame.height == 3
    assert set(frame["column"].to_list()) == {"only_left", "only_right", "changed"}


def test_to_pyarrow_convenience() -> None:
    """ArrowTable.to_pyarrow returns a pyarrow Table."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])
    table = diff.schema_arrow.to_pyarrow()

    assert isinstance(table, pa.Table)
    assert table.num_rows == 3
    assert len(diff.schema_arrow) == 3


def test_empty_diff_exports_empty_table() -> None:
    """An empty schema diff exports an empty, fully-typed Arrow table."""
    left = pa.table({"id": pa.array([1], pa.int64())})
    right = pa.table({"id": pa.array([1], pa.int64())})
    diff = diff_tables(left, right, key=["id"])
    table = pa.table(diff.schema_arrow)

    assert table.num_rows == 0
    assert table.num_columns == 6


# Optional-dependency tests


def test_import_and_diff_without_pyarrow() -> None:
    """deepdiff_rs imports and diffs DuckDB tables with pyarrow unavailable, and to_pyarrow errors.

    DuckDB is the Arrow source here because it exposes __arrow_c_stream__ natively, with no
    pyarrow. pyarrow is made unimportable with a finder that lets libraries probe it (find_spec
    returns a spec, so a graceful pyarrow-optional import path sees "present") but fails the actual
    import, which is what to_pyarrow triggers.
    """
    program = textwrap.dedent(
        """
        import importlib.abc
        import importlib.machinery
        import sys


        class FailLoader(importlib.abc.Loader):
            def create_module(self, spec):
                raise ModuleNotFoundError("No module named 'pyarrow' (blocked for test)")

            def exec_module(self, module):
                raise ModuleNotFoundError("No module named 'pyarrow' (blocked for test)")


        class PyarrowBlocker(importlib.abc.MetaPathFinder):
            def find_spec(self, name, path=None, target=None):
                if name == "pyarrow" or name.startswith("pyarrow."):
                    return importlib.machinery.ModuleSpec(name, FailLoader())
                return None


        sys.modules.pop("pyarrow", None)
        sys.meta_path.insert(0, PyarrowBlocker())

        import duckdb
        import deepdiff_rs

        left = duckdb.sql("SELECT CAST(1 AS BIGINT) AS id, 1 AS a")
        right = duckdb.sql("SELECT CAST(1 AS BIGINT) AS id, 1 AS b")
        diff = deepdiff_rs.diff_tables(left, right, key=["id"])
        assert {record["column"] for record in diff.schema} == {"a", "b"}

        try:
            import pyarrow  # noqa: F401
        except ModuleNotFoundError:
            pass
        else:
            raise AssertionError("pyarrow should be unimportable in this subprocess")

        try:
            diff.schema_arrow.to_pyarrow()
        except ImportError as error:
            assert "deepdiff-rs[arrow]" in str(error), str(error)
        else:
            raise AssertionError("expected ImportError from to_pyarrow without pyarrow")

        print("OK")
        """,
    )
    result = subprocess.run(
        [sys.executable, "-c", program],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().endswith("OK")
