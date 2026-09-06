"""Tests for `polars_spike.py`: its `cells_changed_table` reports the same
rows, by key/column, as `diff_tables`. Skips (not errors) under `perf/arrow`'s
own venv, which carries neither `polars` nor `deepdiff_rs` -- both are only
guaranteed together in `crates/onix-py`'s venv (see `polars_spike.py`'s own
usage docstring).
"""

from pathlib import Path

import pytest

pl = pytest.importorskip("polars")
pytest.importorskip("deepdiff_rs")

import pyarrow as pa  # noqa: E402
import pyarrow.parquet as pq  # noqa: E402
from deepdiff_rs import diff_tables  # noqa: E402

from generate_fixtures import generate  # noqa: E402
from polars_spike import cells_changed_table  # noqa: E402

SEED = 246810


@pytest.mark.slow
def test_cells_changed_table_matches_diff_tables_shape(tmp_path: Path) -> None:
    """Same row count and the same set of `(id, column)` pairs as `diff_tables`, at 100k rows.

    Values may differ in rendering: `cells_changed_table`'s docstring documents where its
    `Utf8`-cast rendering diverges from onix's own Python-rule rendering.
    """
    fixture_dir = tmp_path / "fixture"
    generate(100_000, SEED, fixture_dir)

    left = pq.read_table(fixture_dir / "a.parquet")
    right = pq.read_table(fixture_dir / "b.parquet")
    onix_cells = pa.table(diff_tables(left, right, key=["id"]).cells_changed()).to_pylist()
    onix_pairs = {(row["id"], row["column"]) for row in onix_cells}

    a = pl.read_parquet(fixture_dir / "a.parquet")
    b = pl.read_parquet(fixture_dir / "b.parquet")
    _, table = cells_changed_table(a, b, ["id"], "narrow")
    spike_pairs = set(zip(table["id"].to_list(), table["column"].to_list(), strict=True))

    assert table.height == len(onix_cells)
    assert spike_pairs == onix_pairs
