"""Tests for `oracle_duckdb.py` against the `wide` fixture (#84): the counts
it *can* see match the sidecar exactly, and the two documented gaps
(dictionary retype, timestamp zone-awareness) behave as the module
docstrings describe.
"""

from pathlib import Path

from generate_fixtures import generate
from oracle_duckdb import run

SEED = 13579


def test_wide_oracle_counts_match_the_non_type_changed_sidecar_total(tmp_path: Path) -> None:
    """DuckDB's `cells_changed` is the sidecar's value/null-transition total, never `type_changed`."""
    fixture_dir = tmp_path / "fixture"
    manifest = generate(5000, SEED, fixture_dir, kind="wide")

    summary = run(fixture_dir / "a.parquet", fixture_dir / "b.parquet", ["id"], tmp_path / "oracle")

    expected_cells = manifest["cells_value_changed"] + manifest["cells_became_non_null"] + manifest["cells_became_null"]
    assert summary["rows_added"] == manifest["rows_added"]
    assert summary["rows_removed"] == manifest["rows_deleted"]
    assert summary["duplicate_keys"] == manifest["duplicate_keys"]
    assert summary["cells_changed"] == expected_cells


def test_wide_oracle_schema_changes_excludes_the_dictionary_retype(tmp_path: Path) -> None:
    """`category`'s retype is invisible to the SQL schema diff; the other three show up."""
    fixture_dir = tmp_path / "fixture"
    manifest = generate(5000, SEED, fixture_dir, kind="wide")

    summary = run(fixture_dir / "a.parquet", fixture_dir / "b.parquet", ["id"], tmp_path / "oracle")

    assert summary["schema_changes"] == len(manifest["schema_changes"]) - 1
