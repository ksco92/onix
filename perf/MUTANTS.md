# Mutation testing results

Summary of a full `cargo-mutants` run over `onix-core` and `onix-cli`. Line
coverage proves every line ran; mutation testing proves a test would fail if
that line's logic were wrong.

## Tooling and reproduce

- `cargo-mutants` 27.1.0; toolchain pinned by `rust-toolchain.toml` (Rust 1.98.0).

```sh
cargo install cargo-mutants --locked
make mutants        # cargo mutants --package onix-core --package onix-cli
```

`onix-py` is out of scope; the `Makefile`'s `mutants` target documents why
(same structural reason it is excluded from line coverage).

## What is deterministic, and what the tool classifies unreliably

`make mutants` enumerates a deterministic **979** mutants (20 in `onix-cli`,
959 in `onix-core`). Its classification of each mutant into
caught / missed / timeout / unviable is **not** reproducible run to run: it
depends on wall-clock time (a slow mutant is a "timeout" on one machine and
"missed"/"caught" on another) and, in this workspace, on build caching (a
mutant that fails to compile has been reported both as "unviable" and,
spuriously, as "caught"). So the substance below is what was verified
independently of any single run's labels.

### Kinds of mutant that survive, and why none is a real test gap

1. **Equivalent viable mutants** — a mutation that compiles and runs but
   cannot change any output, so no test can kill it. Confined to these spots,
   each with the argument written at the source:
   - `onix-core/src/lcs.rs`'s `find_longest_match` / `get_matching_blocks`:
     these either force a non-terminating loop (reported as a timeout) or
     produce a wrong-but-terminating result the surrounding comments prove is
     equivalent or non-actionable.
   - `onix-core/src/diff/array.rs`'s `lcs_or_positional_array_diff` `> 1`
     threshold: replacing `> 1` with `>= 1` is verified output-neutral —
     confirmed over ~1.7M scalar-list pairs (zero difference) and by DeepDiff
     9.1.0 parity at the boundary shapes, and `cargo mutants -F
     'array.rs:97:35'` reports this `>=` mutant missed with the sibling `==`
     and `<` mutants caught (the expected signature of an equivalent mutant
     beside non-equivalent ones). The argument for why is at the comment on
     that line.
   - `onix-core/src/path.rs`'s `python_float_repr`: `exponent < 0` → `<=`
     only runs inside the scientific-notation branch, where `exponent == 0`
     is structurally unreachable (`decimal_point <= -4 || decimal_point >
     16` already excludes it) — `<` and `<=` compute the same result for
     every value that branch can see.
   - `onix-core/src/ignore_order/distance.rs`'s `distance_family`: the
     datetime `timestamp` field's `/ 1_000_000.0` → `* 1_000_000.0`.
     `numeric_distance`'s own formula, `cutoff * (n1 - n2) / (n1 + n2)`, is a
     ratio, invariant in the reals under scaling both operands by the same
     nonzero constant — no reachable input has been observed to distinguish
     `/` from `*` here, though `f64` rounding means this is an empirical, not
     an algebraic, guarantee (unlike the `array.rs` case above, exact-integer
     `/` versus `*` on `f64` is not bit-exact in general). The sibling `%`
     mutant on the same line *is* a genuine, non-equivalent rescale and is
     caught.
   - `onix-core/src/ignore_order/memo.rs`'s `IgnoreOrderMemo::should_cache`/
     `is_container`, mutated to always return `true`: both only gate whether
     a candidate pair's distance is *cached*, never what value is computed —
     caching unconditionally costs extra cycles (a scalar pair now pays a
     clone + hashmap round trip it used to skip) but cannot change a result.

2. **`Default`-substitution mutants that cannot compile.** cargo-mutants tries
   replacing a function body with `Default::default()` (and similar). Most fail
   because the return type has no usable `Default` impl — verified directly for
   `parse_diff_args`/`parse_args` in `onix-cli/src/args.rs` (`DiffArgs` derives
   only `Debug, PartialEq, Eq`; `cargo build` fails with "the trait bound
   `DiffArgs: Default` is not satisfied") and `Distance`'s `partial_cmp`/`cmp`
   in `onix-core/src/ignore_order/distance.rs` (`std::cmp::Ordering` has no
   `Default`); likewise `lcs.rs`'s `scalar_key`/`get_matching_blocks`/
   `compute_opcodes`, `ignore_order`'s `item_key`/`HashedList::build`, and
   `dispatch.rs`'s `scoped`. `ignore_order/pairing.rs:93` contributes **two**
   unviable mutants by two independent mechanisms: the
   `HashMap::from_iter([(Default::default(), …)])` one fails on the missing
   `Default` for `ItemKey`, and the `HashMap::new()` one fails because the
   crate's fxhash-backed `HashMap` type alias has no inherent `::new()` (that
   associated fn exists only for the `RandomState`-backed `std` `HashMap`) —
   not a `Default` issue.

Every other viable mutant is caught. `onix-cli`'s only non-caught mutants are
the two uncompilable `Default`-substitutions above; every viable `onix-cli`
mutant is caught.

## A representative run (serial `make mutants`, this tree)

The full PR #21 (type-parity integration) run: 979 mutants tested — 873
caught, 74 unviable, 13 missed, 19 timeout. `cargo-mutants`' own parallel
workers contending with each other (and, in that run, with a concurrent
`make check`) produced false timeouts outside the genuinely non-terminating
`lcs.rs` spots; every one was reproduced by hand, standalone, and is either
fixed (a real gap, now caught — see the PR body for the list) or one of the
equivalent kinds documented above. A scoped re-run of just the touched files,
after the fixes and on an otherwise-idle machine, shows the expected
signature cleanly: 553 tested — 528 caught, 22 unviable, 2 missed (`path.rs`'s
and `distance.rs`'s equivalent mutants above), **0 timeout**.

A fresh full run should therefore land close to: 979 tested, 74 unviable, and
the survivors confined to the two kinds documented above — 8 in `lcs.rs`'s
`get_matching_blocks` plus one in `diff/array.rs`'s `lcs_or_positional_array_diff`
(missed), 6 in `lcs.rs`'s `find_longest_match`/`get_matching_blocks` (genuine
timeouts, non-terminating), `path.rs`'s `python_float_repr` mutant (missed),
`distance.rs`'s `* 1_000_000.0` mutant (missed), and `memo.rs`'s four
caching-gate mutants (missed) — 15 missed + 6 timeout + 74 unviable + 884
caught = 979.

- **Unviable (74):** the `Default`-substitution (and `HashMap::new()`)
  mutants of kind (2), including `args.rs:32`/`args.rs:76` and
  `distance.rs:32`/`distance.rs:38`; none compiles, so no test can exercise it.

Future work that touches this logic should re-run `make mutants` and confirm
that no *viable* mutant survives outside the five documented equivalent spots.
