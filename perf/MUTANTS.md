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

`make mutants` enumerates a deterministic **443** mutants (18 in `onix-cli`,
425 in `onix-core`). Its classification of each mutant into
caught / missed / timeout / unviable is **not** reproducible run to run: it
depends on wall-clock time (a slow mutant is a "timeout" on one machine and
"missed"/"caught" on another) and, in this workspace, on build caching (a
mutant that fails to compile has been reported both as "unviable" and,
spuriously, as "caught"). So the substance below is what was verified
independently of any single run's labels.

### Kinds of mutant that survive, and why none is a real test gap

1. **Equivalent viable mutants** — a mutation that compiles and runs but
   cannot change any output, so no test can kill it. All are confined to two
   spots, each with the argument written at the source:
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

443 mutants tested: 416 caught, 9 missed, 6 timeouts, 12 unviable.

- **Missed (9):** eight in `lcs.rs`'s `get_matching_blocks` and one in
  `diff/array.rs`'s `lcs_or_positional_array_diff` (`> 1` → `>= 1`) — all
  equivalent, per kind (1).
- **Timeouts (6):** all in `lcs.rs` (`find_longest_match` /
  `get_matching_blocks`), per kind (1).
- **Unviable (12):** the `Default`-substitution (and one `HashMap::new()`)
  mutants of kind (2), including `args.rs:32`/`args.rs:76` and
  `distance.rs:32`/`distance.rs:38`; none compiles, so no test can exercise it.

Future work that touches this logic should re-run `make mutants` and confirm
that no *viable* mutant survives outside the two documented equivalent spots.
