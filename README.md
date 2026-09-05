# deepdiff-rs

[![CI](https://img.shields.io/github/actions/workflow/status/ksco92/onix/check.yml?branch=main&label=CI)](https://github.com/ksco92/onix/actions/workflows/check.yml)
[![coverage](https://codecov.io/gh/ksco92/onix/branch/main/graph/badge.svg)](https://codecov.io/gh/ksco92/onix)
[![PyPI](https://img.shields.io/pypi/v/deepdiff-rs.svg)](https://pypi.org/project/deepdiff-rs/)
[![downloads](https://img.shields.io/pypi/dm/deepdiff-rs.svg)](https://pypi.org/project/deepdiff-rs/)
[![license](https://img.shields.io/github/license/ksco92/onix.svg)](LICENSE)
[![last commit](https://img.shields.io/github/last-commit/ksco92/onix.svg)](https://github.com/ksco92/onix/commits/main)

**onix is a Rust rewrite of Python DeepDiff's core: byte-compatible output, 37-4588x faster, with `ignore_order` support included.** Install it as `deepdiff-rs`, a drop-in `DeepDiff` class for Python, or run the diff engine as the `onix` command-line tool.

`deepdiff-rs` reads live Python objects (or JSON) and produces the exact same report [DeepDiff](https://github.com/seperman/deepdiff) does at `verbose_level=2`, so it slots into code that already parses DeepDiff output while running dramatically faster on large or deeply nested inputs.

Status (September 2026): `deepdiff-rs` 0.x is live on PyPI (Python 3.9+, wheels for Linux x86_64/aarch64, macOS arm64/x86_64, and Windows x64, plus an sdist); the `onix` CLI builds from source, and nothing is on crates.io yet. Ordered and `ignore_order` diffing are complete, differentially tested against real DeepDiff 9.1.0, and [benchmarked](perf/RESULTS.md). It is 0.x, not stable or 1.0: the API may still change before 1.0.

## Table of contents

- [Install](#install)
- [Quickstart](#quickstart)
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
uv sync --group test                 # creates .venv, installs pytest + pinned deepdiff
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

## Known limitations

- Only the core diff is implemented: `exclude_paths`, `significant_digits`, custom operators, `verbose_level != 2`, and delta/patch are not (yet) supported.
- Supported value types are `None`, `bool`, `int`, `float`, `str`, `dict` (with `str` keys), `list`, `tuple`, `set`, `frozenset`, `datetime.datetime`, and `datetime.date`; a `set`/`frozenset` member may be any of these except a `list`, `dict` or `set`, matching Python's own hashability rule, transitively through whatever the member nests. `int`s must fit in `i64`/`u64`, `float`s must be finite, and anything else — `time`, `timedelta`, a non-`str` dict key, a custom object, an arbitrary-precision `int`, or a non-finite `float` — raises `TypeError`/`ValueError` naming the exact path it was found at. The **Datetimes** and **Sets** bullets below cover the deliberate divergences for those two types. See [`crates/onix-py/src/convert.rs`](crates/onix-py/src/convert.rs) and [`tests/golden/README.md`](tests/golden/README.md).
- A subclass of a supported type (a `tuple`, `set` or `frozenset` subclass including `namedtuple`, a `datetime`/`date` subclass such as pandas' `Timestamp`) raises `TypeError` rather than being diffed as its base type, because DeepDiff reports each value's own type name. A `type_changes` entry's `old_type`/`new_type` are type *names* in `to_dict()`, where DeepDiff returns the type objects. Both are described in [`tests/golden/README.md`](tests/golden/README.md).
- **Datetimes** compare by instant, with a naive value read as UTC, matching DeepDiff. A changed pair is reported normalized to UTC (`to_json()` renders `...+00:00`, `to_dict()` returns UTC-aware `datetime`s); everywhere else a datetime keeps its raw value. Three deliberate departures: `to_json()` renders a `date` as `YYYY-MM-DD` where DeepDiff's own `to_json()` raises `TypeError` (a documented superset); a `zoneinfo`/`pytz` tzinfo comes back from `to_dict()` as a fixed-offset `datetime.timezone` carrying the offset it was in force at, not the original zone object; and a set holding both a naive and an aware value at one instant reports both as members, where DeepDiff's own digest cache can report only one (see [`crates/onix-py/src/convert.rs`](crates/onix-py/src/convert.rs)). Comparing two datetimes whose UTC form would leave year 1..=9999 raises `ValueError` naming the path, where DeepDiff raises `OverflowError`; under `ignore_order` DeepDiff's hasher normalizes every datetime and so raises for such a value even when it is only added, removed, or shuffled, where onix hashes by instant and reports it normally (see [`tests/golden/README.md`](tests/golden/README.md)). `truncate_datetime`, `time` and `timedelta` are not supported. The normalized-versus-raw split is documented in [`tests/golden/README.md`](tests/golden/README.md).
- **Sets** are diffed deterministically, where DeepDiff's own answers depend on the order the running process happens to iterate a set in (hash order, and `PYTHONHASHSEED`-dependent for `str` members) or on how its digest cache/computation handles a tuple, frozenset, or calendar member independently of Python's own `==`. Each consequence — entry order, which member of an equality class is reported, set-versus-sequence coercion, and a tuple/frozenset member's own (positional, not order-/repetition-insensitive) matching rule — is shown with both tools' output in [`tests/golden/README.md`](tests/golden/README.md)'s "Set iteration order" section. A report holding a `frozenset` value also serializes to JSON here, where DeepDiff's own `to_json()` raises `TypeError` — a superset, not a difference in the findings.
- A `str` containing a lone (unpaired) surrogate code point (e.g. `'\udc80'`, legal in Python but not encodable as UTF-8) raises `ValueError` naming the exact path instead of converting; DeepDiff compares such a string by Python `==`/hash and reports a plain change, or crashes with an unhandled `UnicodeEncodeError` if it is ever hashed (a `set`/`frozenset` member). See [`tests/golden/README.md`](tests/golden/README.md)'s "Known DeepDiff quirks" section.
- A `str` inside a `tuple` or `frozenset` set item is rendered with Python's `repr()`, which escapes every non-printable character; onix escapes those below `U+0100` (the complete set in that range) and passes higher non-printable code points through literally, since escaping them would mean carrying a Unicode category table. Exact for all of ASCII and all printable text. See [`crates/onix-core/src/path.rs`](crates/onix-core/src/path.rs).
- Adversarially deep input raises `MaxDepthError` instead of crashing: the default `max_depth` is 512 and the hard ceiling is `MAX_DEPTH_CEILING` (20,000). See [`crates/onix-py/src/guard.rs`](crates/onix-py/src/guard.rs).
- `ignore_order` pairing is `O(N^2)` in unpaired elements per side and carries a polynomial cost in both time and memory with input depth; it has no `max_passes`/`max_diffs` cutoff, so bound the size and depth of untrusted input yourself. See [`crates/onix-core/src/ignore_order/mod.rs`](crates/onix-core/src/ignore_order/mod.rs).
- Output is byte-identical to DeepDiff except for the cases listed above and two path-rendering quirks; [`tests/golden/README.md`](tests/golden/README.md) enumerates every accepted exception, including integers past `2^53` (the limit of exact `f64` representation) inside ordered scalar lists and `ignore_order` pairing among naive datetimes, which DeepDiff ranks using the *process's local timezone* while onix reads a naive value as UTC everywhere.

## Performance

Two committed, regenerable reports back the numbers below; every figure here is copied verbatim from them.

The Python bindings against real `deepdiff` on **live Python objects**, the number a real caller pays (source: [`crates/onix-py/benchmarks/bench_bindings.py`](crates/onix-py/benchmarks/bench_bindings.py), macOS 26.5.1, Apple M5 Max, median of 11 isolated subprocess runs per side, run on 2026-09-04):

| Shape | deepdiff | deepdiff_rs | Speedup |
| --- | --- | --- | --- |
| `ignore_order`, 10k shuffled ints, ~5% mutated (live objects) | 3164.29ms | 89.08ms | **35.52x** |
| &nbsp;&nbsp;peak RSS | 228.4 MB | 141.5 MB | **1.61x** |
| &nbsp;&nbsp;CPU seconds | 3.162 s | 0.089 s | **35.50x** |
| Heterogeneous API-payload records, n=20,000 (live objects) | 3451.01ms | 151.84ms | **22.73x** |
| &nbsp;&nbsp;peak RSS | 118.1 MB | 147.7 MB | **0.80x** |
| &nbsp;&nbsp;CPU seconds | 3.449 s | 0.152 s | **22.73x** |
| Typed records (datetime/tuple/set fields), n=10,000 (live objects) | 792.96ms | 47.87ms | **16.57x** |
| &nbsp;&nbsp;peak RSS | 60.2 MB | 62.1 MB | **0.97x** |
| &nbsp;&nbsp;CPU seconds | 0.792 s | 0.048 s | **16.56x** |
| Same typed-records shape, `ignore_order` (live objects) | 60483.86ms | 1272.04ms | **47.55x** |
| &nbsp;&nbsp;peak RSS | 112.1 MB | 1005.6 MB | **0.11x** |
| &nbsp;&nbsp;CPU seconds | 60.452 s | 1.271 s | **47.55x** |
| Same `ignore_order` shape, via `diff_json` (JSON-string path) | 3168.05ms | 86.95ms | **36.44x** |
| &nbsp;&nbsp;peak RSS | 228.7 MB | 142.3 MB | **1.61x** |
| &nbsp;&nbsp;CPU seconds | 3.166 s | 0.087 s | **36.46x** |
| Same API-payload shape, via `diff_json` (JSON-string path) | 4568.00ms | 85.37ms | **53.51x** |
| &nbsp;&nbsp;peak RSS | 139.5 MB | 140.9 MB | **0.99x** |
| &nbsp;&nbsp;CPU seconds | 4.566 s | 0.085 s | **53.50x** |
| Same API-payload shape, both tools reading two JSON files from disk | 4565.46ms | 89.43ms | **51.05x** |
| &nbsp;&nbsp;peak RSS | 139.5 MB | 141.0 MB | **0.99x** |
| &nbsp;&nbsp;CPU seconds | 4.564 s | 0.089 s | **51.03x** |

The engine's own diff-only time and peak resident memory against pinned `deepdiff` 9.1.0 (source: [`perf/RESULTS.md`](perf/RESULTS.md), same machine, median over tier-appropriate runs, diff time excluding process startup and JSON parsing on both sides):

| Fixture | onix diff-only (median, min-max) | deepdiff diff-only (median, min-max) | Speedup | onix peak RSS | deepdiff peak RSS | Memory ratio | ≥5x threshold |
|---|---|---|---|---|---|---|---|
| `flat_dict_10k` | 3.093 ms (3.020 ms-3.153 ms) | 143.691 ms (140.322 ms-145.891 ms) | 46.46x | 5.72 MB | 39.46 MB | 6.90x | ✅ |
| `flat_dict_100k` | 38.354 ms (37.963 ms-38.916 ms) | 1.596 s (1.584 s-1.686 s) | 41.60x | 40.53 MB | 110.95 MB | 2.74x | ✅ |
| `flat_dict_1m` | 450.356 ms (449.689 ms-467.911 ms) | 16.936 s (16.889 s-17.154 s) | 37.60x | 478.10 MB | 753.75 MB | 1.58x | ✅ |
| `flat_list_100k` | 81.787 ms (78.266 ms-89.593 ms) | 4.798 s (4.746 s-4.828 s) | 58.67x | 38.16 MB | 155.11 MB | 4.06x | ✅ |
| `nested_uniform_d6_b10` | 208.770 ms (208.269 ms-209.842 ms) | 71.500 s (71.498 s-72.875 s) | 342.48x | 224.58 MB | 908.33 MB | 4.04x | ✅ |
| `api_payloads` | 160.667 ms (153.309 ms-171.032 ms) | 94.138 s (93.625 s-94.669 s) | 585.92x | 269.39 MB | 544.98 MB | 2.02x | ✅ |
| `deep_narrow_d120` | 0.027 ms (0.026 ms-0.028 ms) | 123.792 ms (123.234 ms-124.809 ms) | 4588.45x | 2.18 MB | 41.08 MB | 18.85x | ✅ |
| `startup_trivial` | 0.001 ms (0.001 ms-0.001 ms) | 0.181 ms (0.174 ms-0.207 ms) | 177.24x | 2.18 MB | 32.69 MB | 15.00x | ✅ |
| `ignore_order_10k` | 80.965 ms (80.117 ms-83.510 ms) | 13.010 s (12.900 s-13.023 s) | 160.68x | 108.82 MB | 345.41 MB | 3.17x | ✅ |
| `identical_1m` | 6.996 ms (6.114 ms-7.731 ms) | 15.853 s (15.773 s-16.021 s) | 2266.18x | 315.39 MB | 503.28 MB | 1.60x | ✅ |

Both reports carry their full methodology, fairness rules, and the reproduce command. `perf/RESULTS.md` is an upper bound (JSON parsed straight into the engine, no Python-object conversion); the bindings table is the product-surface number. Regenerate them with `perf/run_bench.sh` and `crates/onix-py/benchmarks/bench_bindings.py` (see [CONTRIBUTING.md](CONTRIBUTING.md)).

## Reference

**Python API.** The public surface is `DeepDiff`, `diff_json`, `MaxDepthError`, and `MAX_DEPTH_CEILING`.

- `DeepDiff(t1, t2, ignore_order=False, max_depth=None)`: diffs two live Python objects of supported value types — `None`, `bool`, `int`, `float`, `str`, `dict` (with `str` keys), `list`, `tuple`, `set`, `frozenset`, `datetime.datetime`, and `datetime.date` (see [Known limitations](#known-limitations) for the exact restrictions and exclusions); `.to_json()` returns the DeepDiff-compatible JSON string, `.to_dict()` the same report as a dict — with Python types preserved, so a value the diff found in a `tuple`, `set` or `frozenset` comes back as one and a `datetime`/`date` comes back as a real `datetime`/`date` — and the instance is falsy when there is no difference. The `set_item_added`/`set_item_removed` categories are lists of path strings, each ending in the item itself (`root['a'][2]`, `root['x']`, `root[(1, 2)]`).
- `diff_json(a, b, ignore_order=False, max_depth=None) -> str`: diffs two JSON strings entirely in Rust and returns the report as a JSON string.
- `MaxDepthError` (a `ValueError` subclass) is raised when input exceeds `max_depth`; `MAX_DEPTH_CEILING` (20,000) is the hard upper bound on `max_depth`.

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
crates/onix-py     # PyO3 bindings, published as `deepdiff-rs`
scripts/           # gen_goldens.py: regenerates tests/golden/ from real DeepDiff
tests/golden       # DeepDiff-generated expected outputs (the compatibility corpus)
perf/              # cross-language benchmark harness and RESULTS.md
```

## Contributing

Issues and pull requests are welcome. Open an issue to report a bug, a DeepDiff divergence (include both inputs and the report each engine produces), or a question. Building from source, the quality gates, the golden corpus, benchmarking, mutation testing, and publishing are all in [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT: see [LICENSE](LICENSE).
