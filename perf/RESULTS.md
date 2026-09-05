# onix vs. DeepDiff — benchmark results

Generated entirely by `perf/run_bench.sh` (via `perf/summarize_results.py`) — every number below traces back to a real, timestamped run captured under `perf/bench_raw/` (gitignored; regenerate with `perf/run_bench.sh`). No number here was hand-written.

## Environment

| | |
|---|---|
| Date (UTC) | 2026-09-05T01:15:52Z |
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
  in `onix-cli`'s own rustdoc (the `run` function's "Stack safety on
  adversarially deep input" section, `crates/onix-cli/src/run.rs`) as
  expected behavior, not a bug — but it means **onix's real depth ceiling
  for JSON-file input is `serde_json`'s 128, not the 512 the CLI flag
  suggests**, and it is the *tighter* of the two tools' ceilings, not the
  looser ~500 originally anticipated.

`deep_narrow_d120` was sized (120, with margin) to a depth both tools can
following this harness's own guiding principle: report the depth
ceiling of each rather than forcing an arbitrary large target like 20k.

## Headline: diff-only time + peak RSS

Diff-only time excludes process startup and JSON parsing on both sides (self-instrumented — onix via `--timing`'s `diff_ns`, deepdiff via `time.perf_counter_ns()` around only the `DeepDiff(...)` call). **Each cell is the MEDIAN over N tier-appropriate runs (the same warmup/run counts as the run-procedure table above), shown with its observed min-max spread — never a single sample** (this harness's own rule: report medians and σ, never single runs). Peak RSS is the median of hyperfine's per-run `memory_usage_byte` (verified against `/usr/bin/time -l`'s "maximum resident set size" — identical value on this machine) over the full process, same runs as the wall-clock sweep.

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

## End-to-end wall clock

Process start to exit, both tools reading the same two JSON files (hyperfine, mean ± σ over the run counts in the procedure table above).

| Fixture | onix wall (mean ± σ) | deepdiff wall (mean ± σ) | Wall speedup |
|---|---|---|---|
| `flat_dict_10k` | 7.445 ms ± 0.118 ms | 200.009 ms ± 1.136 ms | 26.84x |
| `flat_dict_100k` | 64.992 ms ± 0.777 ms | 1.723 s ± 40.787 ms | 26.34x |
| `flat_dict_1m` | 836.912 ms ± 66.683 ms | 17.838 s ± 109.506 ms | 21.97x |
| `flat_list_100k` | 91.937 ms ± 1.800 ms | 4.871 s ± 23.623 ms | 53.13x |
| `nested_uniform_d6_b10` | 400.179 ms ± 18.937 ms | 72.369 s ± 565.800 ms | 180.03x |
| `api_payloads` | 424.671 ms ± 17.050 ms | 95.008 s ± 283.940 ms | 223.82x |
| `deep_narrow_d120` | 1.816 ms ± 0.085 ms | 183.570 ms ± 0.526 ms | 100.54x |
| `startup_trivial` | 1.777 ms ± 0.098 ms | 53.806 ms ± 0.656 ms | 30.50x |
| `ignore_order_10k` | 76.810 ms ± 0.935 ms | 13.084 s ± 100.454 ms | 170.50x |
| `identical_1m` | 323.954 ms ± 9.671 ms | 16.387 s ± 68.901 ms | 50.76x |

## CPU time (user+sys) and allocation profile

CPU time is the cloud-cost-relevant number (instances bill CPU-seconds regardless of wall clock) and doubles as the energy proxy documented in the Energy section below. `tracemalloc peak` is deepdiff's traced-allocation peak during the diff call only; onix's equivalent (a counting global allocator behind a bench-only feature) is a **documented TODO**, not implemented (marked nice-to-have, not required) — see the Deferred work note at the end of this file.

| Fixture | onix CPU (user+sys) | deepdiff CPU (user+sys) | deepdiff tracemalloc peak |
|---|---|---|---|
| `flat_dict_10k` | 6.960 ms | 197.442 ms | 1.84 MB |
| `flat_dict_100k` | 63.526 ms | 1.718 s | 21.40 MB |
| `flat_dict_1m` | 807.038 ms | 17.787 s | 189.39 MB |
| `flat_list_100k` | 90.541 ms | 4.859 s | 38.49 MB |
| `nested_uniform_d6_b10` | 388.317 ms | 72.191 s | 372.47 MB |
| `api_payloads` | 410.064 ms | 94.785 s | 71.44 MB |
| `deep_narrow_d120` | 1.427 ms | 181.105 ms | 4.20 MB |
| `startup_trivial` | 1.374 ms | 51.734 ms | 0.01 MB |
| `ignore_order_10k` | 75.084 ms | 13.008 s | 158.81 MB |
| `identical_1m` | 320.772 ms | 16.340 s | 141.22 MB |

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
| Wall clock (mean ± σ) | 1.777 ms ± 0.098 ms | 53.806 ms ± 0.656 ms |
| Ratio | | 30.50x slower to start |

## Design notes: `ignore_order_10k` (the ignore_order headline comparison)

`ignore_order_10k` is diffed by both tools with `--ignore-order`
(`DeepDiff(..., ignore_order=True)` / `onix diff --ignore-order`) — a real
two-tool comparison like every other fixture (see the Headline table
above for its row: 176.60x diff-only, this run).
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
| MB of input / CPU-second (`api_payloads`) | 125.9 MB/s | 0.5 MB/s |
| Estimated $ / 1M `api_payloads`-sized diffs | $15.04 | $3,475.44 |

This is a **CPU-time-only** cost model (excludes egress, storage, and
per-request platform overhead) meant to illustrate the *relative* economic
gap, not a production cost estimate.

## GO / NO-GO evaluation

This harness's success thresholds: **≥5x faster (diff-only) OR ≥5x less peak memory on the majority of fixtures, and strictly better on `api_payloads`; no fixture where onix is slower** (any regression is a bug to explain, not a caveat to publish).

| Fixture | Meets ≥5x threshold | Diff-only speedup | Memory ratio |
|---|---|---|---|
| `flat_dict_10k` | YES | 44.75x | 6.79x |
| `flat_dict_100k` | YES | 41.47x | 2.73x |
| `flat_dict_1m` | YES | 37.08x | 1.58x |
| `flat_list_100k` | YES | 57.31x | 4.06x |
| `nested_uniform_d6_b10` | YES | 344.80x | 3.82x |
| `api_payloads` | YES | 577.05x | 2.26x |
| `deep_narrow_d120` | YES | 4245.55x | 19.23x |
| `startup_trivial` | YES | 147.75x | 15.22x |
| `ignore_order_10k` | YES | 176.60x | 5.74x |
| `identical_1m` | YES | 1706.35x | 1.60x |

- **Majority of fixtures clear the ≥5x bar:** YES.
- **`api_payloads` strictly better on both axes:** YES (diff-only 577.05x, memory 2.26x).
- **No fixture where onix is slower (diff-only) than deepdiff.**
- **Why the largest flat dicts show the slimmest margin:** onix's diff-only speedup and memory ratio bottom out on `flat_dict_1m` (1M unique keys). The compact model stores each object as a sorted key/value slice looked up by binary search, so at this size the dominant cost is key `memcmp` during lookup — worse cache behavior than a `BTreeMap` descent — and unique keys defeat the interner. It is an accepted constant-factor representation tradeoff: the same compact layout is what earns the large memory wins on realistic record and nested data.

### Verdict: **GO**

This is an **upper bound**, not the product validation — onix here diffs data the CLI stream-parses straight from JSON text into the compact `onix_core::Value`, with no intermediate `serde_json` tree and no FFI or Python-object conversion cost on this path's ledger. The decision-relevant validation is the product surface (real diffing through the Python bindings on live Python objects), where per-node FFI or up-front conversion costs will land on onix's side of the ledger. A clean GO here justifies *continuing* toward that validation, not a claim that the product is proven.

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
