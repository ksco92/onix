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
uv run --group perf generate_fixtures.py --kind wide --rows 100000 --out fixtures/wide-100k
```

`--rows` defaults to the full-size row count for the chosen `--kind` (see
"Sizes" below); `--seed` defaults to a fixed, documented value per kind.
Output is `<out>/a.parquet` (the base table), `<out>/b.parquet` (the mutated
table), and `<out>/manifest.json` (the exact mutation counts -- see
`generate_fixtures.py`'s module docstring for the full mutation mix and
every design decision behind it, both kinds).

Two runs with the same `--rows`/`--seed` are byte-identical (SHA-256 over
two consecutive 100k-row runs):

```
a.parquet:        3813c6204c3e4865983df4e460a6078b4c6ccafb66bf1a80bedc0c6078dfa1a6
b.parquet:        79fd21fffec4c956a413fb77244a2307dea47315401e22035bde4785d1077bbb
manifest.json:    f06a7af894294833a40a03e36c9f5a3fa981d9934d526663369f1147cc765a68
```

`--kind wide` is byte-identical the same way (SHA-256 over two consecutive 100k-row `wide` runs):

```
a.parquet:        04c2568a8e866dd1abfbd8e99c34fcb81a73dc890be3c7c6fc74c6a0d88f134e
b.parquet:        e91cf31ce42c9c79285d95bf112930a9629d99490e02901c1d725dbb4b69e05c
manifest.json:    f02af4a562b91f8fc1c515abed64923c0d23ce56e9fbbd57acee16bcac5ca73c
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

`--kind wide` (#84) trades the five narrow columns for one of every scalar
type the row diff hashes or renders, at the same ~5 GB-per-side target --
its column set and mutation mix are documented in `generate_fixtures.py`'s
own module docstring. Wider rows mean far fewer of them at the same size:

| Rows | `a.parquet` | `b.parquet` | Generation time |
| --- | --- | --- | --- |
| 200,000 | 59.3 MB | 59.4 MB | 4.1 s |
| 2,000,000 | 592.8 MB | 593.8 MB | 43.1 s |
| 16,875,000 (default) | 5,001.5 MB | 5,009.9 MB | 7 min 47 s |

Row density (≈296 bytes/row for `a.parquet`, after adding the
float16/decimal32/decimal64/view/fixed-size-binary columns) is linear over
the same range, so `WIDE_DEFAULT_ROWS` was solved the same way as
`DEFAULT_ROWS` above.

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

`wide`'s own column set and mutation mix (documented in
`generate_fixtures.py`'s module docstring) apply the same kinds of change --
deletions, additions, per-column modifications, and schema changes -- across
every scalar Arrow type instead of five columns.

## Oracle semantics

```sh
cd perf/arrow
uv run --group perf oracle_duckdb.py --left fixtures/100k/a.parquet --right fixtures/100k/b.parquet --key id --out /tmp/oracle_100k
```

`oracle_duckdb.py` computes schema differences, rows added, rows removed,
changed cells (long format: key, column, old_value, new_value, change), and
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

Against `wide`, the oracle picks up two further documented gaps -- a
zone-awareness change is invisible to its value comparison (DuckDB
normalizes to one instant type before comparing), and a `decimal256` above
precision 38 is unusable by either baseline -- both in the same module
docstring.

## Tests

```sh
cd perf/arrow
uv run --group perf pytest tests -q             # fast: 1k-row fixtures + synthetic tables
uv run --group perf pytest tests -q -m slow      # also regenerates and checks the 100k and 1M pairs
```

## Profiling the row diff's passes

The row diff's per-pass wall time and peak RSS come from the committed
`row_diff_profile` example (`crates/onix-arrow/examples/row_diff_profile.rs`),
built with the `profile` feature that compiles the pass boundaries into
`onix-arrow` (off by default, absent from the release wheel). Its own module
docstring is the single home for the method and the shapes; the command is:

```sh
cargo build -p onix-arrow --release --features profile --example row_diff_profile
# narrow-fixture proxy (few changed rows) and wide-fixture proxy (all rows
# changed, every value column spilled), 1M rows per side, default 18 threads:
target/release/examples/row_diff_profile 1000000 linear 18
target/release/examples/row_diff_profile 1000000 manycols 18 34 64
```

Every row-diff performance change posts its before/after per-pass table from this
command in its PR body, and updates the per-pass section of
[`RESULTS.md`](RESULTS.md).

## Benchmark against hand-rolled baselines

`bench_tables.py` times `diff_tables` against a DuckDB SQL diff and a polars join-based diff on
the narrow and wide fixture pairs (`--kind narrow`/`--kind wide`), at 1M rows and full size each;
its own module docstring is the single home for the methodology, correctness check, and fairness
rules. Results for both kinds, and both fixture pairs' disk usage (about 20.8 GB for all four
sizes at once), are in [`RESULTS.md`](RESULTS.md). The full-size `wide` run peaks at about 33.1 GB
of resident memory for `onix` alone at the default 18 threads (the streaming cell pass, issue #87;
it was about 67 GB before); size the runner accordingly before starting it.
