# onix

Rust rewrite of Python DeepDiff's core: byte-compatible deep diffing of JSON, 64-5757x faster, ignore_order included

See [DeepDiff](https://github.com/seperman/deepdiff) for the Python original
onix matches report-for-report.

**Status (September 2026):** pre-alpha proof of concept, unpublished; ordered
+ `ignore_order` diffing complete and benchmarked (see
[perf/RESULTS.md](perf/RESULTS.md)); Python bindings next.

## Reading path (first-time visitor)

In order, each building on the last:

1. **This README** — what onix is, how to build it, and where everything
   lives (see "Layout" below).
2. **`crates/onix-core/src/lib.rs`'s doc comment** — the crate's front
   door: an architecture map (parse → diff dispatch → ordered/`ignore_order`
   container comparison → `Report` → render) naming the actual module each
   step lives in, plus the design decisions behind it (why
   `serde_json::Value` with no value-model abstraction, why a recursive
   engine with a depth guard rather than an iterative one yet, etc.).
   Follow it into `crates/onix-core/src/diff/mod.rs` and
   `crates/onix-core/src/ignore_order/mod.rs` — each is itself a module-doc
   front door to its own submodules, one seam per file (see each
   directory's `mod.rs` "Internal layout" section).
3. **`tests/golden/README.md`** — what the golden corpus pins down, and the
   one documented `DeepDiff` quirk it doesn't — the empirical ground truth
   the code above matches. `crates/onix-core/src/ignore_order/mod.rs`'s own
   doc comment carries the full, source-cited `ignore_order=True` spec this
   crate implements.

## Setup from scratch

Prerequisites: a Unix-ish system with `curl`, `make`, and `git`. All commands
are run from the repository root.

1. **Install the Rust toolchain** (skip if `cargo -V` already works):

   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
   . "$HOME/.cargo/env"
   ```

   The exact toolchain version is pinned in
   [`rust-toolchain.toml`](rust-toolchain.toml); rustup installs it (plus the
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
installs its own pinned Python 3.13 and `deepdiff` on demand — no separate
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
consumption (golden-file comparison, the M6 benchmark harness), where a
single deterministic line is easiest to diff byte-for-byte.

- `--max-depth N` overrides the recursion-depth bound passed to
  `onix_core::diff_with_options`. Without the flag, the default comes
  from the `ONIX_MAX_DEPTH` environment variable if it's set to a parseable
  number, else `onix_core::DEFAULT_MAX_DEPTH` (512). This ambient-environment
  default is CLI-only — the `onix-core` library itself stays a pure function
  with no environment dependence.
- `--ignore-order` mirrors `DeepDiff(..., ignore_order=True)`: every
  list/tuple anywhere in the tree is compared by hash-based matching and
  nearest-neighbor pairing instead of index-aligned/LCS comparison (dicts
  are unaffected — they're always compared by key). See
  `crates/onix-core/src/ignore_order/mod.rs`'s module doc for the full,
  source-cited spec this implements. **Known divergences** (both `ignore_order`- and
  ordered-path-related) are tracked in
  [`tests/golden/README.md`](tests/golden/README.md)'s "Known DeepDiff
  quirks" section — an intentionally-unchased path-rendering edge case and
  a narrow list-LCS numeric-precision limit.
- `--timing` prints exactly one line of JSON to **stderr** —
  `{"parse_ns": N, "diff_ns": N}` — measuring only the `serde_json` parse of
  the two inputs and only the diff call, respectively — the same
  "diff-only self-instrumentation" the benchmark harness under `perf/`
  uses to isolate diff time from process startup and JSON parsing. Without
  `--timing`, stderr stays empty on a successful run.

Exit codes:

| Code | Meaning |
| --- | --- |
| `0` | The diff was computed successfully. This holds **whether or not** the report is empty — presence/absence of differences is carried in the stdout JSON, not the exit code. |
| `1` | Usage error: missing/unknown subcommand, wrong number of positional arguments, an unknown flag, or a non-numeric `--max-depth`. Details plus a usage line go to stderr. |
| `2` | I/O error (e.g. a missing input file) or a JSON-parse error on either input. |
| `3` | `onix_core::Error::MaxDepthExceeded` — the error's message (which path tripped the bound) goes to stderr. |

Example:

```sh
$ echo '{"a": 1}' > left.json
$ echo '{"a": 2}' > right.json
$ onix diff left.json right.json
{"values_changed":{"root['a']":{"new_value":2,"old_value":1}}}
$ echo $?
0
```

## Quality gates

`make check` is the merge gate; every part must pass:

| Target | Command | Bar |
| --- | --- | --- |
| `make fmt` | `cargo fmt --all --check` | no diffs |
| `make clippy` | `cargo clippy --all-targets --all-features -- -D warnings` | zero warnings (pedantic enabled at warn) |
| `make test` | `cargo test --workspace` | all pass |
| `make coverage` | `cargo llvm-cov --workspace --fail-under-lines 95` | ≥95% line coverage |
| `make docs` | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` | no rustdoc warnings |
| `make deny` | `cargo deny check` | advisories/licenses/bans/sources clean |
| `make machete` | `cargo machete` | no unused dependencies |

(`make mutants` — mutation testing — exists as a per-milestone target from M4 on
and is deliberately not part of `check`.)

`onix-cli/src/main.rs` was excluded from the coverage denominator through M4
(it was a logic-free shim); M5a gave it real `diff` subcommand logic plus its
own unit tests (`src/tests.rs`, alongside `src/args.rs`/`src/run.rs`'s
production code) and end-to-end integration tests (`tests/cli.rs`), so the
exclusion is gone and it's held to the same ≥95% bar as the rest of the
workspace.

**Known accepted artifact (M7b):** a `#[cfg(test)] #[path = "..."] mod
tests;` file (`diff/tests.rs`, `ignore_order/tests.rs`, `lcs_tests.rs`,
`report_tests.rs`, `onix-cli/src/tests.rs`) doesn't appear anywhere in
`cargo-llvm-cov`'s report — not as its own row, not folded into another
file's totals — so the reported percentage moved (a smaller denominator)
even though the absolute miss counts are identical to before the M7b split
(27 regions / 10 lines / 2 functions, both times). See the `Makefile`'s
`coverage` target comment for the full explanation and what it does (and
doesn't) mean for what's actually tested.

## Golden corpus

`crates/onix-core/tests/golden.rs` (part of `make test`/`make check`) proves
`onix`'s report is byte-identical (canonical JSON) to real
[DeepDiff](https://github.com/seperman/deepdiff)'s `to_json()` output at
`verbose_level=2`, on the hand-designed corpus committed under
`tests/golden/` — with one documented exception (an adversarial
path-rendering collision; see below). Each case directory carries its own
`options.json` (currently just `{"ignore_order": bool}`), read per case so
the same test runs both the ordered and `ignore_order=True` corpora. This is
the correctness bar for the whole compatibility claim — see
[`tests/golden/README.md`](tests/golden/README.md) for the pinned
`deepdiff`/Python versions, the case list, and the regeneration command
(`uv run scripts/gen_goldens.py`). `scripts/m7_differential_fuzz.py` is a
separate, non-`make-check` development-time tool that fuzzes `--ignore-order`
against real `deepdiff` directly (not just the fixed golden corpus).

### Mutation testing

`make mutants` runs [`cargo-mutants`](https://mutants.rs/) against
`onix-core` (scoped the same way, and for the same reason, as the coverage
exclusion above — see the `Makefile`'s `mutants` target). It's the
coverage gate's honest sibling: 100% line coverage only proves every line
*ran*, not that a test would notice if that line's logic were wrong.
Mutation testing rewrites the code in small, deliberately-wrong ways (delete
a match arm, flip an operator, swap a comparison) and checks whether the
test suite actually fails — a survivor is a real gap in what's tested.

It's slow by design (it rebuilds and re-tests once per mutant), so it runs
per milestone rather than on every `make check`:

```sh
cargo install cargo-mutants --locked
make mutants
```

Every mutant `make mutants` reports — caught, missed, or unviable (a mutant
that doesn't even compile, e.g. because a type has no meaningful
`Default`) — gets triaged: a missed mutant either gets a new regression
test that kills it, or, if justified, an explanation (kept alongside the
code it concerns, as source comments) of why it's equivalent/non-actionable.

**Standing result, by area** (most recent targeted run each area received;
`make mutants` now runs `--workspace`, so a full run covers both crates at
once):

| Area | Mutants tested | Caught | Missed | Unviable | Notes |
| --- | --- | --- | --- | --- | --- |
| `onix-core`'s depth-hardening (traversal/value-depth guards) | 109 | 107 | 0 | 2 | — |
| `onix-cli`'s `diff` subcommand logic | 124 | 121 | 0 | 3 | — |
| List diffing (`crate::lcs`, index-aligned/LCS matching) | 265 | 244 | 9 | 6 | plus 6 timeouts; all 9 missed and all 6 timeouts are proven equivalent/non-actionable (see the surrounding source comments for the mathematical argument) |
| `ignore_order=True` engine (`crate::ignore_order`) | 140 | 133 | 1 | 6 | the 1 missed mutant is proven unreachable (see the surrounding source comments) |

A "timeout" is `cargo-mutants`' own non-passing category for a mutant that
forces an infinite loop rather than a wrong-but-terminating result — not
something a test suite could plausibly race against faster than the tool's
own per-mutant timeout already does, so no action is taken on those either.
Future work that touches this logic should re-run `make mutants` and keep
this table current rather than letting missed mutants go unexamined.

## Layout

```
crates/onix-core   # the diff engine (library, no I/O)
crates/onix-cli    # `onix` binary (thin CLI over the core)
scripts/           # scripts/gen_goldens.py: regenerates tests/golden/ from real DeepDiff
tests/golden       # DeepDiff-generated expected outputs (M5b golden corpus)
perf/              # cross-language benchmark harness (M6) — see "Benchmarks" below
```

## Benchmarks

`perf/RESULTS.md` is onix's own published performance claim against pinned
real `deepdiff` (9.1.0), covering the full fixture matrix including the M7
`ignore_order_10k` comparison — its own "Run procedure" section documents
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

This regenerates the deterministic fixture matrix (`perf/fixtures/` —
gitignored, seeded, byte-identical on regeneration), builds
`cargo build --release`, runs a correctness precheck (onix vs. real DeepDiff
must produce byte-identical canonical JSON on every fixture — the run
aborts otherwise, since a perf number on divergent output is void), sweeps
wall clock/CPU time/peak RSS with `hyperfine`, and writes `perf/RESULTS.md`.
Every number in that file traces back to a real run; none are hand-written.
Raw per-run JSON lands in `perf/bench_raw/` (also gitignored).

`perf/generate_fixtures.py` and `perf/run_deepdiff.py` are both uv/PEP-723
pinned scripts (same pattern as `scripts/gen_goldens.py`) — no separate
Python environment setup needed beyond `uv` itself.
