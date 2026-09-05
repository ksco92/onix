"""Tests for the Arrow table diff: schema diff, ingestion, export, and safety."""

import ctypes
import decimal
import json
import os
import subprocess
import sys
import textwrap

import duckdb
import polars as pl
import pyarrow as pa
import pytest

from deepdiff_rs import MaxDepthError, diff_tables

# The library-pair matrix reused by every cross-library test: same-library on
# each side, plus mixed pairs (the acceptance example diffs a polars table
# against a pyarrow one).
_LIBRARY_PAIRS = [
    ("pyarrow", "pyarrow"),
    ("polars", "polars"),
    ("duckdb", "duckdb"),
    ("polars", "pyarrow"),
    ("duckdb", "polars"),
]

# Helpers


def _int_tables() -> tuple[pa.Table, pa.Table]:
    """Build a left/right pair with one integer column of every schema change kind."""
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
    """Re-present a pyarrow table as the given library's (pyarrow/polars/duckdb) table object."""
    if lib == "pyarrow":
        return table

    if lib == "polars":
        return pl.from_arrow(table)

    return duckdb.from_arrow(table)


def _keyed(columns: dict) -> pa.Table:
    """Build a one-row pyarrow table with an int64 ``id`` key plus the given columns."""
    return pa.table({"id": pa.array([1], pa.int64()), **columns})


def _by_change(diff: object) -> dict:
    """Group a diff's schema records into change kind -> sorted list of column names."""
    grouped: dict = {"added": [], "removed": [], "type_changed": []}

    for record in diff.schema:
        grouped[record["change"]].append(record["column"])

    return {kind: sorted(columns) for kind, columns in grouped.items()}


def _assert_default_changes(diff: object) -> None:
    """Assert the diff matches the single-change-of-each-kind result of ``_int_tables``."""
    assert _by_change(diff) == {
        "added": ["only_right"],
        "removed": ["only_left"],
        "type_changed": ["changed"],
    }


def _run_isolated(program: str, env: dict | None = None) -> subprocess.CompletedProcess:
    """Run a program in a subprocess so a native crash surfaces as a non-zero return code."""
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(program)],
        capture_output=True,
        text=True,
        check=False,
        env={**os.environ, **env} if env else None,
    )


# Cross-library ingestion tests


@pytest.mark.parametrize(("left_lib", "right_lib"), _LIBRARY_PAIRS)
def test_identical_results_across_input_libraries(left_lib: str, right_lib: str) -> None:
    """The schema diff is identical no matter which library supplies each table."""
    left_pa, right_pa = _int_tables()
    diff = diff_tables(_as(left_lib, left_pa), _as(right_lib, right_pa), key=["id"])

    _assert_default_changes(diff)
    assert diff.summary() == {
        "columns_added": 1,
        "columns_removed": 1,
        "columns_type_changed": 1,
        "rows_added": 0,
        "rows_removed": 0,
        "rows_changed": 0,
        "duplicate_keys": 0,
        "null_keys": 0,
        "cells_changed": 0,
    }


def _row_diff_pair() -> tuple[pa.Table, pa.Table]:
    """A left/right pair exercising every row outcome: a duplicate, a removed, a changed, an added, and a null key."""
    left = pa.table(
        {
            "id": pa.array([1, 1, 2, 3, None], pa.int64()),
            "v": pa.array([10, 11, 20, 30, 50], pa.int64()),
        },
    )
    right = pa.table(
        {
            "id": pa.array([3, 4, None], pa.int64()),
            "v": pa.array([31, 40, 50], pa.int64()),
        },
    )

    return left, right


def _row_fingerprint(diff: object) -> tuple:
    """A comparable snapshot of a diff's row results (summary plus the added/removed/duplicate key sets)."""
    added = sorted(pa.table(diff.rows_added()).column("id").to_pylist(), key=lambda x: (x is None, x))
    removed = sorted(pa.table(diff.rows_removed()).column("id").to_pylist(), key=lambda x: (x is None, x))
    dups = sorted(pa.table(diff.duplicate_keys()).column("id").to_pylist(), key=lambda x: (x is None, x))

    return (tuple(sorted(diff.summary().items())), tuple(added), tuple(removed), tuple(dups))


@pytest.mark.parametrize(("left_lib", "right_lib"), _LIBRARY_PAIRS)
def test_row_diff_identical_across_input_libraries(left_lib: str, right_lib: str) -> None:
    """The full row diff (added, removed, changed, duplicate, null keys) is identical no matter which library supplies each table."""
    left_pa, right_pa = _row_diff_pair()
    baseline = _row_fingerprint(diff_tables(left_pa, right_pa, key=["id"]))
    diff = diff_tables(_as(left_lib, left_pa), _as(right_lib, right_pa), key=["id"])

    assert _row_fingerprint(diff) == baseline
    assert diff.summary()["rows_added"] == 1
    assert diff.summary()["rows_removed"] == 1
    assert diff.summary()["rows_changed"] == 1
    assert diff.summary()["duplicate_keys"] == 1
    assert diff.summary()["null_keys"] == 1


def _struct_array(table: pa.Table) -> pa.StructArray:
    """Build a StructArray (an __arrow_c_array__-only object) from a table's columns."""
    return pa.StructArray.from_arrays(
        [table.column(name).combine_chunks() for name in table.column_names],
        names=table.column_names,
    )


def test_struct_array_c_array_protocol_input() -> None:
    """A pyarrow StructArray (the __arrow_c_array__ path, no stream) is accepted."""
    left_pa, right_pa = _int_tables()
    left = _struct_array(left_pa)
    right = _struct_array(right_pa)
    assert not hasattr(left, "__arrow_c_stream__")
    diff = diff_tables(left, right, key=["id"])

    _assert_default_changes(diff)


def test_record_batch_stream_protocol_input() -> None:
    """A pyarrow RecordBatch (which exposes __arrow_c_stream__) is accepted."""
    left_pa, right_pa = _int_tables()
    left_batch = left_pa.to_batches()[0]
    right_batch = right_pa.to_batches()[0]
    assert hasattr(left_batch, "__arrow_c_stream__")
    diff = diff_tables(left_batch, right_batch, key=["id"])

    _assert_default_changes(diff)


def test_record_batch_reader_input() -> None:
    """A pyarrow RecordBatchReader is accepted; only its schema is read in this version."""
    left_pa, right_pa = _int_tables()
    left = pa.RecordBatchReader.from_batches(left_pa.schema, left_pa.to_batches())
    right = pa.RecordBatchReader.from_batches(right_pa.schema, right_pa.to_batches())
    diff = diff_tables(left, right, key=["id"])

    _assert_default_changes(diff)


def test_multi_chunk_table_input() -> None:
    """A table with more than one chunk is accepted (schema read only)."""
    left_pa, right_pa = _int_tables()
    left = pa.concat_tables([left_pa, left_pa])
    right = pa.concat_tables([right_pa, right_pa])
    assert left.column("id").num_chunks == 2
    diff = diff_tables(left, right, key=["id"])

    _assert_default_changes(diff)


@pytest.mark.parametrize(("left_lib", "right_lib"), _LIBRARY_PAIRS)
def test_string_columns_identical_across_libraries(left_lib: str, right_lib: str) -> None:
    """A string column present on both sides is never a type change, whatever library supplies it."""
    left_pa = pa.table({"id": pa.array([1], pa.int64()), "name": pa.array(["a"], pa.string())})
    right_pa = pa.table({"id": pa.array([1], pa.int64()), "name": pa.array(["b"], pa.string())})
    diff = diff_tables(_as(left_lib, left_pa), _as(right_lib, right_pa), key=["id"])

    assert diff.schema == []


@pytest.mark.parametrize(("left_lib", "right_lib"), _LIBRARY_PAIRS)
def test_fixed_size_list_string_column_identical_across_libraries(left_lib: str, right_lib: str) -> None:
    """A fixed-size-list-of-string column is never a spurious type change across libraries."""
    left_pa = pa.table({"id": pa.array([1], pa.int64()), "v": pa.array([["a", "b"]], pa.list_(pa.string(), 2))})
    right_pa = pa.table({"id": pa.array([2], pa.int64()), "v": pa.array([["c", "d"]], pa.list_(pa.string(), 2))})
    diff = diff_tables(_as(left_lib, left_pa), _as(right_lib, right_pa), key=["id"])

    assert diff.schema == []


def _kv_list_table() -> pa.Table:
    """Build a one-column table of a list of two-field key/value structs (nullable key)."""
    kv = pa.struct([pa.field("key", pa.string()), pa.field("value", pa.int32())])
    return pa.schema([pa.field("id", pa.int64()), pa.field("m", pa.list_(kv))]).empty_table()


@pytest.mark.parametrize(("left_lib", "right_lib"), _LIBRARY_PAIRS)
def test_list_of_key_value_struct_identical_across_libraries(left_lib: str, right_lib: str) -> None:
    """A list of key/value structs reads the same across libraries (polars re-exports it as a large list)."""
    left_pa = _kv_list_table()
    right_pa = _kv_list_table()
    diff = diff_tables(_as(left_lib, left_pa), _as(right_lib, right_pa), key=["id"])

    assert diff.schema == []


def test_list_of_key_value_struct_and_map_are_not_distinguished() -> None:
    """Accepted false negative: a list of key/value structs and a real map compare equal, since polars cannot tell them apart."""
    as_list = _kv_list_table()
    as_map = pa.schema([pa.field("id", pa.int64()), pa.field("m", pa.map_(pa.string(), pa.int32()))]).empty_table()
    diff = diff_tables(as_list, as_map, key=["id"])

    assert diff.schema == []


def test_list_view_normalizes_like_list() -> None:
    """A list_view column equals the matching list, for scalar elements and for a key/value-struct (map) element."""
    kv = pa.struct([pa.field("key", pa.string()), pa.field("value", pa.int32())])
    for element in (pa.int64(), kv):
        left = pa.schema([pa.field("id", pa.int64()), pa.field("v", pa.list_view(element))]).empty_table()
        right = pa.schema([pa.field("id", pa.int64()), pa.field("v", pa.list_(element))]).empty_table()
        assert diff_tables(left, right, key=["id"]).schema == []


# Schema-comparison semantics tests


def test_identical_schemas_have_no_schema_changes_but_report_row_changes() -> None:
    """Two tables with the same schema report no schema changes; a differing non-key cell is a row change."""
    left = _keyed({"a": pa.array([1], pa.int64())})
    right = _keyed({"a": pa.array([9], pa.int64())})
    diff = diff_tables(left, right, key=["id"])

    assert diff.schema == []
    assert diff.summary() == {
        "columns_added": 0,
        "columns_removed": 0,
        "columns_type_changed": 0,
        "rows_added": 0,
        "rows_removed": 0,
        "rows_changed": 1,
        "duplicate_keys": 0,
        "null_keys": 0,
        "cells_changed": 1,
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


def test_large_utf8_equals_utf8() -> None:
    """A LargeUtf8 column equals a plain Utf8 column."""
    left = pa.schema([pa.field("id", pa.int64()), pa.field("name", pa.large_string())]).empty_table()
    right = pa.schema([pa.field("id", pa.int64()), pa.field("name", pa.string())]).empty_table()
    diff = diff_tables(left, right, key=["id"])

    assert diff.schema == []


def test_list_of_dictionary_equals_list_of_string() -> None:
    """A list-of-dictionary-string column equals a large-list-of-large-string column."""
    left = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("tags", pa.list_(pa.dictionary(pa.int32(), pa.string()))),
        ],
    ).empty_table()
    right = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("tags", pa.large_list(pa.large_string())),
        ],
    ).empty_table()
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


def test_duplicate_column_on_left_is_rejected() -> None:
    """A side with two columns of the same name raises ValueError naming it."""
    left = pa.schema(
        [
            pa.field("id", pa.int64()),
            pa.field("x", pa.int64()),
            pa.field("x", pa.string()),
        ],
    ).empty_table()
    right = pa.schema([pa.field("id", pa.int64())]).empty_table()

    with pytest.raises(ValueError, match='more than one column named "x"'):
        diff_tables(left, right, key=["id"])


def test_every_row_member_returns_an_arrow_table() -> None:
    """cells_changed and the other row members all return Arrow tables (the int pair has no differing rows)."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])

    for member in ("rows_added", "rows_removed", "duplicate_keys", "cells_changed"):
        assert len(getattr(diff, member)()) == 0


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


def test_schema_arrow_exports_schema_capsule() -> None:
    """schema_arrow exposes __arrow_c_schema__, consumable by pa.schema()."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])
    schema = pa.schema(diff.schema_arrow)

    assert schema.names == [
        "column",
        "change",
        "left_type",
        "right_type",
        "left_nullable",
        "right_nullable",
    ]


def test_reprs() -> None:
    """TableDiff and ArrowTable have informative reprs."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])

    assert repr(diff) == (
        "TableDiff(columns_added=1, columns_removed=1, columns_type_changed=1, "
        "rows_added=0, rows_removed=0, rows_changed=0, duplicate_keys=0)"
    )
    assert repr(diff.schema_arrow) == "ArrowTable(3 rows x 6 columns)"


def test_bad_requested_schema_capsule_raises_value_error() -> None:
    """A malformed requested_schema capsule raises ValueError, not a native crash or PanicException."""
    left, right = _int_tables()
    diff = diff_tables(left, right, key=["id"])

    new_capsule = ctypes.pythonapi.PyCapsule_New
    new_capsule.restype = ctypes.py_object
    new_capsule.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_void_p]
    # A zeroed ArrowSchema: its format pointer is NULL, which the arrow importer
    # asserts against (a panic), never a valid schema. Kept alive for the call.
    buffer = (ctypes.c_char * 256)()
    capsule = new_capsule(ctypes.cast(buffer, ctypes.c_void_p), b"arrow_schema", None)

    with pytest.raises(ValueError, match="not a valid Arrow C schema"):
        diff.schema_arrow.__arrow_c_stream__(requested_schema=capsule)


# Optional-dependency tests


def test_import_and_diff_without_pyarrow() -> None:
    """deepdiff_rs imports and diffs DuckDB tables with pyarrow unavailable, and to_pyarrow errors."""
    result = _run_isolated(
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

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().endswith("OK")


def test_to_pyarrow_propagates_a_broken_pyarrow() -> None:
    """A pyarrow that fails to import for a reason other than absence is propagated, not masked."""
    result = _run_isolated(
        """
        import importlib.abc
        import importlib.machinery
        import sys


        class BoomLoader(importlib.abc.Loader):
            def create_module(self, spec):
                return None

            def exec_module(self, module):
                raise ImportError("pyarrow is installed but broken")


        class BrokenPyarrow(importlib.abc.MetaPathFinder):
            def find_spec(self, name, path=None, target=None):
                if name == "pyarrow" or name.startswith("pyarrow."):
                    return importlib.machinery.ModuleSpec(name, BoomLoader())
                return None


        sys.modules.pop("pyarrow", None)
        sys.meta_path.insert(0, BrokenPyarrow())

        import duckdb
        import deepdiff_rs

        left = duckdb.sql("SELECT CAST(1 AS BIGINT) AS id, 1 AS a")
        right = duckdb.sql("SELECT CAST(1 AS BIGINT) AS id, 1 AS b")
        diff = deepdiff_rs.diff_tables(left, right, key=["id"])

        try:
            diff.schema_arrow.to_pyarrow()
        except ImportError as error:
            # The real failure is propagated; the install-the-extra hint is only
            # for a genuinely-absent pyarrow (ModuleNotFoundError).
            assert "installed but broken" in str(error), str(error)
            assert "deepdiff-rs[arrow]" not in str(error), str(error)
        else:
            raise AssertionError("expected the broken pyarrow's ImportError to propagate")

        print("OK")
        """,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().endswith("OK")


# Deep-nesting safety tests
#
# Arrow type nesting is attacker-controlled. Importing, comparing, and dropping
# a deeply nested type all recurse on the native stack and would SIGSEGV the
# interpreter; diff_tables must instead raise MaxDepthError. Each subprocess case
# turns a regression into a non-zero exit, and the first is the control proving
# that mechanism catches a native crash.


def test_subprocess_harness_detects_a_native_crash() -> None:
    """Control: a genuine SIGSEGV makes the subprocess exit non-zero, so the guard tests below mean something."""
    result = _run_isolated(
        """
        import ctypes

        ctypes.string_at(0)  # null dereference -> SIGSEGV
        print("SHOULD NOT REACH")
        """,
    )

    assert result.returncode != 0
    assert "SHOULD NOT REACH" not in result.stdout


def test_deeply_nested_type_raises_max_depth_error_not_a_crash() -> None:
    """A column nested well past the bound raises MaxDepthError and the process stays alive."""
    result = _run_isolated(
        """
        import pyarrow as pa
        from deepdiff_rs import diff_tables, MaxDepthError

        t = pa.int64()
        for _ in range(500):
            t = pa.struct([pa.field("f", t)])
        schema = pa.schema([pa.field("id", pa.int64()), pa.field("deep", t)])
        left = schema.empty_table()
        right = schema.empty_table()

        try:
            diff_tables(left, right, key=["id"])
        except MaxDepthError:
            print("OK")
        else:
            raise AssertionError("expected MaxDepthError")
        """,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().endswith("OK")


def test_max_depth_error_is_a_value_error_subclass() -> None:
    """MaxDepthError raised by diff_tables is catchable as ValueError."""
    left = pa.schema([pa.field("id", pa.int64())]).empty_table()
    inner = pa.int64()
    for _ in range(200):
        inner = pa.struct([pa.field("f", inner)])
    right = pa.schema([pa.field("id", pa.int64()), pa.field("deep", inner)]).empty_table()

    assert issubclass(MaxDepthError, ValueError)
    with pytest.raises(MaxDepthError):
        diff_tables(left, right, key=["id"])


# Machine-dependence tests


def test_duckdb_session_timezone_labelling_is_documented_and_workaround_works() -> None:
    """DuckDB labels TIMESTAMPTZ with the session time zone on Arrow export; SET TimeZone='UTC' makes it deterministic."""
    result = _run_isolated(
        """
        import pyarrow as pa
        import duckdb
        from deepdiff_rs import diff_tables

        pa_utc = pa.table({
            "id": pa.array([1], pa.int64()),
            "ts": pa.array([0], pa.timestamp("us", tz="UTC")),
        })

        rel = duckdb.sql("SELECT CAST(1 AS BIGINT) AS id, TIMESTAMPTZ '2020-01-01 00:00:00+00' AS ts")
        changed = {r["column"] for r in diff_tables(rel, pa_utc, key=["id"]).schema}
        assert changed == {"ts"}, changed

        duckdb.sql("SET TimeZone='UTC'")
        rel_utc = duckdb.sql("SELECT CAST(1 AS BIGINT) AS id, TIMESTAMPTZ '2020-01-01 00:00:00+00' AS ts")
        assert diff_tables(rel_utc, pa_utc, key=["id"]).schema == []

        print("OK")
        """,
        env={"TZ": "America/New_York"},
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip().endswith("OK")
