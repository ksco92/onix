# deepdiff-rs

[![CI](https://img.shields.io/github/actions/workflow/status/ksco92/onix/check.yml?branch=main&label=CI)](https://github.com/ksco92/onix/actions/workflows/check.yml)
[![coverage](https://codecov.io/gh/ksco92/onix/branch/main/graph/badge.svg)](https://codecov.io/gh/ksco92/onix)
[![PyPI](https://img.shields.io/pypi/v/deepdiff-rs.svg)](https://pypi.org/project/deepdiff-rs/)
[![downloads](https://img.shields.io/pypi/dm/deepdiff-rs.svg)](https://pypi.org/project/deepdiff-rs/)
[![license](https://img.shields.io/github/license/ksco92/onix.svg)](LICENSE)
[![last commit](https://img.shields.io/github/last-commit/ksco92/onix.svg)](https://github.com/ksco92/onix/commits/main)

**onix is a Rust rewrite of Python DeepDiff's core: byte-compatible output, 37-4245x faster, with `ignore_order` support included.** Install it as `deepdiff-rs`, a drop-in `DeepDiff` class for Python, or run the diff engine as the `onix` command-line tool.

`deepdiff-rs` reads live Python objects (or JSON) and produces the exact same report [DeepDiff](https://github.com/seperman/deepdiff) does at `verbose_level=2`, so it slots into code that already parses DeepDiff output while running dramatically faster on large or deeply nested inputs.

Status (September 2026): `deepdiff-rs` 0.x is live on PyPI (Python 3.9+, wheels for Linux x86_64/aarch64, macOS arm64/x86_64, and Windows x64, plus an sdist); the `onix` CLI builds from source, and nothing is on crates.io yet. Ordered and `ignore_order` diffing are complete, differentially tested against real DeepDiff 9.1.0, and [benchmarked](perf/RESULTS.md). It is 0.x, not stable or 1.0: the API may still change before 1.0.

## Table of contents

- [Install](#install)
- [Quickstart](#quickstart)
- [Diffing tables](#diffing-tables)
- [Known limitations](#known-limitations)
- [Performance](#performance)
- [Reference](#reference)
- [Layout](#layout)
- [Contributing](#contributing)
- [License](#license)

## Install

Python (the `deepdiff-rs` package, import name `deepdiff_rs`):

```sh
pip install deepdiff-rs
```

From source:

```sh
cd crates/onix-py
uv tool install maturin              # the build tool (skip if already installed)
uv sync --group test                 # creates .venv, installs pytest, pinned deepdiff, and pyarrow/polars/duckdb for the table-diff tests
uv run --group test maturin develop --release
```

CLI (the `onix` binary), from a clean clone:

```sh
cargo install --path crates/onix-cli
```

Library crate (`onix-core`), a path dependency only (it sets `publish = false`):

```toml
[dependencies]
onix-core = { path = "crates/onix-core" }
```

## Quickstart

The drop-in `DeepDiff` class, on live Python objects:

```python
from deepdiff_rs import DeepDiff

diff = DeepDiff({"a": 1}, {"a": 2})
if diff:
    print(diff.to_json())   # byte-compatible with DeepDiff(...).to_json() at verbose_level=2
    print(diff.to_dict())   # the same report as a native Python dict
```

```
{"values_changed":{"root['a']":{"new_value":2,"old_value":1}}}
{'values_changed': {"root['a']": {'new_value': 2, 'old_value': 1}}}
```

`diff_json`, the fast path when you already have JSON text (it parses, diffs, and serializes entirely in Rust, with no Python-object conversion):

```python
from deepdiff_rs import diff_json

print(diff_json('{"a": 1}', '{"a": 2}'))
```

```
{"values_changed":{"root['a']":{"new_value":2,"old_value":1}}}
```

The `onix` CLI, diffing two JSON files (compact JSON to stdout, `{}` for no differences):

```sh
$ echo '{"a": 1}' > left.json
$ echo '{"a": 2}' > right.json
$ onix diff left.json right.json
{"values_changed":{"root['a']":{"new_value":2,"old_value":1}}}
```

Pass `--ignore-order` to compare every list by value instead of by position, mirroring `DeepDiff(..., ignore_order=True)`.

## Diffing tables

`diff_tables` compares two tables the way `DeepDiff` compares two objects. It takes any object implementing the [Arrow PyCapsule interface](https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html) — a pyarrow `Table` or `RecordBatch`, a polars `DataFrame`, a DuckDB relation — and imports it with no Python round trip. The two tables are matched on a required, non-empty set of key columns (the table's primary key).

It reports the **schema** diff (which columns were added, removed, or changed type) and the keyed **row** diff (which rows were added, removed, or changed, and which keys are duplicated). Rows are matched by the key columns; `rows_added`, `rows_removed`, and `duplicate_keys` return Arrow tables, and `summary()` counts each outcome. The per-cell diff (`cells_changed`) arrives in a later version and raises `NotImplementedError` until then.

```python
import pyarrow as pa
from deepdiff_rs import diff_tables

left = pa.table({
    "id": pa.array([1, 2, 3], pa.int64()),
    "amount": pa.array([10, 20, 30], pa.int32()),
})
right = pa.table({
    "id": pa.array([2, 3, 4], pa.int64()),
    "amount": pa.array([20, 31, 40], pa.int64()),
    "note": pa.array(["a", "b", "c"], pa.string()),
})

diff = diff_tables(left, right, key=["id"])
print(diff.summary())
print("added ids:", pa.table(diff.rows_added()).column("id").to_pylist())
print("removed ids:", pa.table(diff.rows_removed()).column("id").to_pylist())
```

```
{'columns_added': 1, 'columns_removed': 0, 'columns_type_changed': 1, 'rows_added': 1, 'rows_removed': 1, 'rows_changed': 1, 'duplicate_keys': 0, 'null_keys': 0}
added ids: [4]
removed ids: [1]
```

A key appearing more than once on either side is reported in `duplicate_keys` (with `left_count` and `right_count`) and excluded from the added/removed/changed sets; a null key matches its counterpart and is counted in `null_keys`. Rows are compared by the non-key columns present on *both* sides, with onix's value semantics (integers and integral floats fold together, `1.00` equals `1.0000`, a timestamp compares by its instant across units, dictionary-encoded values equal their plain form, and null equals null); a nested non-key column is out of scope and is skipped rather than compared. The exact value-comparison rules are documented on the hashing functions in [`crates/onix-arrow/src/row_diff.rs`](crates/onix-arrow/src/row_diff.rs).

Type comparison uses the full logical Arrow type (timestamp unit and timezone, decimal precision and scale, and so on), but physical encodings that carry the same logical type compare equal — a dictionary-encoded string equals a plain string, polars' `Utf8View` equals pyarrow's `Utf8`, the list variants normalize together, and a map compares equal however a library spells it — so the same table read through pyarrow, polars, or DuckDB reports no spurious type changes. The full normalization rules are documented on `normalized_type` (and `map_entries`) in [`crates/onix-arrow/src/schema.rs`](crates/onix-arrow/src/schema.rs); nullability is ignored but reported in each record. Column names must be unique on each side; a repeated name raises `ValueError`. `diff.schema_arrow` is the same result as an Arrow table: it implements `__arrow_c_stream__`, so `polars.DataFrame(diff.schema_arrow)` and `pandas` consume it directly, and `diff.schema_arrow.to_pyarrow()` returns a `pyarrow.Table`.

`pyarrow` is optional: install it with `pip install deepdiff-rs[arrow]`. It is needed only for `to_pyarrow()` and for passing pyarrow objects in — importing `deepdiff_rs` and diffing polars or DuckDB tables need it not at all. Passing an object that implements neither Arrow protocol raises `TypeError`; calling `to_pyarrow()` without pyarrow installed raises `ImportError` naming the extra.

## Known limitations

- Only the core diff is implemented: `exclude_paths`, `significant_digits`, custom operators, `verbose_level != 2`, and delta/patch are not (yet) supported.
- Supported value types are `None`, `bool`, `int`, `float`, `str`, `dict` (with `str` keys), `list`, `tuple`, `set`, `frozenset`, `datetime.datetime`, and `datetime.date`; a `set`/`frozenset` member may be any of these except a `list`, `dict` or `set`, matching Python's own hashability rule, transitively through whatever the member nests. `int`s must fit in `i64`/`u64`, `float`s must be finite, and anything else — `time`, `timedelta`, a non-`str` dict key, a custom object, an arbitrary-precision `int`, or a non-finite `float` — raises `TypeError`/`ValueError` naming the exact path it was found at. The **Datetimes** and **Sets** bullets below cover the deliberate divergences for those two types. See [`crates/onix-py/src/convert.rs`](crates/onix-py/src/convert.rs) and [`tests/golden/README.md`](tests/golden/README.md).
- A subclass of a supported type (a `tuple`, `set` or `frozenset` subclass including `namedtuple`, a `datetime`/`date` subclass such as pandas' `Timestamp`) raises `TypeError` rather than being diffed as its base type, because DeepDiff reports each value's own type name. A `type_changes` entry's `old_type`/`new_type` are type *names* in `to_dict()`, where DeepDiff returns the type objects. Both are described in [`tests/golden/README.md`](tests/golden/README.md).
- **Datetimes** compare by instant, with a naive value read as UTC, matching DeepDiff. A changed pair is reported normalized to UTC (`to_json()` renders `...+00:00`, `to_dict()` returns UTC-aware `datetime`s); everywhere else a datetime keeps its raw value. Three deliberate departures: `to_json()` renders a `date` as `YYYY-MM-DD` where DeepDiff's own `to_json()` raises `TypeError` (a documented superset); a `zoneinfo`/`pytz` tzinfo comes back from `to_dict()` as a fixed-offset `datetime.timezone` carrying the offset it was in force at, not the original zone object; and a set holding both a naive and an aware value at one instant reports both as members, where DeepDiff's own digest cache can report only one (see [`crates/onix-py/src/convert.rs`](crates/onix-py/src/convert.rs)). Comparing two datetimes whose UTC form would leave year 1..=9999 raises `ValueError` naming the path, where DeepDiff raises `OverflowError`; under `ignore_order` DeepDiff's hasher normalizes every datetime and so raises for such a value even when it is only added, removed, or shuffled, where onix hashes by instant and reports it normally (see [`tests/golden/README.md`](tests/golden/README.md)). `truncate_datetime`, `time` and `timedelta` are not supported. The normalized-versus-raw split is documented in [`tests/golden/README.md`](tests/golden/README.md).
- **Sets** are diffed deterministically, where DeepDiff's own answers depend on the order the running process happens to iterate a set in (hash order, and `PYTHONHASHSEED`-dependent for `str` members) or on how its digest cache/computation handles a tuple, frozenset, or calendar member independently of Python's own `==`. Each consequence — entry order, which member of an equality class is reported, set-versus-sequence coercion, and a tuple/frozenset member's own (positional, not order-/repetition-insensitive) matching rule — is shown with both tools' output in [`tests/golden/README.md`](tests/golden/README.md)'s "Set iteration order" section. A report holding a `frozenset` value also serializes to JSON here, where DeepDiff's own `to_json()` raises `TypeError` — a superset, not a difference in the findings.
- A `str` containing a lone (unpaired) surrogate code point (e.g. `'\udc80'`, legal in Python but not encodable as UTF-8) raises `ValueError` naming the exact path on either side, before the two values are ever compared — including a pair DeepDiff would call equal and report as no change, since DeepDiff's scalar equality is plain Python `==` and never hits the encoding problem; DeepDiff does report a plain change for a *differing* pair, and crashes with an unhandled `UnicodeEncodeError` if such a string is ever hashed (a `set`/`frozenset` member). See [`tests/golden/README.md`](tests/golden/README.md)'s "Known DeepDiff quirks" section.
- A `str` inside a `tuple` or `frozenset` set item is rendered with Python's `repr()`, which escapes every non-printable character; onix escapes those below `U+0100` (the complete set in that range) and passes higher non-printable code points through literally, since escaping them would mean carrying a Unicode category table. Exact for all of ASCII and all printable text. See [`crates/onix-core/src/path.rs`](crates/onix-core/src/path.rs).
- Adversarially deep input raises `MaxDepthError` instead of crashing: the default `max_depth` is 512 and the hard ceiling is `MAX_DEPTH_CEILING` (20,000). See [`crates/onix-py/src/guard.rs`](crates/onix-py/src/guard.rs).
- `ignore_order` pairing is `O(N^2)` in unpaired elements per side and carries a polynomial cost in both time and memory with input depth; it has no `max_passes`/`max_diffs` cutoff, so bound the size and depth of untrusted input yourself. See [`crates/onix-core/src/ignore_order/mod.rs`](crates/onix-core/src/ignore_order/mod.rs).
- A `values_changed` between two multi-line strings runs a `difflib`-style `O(N*M)` line diff on the default path with no opt-out, worst when changes are spread evenly through the text (about 35 s for a heavily edited 1 MB string and growing quadratically, so a few megabytes is minutes), so bound the size of untrusted strings yourself. See [`crates/onix-core/src/unified_diff.rs`](crates/onix-core/src/unified_diff.rs).
- Ordered sequences (`list` or `tuple`) of scalars (null, bool, number, string, datetime, date) run a `difflib`-style `O(N*M)` matcher on the default path with its popular-element (autojunk) purge disabled for `DeepDiff` parity, worst for sequences of a few repeated values with dense edits (about 620 s for two 8,000-element lists of two repeated values with every other element changed, growing faster than quadratically), so bound the size of untrusted sequences yourself. See [`crates/onix-core/src/lcs.rs`](crates/onix-core/src/lcs.rs).
- In `diff_tables`, a column name containing an embedded NUL byte (`\0`) arrives truncated at the NUL through the Arrow C Data Interface, and the report shows the truncated name; such names are rare in practice.
- In `diff_tables`, a list of structs named exactly `key`/`value` with a nullable key and a real map are not distinguished, so a migration between the two is not reported as a type change (polars exports both as the same Arrow type); see `map_entries` in [`crates/onix-arrow/src/schema.rs`](crates/onix-arrow/src/schema.rs).
- In `diff_tables`, DuckDB labels a `TIMESTAMP WITH TIME ZONE` column with the connection's *session* time zone when it exports to Arrow (a UTC session as `Timestamp(µs, "UTC")`, an `America/New_York` session as `Timestamp(µs, "America/New_York")`), so on a non-UTC machine such a column can be reported as a type change against a UTC column from another library. Run `SET TimeZone='UTC'` on the DuckDB connection first for a deterministic, machine-independent result.
- `diff_tables` refuses a column whose Arrow type is nested deeper than `MAX_NESTING_DEPTH` (128) with a `MaxDepthError`, because comparing arbitrarily deep nesting would overflow the native stack; 128 is far beyond any real schema. Importing a schema nested many thousands of levels deep is also slow regardless, a cost of the Arrow C Data Interface itself.
- `diff_tables`'s row diff keeps a 32-byte hash per row per side and sorts them, so peak memory grows linearly with the row count (about 75 MB at 1M rows per side, about 660 MB at 10M) and time grows as `N·log N` in the total row count (about 2 s for a 10M-row pair). A second term is the duplicate-key report, which holds the actual key values of every *distinct duplicated* key, so a duplicate-heavy input costs an extra amount proportional to that count times the key width: an all-duplicate table of 200k rows per side (100k distinct duplicated keys) peaks at about 37 MB with 16-byte keys and about 1.05 GB with 1 KB keys, and 1M rows per side (500k distinct) at about 165 MB with 16-byte keys. None of these has a built-in cap: bound the row count and, for duplicate-heavy data, the key width of untrusted input yourself. Figures are the peak resident set of `cargo run -p onix-arrow --release --example row_diff_rss` (see that example), single run, macOS on an Apple M-series laptop, 2026-09-05; the method matches the [Performance](#performance) table. See [`crates/onix-arrow/src/row_diff.rs`](crates/onix-arrow/src/row_diff.rs).
- `diff_tables` reads each input twice (once to hash every row, once to emit only the differing rows), so the Python bindings spool each input to an anonymous temporary file (created with `tempfile`: unlinked at once, mode 0600, no predictable name — nothing is left on disk even if the process is killed) under the system temp directory for the duration of the call. Both spools are resident at once, so peak temp-disk use is the decoded size of both inputs together (about 315 MB for the 1M-row fixture pair — 161 MB plus 154 MB of uncompressed Arrow IPC, measured 2026-09-05; on the order of 10 GB for the full 5 GB-per-side fixture pair, which on Linux may be a RAM-backed `tmpfs`); a spool write that runs out of space raises `ValueError` naming the temp directory and `TMPDIR`.
- A key column whose type differs across the two inputs (after onix's encoding normalization) raises `ValueError`: a primary key that changed type is not a keyed comparison, so `diff_tables` refuses it rather than coercing one side. Nested non-key columns (list, struct, map, union) are skipped by the row diff (out of scope), so two rows differing only in such a column are reported unchanged; a nested *key* column raises `ValueError`. Which scalar types are hashed versus refused is documented on `hash_cell` in [`crates/onix-arrow/src/row_diff.rs`](crates/onix-arrow/src/row_diff.rs).
- Output is byte-identical to DeepDiff except for the cases listed above and two path-rendering quirks; [`tests/golden/README.md`](tests/golden/README.md) enumerates every accepted exception, including integers past `2^53` (the limit of exact `f64` representation) inside ordered scalar lists and `ignore_order` pairing among naive datetimes, which DeepDiff ranks using the *process's local timezone* while onix reads a naive value as UTC everywhere.

## Performance

Two committed, regenerable reports back the numbers below; every figure here is copied verbatim from them.

The Python bindings against real `deepdiff` on **live Python objects**, the number a real caller pays (source: [`crates/onix-py/benchmarks/bench_bindings.py`](crates/onix-py/benchmarks/bench_bindings.py), macOS 26.5.1, Apple M5 Max, median of 11 isolated subprocess runs per side, run on 2026-09-04):

| Shape | deepdiff | deepdiff_rs | Speedup |
| --- | --- | --- | --- |
| `ignore_order`, 10k shuffled ints, ~5% mutated (live objects) | 3111.56ms | 71.68ms | **43.41x** |
| &nbsp;&nbsp;peak RSS | 228.5 MB | 93.2 MB | **2.45x** |
| &nbsp;&nbsp;CPU seconds | 3.110 s | 0.072 s | **43.42x** |
| Heterogeneous API-payload records, n=20,000 (live objects) | 3439.96ms | 153.83ms | **22.36x** |
| &nbsp;&nbsp;peak RSS | 118.1 MB | 147.8 MB | **0.80x** |
| &nbsp;&nbsp;CPU seconds | 3.439 s | 0.154 s | **22.36x** |
| Typed records (datetime/tuple/set fields), n=10,000 (live objects) | 795.17ms | 48.48ms | **16.40x** |
| &nbsp;&nbsp;peak RSS | 60.2 MB | 62.2 MB | **0.97x** |
| &nbsp;&nbsp;CPU seconds | 0.795 s | 0.048 s | **16.40x** |
| Same typed-records shape, `ignore_order` (live objects) | 60506.65ms | 775.38ms | **78.03x** |
| &nbsp;&nbsp;peak RSS | 110.3 MB | 121.6 MB | **0.91x** |
| &nbsp;&nbsp;CPU seconds | 60.471 s | 0.774 s | **78.10x** |
| Same `ignore_order` shape, via `diff_json` (JSON-string path) | 3116.22ms | 73.85ms | **42.20x** |
| &nbsp;&nbsp;peak RSS | 228.8 MB | 93.8 MB | **2.44x** |
| &nbsp;&nbsp;CPU seconds | 3.114 s | 0.074 s | **42.20x** |
| Same API-payload shape, via `diff_json` (JSON-string path) | 4559.90ms | 87.02ms | **52.40x** |
| &nbsp;&nbsp;peak RSS | 139.5 MB | 140.9 MB | **0.99x** |
| &nbsp;&nbsp;CPU seconds | 4.558 s | 0.087 s | **52.58x** |
| Same API-payload shape, both tools reading two JSON files from disk | 4555.37ms | 85.62ms | **53.20x** |
| &nbsp;&nbsp;peak RSS | 139.5 MB | 141.0 MB | **0.99x** |
| &nbsp;&nbsp;CPU seconds | 4.553 s | 0.086 s | **53.21x** |

The engine's own diff-only time and peak resident memory against pinned `deepdiff` 9.1.0 (source: [`perf/RESULTS.md`](perf/RESULTS.md), same machine, median over tier-appropriate runs, diff time excluding process startup and JSON parsing on both sides):

| Fixture | onix diff-only (median, min-max) | deepdiff diff-only (median, min-max) | Speedup | onix peak RSS | deepdiff peak RSS | Memory ratio | ≥5x threshold |
|---|---|---|---|---|---|---|---|
| `flat_dict_10k` | 3.154 ms (3.058 ms-3.220 ms) | 141.155 ms (140.606 ms-142.781 ms) | 44.75x | 5.78 MB | 39.29 MB | 6.79x | ✅ |
| `flat_dict_100k` | 38.440 ms (38.000 ms-38.806 ms) | 1.594 s (1.581 s-1.602 s) | 41.47x | 40.57 MB | 110.82 MB | 2.73x | ✅ |
| `flat_dict_1m` | 460.082 ms (454.820 ms-465.133 ms) | 17.061 s (16.926 s-17.162 s) | 37.08x | 478.15 MB | 753.65 MB | 1.58x | ✅ |
| `flat_list_100k` | 82.907 ms (81.131 ms-84.654 ms) | 4.751 s (4.715 s-4.820 s) | 57.31x | 38.17 MB | 154.95 MB | 4.06x | ✅ |
| `nested_uniform_d6_b10` | 207.242 ms (205.249 ms-217.027 ms) | 71.458 s (70.959 s-71.599 s) | 344.80x | 227.41 MB | 868.32 MB | 3.82x | ✅ |
| `api_payloads` | 162.489 ms (159.446 ms-178.736 ms) | 93.764 s (93.687 s-94.474 s) | 577.05x | 270.09 MB | 609.93 MB | 2.26x | ✅ |
| `deep_narrow_d120` | 0.029 ms (0.028 ms-0.030 ms) | 123.650 ms (123.230 ms-125.018 ms) | 4245.55x | 2.15 MB | 41.27 MB | 19.23x | ✅ |
| `startup_trivial` | 0.001 ms (0.001 ms-0.001 ms) | 0.175 ms (0.169 ms-0.183 ms) | 147.75x | 2.15 MB | 32.67 MB | 15.22x | ✅ |
| `ignore_order_10k` | 73.475 ms (71.672 ms-73.940 ms) | 12.976 s (12.900 s-13.005 s) | 176.60x | 60.11 MB | 345.19 MB | 5.74x | ✅ |
| `identical_1m` | 9.254 ms (6.726 ms-9.875 ms) | 15.790 s (15.660 s-15.989 s) | 1706.35x | 315.41 MB | 503.19 MB | 1.60x | ✅ |

Both reports carry their full methodology, fairness rules, and the reproduce command. `perf/RESULTS.md` is an upper bound (JSON parsed straight into the engine, no Python-object conversion); the bindings table is the product-surface number. Regenerate them with `perf/run_bench.sh` and `crates/onix-py/benchmarks/bench_bindings.py` (see [CONTRIBUTING.md](CONTRIBUTING.md)).

## Reference

**Python API.** The public surface is `DeepDiff`, `diff_json`, `diff_tables` (returning a `TableDiff`), `MaxDepthError`, and `MAX_DEPTH_CEILING`.

- `DeepDiff(t1, t2, ignore_order=False, max_depth=None)`: diffs two live Python objects of supported value types — `None`, `bool`, `int`, `float`, `str`, `dict` (with `str` keys), `list`, `tuple`, `set`, `frozenset`, `datetime.datetime`, and `datetime.date` (see [Known limitations](#known-limitations) for the exact restrictions and exclusions); `.to_json()` returns the DeepDiff-compatible JSON string, `.to_dict()` the same report as a dict — with Python types preserved, so a value the diff found in a `tuple`, `set` or `frozenset` comes back as one and a `datetime`/`date` comes back as a real `datetime`/`date` — and the instance is falsy when there is no difference. The `set_item_added`/`set_item_removed` categories are lists of path strings, each ending in the item itself (`root['a'][2]`, `root['x']`, `root[(1, 2)]`).
- `diff_json(a, b, ignore_order=False, max_depth=None) -> str`: diffs two JSON strings entirely in Rust and returns the report as a JSON string.
- `MaxDepthError` (a `ValueError` subclass) is raised when input exceeds `max_depth`; `MAX_DEPTH_CEILING` (20,000) is the hard upper bound on `max_depth`.
- `diff_tables(left, right, key=[...]) -> TableDiff`: diffs two Arrow tables (see [Diffing tables](#diffing-tables)). `TableDiff.schema` is the list of changed columns, `.schema_arrow` the same as an Arrow table, `.summary()` the schema and row change counts, `.to_json()` the schema diff as JSON; `.rows_added()`, `.rows_removed()`, and `.duplicate_keys()` return Arrow tables, and `.cells_changed()` raises `NotImplementedError` until a later version.

**CLI.** `onix diff <a.json> <b.json> [--max-depth N] [--ignore-order] [--timing]` reads both files as JSON and prints a compact, single-line DeepDiff-compatible report to stdout (`{}` when there is no difference).

- `--max-depth N` overrides the recursion-depth bound (default: the `ONIX_MAX_DEPTH` environment variable if set, else 512).
- `--ignore-order` compares every list by hash-based matching instead of by position, mirroring `DeepDiff(..., ignore_order=True)`.
- `--timing` prints one line of JSON (`{"parse_ns": N, "diff_ns": N}`) to stderr.

Exit codes:

| Code | Meaning |
| --- | --- |
| `0` | Diff computed successfully (whether or not the report is empty; differences are carried in the stdout JSON, not the exit code). |
| `1` | Usage error (missing/unknown subcommand, wrong argument count, unknown flag, non-numeric `--max-depth`); details and a usage line go to stderr. |
| `2` | I/O error (e.g. a missing input file) or a JSON-parse error on either input. |
| `3` | `max_depth` exceeded; the path that tripped the bound goes to stderr. |

## Layout

```
crates/onix-core   # the diff engine (library, no I/O)
crates/onix-cli    # the `onix` binary (thin CLI over the core)
crates/onix-arrow  # Arrow table diffing (schema diff and keyed row diff)
crates/onix-py     # PyO3 bindings, published as `deepdiff-rs`
scripts/           # gen_goldens.py: regenerates tests/golden/ from real DeepDiff
tests/golden       # DeepDiff-generated expected outputs (the compatibility corpus)
perf/              # cross-language benchmark harness and RESULTS.md
```

## Contributing

Issues and pull requests are welcome. Open an issue to report a bug, a DeepDiff divergence (include both inputs and the report each engine produces), or a question. Building from source, the quality gates, the golden corpus, benchmarking, mutation testing, and publishing are all in [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT: see [LICENSE](LICENSE).

onix reimplements algorithms from CPython's `difflib` (PSF License) and
reproduces the behavior of DeepDiff (MIT); their notices and license texts are
in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
