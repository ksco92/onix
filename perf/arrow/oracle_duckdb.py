"""Independent DuckDB SQL oracle for the Arrow table-diff fixture pair.

Computes, using DuckDB SQL only (joins, `GROUP BY`, and set membership --
no row-by-row Python), what a correct table diff of `a.parquet`/`b.parquet`
must report: schema differences, rows added, rows removed, changed rows
(long format: key, column, old_value, new_value), and duplicate keys. This
serves two purposes later in the project: a correctness reference the Rust
`onix-arrow` crate's own results are checked against (#39, #40), and one of
two speed baselines a data engineer would otherwise reach for (#43).

# Value-comparison semantics (for the Rust implementation to match)

* **Nulls.** Every comparison uses `IS DISTINCT FROM`, never `=`/`<>`: SQL's
  three-valued `=` returns `NULL` (not `TRUE`) when either side is `NULL`,
  which would silently drop a null-involving row out of a `WHERE` filter
  instead of reporting it as changed. `IS DISTINCT FROM` treats two `NULL`s
  as equal (unchanged) and `NULL` vs. non-`NULL` as different -- the
  `became_null`/`became_non_null` cases #40 defines. This fixture has no
  `NULL` non-key cells (every base column is always populated on both
  sides), so the rule is documented but not exercised by this pair; a
  future fixture that adds nullable cells should add a duplicate-run
  determinism check that also probes `IS DISTINCT FROM`'s branches.
* **Null keys.** Per #39, "null in a key column counts as a distinct key
  value that equals itself" -- a null-keyed row must still be matched
  against its counterpart on the other side, not treated as a non-match.
  Plain equality (`=`, and `USING (...)`'s implicit `=`) breaks this: `NULL
  = NULL` is `UNKNOWN`, so a join on `=` silently excludes every null-keyed
  row from a match, reporting it as both added and removed, and reports a
  null key duplicated on both sides as two separate `duplicate_keys` rows
  instead of one with combined counts. Every join on the key columns in
  this module (`_build_key_summary`, `_write_cells_changed`, and `run`'s
  added/removed queries) therefore uses `_null_safe_join`'s `IS NOT
  DISTINCT FROM` predicate per key column, not `USING`/`=`. `GROUP BY`
  needs no such fix: it already groups `NULL`s together per standard SQL
  grouping semantics, so `key_summary`'s per-side row counts are correct
  as soon as the join that reunites the two sides' counts is null-safe. A
  composite key matches null-safely component-by-component, so one `NULL`
  component doesn't prevent the other components from still requiring an
  exact match.
* **Decimals.** `amount` is `DECIMAL(18,4)` on both sides here, so DuckDB
  compares exact scaled integers with no floating-point rounding --
  `IS DISTINCT FROM` on two same-scale decimals is exact. A pair whose
  scale actually changes between sides would need an explicit `CAST` to a
  common scale before comparing (not exercised by this fixture, since
  `amount`'s scale never changes in the mutation mix -- see
  `generate_fixtures.py`'s module docstring).
* **Timestamps across units.** `a.ts` is `timestamp[us, UTC]`; `b.ts` is
  `timestamp[ms, UTC]`. For *value* comparisons, DuckDB's parquet reader
  normalizes both to its own internal microsecond-precision `TIMESTAMP WITH
  TIME ZONE` at read time, so `a.ts IS DISTINCT FROM b.ts` already compares
  by instant with no explicit unit-normalization cast needed in the query
  text -- confirmed by `tests/test_oracle_duckdb.py`'s same-instant and
  different-sub-millisecond-instant timestamp tests. That same
  normalization, however, means the unit change is invisible at the
  *schema* level through `DESCRIBE`/`pragma_table_info` (both columns
  report the identical DuckDB type, confirmed empirically); `_schema_diff`
  below reads `parquet_schema()`'s `converted_type` column instead, which
  does still show `TIMESTAMP_MICROS` vs. `TIMESTAMP_MILLIS`, because that
  reports the file's actual stored Parquet annotation rather than DuckDB's
  own normalized SQL type.
* **Floats.** This fixture has no `FLOAT`/`DOUBLE` column (`amount` is a
  fixed-point `DECIMAL`), so this oracle carries no opinion on float
  equality (significant digits, signed zero, NaN); a fixture that adds a
  float column must state its own comparison rule before this oracle can
  be trusted for it.
* **Dictionary encoding is invisible here.** Parquet's own type system has
  no "dictionary-encoded string" logical type -- it is purely an Arrow-side
  annotation that `pyarrow` round-trips through a private `ARROW:schema`
  key in the file's footer metadata, which DuckDB's parquet reader does not
  decode. `category`'s `string -> dictionary<int32, string>` retype
  (`generate_fixtures.py`) is therefore **not** one of the schema changes
  this oracle can detect over SQL: `DESCRIBE`/`pragma_table_info` reports
  `category` as `VARCHAR` on both sides, both before and after the retype,
  because that is genuinely what the Parquet file's physical schema says.
  This is an accepted, structural limitation of a SQL-only, Parquet-reading
  oracle, not a bug -- `perf/arrow/README.md`'s "Oracle semantics" section
  restates it, and the dictionary retype is instead verified directly via
  `pyarrow` in `tests/test_oracle_duckdb.py`.

Usage::

    cd perf/arrow
    uv run --group perf oracle_duckdb.py --left fixtures/1k/a.parquet --right fixtures/1k/b.parquet --key id --out /tmp/oracle_1k

Writes `<out>/schema_diff.parquet`, `<out>/rows_added.parquet`,
`<out>/rows_removed.parquet`, `<out>/cells_changed.parquet`, and
`<out>/duplicate_keys.parquet`, and prints a JSON summary of counts to
stdout (comparable against `generate_fixtures.py`'s sidecar `manifest.json`).
"""

import argparse
import json
from pathlib import Path

import duckdb

##############################################
##############################################
##############################################
##############################################
# SQL-building helpers (identifier/literal quoting, null-safe key joins)


def _quote_ident(name: str) -> str:
    """
    Quote `name` as a DuckDB identifier, escaping embedded double quotes.

    Every column name interpolated into a query in this module goes
    through this (never a bare f-string), so a parquet file with a column
    named with a quote, a space, a semicolon, or a reserved word can't
    break the generated SQL.

    :param name: The raw column name.
    :return: The double-quoted, escaped identifier.
    """
    return '"' + name.replace('"', '""') + '"'


def _quote_literal(value: str) -> str:
    """
    Quote `value` as a DuckDB string literal, escaping embedded single quotes.

    :param value: The raw string value.
    :return: The single-quoted, escaped literal.
    """
    return "'" + value.replace("'", "''") + "'"


def _null_safe_join(alias_a: str, alias_b: str, key_columns: list[str]) -> str:
    """
    Build a null-safe `ON` predicate over every key column.

    Plain equality (`=`, and `USING (...)`'s implicit `=`) treats `NULL =
    NULL` as `UNKNOWN`, silently excluding every null-keyed row from a
    join -- see the module docstring's "Null keys" rule. Every join on the
    key columns in this module uses this predicate instead of `USING`.

    :param alias_a: Left-side table alias.
    :param alias_b: Right-side table alias.
    :param key_columns: Key column names.
    :return: The predicate text (no leading `ON`), e.g.
        `a."id" IS NOT DISTINCT FROM b."id"`.
    """
    return " AND ".join(
        f"{alias_a}.{_quote_ident(c)} IS NOT DISTINCT FROM {alias_b}.{_quote_ident(c)}" for c in key_columns
    )


##############################################
##############################################
##############################################
##############################################
# Schema diff


def _schema_diff(con: duckdb.DuckDBPyConnection, left: Path, right: Path) -> list[tuple[str, str | None, str | None, str]]:
    """
    Compare `left`'s and `right`'s top-level column types via a full outer
    join on column name, using `parquet_schema()` rather than
    `DESCRIBE`/`pragma_table_info`: DuckDB normalizes both `timestamp[us]`
    and `timestamp[ms]` to the same internal `TIMESTAMP WITH TIME ZONE` SQL
    type (verified empirically -- see the module docstring), which would
    hide a real unit change from a `DESCRIBE`-based diff. `parquet_schema()`
    instead reports each file's actual stored Parquet annotation
    (`converted_type`, e.g. `TIMESTAMP_MICROS` vs. `TIMESTAMP_MILLIS`),
    which does reflect it.

    :param con: Connection to run the query on.
    :param left: Path to the base (`a`) parquet file.
    :param right: Path to the changed (`b`) parquet file.
    :return: `(column, left_type, right_type, change)` rows for every
        column whose presence or type differs; `change` is one of
        `"added"`, `"removed"`, `"type_changed"`.
    """
    rows = con.sql(
        f"""
        WITH left_schema AS (
            SELECT name, CASE WHEN converted_type LIKE 'TIMESTAMP%' THEN converted_type ELSE duckdb_type END AS type_label
            FROM parquet_schema({_quote_literal(str(left))}) WHERE name != 'schema'
        ), right_schema AS (
            SELECT name, CASE WHEN converted_type LIKE 'TIMESTAMP%' THEN converted_type ELSE duckdb_type END AS type_label
            FROM parquet_schema({_quote_literal(str(right))}) WHERE name != 'schema'
        )
        SELECT COALESCE(a.name, b.name) AS "column", a.type_label AS left_type, b.type_label AS right_type
        FROM left_schema a
        FULL OUTER JOIN right_schema b ON a.name = b.name
        WHERE a.name IS NULL OR b.name IS NULL OR a.type_label != b.type_label
        ORDER BY "column"
        """,
    ).fetchall()

    def classify(left_type: str | None, right_type: str | None) -> str:
        """
        :param left_type: The column's type in `left`, or `None` if absent.
        :param right_type: The column's type in `right`, or `None` if absent.
        :return: `"added"`, `"removed"`, or `"type_changed"`.
        """
        if left_type is None:
            return "added"

        if right_type is None:
            return "removed"

        return "type_changed"

    return [(column, left_type, right_type, classify(left_type, right_type)) for column, left_type, right_type in rows]


##############################################
##############################################
##############################################
##############################################
# Key membership (added / removed / changed / duplicates)


def _build_key_summary(con: duckdb.DuckDBPyConnection, key_columns: list[str]) -> None:
    """
    Create the `key_summary` temp table: one row per distinct key value
    across `va`/`vb`, with each side's row count -- the basis for every
    added/removed/changed/duplicate classification below.

    :param con: Connection with `va`/`vb` views already created.
    :param key_columns: The key column names (composite keys join on all of them).
    """
    key_list = ", ".join(_quote_ident(c) for c in key_columns)
    coalesce_list = ", ".join(
        f"COALESCE(a.{_quote_ident(c)}, b.{_quote_ident(c)}) AS {_quote_ident(c)}" for c in key_columns
    )
    join_predicate = _null_safe_join("a", "b", key_columns)
    con.execute(
        f"""
        CREATE TEMP TABLE key_summary AS
        WITH a_counts AS (SELECT {key_list}, COUNT(*) AS left_count FROM va GROUP BY {key_list}),
             b_counts AS (SELECT {key_list}, COUNT(*) AS right_count FROM vb GROUP BY {key_list})
        SELECT
            {coalesce_list},
            COALESCE(a.left_count, 0) AS left_count,
            COALESCE(b.right_count, 0) AS right_count
        FROM a_counts a
        FULL OUTER JOIN b_counts b ON {join_predicate}
        """,
    )


##############################################
##############################################
##############################################
##############################################
# Changed cells (long format)


def _common_non_key_columns(con: duckdb.DuckDBPyConnection, key_columns: list[str]) -> list[str]:
    """
    :param con: Connection with `va`/`vb` views already created.
    :param key_columns: Columns to exclude (they identify the row, not a cell).
    :return: Column names present in both `va` and `vb`, excluding the key
        -- columns present on only one side are schema differences, never
        cell changes (per #40's contract).
    """
    rows = con.sql(
        """
        SELECT a.name
        FROM pragma_table_info('va') a
        INNER JOIN pragma_table_info('vb') b ON a.name = b.name
        ORDER BY a.name
        """,
    ).fetchall()

    return [name for (name,) in rows if name not in key_columns]


def _write_cells_changed(
    con: duckdb.DuckDBPyConnection,
    key_columns: list[str],
    compare_columns: list[str],
    out_path: Path,
) -> int:
    """
    Compute and write the long-format `cells_changed` table: one row per
    `(key, column)` pair whose value differs between a matched, non-duplicate
    key's `va` row and `vb` row.

    :param con: Connection with `va`/`vb` views and `key_summary` already built.
    :param key_columns: The key column names.
    :param compare_columns: Non-key columns present on both sides.
    :param out_path: Parquet file to write.
    :return: Number of changed-cell rows written.
    """
    key_list = ", ".join(_quote_ident(c) for c in key_columns)
    key_select = ", ".join(f"a.{_quote_ident(c)}" for c in key_columns)
    matched_join = _null_safe_join("a", "k", key_columns)
    cell_join = _null_safe_join("a", "b", key_columns)
    per_column_selects = " UNION ALL ".join(
        f"""
        SELECT {key_select}, {_quote_literal(column)} AS "column",
               CAST(a.{_quote_ident(column)} AS VARCHAR) AS old_value, CAST(b.{_quote_ident(column)} AS VARCHAR) AS new_value
        FROM matched a
        JOIN vb b ON {cell_join}
        WHERE a.{_quote_ident(column)} IS DISTINCT FROM b.{_quote_ident(column)}
        """
        for column in compare_columns
    )
    con.execute(
        f"""
        COPY (
            WITH matched AS (
                SELECT a.* FROM va a
                INNER JOIN (SELECT {key_list} FROM key_summary WHERE left_count = 1 AND right_count = 1) k
                    ON {matched_join}
            )
            {per_column_selects}
            ORDER BY {key_list}, "column"
        ) TO {_quote_literal(str(out_path))} (FORMAT PARQUET)
        """,
    )

    return con.sql(f"SELECT COUNT(*) FROM read_parquet({_quote_literal(str(out_path))})").fetchone()[0]


##############################################
##############################################
##############################################
##############################################
# Top-level run


def run(left: Path, right: Path, key_columns: list[str], out_dir: Path) -> dict[str, object]:
    """
    Run every SQL computation against `left`/`right` and write the five
    output parquet files under `out_dir`.

    :param left: Path to the base (`a`) parquet file.
    :param right: Path to the changed (`b`) parquet file.
    :param key_columns: Row-identity key column name(s).
    :param out_dir: Directory to write the five result parquet files into.
    :return: A JSON-serializable summary of counts.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    con = duckdb.connect()
    # DuckDB renders a TIMESTAMPTZ's CAST(... AS VARCHAR) in the session's
    # timezone, not UTC -- pinned here so `cells_changed`'s old_value/new_value
    # strings are the same on every machine regardless of its local timezone.
    con.execute("SET TimeZone = 'UTC'")
    con.execute(f"CREATE VIEW va AS SELECT * FROM read_parquet({_quote_literal(str(left))})")
    con.execute(f"CREATE VIEW vb AS SELECT * FROM read_parquet({_quote_literal(str(right))})")

    schema_rows = _schema_diff(con, left, right)
    con.execute(
        'CREATE TEMP TABLE schema_diff ("column" VARCHAR, left_type VARCHAR, right_type VARCHAR, change VARCHAR)',
    )
    if schema_rows:
        con.executemany("INSERT INTO schema_diff VALUES (?, ?, ?, ?)", schema_rows)
    con.execute(f"COPY schema_diff TO {_quote_literal(str(out_dir / 'schema_diff.parquet'))} (FORMAT PARQUET)")

    _build_key_summary(con, key_columns)
    key_list = ", ".join(_quote_ident(c) for c in key_columns)
    added_join = _null_safe_join("b", "k", key_columns)
    removed_join = _null_safe_join("a", "k", key_columns)

    con.execute(
        f"""
        COPY (
            SELECT b.* FROM vb b
            INNER JOIN (SELECT {key_list} FROM key_summary WHERE left_count = 0 AND right_count = 1) k ON {added_join}
        ) TO {_quote_literal(str(out_dir / "rows_added.parquet"))} (FORMAT PARQUET)
        """,
    )
    con.execute(
        f"""
        COPY (
            SELECT a.* FROM va a
            INNER JOIN (SELECT {key_list} FROM key_summary WHERE left_count = 1 AND right_count = 0) k ON {removed_join}
        ) TO {_quote_literal(str(out_dir / "rows_removed.parquet"))} (FORMAT PARQUET)
        """,
    )
    con.execute(
        f"""
        COPY (
            SELECT {key_list}, left_count, right_count FROM key_summary
            WHERE left_count > 1 OR right_count > 1
            ORDER BY {key_list}
        ) TO {_quote_literal(str(out_dir / "duplicate_keys.parquet"))} (FORMAT PARQUET)
        """,
    )

    compare_columns = _common_non_key_columns(con, key_columns)
    cells_changed_count = _write_cells_changed(con, key_columns, compare_columns, out_dir / "cells_changed.parquet")

    null_key_predicate = " OR ".join(f"{_quote_ident(c)} IS NULL" for c in key_columns)
    counts = con.sql(
        f"""
        SELECT
            COALESCE(SUM(CASE WHEN left_count = 0 AND right_count = 1 THEN 1 ELSE 0 END), 0) AS rows_added,
            COALESCE(SUM(CASE WHEN left_count = 1 AND right_count = 0 THEN 1 ELSE 0 END), 0) AS rows_removed,
            COALESCE(SUM(CASE WHEN left_count > 1 OR right_count > 1 THEN 1 ELSE 0 END), 0) AS duplicate_keys,
            COALESCE(SUM(CASE WHEN {null_key_predicate} THEN 1 ELSE 0 END), 0) AS null_keys
        FROM key_summary
        """,
    ).fetchone()

    con.close()

    return {
        "rows_added": counts[0],
        "rows_removed": counts[1],
        "duplicate_keys": counts[2],
        "null_keys": counts[3],
        "cells_changed": cells_changed_count,
        "schema_changes": len(schema_rows),
    }


def main() -> None:
    """Parse CLI arguments and run the oracle once."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--left", type=Path, required=True, help="Base (a) parquet file.")
    parser.add_argument("--right", type=Path, required=True, help="Changed (b) parquet file.")
    parser.add_argument(
        "--key", action="append", required=True, dest="key_columns", help="Key column(s); repeatable for a composite key.",
    )
    parser.add_argument("--out", type=Path, required=True, help="Output directory for the result parquet files.")
    args = parser.parse_args()

    summary = run(args.left, args.right, args.key_columns, args.out)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
