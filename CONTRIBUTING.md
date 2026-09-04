# Contributing to onix

onix is the diff engine behind the `deepdiff-rs` Python package and the `onix`
CLI (see the [README](README.md) for what it does and how to use it). This
guide is for working on onix itself: building from source, the quality gates,
the compatibility corpus, benchmarking, mutation testing, and publishing.

Issues and pull requests are welcome. To report a DeepDiff divergence, open an
issue with both inputs and the report each engine produces.

## Setup from scratch

Prerequisites: a Unix-ish system with `curl`, `make`, and `git`. Run every
command from the repository root.

1. Install the Rust toolchain (skip if `cargo -V` already works):

   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   . "$HOME/.cargo/env"
   ```

   The exact version is pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
   (Rust 1.98.0 with `clippy`, `rustfmt`, and `llvm-tools-preview`); rustup
   installs it automatically the first time you run `cargo` in this repo.

2. Install the gate tooling that `make check` needs:

   ```sh
   brew install cargo-llvm-cov cargo-deny   # macOS
   cargo install cargo-machete --locked     # no Homebrew formula exists
   # or, on any platform:
   cargo install cargo-llvm-cov cargo-deny cargo-machete --locked
   ```

3. Build and verify:

   ```sh
   cargo build
   make check
   ```

Regenerating the golden corpus and benchmarking need `uv`, which installs its
own pinned Python and dependencies on demand. Benchmarking additionally needs
`hyperfine` (`brew install hyperfine`, or `cargo install hyperfine`). The
Python binding suite needs `uv` plus `maturin` (`uv tool install maturin`).

## Quality gates

`make check` is the merge gate; it is also the exact command CI runs
(`.github/workflows/check.yml`, on every pull request and push to `main`).
Every part must pass:

| Target | Command | Bar |
| --- | --- | --- |
| `make fmt` | `cargo fmt --all --check` | no diffs |
| `make clippy` | `cargo clippy --all-targets --all-features -- -D warnings` | zero warnings (pedantic enabled at warn) |
| `make test` | `cargo test --workspace` | all pass (includes doctests) |
| `make coverage` | `cargo llvm-cov --workspace --fail-under-lines 95` (`onix-py` excluded, see below) | ≥95% line coverage |
| `make docs` | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` | no rustdoc warnings |
| `make deny` | `cargo deny check` | advisories/licenses/bans/sources clean |
| `make machete` | `cargo machete` | no unused dependencies |

`make python-test` and `make mutants` are separate targets (see below); they
are not part of `make check`.

**Coverage scope.** `onix-cli` is held to the same 95% bar as `onix-core`
(its `diff` subcommand has unit tests in `crates/onix-cli/src/tests.rs` and
end-to-end tests in `crates/onix-cli/tests/cli.rs`). `onix-py` is excluded
from the line-coverage denominator: it is a `cdylib` whose logic is
Python-object conversion and PyO3 glue, only meaningfully exercised by calling
the compiled wheel from real Python, so `make python-test` is its coverage
authority instead. One tooling quirk to know: `cargo-llvm-cov` does not
attribute lines in `#[path = "..."]`-included test modules to any file, which
shrinks the denominator without changing what is tested; the `Makefile`'s
`coverage` target documents the full mechanism.

CI also exports the same `cargo llvm-cov` line-coverage run as an lcov file
and uploads it to Codecov for the README coverage badge, so `onix-py` is
excluded there for the identical reason it is excluded from `make coverage`.

## Reading path

The code is best read in this order, each step building on the last:

1. This file and the [README](README.md): what onix is, how to build it, and
   where everything lives.
2. [`crates/onix-core/src/lib.rs`](crates/onix-core/src/lib.rs)'s module doc:
   the engine's front door, an architecture map (parse, diff dispatch,
   ordered/`ignore_order` comparison, `Report`, render) that names the module
   each step lives in. Follow it into
   [`crates/onix-core/src/diff/mod.rs`](crates/onix-core/src/diff/mod.rs) and
   [`crates/onix-core/src/ignore_order/mod.rs`](crates/onix-core/src/ignore_order/mod.rs),
   each its own module-doc front door.
3. [`tests/golden/README.md`](tests/golden/README.md): what the compatibility
   corpus pins down, and the documented DeepDiff quirks it deliberately does
   not chase.
4. [`crates/onix-py/src/lib.rs`](crates/onix-py/src/lib.rs)'s module doc: how
   the Python bindings sit on top, converting Python objects to the engine's
   value model once
   ([`crates/onix-py/src/convert.rs`](crates/onix-py/src/convert.rs)) before
   calling the same core the CLI does.

## Compatibility policy

Parity with real DeepDiff is byte-for-byte for real semantics. Where DeepDiff's own
result depends on something outside the diff itself — Python's set hash order,
`PYTHONHASHSEED`, the process timezone — or would make DeepDiff crash, onix instead
picks the simpler, deterministic behavior and documents the difference in
[`tests/golden/README.md`](tests/golden/README.md) plus one sentence in this
repository's `README.md`. No machinery is added solely to reproduce such a nuance.

The differences shipped as of 0.4.0:

- **Canonical set order.** A set's members, wherever they become output, are emitted
  in one documented order rather than DeepDiff's hash order. See
  [`onix_core::value::SetItems`](crates/onix-core/src/value.rs)'s own doc and
  `tests/golden/README.md`'s "Set iteration order" section.
- **Order-independent set membership and coercion.** Which member of a Python-equality
  class a `frozenset` reports, and whether a set-versus-sequence pairing stays a
  `type_changes`, are answered by the rule DeepDiff's cache implements rather than by
  replaying its iteration order. See `tests/golden/README.md`'s "Set iteration order"
  section.
- **`frozenset` and `date` supersets.** A report holding a `frozenset` or a `date`
  value still serializes to JSON here, where DeepDiff's own `to_json()` raises
  `TypeError` on either. See `tests/golden/README.md`'s "The `date` superset" section.
- **Naive datetimes read as UTC.** A naive `datetime`/`date` is always treated as UTC,
  including for `ignore_order` pairing, where DeepDiff's own pairing distance reads a
  naive value in the process's *local* timezone. See
  [`crates/onix-core/src/ignore_order/distance.rs`](crates/onix-core/src/ignore_order/distance.rs)'s
  `distance_family` doc.
- **Year-boundary rejection.** Comparing two datetimes whose UTC form would leave a
  year outside `1..=9999` raises a clean, documented error rather than propagating
  whatever DeepDiff's own `OverflowError` would leave in an inconsistent report. See
  `tests/golden/README.md`'s "Known DeepDiff quirks" section.
- **Tuple-subclass and namedtuple refusal.** A `tuple`/`set`/`frozenset` subclass
  (including a `namedtuple`) is refused outright rather than silently diffed as its
  base type, since DeepDiff reports every value under its own type name and this MVP
  has no field-walking conversion for one. See
  [`crates/onix-py/src/convert.rs`](crates/onix-py/src/convert.rs)'s module doc.

## Golden corpus

`crates/onix-core/tests/golden.rs` (part of `make test`) proves onix's report
is byte-identical (canonical JSON) to real DeepDiff's `to_json()` output at
`verbose_level=2`, on the hand-designed corpus under `tests/golden/`, with the
documented exceptions in [`tests/golden/README.md`](tests/golden/README.md).
Each case directory carries its own `options.json`, read per case.

Regenerate the corpus from real DeepDiff (Python 3.13 and `deepdiff==9.1.0`,
both pinned in the script and installed on demand by `uv`):

```sh
uv run scripts/gen_goldens.py
```

Never hand-edit files under `tests/golden/`; every case is defined in
`scripts/gen_goldens.py`. A case value JSON cannot express (a tuple, a set,
a frozenset, a datetime or a date) is written in the tagged encoding
`scripts/golden_tags.py` defines and
[`tests/golden/README.md`](tests/golden/README.md) documents; the product's
own parse paths never interpret those tags. `scripts/differential_fuzz.py`
is a separate, development-time fuzzer that compares `--ignore-order`
against real `deepdiff` across thousands of generated cases, beyond the
fixed corpus.

## Python bindings

Build the extension into a virtualenv and run its test suite:

```sh
make python-test
```

This runs `uv sync --group test`, `maturin develop --release`, then `pytest`
against the compiled extension: golden-corpus parity against real DeepDiff, a
live-object differential fuzzer, every conversion-error path, and depth-guard
tests proving deep input raises cleanly rather than crashing. It is `onix-py`'s
coverage authority (see the coverage note above). Rebuilding the extension
after a change is `maturin develop --release`; if a fix does not bump the
version, run `uv cache clean` first so the reinstall cannot serve a cached
pre-fix binary.

## Benchmarking

Two benchmarks live in the repo; both are reproduced from source here, and the
README's [Performance](README.md#performance) section holds the numbers and
what they mean.

[`perf/RESULTS.md`](perf/RESULTS.md) is the engine benchmark against pinned
real `deepdiff` 9.1.0 over the full fixture matrix. Regenerate it from scratch:

```sh
brew install hyperfine   # or: cargo install hyperfine
perf/run_bench.sh
```

This regenerates the deterministic fixture matrix (written to `perf/fixtures/`,
gitignored), builds `cargo build --release`, runs a correctness precheck (onix
and DeepDiff must produce byte-identical canonical JSON on every fixture, else
the run aborts), sweeps wall clock/CPU/peak RSS with `hyperfine`, and writes
`perf/RESULTS.md`; raw per-run JSON lands in `perf/bench_raw/` (also
gitignored). The report's own "Run procedure", "Correctness precheck", and
"Deferred work" sections carry the full methodology and every deliberately
scaled-down part of the matrix.

The Python-bindings benchmark is a separate script:

```sh
cd crates/onix-py
uv sync --group test
uv run --group test maturin develop --release   # release, not debug: a debug build understates onix by 10x or more
uv run --group test python benchmarks/bench_bindings.py
```

`crates/onix-py/benchmarks/bench_bindings.py`'s docstring explains its
subprocess isolation, the median-of-11 sampling, and the live-object fixture
choices.

## Mutation testing

`make mutants` runs [`cargo-mutants`](https://mutants.rs/) against `onix-core`
and `onix-cli` (the two crates coverage holds to the 95% bar). It is the
coverage gate's honest sibling: 95% line coverage proves every line ran, not
that a test would notice if that line's logic were wrong. It is slow by design
(one rebuild and re-test per mutant), so it runs periodically, not on every
`make check`:

```sh
cargo install cargo-mutants --locked
make mutants
```

**Standing result.** `make mutants` enumerates a deterministic **443** mutants
(18 in `onix-cli`, 425 in `onix-core`). Every viable mutant is caught except
two documented, harmless kinds: equivalent mutants confined to
`onix-core/src/lcs.rs` and the `> 1` threshold in
`onix-core/src/diff/array.rs` (where `>= 1` is provably output-neutral), and
`Default`-substitution mutants that do not compile. The exact classification of
each mutant (caught/missed/timeout/unviable) is noisy run to run, but those two
kinds never change; [`perf/MUTANTS.md`](perf/MUTANTS.md) carries the tool
version, the reproduce command, and the full argument for why no reported
survivor is a real test gap. Work that touches this logic should re-run
`make mutants` and confirm no viable mutant survives outside those two spots.

## Wheels and publishing

Build a single abi3 wheel (`deepdiff_rs-<version>-cp39-abi3-<platform>.whl`,
Python 3.9 or newer, one wheel per platform):

```sh
cd crates/onix-py
maturin build --release
```

Publishing `deepdiff-rs` to PyPI is automated: merging a workspace version
bump to `main` is the release action. `.github/workflows/publish.yml` builds
the wheel matrix (Linux x86_64/aarch64, macOS arm64/x86_64, Windows x64, plus
the sdist) and publishes via PyPI trusted publishing (OIDC, no stored token)
whenever the `Cargo.toml` version isn't already on PyPI; otherwise it's a
no-op. There is no separate tag or release step.

The `onix-core`, `onix-cli`, and `onix-py` crates all set `publish = false` in
their manifests; crates.io publishing is a later, deliberate decision.
