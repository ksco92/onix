# Arrow table-diff benchmark fixtures

A seeded, streaming parquet fixture pair for the Arrow table diff (#38-#41),
plus an independent DuckDB SQL oracle used both as a correctness reference
and as one of the two speed baselines the table diff is benchmarked against
(#43). Nothing here is committed: every fixture is regenerated from its seed.

## Generating fixtures

```sh
cd perf/arrow
uv sync --group perf
uv run --group perf generate_fixtures.py --rows 100000 --out fixtures/100k
```

`--rows` defaults to the full-size row count (see "Sizes" below); `--seed`
defaults to a fixed, documented value. Output is `<out>/a.parquet` (the base
table), `<out>/b.parquet` (the mutated table), and `<out>/manifest.json` (the
exact mutation counts -- see `generate_fixtures.py`'s module docstring for
the full mutation mix and every design decision behind it).

Two runs with the same `--rows`/`--seed` are byte-identical (SHA-256 over
two consecutive 100k-row runs):

```
a.parquet:        3813c6204c3e4865983df4e460a6078b4c6ccafb66bf1a80bedc0c6078dfa1a6
b.parquet:        79fd21fffec4c956a413fb77244a2307dea47315401e22035bde4785d1077bbb
manifest.json:    f06a7af894294833a40a03e36c9f5a3fa981d9934d526663369f1147cc765a68
```

## Sizes

Measured on the machine this file was written on (Apple Silicon, `uv`-managed
CPython 3.13, `pyarrow` 25.0.1). Generation time is the whole process,
including Python/pyarrow startup:

| Rows | `a.parquet` | `b.parquet` | Generation time |
| --- | --- | --- | --- |
| 1,000 | 0.1 MB | 0.1 MB | 0.20 s |
| 100,000 | 13.7 MB | 13.6 MB | 0.62 s |
| 1,000,000 | 135.2 MB | 134.2 MB | 5.2 s |
| 37,000,000 (default) | 5,004.0 MB | 4,966.9 MB | 2 min 55 s |

The row density (≈135 bytes/row for `a.parquet`) is linear from 1,000 rows up
through the full pair, so `DEFAULT_ROWS` (37,000,000) was set by solving for
the row count that lands `a.parquet` at 5 GB compressed at that density; the
resulting pair (5.00 GB + 4.97 GB) is close to the ~10 GB total the fixture
targets. Re-run and re-measure if `pyarrow`'s default parquet compression
settings ever change, since the density this constant was tuned against
would change with them.

## Mutation mix

`a.parquet` has five columns: `id` (int64, unique, ascending), `ts`
(`timestamp[us, UTC]`), `category` (string, 20 distinct values), `amount`
(`decimal(18,4)`), `payload` (string, 20-200 chars). `b.parquet` applies, in
one streaming pass from the same seed:

* 2% of surviving rows modified -- half get a new `amount`, half a new
  `payload` (never both; see `generate_fixtures.py`'s
  `test_modified_rows_change_exactly_one_of_amount_or_payload`).
* 1% of rows deleted.
* 1% new rows appended with fresh, higher ids.
* `category` re-typed to `dictionary<int32, string>` (values unchanged).
* `ts` cast from `timestamp[us, UTC]` to `timestamp[ms, UTC]` (lossless: every
  `ts` lands on a whole second).
* A new `note` column: `null` for every carried-over row, `"added"` for new rows.

No duplicate `id` appears on either side by construction -- this fixture's
ids are always unique. Duplicate-key handling (#39) is exercised by that
slice's own small synthetic/property tests, not by this fixture; the oracle
below still implements and tests real duplicate-key detection (see
`tests/test_oracle_duckdb.py`), it's just never triggered by the shared 5%-mutation
pair.

## Oracle semantics

```sh
cd perf/arrow
uv run --group perf oracle_duckdb.py --left fixtures/100k/a.parquet --right fixtures/100k/b.parquet --key id --out /tmp/oracle_100k
```

`oracle_duckdb.py` computes schema differences, rows added, rows removed,
changed cells (long format: key, column, old_value, new_value), and
duplicate keys, using DuckDB SQL (joins and `GROUP BY`, no row-by-row
Python), and writes each as a parquet file under `--out`. Its counts match
`generate_fixtures.py`'s sidecar exactly at 1k, 100k, and 1M rows (see
`tests/test_oracle_duckdb.py`), except for one documented, structural gap:
the `category` dictionary retype has no footprint in Parquet's own schema,
so it's invisible to a SQL-only schema diff (`oracle_duckdb.py`'s module
docstring, "Dictionary encoding is invisible here").

Its full value-comparison semantics -- nulls (including null keys, per #39),
decimals, cross-unit timestamps, and floats (none are exercised, since this
fixture has no float column) -- are documented in `oracle_duckdb.py`'s own
module docstring, since that's also where the SQL implementing each rule
lives.

## Tests

```sh
cd perf/arrow
uv run --group perf pytest tests -q             # fast: 1k-row fixtures + synthetic tables
uv run --group perf pytest tests -q -m slow      # also regenerates and checks the 100k pair
```
