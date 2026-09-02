# onix vs. DeepDiff — benchmark results

Generated entirely by `perf/run_bench.sh` (via `perf/summarize_results.py`) — every number below traces back to a real, timestamped run captured under `perf/bench_raw/` (gitignored; regenerate with `perf/run_bench.sh`). No number here was hand-written.

> **Note (2026-09-02):** These figures were captured before the engine's value-model migration, when the diff operated on `serde_json::Value` directly. The engine now operates on the compact `onix_core::Value`, and the CLI converts from `serde_json::Value` at its boundary — a conversion that currently sits inside the CLI's `--timing` diff window, so the reported diff-only time transiently includes it. A full re-measurement accompanies the upcoming parse/bindings migration; until then, treat the diff-only figures here as pre-migration.

## Environment

| | |
|---|---|
| Date (UTC) | 2026-09-01T20:26:09Z |
| OS | macOS 26.5.1 (build 25F80) |
| CPU | Apple M5 Max |
| Cores | 18 |
| Memory | 137438.95 MB |
| rustc | rustc 1.98.0 (88d9e12ae 2026-08-18) |
| cargo | cargo 1.98.0 (797e8a9bc 2026-08-05) |
| Python | Python 3.13.14 (uv-managed, pinned `==3.13.*`) |
| deepdiff | 9.1.0 (pinned) |
| uv | uv 0.11.25 (Homebrew 2026-06-26 aarch64-apple-darwin) |
| hyperfine | hyperfine 1.20.0 |
| Build | `cargo build --release` (`lto = true`, `codegen-units = 1`) |

## Fixture matrix

Generated deterministically by `perf/generate_fixtures.py` (fixed seed `20260831`, recorded there — regeneration is byte-identical; see that file's module docstring for the verification command). ~5% values changed, ~2% added, ~2% removed between each fixture's `a`/`b` pair, except `identical_1m` (byte-identical copy), `startup_trivial` (`{}` vs `{}`), and `ignore_order_10k` (pure shuffle + ~5% value-changed, no add/remove — see `ignore_order.rs`).

| Fixture | What it stresses | Input size (a+b) |
|---|---|---|
| `flat_dict_10k` | 1-level dict, 10k keys — dict key-set ops | 0.42 MB |
| `flat_dict_100k` | 1-level dict, 100k keys — dict at moderate scale | 4.20 MB |
| `flat_dict_1m` | 1-level dict, 1M keys — dict at scale, memory | 41.97 MB |
| `flat_list_100k` | 1-level list, 100k scalar items — LCS-matched scalar list diffing | 1.39 MB |
| `nested_uniform_d6_b10` | tree, depth 6, branch 10 (~1M leaves) — recursion overhead | 25.54 MB |
| `api_payloads` | heterogeneous record list — the "real world" headline number | 51.63 MB |
| `deep_narrow_d120` | single-chain nesting, depth 120 — both tools' depth ceiling | 0.00 MB |
| `startup_trivial` | {} vs {} — isolates interpreter/binary startup + import cost | 0.00 MB |
| `ignore_order_10k` | list, shuffled + 5% mutated, diffed with `--ignore-order` — the ignore_order headline comparison | 0.14 MB |
| `identical_1m` | flat_dict_1m vs itself — the no-diff fast path | 41.78 MB |

## Run procedure (as actually executed)

Deterministic by design: fixed warmup/run counts below, no adaptive
sampling, same command sequence every invocation of `run_bench.sh`.

| Tier | Fixtures | Warmup | Runs |
|---|---|---|---|
| standard | `flat_dict_10k`, `flat_dict_100k`, `flat_list_100k`, `deep_narrow_d120` | 3 | 10 |
| startup (cheap; more runs for tighter statistics) | `startup_trivial` | 5 | 20 |
| heavy (~12-17s/diff on this machine) | `flat_dict_1m`, `identical_1m`, `ignore_order_10k` | 1 | 5 |
| very heavy (~1-1.5min/diff on this machine) | `nested_uniform_d6_b10`, `api_payloads` | 0 | 3 |

The two "very heavy" fixtures use only 3 runs (no warmup) purely for total
harness runtime — a single deepdiff diff-only call already takes over a
minute at that size; onix's own run count is unaffected by this (it is not
what makes those fixtures slow) but hyperfine measures both commands
together in one comparison sweep.

Three independent measurement passes run per fixture, each using this same
warmup/run tier: the correctness precheck (one run per tool, not tallied
above — its only job is the byte-identical canonical-JSON comparison), the
diff-only timing sample loop (the tier's full warmup+runs, feeding the
Headline table's medians below), and the hyperfine sweep (also the tier's
full warmup+runs, feeding wall clock/CPU/RSS). Diff-only timing is
deliberately its own pass, not reused from the precheck or hyperfine runs
— this harness always reports a median over N runs, never a single
sample, and hyperfine's own runs don't expose per-run stderr to extract
`diff_ns` from.

## Correctness precheck

**Every fixture below reached this file only after its onix and DeepDiff
outputs were canonicalized (`jq -S`, matching `crates/onix-core/tests/golden.rs`'s
own "sorted-keys, order-sensitive-arrays" notion of canonical equality) and
found byte-identical.** `run_bench.sh` aborts the entire run — no
`RESULTS.md` gets written at all — the moment any fixture's outputs
diverge — a perf number on divergent output is void.

All 10 fixtures in the matrix matched on this run —
`ignore_order_10k` included: it's a real `onix --ignore-order`
vs. `DeepDiff(..., ignore_order=True)` comparison, not a deepdiff-only
baseline, and it clears the exact same precheck as every other fixture.
It's also an all-numeric flat list, so it never reaches the disclosed,
pre-existing `threshold_to_diff_deeper` dict-vs-dict divergence already
tracked by `crates/onix-core/tests/golden.rs`'s `KNOWN_DIVERGENT_CASES`
— no special-casing was needed here.

`api_payloads` wraps each scalar in its `tags` and `metadata.flags` lists
in a single-key dict. An earlier concern was a divergence on the default
*ordered* path: real DeepDiff 9.1.0 applies an LCS-style "cheapest edit"
match for lists of *hashable* scalars, which onix's then-simpler
index-aligned list algorithm did not mirror, so two same-length
low-cardinality scalar lists sharing values at different offsets could
diverge. That gap is closed — `crates/onix-core/src/lcs.rs` now dispatches
scalar-only lists to the same LCS/`difflib` matching DeepDiff uses — and
differential testing confirms both tools agree without the wrapping. It is
retained only so the generated fixture byte-matches the shape these
published measurements used.

## Finding: onix's practical depth ceiling is lower than expected

The `deep_narrow_dN` fixture's target depth was originally set to
~500, gated by DeepDiff's own Python recursion limit. Two independent ceilings were
empirically probed while building this fixture (see
`perf/generate_fixtures.py`'s `DEEP_NESTING_DEPTH` constant):

- **Real DeepDiff 9.1.0** (default `sys.getrecursionlimit() == 1000`) on
  this single-chain dict shape raises `RecursionError` starting at
  **~depth 495** — probed at 495 (succeeds) and 496 (fails) on this
  machine, but this is a Python C-stack-depth limit, not a pure
  Python-frame-count one, so the exact boundary can shift by a few levels
  run to run depending on intervening C-stack usage. Treat "~495" as an
  approximate, not exact, ceiling.
- **`onix-cli`'s actual ceiling is much lower and IS exact: 126**, and it
  fails to *parse*, not diff. `onix-cli` parses with `serde_json`'s default
  (non-`unbounded_depth`) parser, which hard-caps at 128 levels of *parser*
  recursion — completely independent of `onix_core::diff_with_max_depth`'s
  own `--max-depth`/`DEFAULT_MAX_DEPTH` guard (512 by default), which never
  even gets exercised here because parsing fails first. This is documented
  in `onix-cli`'s own `README.md`/rustdoc as expected behavior, not a bug —
  but it means **onix's real depth ceiling for JSON-file input is `serde_json`'s
  128, not the 512 the CLI flag suggests**, and it is the *tighter* of the
  two tools' ceilings, not the looser ~500 originally anticipated.

`deep_narrow_d120` was sized (120, with margin) to a depth both tools can
following this harness's own guiding principle: report the depth
ceiling of each rather than forcing an arbitrary large target like 20k.

## Headline: diff-only time + peak RSS

Diff-only time excludes process startup and JSON parsing on both sides (self-instrumented — onix via `--timing`'s `diff_ns`, deepdiff via `time.perf_counter_ns()` around only the `DeepDiff(...)` call). **Each cell is the MEDIAN over N tier-appropriate runs (the same warmup/run counts as the run-procedure table above), shown with its observed min-max spread — never a single sample** (this harness's own rule: report medians and σ, never single runs). Peak RSS is the median of hyperfine's per-run `memory_usage_byte` (verified against `/usr/bin/time -l`'s "maximum resident set size" — identical value on this machine) over the full process, same runs as the wall-clock sweep.

| Fixture | onix diff-only (median, min-max) | deepdiff diff-only (median, min-max) | Speedup | onix peak RSS | deepdiff peak RSS | Memory ratio | ≥5x threshold |
|---|---|---|---|---|---|---|---|
| `flat_dict_10k` | 1.516 ms (1.450 ms-1.661 ms) | 135.661 ms (135.093 ms-140.955 ms) | 89.49x | 5.83 MB | 39.44 MB | 6.76x | ✅ |
| `flat_dict_100k` | 17.298 ms (17.052 ms-18.279 ms) | 1.542 s (1.533 s-1.570 s) | 89.15x | 39.55 MB | 111.00 MB | 2.81x | ✅ |
| `flat_dict_1m` | 184.356 ms (182.837 ms-188.048 ms) | 16.588 s (16.477 s-16.772 s) | 89.98x | 377.68 MB | 753.68 MB | 2.00x | ✅ |
| `flat_list_100k` | 71.966 ms (71.258 ms-73.355 ms) | 4.621 s (4.589 s-4.650 s) | 64.21x | 39.62 MB | 155.17 MB | 3.92x | ✅ |
| `nested_uniform_d6_b10` | 158.700 ms (154.202 ms-162.214 ms) | 68.085 s (68.016 s-68.723 s) | 429.02x | 306.53 MB | 908.39 MB | 2.96x | ✅ |
| `api_payloads` | 115.573 ms (114.300 ms-119.383 ms) | 90.578 s (90.295 s-90.634 s) | 783.73x | 866.12 MB | 866.16 MB | 1.00x | ✅ |
| `deep_narrow_d120` | 0.020 ms (0.020 ms-0.025 ms) | 116.934 ms (116.428 ms-118.501 ms) | 5756.74x | 2.15 MB | 41.14 MB | 19.17x | ✅ |
| `startup_trivial` | 0.001 ms (0.001 ms-0.001 ms) | 0.157 ms (0.150 ms-0.165 ms) | 183.49x | 2.15 MB | 32.70 MB | 15.24x | ✅ |
| `ignore_order_10k` | 71.159 ms (69.940 ms-72.444 ms) | 12.550 s (12.529 s-12.598 s) | 176.37x | 108.97 MB | 345.55 MB | 3.17x | ✅ |
| `identical_1m` | 76.049 ms (75.782 ms-77.303 ms) | 15.343 s (15.292 s-15.378 s) | 201.76x | 319.24 MB | 503.17 MB | 1.58x | ✅ |

## End-to-end wall clock

Process start to exit, both tools reading the same two JSON files (hyperfine, mean ± σ over the run counts in the procedure table above).

| Fixture | onix wall (mean ± σ) | deepdiff wall (mean ± σ) | Wall speedup |
|---|---|---|---|
| `flat_dict_10k` | 5.692 ms ± 0.123 ms | 188.710 ms ± 4.536 ms | 32.60x |
| `flat_dict_100k` | 49.315 ms ± 0.570 ms | 1.628 s ± 7.853 ms | 32.98x |
| `flat_dict_1m` | 519.171 ms ± 11.073 ms | 16.984 s ± 64.976 ms | 32.63x |
| `flat_list_100k` | 77.718 ms ± 0.412 ms | 4.681 s ± 12.134 ms | 60.14x |
| `nested_uniform_d6_b10` | 307.527 ms ± 3.802 ms | 68.505 s ± 109.718 ms | 222.42x |
| `api_payloads` | 331.801 ms ± 5.178 ms | 90.251 s ± 174.241 ms | 270.07x |
| `deep_narrow_d120` | 1.595 ms ± 0.082 ms | 167.600 ms ± 0.841 ms | 105.66x |
| `startup_trivial` | 1.570 ms ± 0.125 ms | 46.714 ms ± 0.346 ms | 29.86x |
| `ignore_order_10k` | 76.467 ms ± 0.336 ms | 12.503 s ± 90.086 ms | 163.46x |
| `identical_1m` | 369.904 ms ± 2.747 ms | 15.659 s ± 57.523 ms | 42.31x |

## CPU time (user+sys) and allocation profile

CPU time is the cloud-cost-relevant number (instances bill CPU-seconds regardless of wall clock) and doubles as the energy proxy documented in the Energy section below. `tracemalloc peak` is deepdiff's traced-allocation peak during the diff call only; onix's equivalent (a counting global allocator behind a bench-only feature) is a **documented TODO**, not implemented (marked nice-to-have, not required) — see the Deferred work note at the end of this file.

| Fixture | onix CPU (user+sys) | deepdiff CPU (user+sys) | deepdiff tracemalloc peak |
|---|---|---|---|
| `flat_dict_10k` | 5.355 ms | 186.241 ms | 1.84 MB |
| `flat_dict_100k` | 48.375 ms | 1.624 s | 21.40 MB |
| `flat_dict_1m` | 515.133 ms | 16.965 s | 189.39 MB |
| `flat_list_100k` | 76.605 ms | 4.674 s | 38.49 MB |
| `nested_uniform_d6_b10` | 304.590 ms | 68.442 s | 372.47 MB |
| `api_payloads` | 324.690 ms | 90.169 s | 71.44 MB |
| `deep_narrow_d120` | 1.262 ms | 165.201 ms | 4.20 MB |
| `startup_trivial` | 1.220 ms | 44.674 ms | 0.01 MB |
| `ignore_order_10k` | 74.533 ms | 12.489 s | 158.81 MB |
| `identical_1m` | 366.666 ms | 15.641 s | 141.22 MB |

## Startup/import cost

`startup_trivial` (`{}` vs `{}`) isolates process startup: the diff
itself is trivially empty, so its wall-clock time is dominated by
interpreter startup + `import deepdiff` on the Python side, and binary
exec-to-main on the Rust side.

**Caveat: the deepdiff number is measured via `uv run perf/run_deepdiff.py`**
(per this harness's own fairness rule), not a bare `python`
invocation, so it also includes `uv`'s own subprocess-launch and
environment-resolution overhead (typically ~10-30ms on a cached
environment) on top of pure interpreter+import cost. This number is real
and reproducible as measured, but is not a pure "Python interpreter +
`import deepdiff`" figure — a bare-interpreter comparison would show a
smaller gap.

| | onix | deepdiff (via `uv run`) |
|---|---|---|
| Wall clock (mean ± σ) | 1.570 ms ± 0.125 ms | 46.714 ms ± 0.346 ms |
| Ratio | | 29.86x slower to start |

## Design notes: `ignore_order_10k` (the ignore_order headline comparison)

`ignore_order_10k` is diffed by both tools with `--ignore-order`
(`DeepDiff(..., ignore_order=True)` / `onix diff --ignore-order`) — a real
two-tool comparison like every other fixture (see the Headline table
above for its row: 176.37x diff-only, this run).
This was DeepDiff's own documented headline slowness (its `O(changed²)`
candidate-pairing built from real Python objects) and the motivating
reason for `onix-core`'s ignore_order support. Three design choices explain the size of the
gap:

- **The numeric fast path never builds a `Report`.** For a flat list of
  ints like this fixture, every pairing candidate's distance is computed
  by [`crate::ignore_order::numeric_distance`] alone (closed-form
  arithmetic), never touching the structural fallback that would
  otherwise pay for `PathSegment` allocations, `Value` clones, and
  `BTreeMap` inserts per candidate — replicating DeepDiff's own
  per-candidate object-construction cost in Rust would have defeated the
  point of this port.
- **Every item is hashed exactly once per list** (`HashedList::build`),
  not recomputed per candidate comparison — the
  `O(hashes_added × hashes_removed)` candidate loop only ever does `O(1)`
  hash-map lookups against already-computed keys.
- **A from-scratch, dependency-free `FxHasher`** (this crate's own quality
  bar has no new-dependency budget) replaces the standard library's
  default `SipHash` for this module's `HashMap`/`HashSet`s. `SipHash`'s
  DoS-resistance is a real per-call cost: switching the input-keyed maps to
  it slowed this shape's diff by a measurable margin, so `FxHash` is kept
  and the residual hash-flooding exposure on attacker-controlled keys is
  documented as an accepted trade-off — see `ignore_order.rs`'s own doc.

The cost is dominated by `O(change_n²)` (the candidate-pairing loop), not
`O(n²)` — matching real `DeepDiff`'s own documented cost anatomy (see
`crate::ignore_order`'s own module doc for the full, source-cited
scaling-signature analysis; not re-run here, since it validates the
algorithm's asymptotic behavior, not this fixture's specific numbers).

## Energy (best-effort — fell back to the documented proxy)

`sudo powermetrics` needs root; `sudo -n true` failed non-interactively in
this environment (the common case for a headless/sandboxed run), so energy
sampling was **skipped**, falling back explicitly to **CPU
time (user+sys), already reported in the "CPU time (user+sys) and
allocation profile" table above, as the documented proxy**
(roughly proportional to energy at fixed clock speed).

To get a real Joules/diff number, the repository owner can run, on this
same machine:

```sh
sudo powermetrics --samplers cpu_power -i 200 -n 25 --show-process-energy
```

while looping a fixture diff (see `run_bench.sh`'s Step 6 for the exact
loop it would otherwise run) and dividing the reported package energy by
the iteration count.

## Derived: throughput and cost

MB of input processed per CPU-second (higher is better), and an estimated
cost for 1 million `api_payloads`-sized diffs on **AWS EC2 r7i.large (2 vCPU, 16 GiB, us-east-1, on-demand)**
($0.132/hour on-demand; source:
instances.vantage.sh/aws/ec2/r7i.large, accessed 2026-08-31) —
CPU-seconds (user+sys), not wall clock, is what a shared/serverless
CPU-billed instance actually bills.

| | onix | deepdiff |
|---|---|---|
| MB of input / CPU-second (`api_payloads`) | 159.0 MB/s | 0.6 MB/s |
| Estimated $ / 1M `api_payloads`-sized diffs | $11.91 | $3,306.19 |

This is a **CPU-time-only** cost model (excludes egress, storage, and
per-request platform overhead) meant to illustrate the *relative* economic
gap, not a production cost estimate.

## GO / NO-GO evaluation

This harness's success thresholds: **≥5x faster (diff-only) OR ≥5x less peak memory on the majority of fixtures, and strictly better on `api_payloads`; no fixture where onix is slower** (any regression is a bug to explain, not a caveat to publish).

| Fixture | Meets ≥5x threshold | Diff-only speedup | Memory ratio |
|---|---|---|---|
| `flat_dict_10k` | YES | 89.49x | 6.76x |
| `flat_dict_100k` | YES | 89.15x | 2.81x |
| `flat_dict_1m` | YES | 89.98x | 2.00x |
| `flat_list_100k` | YES | 64.21x | 3.92x |
| `nested_uniform_d6_b10` | YES | 429.02x | 2.96x |
| `api_payloads` | YES | 783.73x | 1.00x |
| `deep_narrow_d120` | YES | 5756.74x | 19.17x |
| `startup_trivial` | YES | 183.49x | 15.24x |
| `ignore_order_10k` | YES | 176.37x | 3.17x |
| `identical_1m` | YES | 201.76x | 1.58x |

- **Majority of fixtures clear the ≥5x bar:** YES.
- **`api_payloads` strictly better on both axes:** YES (diff-only 783.73x, memory 1.00x).
- **No fixture where onix is slower (diff-only) than deepdiff.**

### Verdict: **GO**

This is an **upper bound**, not the product validation — onix here diffs data read from JSON files (parsed at the CLI boundary and converted into the engine's compact `onix_core::Value`), with no FFI or Python-object conversion cost on its ledger. The decision-relevant validation is the product surface (real diffing through the Python bindings on live Python objects), where per-node FFI or up-front conversion costs will land on onix's side of the ledger. A clean GO here justifies *continuing* toward that validation, not a claim that the product is proven.

## Deferred work (documented, not silently dropped)

**Fixture matrix scaled down from the original full table** (this benchmark
was explicitly scoped to build "a scalable, representative subset", not the
full matrix — but here is every cut, not just the headline one):

- **`flat_list_5m`** (the originally envisioned 5-million-item list,
  "throughput, memory") — **not built at all**. Only `flat_list_100k` is in this run's
  matrix; a multi-million-item list fixture is a candidate follow-up if
  finer-grained throughput data at that scale is ever needed.
- **`api_payloads`** capped at 50,000 records rather than the originally
  suggested ~50-200MB (see the actual measured size in the Fixture matrix
  table above) — see `perf/generate_fixtures.py`'s `API_PAYLOAD_RECORD_COUNT`
  comment: at 100k records deepdiff's diff-only call already took ~3
  minutes, which made the full deterministic harness (every fixture run
  multiple times) impractical to run in one sitting; 50k records already
  makes deepdiff take ~90 seconds per diff — "meaningfully long" per the
  brief's own bar.
- **`deep_narrow_dN`** at depth 120, not the originally-envisioned 20k
  (nor even the ~500 fallback) — see the "Finding: onix's practical
  depth ceiling is lower than expected" section above for why.

Also deferred, unrelated to matrix scale:

- **Rust-side counting allocator** (marked nice-to-have, not required):
  not implemented here. onix's allocation profile is inferred
  only indirectly, via peak RSS and the (already dramatic) CPU-time gap.
  Left for follow-up if the allocation-churn detail is ever
  decision-relevant.
- **Criterion micro-benches**: not implemented here —
  `run_bench.sh`'s cross-language sweep was the priority; a
  per-fixture-shape Criterion suite inside `onix-core` is a natural
  follow-up once the cross-language number exists to compare against.
- **Energy sampling**: see the Energy section above — CPU-seconds is the
  documented fallback proxy; a real Joules/diff number needs a manual
  `sudo` run by the repository owner (exact command provided there).
