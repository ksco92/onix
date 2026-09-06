"""Ceiling for a polars-backed table diff: the cost of also producing onix's
per-cell `cells_changed` report, not just `bench_tables.py`'s
`_polars_counts` join-based counts (no per-cell rows, no rendering, no
order, a duplicate key fanned out rather than reported) (#93).

`prepare` runs the same anti/inner joins as `_polars_counts`, null-safe
(`nulls_equal=True`, renamed from `join_nulls` in 1.24, onix's own
null-key rule); a key occurring more than once on either side is found
first (`group_by(key).len()`), excluded before any join, and reported with
its per-side counts, matching onix's duplicate-key handling.
`cells_changed_table` compares every common non-key column (left-schema
order, `_compare_columns`) with `.ne_missing()` (like SQL's `IS DISTINCT
FROM`), casts both sides to `Utf8` for the differing rows, and
concatenates the per-column fragments; `change` is
`became_null`/`became_non_null` on a null transition, else `type_changed`
if the column's polars dtype differs between the two sides (a cheap,
schema-level check) else `value_changed`. The sort key is each key column
cast to `Utf8` (nulls first, polars' `nulls_last` default) then column
rank -- onix's record order (`row_diff.rs`'s module doc).

Divergences from onix (the point is the timing ceiling, not byte parity):
`_render` is polars' own formatter, not onix's Python `repr`/`str`
rendering (digit count or format can differ; `Binary` hex-encodes and
`Duration` casts by physical value, since neither casts to `Utf8`
directly). onix's `type_changed` fires only on a value-domain mismatch; a
lossless width/scale/precision change is `value_changed` there when the
value differs, where this script's schema-level check calls it
`type_changed` regardless (`wide`'s `dec_scale4` is the concrete case).
All frames (both tables, matched, long-format) are full copies resident at
once -- no streamed, bounded-memory path. A comparison a join can't align
at all (`wide`'s zone-aware-vs-naive `ts_cast`) is excluded by
`_compare_columns`, for the same reason `_polars_counts` excludes it
(`SchemaError`).

Usage (from `crates/onix-py`'s venv, which carries polars), also runnable
as a benchmark worker mirroring `bench_tables.py --worker`::

    cd crates/onix-py
    uv run --group test python ../../perf/arrow/polars_spike.py --left a.parquet --right b.parquet --key id --out /tmp/spike
    uv run --group test python ../../perf/arrow/polars_spike.py --worker cells /path/to/fixture_dir narrow id
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Final

import polars as pl

from bench_tables import _compare_columns, _time_call

CHANGE_BECAME_NULL: Final[str] = "became_null"
CHANGE_BECAME_NON_NULL: Final[str] = "became_non_null"
CHANGE_TYPE: Final[str] = "type_changed"
CHANGE_VALUE: Final[str] = "value_changed"


def _render(expr: pl.Expr, dtype: pl.DataType) -> pl.Expr:
    """Cast to `Utf8`; `Binary`/`Duration` can't cast directly, so those hex-encode / cast by physical value first."""
    if dtype == pl.Binary:
        return expr.bin.encode("hex")
    return expr.to_physical().cast(pl.Utf8) if dtype == pl.Duration else expr.cast(pl.Utf8)


def prepare(a: pl.DataFrame, b: pl.DataFrame, key: list[str], kind: str) -> dict[str, object]:
    """
    Run the anti/inner joins with duplicate keys excluded and reported.

    :param a: The base table.
    :param b: The changed table.
    :param key: Key column names.
    :param kind: `"narrow"` or `"wide"` (`"wide"` drops `ts_cast`).
    :return: `matched` (non-duplicate frame, right columns suffixed `"_b"`),
        `rows_added`/`rows_removed`/`duplicate_keys`, and `compare_columns` (left-schema order).
    """
    left_counts = a.group_by(key).len().rename({"len": "left_count"})
    right_counts = b.group_by(key).len().rename({"len": "right_count"})
    summary = left_counts.join(right_counts, on=key, how="full", nulls_equal=True, coalesce=True)
    summary = summary.with_columns(pl.col("left_count").fill_null(0), pl.col("right_count").fill_null(0))
    duplicate_keys = summary.filter((pl.col("left_count") > 1) | (pl.col("right_count") > 1)).height
    clean_keys = summary.filter((pl.col("left_count") <= 1) & (pl.col("right_count") <= 1)).select(key)

    a_clean = a.join(clean_keys, on=key, how="inner", nulls_equal=True)
    b_clean = b.join(clean_keys, on=key, how="inner", nulls_equal=True)
    added = b_clean.join(a_clean.select(key), on=key, how="anti", nulls_equal=True).height
    removed = a_clean.join(b_clean.select(key), on=key, how="anti", nulls_equal=True).height
    matched = a_clean.join(b_clean, on=key, how="inner", nulls_equal=True, suffix="_b")

    compare_columns = _compare_columns(a, b, key, kind)

    return {
        "matched": matched,
        "rows_added": added,
        "rows_removed": removed,
        "duplicate_keys": duplicate_keys,
        "compare_columns": compare_columns,
    }


def joins_only(a: pl.DataFrame, b: pl.DataFrame, key: list[str], kind: str) -> dict[str, int]:
    """
    Phase (a): the anti/inner-join counts alone (`rows_added`,
    `rows_removed`, `cells_changed`, `duplicate_keys`), no per-cell table.
    Same parameters as `prepare`.
    """
    p = prepare(a, b, key, kind)
    matched = p["matched"]
    cells_changed = sum(int(matched[c].ne_missing(matched[f"{c}_b"]).sum()) for c in p["compare_columns"])

    return {
        "rows_added": p["rows_added"],
        "rows_removed": p["rows_removed"],
        "cells_changed": cells_changed,
        "duplicate_keys": p["duplicate_keys"],
    }


def cells_changed_table(a: pl.DataFrame, b: pl.DataFrame, key: list[str], kind: str) -> tuple[dict[str, int], pl.DataFrame]:
    """
    Phase (b): `joins_only`'s counts, plus the rendered, ordered
    `cells_changed` table (key columns, `column`, `old_value`, `new_value`,
    `change`, in onix's record order). Same parameters as `prepare`.
    """
    p = prepare(a, b, key, kind)
    matched = p["matched"]
    parts = []
    for rank, name in enumerate(p["compare_columns"]):
        left_col, right_col = pl.col(name), pl.col(f"{name}_b")
        type_changed = a.schema[name] != b.schema[name]
        change_expr = (
            pl.when(left_col.is_null() & right_col.is_not_null())
            .then(pl.lit(CHANGE_BECAME_NON_NULL))
            .when(left_col.is_not_null() & right_col.is_null())
            .then(pl.lit(CHANGE_BECAME_NULL))
            .when(pl.lit(type_changed))
            .then(pl.lit(CHANGE_TYPE))
            .otherwise(pl.lit(CHANGE_VALUE))
        )
        part = matched.filter(left_col.ne_missing(right_col)).select(
            *key,
            pl.lit(name).alias("column"),
            _render(left_col, a.schema[name]).alias("old_value"),
            _render(right_col, b.schema[name]).alias("new_value"),
            change_expr.alias("change"),
            pl.lit(rank).alias("__rank"),
        )
        if part.height:
            parts.append(part)

    if parts:
        table = pl.concat(parts, how="vertical")
        key_str = [pl.col(k).cast(pl.Utf8).alias(f"__keystr_{i}") for i, k in enumerate(key)]
        sort_cols = [f"__keystr_{i}" for i in range(len(key))]
        table = table.with_columns(key_str).sort(by=[*sort_cols, "__rank"]).drop([*sort_cols, "__rank"])
    else:
        schema = {
            **{k: a.schema[k] for k in key},
            "column": pl.Utf8,
            "old_value": pl.Utf8,
            "new_value": pl.Utf8,
            "change": pl.Utf8,
        }
        table = pl.DataFrame(schema=schema)

    counts = {
        "rows_added": p["rows_added"],
        "rows_removed": p["rows_removed"],
        "cells_changed": table.height,
        "duplicate_keys": p["duplicate_keys"],
    }
    return counts, table


def _run_worker(mode: str, fixture_dir: Path, key: list[str], kind: str) -> None:
    """
    Run one phase (`mode`: `"joins"` or `"cells"`) over `fixture_dir`'s pair
    and print its wall/CPU/RSS measurement as JSON, via `bench_tables.py`'s
    shared `_time_call` (same argument order as its own `_run_worker`).
    """

    def run() -> None:
        a = pl.read_parquet(fixture_dir / "a.parquet")
        b = pl.read_parquet(fixture_dir / "b.parquet")
        if mode == "joins":
            joins_only(a, b, key, kind)
        else:
            cells_changed_table(a, b, key, kind)

    print(json.dumps(_time_call(run)))


def main() -> None:
    """Parse CLI arguments, run the spike once, and write its output table."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--left", type=Path, required=True, help="Base (a) parquet file.")
    parser.add_argument("--right", type=Path, required=True, help="Changed (b) parquet file.")
    parser.add_argument("--key", action="append", required=True, help="Key column(s); repeatable.")
    parser.add_argument("--kind", choices=("narrow", "wide"), default="narrow", help="Fixture kind.")
    parser.add_argument("--out", type=Path, required=True, help="Output directory for cells_changed.parquet.")
    args = parser.parse_args()

    a = pl.read_parquet(args.left)
    b = pl.read_parquet(args.right)
    counts, table = cells_changed_table(a, b, args.key, args.kind)
    args.out.mkdir(parents=True, exist_ok=True)
    table.write_parquet(args.out / "cells_changed.parquet")
    print(json.dumps(counts, indent=2))


if __name__ == "__main__":
    if len(sys.argv) >= 5 and sys.argv[1] == "--worker":
        _run_worker(sys.argv[2], Path(sys.argv[3]), sys.argv[5:], sys.argv[4])
    else:
        main()
