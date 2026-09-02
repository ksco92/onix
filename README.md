# onix

**onix is a Rust rewrite of Python DeepDiff's core: byte-compatible output, 64-5757x faster, with `ignore_order` support included.**

See [DeepDiff](https://github.com/seperman/deepdiff) for the Python original
that onix matches report-for-report. Output is byte-identical apart from one
documented boundary case: integers past `2^53`, the limit of exact `f64`
representation, inside ordered scalar lists (detailed in
[`tests/golden/README.md`](https://github.com/ksco92/onix/blob/main/tests/golden/README.md)).

**Status (September 2026):** pre-alpha proof of concept, unpublished; ordered
+ `ignore_order` diffing complete and benchmarked (see
[perf/RESULTS.md](https://github.com/ksco92/onix/blob/main/perf/RESULTS.md)); Python bindings (PyO3, `deepdiff-rs` on
PyPI once published: see "Python" below) built, not yet published.

## Table of contents

- [Reading path (first-time visitor)](https://github.com/ksco92/onix/blob/main/README.md#reading-path-first-time-visitor)
- [Setup from scratch](https://github.com/ksco92/onix/blob/main/README.md#setup-from-scratch)
- [Usage: `onix diff`](https://github.com/ksco92/onix/blob/main/README.md#usage-onix-diff)
- [Python](https://github.com/ksco92/onix/blob/main/README.md#python)
- [Quality gates](https://github.com/ksco92/onix/blob/main/README.md#quality-gates)
- [Golden corpus](https://github.com/ksco92/onix/blob/main/README.md#golden-corpus)
- [Benchmarks](https://github.com/ksco92/onix/blob/main/README.md#benchmarks)
- [Layout](https://github.com/ksco92/onix/blob/main/README.md#layout)
- [Contributing and support](https://github.com/ksco92/onix/blob/main/README.md#contributing-and-support)
- [License](https://github.com/ksco92/onix/blob/main/README.md#license)

## Reading path (first-time visitor)

In order, each building on the last:

1. **This README**: what onix is, how to build it, and where everything
   lives (see "Layout" below).
2. **`crates/onix-core/src/lib.rs`'s doc comment**: the crate's front
   door: an architecture map (parse → diff dispatch → ordered/`ignore_order`
   container comparison → `Report` → render) naming the actual module each
   step lives in, plus the design decisions behind it (why the engine
   operates on the compact `onix_core::Value` model, which the CLI and
   bindings build directly, why the engine is recursive with a depth guard,
   not iterative (yet), etc.).
   Follow it into `crates/onix-core/src/diff/mod.rs` and
   `crates/onix-core/src/ignore_order/mod.rs`; each is itself a module-doc
   front door to its own submodules, one seam per file (see each
   directory's `mod.rs` "Internal layout" section).
3. **`tests/golden/README.md`**: what the golden corpus pins down, and the
   one documented `DeepDiff` quirk it doesn't. The corpus is the empirical
   ground truth the code above matches. `crates/onix-core/src/ignore_order/mod.rs`'s own
   doc comment carries the full, source-cited `ignore_order=True` spec this
   crate implements.
4. **`crates/onix-py/src/lib.rs`'s doc comment** (see "Python" below): how
   the Python bindings sit on top of everything above: a one-time
   Python-object-to-`Value` conversion (`crates/onix-py/src/convert.rs`)
   feeding the exact same `onix_core::diff_with_options` this README's
   `onix diff` CLI calls.

## Setup from scratch

Prerequisites: a Unix-ish system with `curl`, `make`, and `git`. All commands
are run from the repository root.

1. **Install the Rust toolchain** (skip if `cargo -V` already works):

   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   . "$HOME/.cargo/env"
   ```

   The exact toolchain version is pinned in
   [`rust-toolchain.toml`](https://github.com/ksco92/onix/blob/main/rust-toolchain.toml); rustup installs it (plus the
   `clippy`, `rustfmt`, and `llvm-tools-preview` components) automatically the
   first time you run a `cargo` command in this repo.

2. **Install cargo-llvm-cov** (coverage tooling, needed by `make check`):

   ```sh
   brew install cargo-llvm-cov cargo-deny   # macOS
   cargo install cargo-machete --locked     # no Homebrew formula exists
   # or, on any platform:
   cargo install cargo-llvm-cov cargo-deny cargo-machete --locked
   ```

3. **Build and verify:**

   ```sh
   cargo build
   make check
   ```

Regenerating the golden corpus (see "Golden corpus" below) needs `uv`, which
installs its own pinned Python 3.13 and `deepdiff` on demand: no separate
setup required. Benchmarking (see "Benchmarks" below) additionally needs
`hyperfine` (`brew install hyperfine`, or any platform: `cargo install
hyperfine`).

## Usage: `onix diff`

```sh
onix diff <a.json> <b.json> [--max-depth N] [--ignore-order] [--timing]
```

Reads both files as JSON and prints a DeepDiff-compatible report to **stdout**
as a single line of compact JSON (an empty report prints `{}`); compact,
rather than pretty-printed, because this output is meant for machine
consumption (golden-file comparison, the benchmark harness), where a
single deterministic line is easiest to diff byte-for-byte.

- `--max-depth N` overrides the recursion-depth bound passed to
  `onix_core::diff_with_options`. Without the flag, the default comes
  from the `ONIX_MAX_DEPTH` environment variable if it's set to a parseable
  number, else `onix_core::DEFAULT_MAX_DEPTH` (512). This ambient-environment
  default is CLI-only; the `onix-core` library itself stays a pure function
  with no environment dependence.
- `--ignore-order` mirrors `DeepDiff(..., ignore_order=True)`: every
  list/tuple anywhere in the tree is compared by hash-based matching and
  nearest-neighbor pairing instead of index-aligned/LCS comparison (dicts
  are unaffected; they're always compared by key). See
  `crates/onix-core/src/ignore_order/mod.rs`'s module doc for the full,
  source-cited spec this implements. **Known divergences** (both `ignore_order`- and
  ordered-path-related) are tracked in
  [`tests/golden/README.md`](https://github.com/ksco92/onix/blob/main/tests/golden/README.md)'s "Known DeepDiff
  quirks" section: an intentionally-unchased path-rendering edge case and
  a narrow list-LCS numeric-precision limit.
- `--timing` prints exactly one line of JSON to **stderr**:
  `{"parse_ns": N, "diff_ns": N}` (measuring the parse of the two inputs
  straight into the engine's compact value model, and the diff call,
  respectively) — the same
  "diff-only self-instrumentation" the benchmark harness under `perf/`
  uses to isolate diff time from process startup and JSON parsing. Without
  `--timing`, stderr stays empty on a successful run.

Exit codes:

| Code | Meaning |
| --- | --- |
| `0` | The diff was computed successfully. This holds **whether or not** the report is empty: presence/absence of differences is carried in the stdout JSON, not the exit code. |
| `1` | Usage error: missing/unknown subcommand, wrong number of positional arguments, an unknown flag, or a non-numeric `--max-depth`. Details plus a usage line go to stderr. |
| `2` | I/O error (e.g. a missing input file) or a JSON-parse error on either input. |
| `3` | `onix_core::Error::MaxDepthExceeded`: the error's message (which path tripped the bound) goes to stderr. |

Example:

```sh
$ echo '{"a": 1}' > left.json
$ echo '{"a": 2}' > right.json
$ onix diff left.json right.json
{"values_changed":{"root['a']":{"new_value":2,"old_value":1}}}
$ echo $?
0
```

## Python

PyO3 bindings (`crates/onix-py`), published to PyPI as **`deepdiff-rs`**
(Python import name `deepdiff_rs`). See `crates/onix-py/src/lib.rs`'s doc
comment for the module map. Not yet published (see "Wheels" below); install
from source for now.

### Install

```sh
pip install deepdiff-rs   # once published, see "Wheels" below
```

From source (this repo):

```sh
cd crates/onix-py
uv tool install maturin      # the build tool this crate uses (skip if already installed)
uv sync --group test         # creates .venv, installs pytest + pinned deepdiff==9.1.0
uv run --group test maturin develop --release
```

### The drop-in class

```python
from deepdiff_rs import DeepDiff

diff = DeepDiff({"a": 1}, {"a": 2})
if diff:
    print(diff.to_json())   # byte-compatible with real DeepDiff(...).to_json() at verbose_level=2
    print(diff.to_dict())   # the parsed form, as a native Python dict
```

`DeepDiff(t1, t2, ignore_order=False, max_depth=None)` accepts live Python
objects (`None`/`bool`/`int`/`float`/`str`/`dict`/`list`, arbitrarily
nested), converts them to `onix_core`'s value model exactly once up front,
then diffs and renders natively. See `crates/onix-py/src/convert.rs`'s
module doc for the complete conversion table and every error case below.

### The fast path

If you already have (or are happy to produce) JSON text rather than live
Python objects, skip the Python-object conversion entirely:

```python
from deepdiff_rs import diff_json

diff_json('{"a": 1}', '{"a": 2}')
# '{"values_changed":{"root[\'a\']":{"new_value":2,"old_value":1}}}'
```

`diff_json(a, b, ignore_order=False, max_depth=None) -> str` parses both
JSON documents, diffs, and serializes the result back to a JSON string
entirely in Rust.

### MVP limitations (documented, not accidental)

- **Supported types:** `None`, `bool`, `int`, `float`, `str`, `dict` (`str`
  keys only), `list`. The rest of `deepdiff.DeepDiff`'s option surface
  (`exclude_paths`, `significant_digits`, custom operators, `verbose_level
  != 2`, delta/patch, …) is not yet implemented.
- **Integers** must fit in `i64` or `u64`; arbitrary-precision Python `int`s
  (beyond that range) raise `ValueError`; real `DeepDiff` supports them
  natively.
- **Floats** must be finite; `NaN`/`inf`/`-inf` raise `ValueError` (JSON has
  no representation for any of the three).
- **`dict` keys** must be `str`; a non-`str` key raises `TypeError` naming
  the key's type and the path to the dict containing it (e.g. `"dict keys
  must be str, got key of type int at root['a']"`).
- **Tuples, sets, frozensets, dates, and custom objects** are not
  representable in the JSON-shaped value model and raise `TypeError` naming
  the type and the exact path it was found at, no matter how deeply nested
  (e.g. `"unsupported type for diffing: tuple at root['a'][2]"`).
- **`MaxDepthError`** (a `ValueError` subclass) replaces a native crash on
  adversarially deep input, mirroring `onix_core::Error::MaxDepthExceeded`.
  Unlike `onix_core::diff_with_max_depth` (which never raises on inputs that
  are equal, however deep), the Python bindings raise even for equal inputs
  beyond `max_depth`, because conversion happens (and is depth-checked)
  before the equality check ever runs. Default `max_depth` is
  `onix_core::DEFAULT_MAX_DEPTH` (512).
- **`max_depth` has a hard ceiling**, exposed as `deepdiff_rs.MAX_DEPTH_CEILING`
  (currently 20,000). A `max_depth` above it is refused up front with a plain
  `ValueError` naming the ceiling (not a `MaxDepthError`); at or below it, the
  outcome is always a correct result or a catchable exception, never a process
  crash. The default (512) is unaffected. The rationale for the specific
  ceiling is on `MAX_DEPTH_CEILING` in `crates/onix-py/src/guard.rs`.
- **`ignore_order` pairing is `O(N²)`** in the number of unpaired elements
  per side: pairing the leftover elements of two lists compares every
  remaining candidate against every other, the same core cost real `DeepDiff`
  pays for this option. `DeepDiff`'s `max_passes`/`max_diffs` bounds on that
  work are not implemented here, so a caller feeding untrusted input should
  bound the input size itself rather than rely on an internal cutoff. The
  cost also grows steeply with the **nesting depth** of unpaired containers:
  ranking candidate pairs computes a recursive pairwise structural distance,
  so a caller should bound both the size *and* the nesting of untrusted
  `ignore_order` inputs (a distance-memoization optimization is tracked
  follow-up work, not in this release). Note
  too that, unlike upstream `DeepDiff` (which Python's randomized string
  hashing protects), onix's `ignore_order` hashes input-derived keys with a
  fixed-seed hash, so crafted inputs can additionally degrade hashing
  performance. See `crates/onix-core/src/ignore_order/fxhash.rs` for the
  accepted trade-off.

### Bindings benchmark

`crates/onix-py/benchmarks/bench_bindings.py` times real `DeepDiff` against
`deepdiff_rs` on **live Python objects**: unlike `perf/RESULTS.md`, which
diffs already-parsed JSON and is explicitly an upper bound, this number
includes the Python-object-to-`Value` conversion cost a real caller actually
pays. Each side is the median of 11 independent, isolated subprocess runs
(one diff per process, so peak memory can be attributed to a side; see the
script's docstring), measured on the same machine (macOS 26.5.1, Apple M5
Max) as `perf/RESULTS.md`:

| Shape | deepdiff | deepdiff_rs | Speedup |
| --- | --- | --- | --- |
| `ignore_order`, 10k shuffled ints, ~5% mutated (live objects) | 2870.45ms | 70.69ms | **40.61x** |
| &nbsp;&nbsp;peak RSS | 227.5 MB | 141.1 MB | **1.61x** |
| &nbsp;&nbsp;CPU seconds | 2.868 s | 0.071 s | **40.61x** |
| Heterogeneous API-payload records, n=20,000 (live objects) | 3234.68ms | 130.58ms | **24.77x** |
| &nbsp;&nbsp;peak RSS | 117.6 MB | 276.2 MB | **0.43x** |
| &nbsp;&nbsp;CPU seconds | 3.232 s | 0.130 s | **24.77x** |
| Same `ignore_order` shape, via `diff_json` (JSON-string path) | 2952.13ms | 70.91ms | **41.63x** |
| &nbsp;&nbsp;peak RSS | 227.8 MB | 141.2 MB | **1.61x** |
| &nbsp;&nbsp;CPU seconds | 2.950 s | 0.071 s | **41.63x** |
| Same API-payload shape, via `diff_json` (JSON-string path) | 4365.52ms | 80.22ms | **54.42x** |
| &nbsp;&nbsp;peak RSS | 138.8 MB | 270.8 MB | **0.51x** |
| &nbsp;&nbsp;CPU seconds | 4.362 s | 0.080 s | **54.42x** |

Wall time is the first row of each shape; peak RSS (resident memory) and CPU
seconds (user + system) are the sub-rows, each the median of the same 11
isolated subprocess runs per side (the script's docstring explains how the
subprocess isolation attributes memory to a side). The Speedup column carries
the deepdiff / deepdiff_rs ratio on every row, computed from full-precision
values before rounding for display. A value below 1x on a memory or CPU
row means `deepdiff_rs` used more than `deepdiff` there, and hand-dividing the
displayed 3-decimal figures can differ slightly from the bolded ratio. CPU
tracks wall time closely (single-threaded, CPU-bound work); peak RSS is
actually higher for `deepdiff_rs` on the heterogeneous records (there the
binding first converts the whole live object tree into its own value model)
and lower on the flat-integer shape.

**Fixture note:** the live-object API-payload row (**~25x**) builds `b` as a
`copy.deepcopy` of `a`, so the two inputs share no identity, matching how
independently deserialized API responses actually arrive in practice. The
full rationale for that choice is on `build_api_payloads_case` in
`crates/onix-py/benchmarks/bench_bindings.py`.

That multiple is well short of onix-core's own headline numbers because
converting 20,000 realistic nested Python records into `onix_core`'s value
model one object at a time is genuine, measurable overhead. A proxy that
isolates it, `DeepDiff(a, copy.deepcopy(a))` (which pays the full conversion
plus onix's cheap whole-tree equality check, but none of the per-node diff
bookkeeping the mutated case also pays), accounts for roughly **82%** of the
mutated case's own total time on this shape; `bench_bindings.py` computes and
prints that percentage as part of its output. The JSON-string-only
`diff_json` path skips the conversion entirely and recovers a much larger
multiple (**~54x**) on the same data. Use `diff_json` when you already have
(or can produce) JSON text; reach for the drop-in `DeepDiff` class when you
genuinely need to diff live Python objects, and accept the conversion cost
as part of that convenience. Reproduce with:

```sh
cd crates/onix-py
uv sync --group test
uv run --group test maturin develop --release   # release, not debug -- a debug
                                                 # build understates onix by 10x+
uv run --group test python benchmarks/bench_bindings.py
```

**Per-call overhead and where the diff runs.** A diff whose two inputs are
both nested no deeper than a small fixed threshold runs inline on the calling
thread, so a trivial diff costs only a microsecond or two. A diff of more
deeply nested inputs runs instead on a worker thread with a large, explicitly
sized stack (with the GIL released), which is what keeps the recursive engine
from overflowing the native stack on adversarially deep input; that worker
path adds a fixed thread-handoff cost of a few tens of microseconds per call.
The shapes in the table above are dominated by per-node work (thousands to
tens of thousands of elements), so this fixed cost is a rounding error there,
but it is real for a high-frequency stream of individually deep diffs. See
`crates/onix-py/src/guard.rs` for the threshold and its stack-safety basis.

### Testing

```sh
make python-test
```

Runs the real pytest suite (`crates/onix-py/tests/`) against the compiled
extension: golden-corpus parity against real `DeepDiff` (reusing
`tests/golden/`), a differential fuzzer (600 live-object cases, ordered and
`ignore_order`, against real `DeepDiff`), every conversion-error path above,
and depth-guard tests proving adversarially deep input raises cleanly
rather than crashing the process. **Not** part of `make check`: it needs a
Python venv (`uv`) and a built extension module, which the Rust-only gate
doesn't set up. This pytest suite is `crates/onix-py`'s coverage authority.
See the `Makefile`'s `coverage` target for why `cargo-llvm-cov` excludes
it (a `cdylib` whose logic is only meaningfully exercised by calling the
compiled wheel from real Python).

### Wheels

```sh
cd crates/onix-py
maturin build --release
```

Builds a single abi3 wheel (`deepdiff_rs-<version>-cp39-abi3-<platform>.whl`,
Python ≥3.9, one wheel per platform rather than per-CPython-minor-version).
Publishing to PyPI is not yet set up (needs the
maintainer's PyPI credentials or trusted-publishing setup); the exact
command, once ready, is:

```sh
maturin publish --release
```

## Quality gates

`make check` is the merge gate; every part must pass:

| Target | Command | Bar |
| --- | --- | --- |
| `make fmt` | `cargo fmt --all --check` | no diffs |
| `make clippy` | `cargo clippy --all-targets --all-features -- -D warnings` | zero warnings (pedantic enabled at warn) |
| `make test` | `cargo test --workspace` | all pass |
| `make coverage` | `cargo llvm-cov --workspace --fail-under-lines 95` (`onix-py` excluded: see below) | ≥95% line coverage |
| `make docs` | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` | no rustdoc warnings |
| `make deny` | `cargo deny check` | advisories/licenses/bans/sources clean |
| `make machete` | `cargo machete` | no unused dependencies |

(mutation testing via `make mutants` is a separate target that runs
periodically rather than on every `check`.)

`onix-cli/src/main.rs` is held to the same ≥95% line-coverage bar as the rest
of the workspace: its `diff` subcommand logic has unit tests (`src/tests.rs`,
alongside `src/args.rs`/`src/run.rs`'s production code) and end-to-end
integration tests (`tests/cli.rs`).

`crates/onix-py` (the PyO3 bindings crate) is excluded for a different,
structural reason: it's a `cdylib` whose logic is Python-object conversion
and PyO3 glue, only meaningfully exercised by calling the *compiled wheel*
from real Python; its coverage authority is `make python-test` instead
(see the "Python" section's "Testing" subsection above).

**Coverage-tooling quirk:** `cargo-llvm-cov` doesn't attribute lines in
`#[path = "..."]`-included test modules (`diff/tests.rs`,
`ignore_order/tests.rs`, `lcs_tests.rs`, `report_tests.rs`,
`onix-cli/src/tests.rs`) to any file in its report. That shrinks the
denominator and can move the reported percentage without changing what is
actually tested. See the `Makefile`'s `coverage` target comment for the full
mechanism.

## Golden corpus

`crates/onix-core/tests/golden.rs` (part of `make test`/`make check`) proves
`onix`'s report is byte-identical (canonical JSON) to real
[DeepDiff](https://github.com/seperman/deepdiff)'s `to_json()` output at
`verbose_level=2`, on the hand-designed corpus committed under
`tests/golden/`, with one documented exception (an adversarial
path-rendering collision; see below). Each case directory carries its own
`options.json` (currently just `{"ignore_order": bool}`), read per case so
the same test runs both the ordered and `ignore_order=True` corpora. This is
the correctness bar for the whole compatibility claim. See
[`tests/golden/README.md`](https://github.com/ksco92/onix/blob/main/tests/golden/README.md) for the pinned
`deepdiff`/Python versions, the case list, and the regeneration command
(`uv run scripts/gen_goldens.py`). `scripts/differential_fuzz.py` is a
separate, non-`make-check` development-time tool that fuzzes `--ignore-order`
against real `deepdiff` directly (not just the fixed golden corpus).

### Mutation testing

`make mutants` runs [`cargo-mutants`](https://mutants.rs/) against
`onix-core` and `onix-cli` (the two crates coverage holds to the 95% bar),
excluding `onix-py` for the same structural reason (see the `Makefile`'s
`mutants` target). It's the
coverage gate's honest sibling: 100% line coverage only proves every line
*ran*, not that a test would notice if that line's logic were wrong.
Mutation testing rewrites the code in small, deliberately-wrong ways (delete
a match arm, flip an operator, swap a comparison) and checks whether the
test suite actually fails — a survivor is a real gap in what's tested.

It's slow by design (it rebuilds and re-tests once per mutant), so it runs
periodically rather than on every `make check`:

```sh
cargo install cargo-mutants --locked
make mutants
```

Every mutant `make mutants` reports falls into one of three buckets
(caught, missed, or unviable; unviable meaning it doesn't even compile,
e.g. because a type has no meaningful `Default`), and each gets triaged: a
missed mutant either gets a new regression test that kills it, or, if
justified, an explanation (kept alongside the code it concerns, as source
comments) of why it's equivalent/non-actionable.

**Standing result.** `make mutants` enumerates a deterministic **443**
mutants (18 in `onix-cli`, 425 in `onix-core`). Every mutant that is not
caught is harmless, of one of two kinds. First, **equivalent viable mutants**:
mutations that compile and run but can't change any output. These are
confined to two spots: `onix-core/src/lcs.rs`'s
`find_longest_match`/`get_matching_blocks`, and the `> 1` threshold in
`onix-core/src/diff/array.rs`'s `lcs_or_positional_array_diff` (`>= 1` is
provably equivalent there, verified over ~1.7M scalar-list pairs and against
DeepDiff 9.1.0 at the boundary). Second, **`Default`-substitution mutants that
don't compile**, because the return type has no usable `Default` impl. This
covers both of `onix-cli`'s non-caught mutants; `perf/MUTANTS.md` notes a
second, unrelated reason one `pairing.rs` case also fails to compile. No
*viable* mutant survives outside those two documented spots.

`cargo-mutants` classifies mutants into caught/missed/timeout/unviable partly
by wall-clock time, and that classification is noisy at the edges in this
workspace, so the exact split can shift between runs even though the two
harmless kinds above never change. Tool version and reproduce command are in
[perf/MUTANTS.md](https://github.com/ksco92/onix/blob/main/perf/MUTANTS.md).
Future work that touches this logic should re-run `make mutants` and confirm
no viable mutant survives outside those two documented spots.

## Benchmarks

`perf/RESULTS.md` is onix's own published performance claim against pinned
real `deepdiff` (9.1.0), covering the full fixture matrix including the
`ignore_order_10k` comparison; its own "Run procedure" section documents
what's measured and this harness's fairness rules, and its "Deferred work"
section discloses every deliberately scaled-down or deferred part of the
fixture matrix rather than silently dropping it. `perf/RESULTS.md` is
always the live, regenerable claim; superseded point-in-time reports are
not kept in the published tree.

To reproduce it from scratch:

```sh
brew install hyperfine   # or: cargo install hyperfine
perf/run_bench.sh
```

This regenerates the deterministic fixture matrix (`perf/fixtures/`,
gitignored, seeded, byte-identical on regeneration); builds
`cargo build --release`; runs a correctness precheck (onix vs. real DeepDiff
must produce byte-identical canonical JSON on every fixture; the run
aborts otherwise, since a perf number on divergent output is void); sweeps
wall clock/CPU time/peak RSS with `hyperfine`; and writes `perf/RESULTS.md`.
Every number in that file traces back to a real run; none are hand-written.
Raw per-run JSON lands in `perf/bench_raw/` (also gitignored).

`perf/generate_fixtures.py` and `perf/run_deepdiff.py` are both uv/PEP-723
pinned scripts (same pattern as `scripts/gen_goldens.py`): no separate
Python environment setup needed beyond `uv` itself.

## Layout

```
crates/onix-core   # the diff engine (library, no I/O)
crates/onix-cli    # `onix` binary (thin CLI over the core)
crates/onix-py     # PyO3 bindings (`deepdiff-rs` on PyPI), see "Python" above
scripts/           # scripts/gen_goldens.py: regenerates tests/golden/ from real DeepDiff
tests/golden       # DeepDiff-generated expected outputs (golden corpus)
perf/              # cross-language benchmark harness, see "Benchmarks" above
```

## Contributing and support

This is a pre-alpha proof of concept, and issues and pull requests are
welcome. There is no separate contributor process yet: open an issue on the
[GitHub repository](https://github.com/ksco92/onix) to report a bug, a
`DeepDiff` divergence, or a question, and include the two inputs and the
reports both engines produce for a divergence.

Before sending a change, run the full gate from the repository root:

```sh
make check        # fmt, clippy, tests, coverage, docs, cargo-deny, cargo-machete
make python-test  # the Python binding suite (needs uv + a built extension)
```

Both must pass; `make check` is the same gate the project holds itself to.

## License

MIT: see [LICENSE](https://github.com/ksco92/onix/blob/main/LICENSE).
