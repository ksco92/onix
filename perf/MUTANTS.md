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
caught / missed / timeout / unviable is **not** reproducible run to run, and
in this workspace it is unreliable at the edges: it depends on wall-clock
time (a slow mutant is a "timeout" on one machine and "missed"/"caught" on
another) and, empirically here, on build caching (a mutant that fails to
compile has been reported both as "unviable" and, spuriously, as "caught";
one that the suite deterministically catches has been reported "missed"). So
the substance below is what was verified independently of any single run's
labels.

### Two kinds of mutant are never a real test gap

1. **`Default`-substitution mutants that cannot compile.** cargo-mutants tries
   replacing a function body with `Default::default()` (and similar). On a
   return type with no usable `Default` impl this does not compile, so it is
   not a meaningful behavioral mutation. Verified directly:
   `parse_diff_args`/`parse_args` in `onix-cli/src/args.rs` (`DiffArgs` derives
   only `Debug, PartialEq, Eq` — no `Default`; `cargo build` fails with
   "the trait bound `DiffArgs: Default` is not satisfied"), and `Distance`'s
   `partial_cmp`/`cmp` in `onix-core/src/ignore_order/distance.rs`
   (`std::cmp::Ordering` has no `Default`). The other `Default`-substitution
   mutants a run labels unviable are the same class
   (`lcs.rs`'s `scalar_key`/`get_matching_blocks`/`compute_opcodes`,
   `ignore_order`'s `item_key`/`HashedList::build`/`compute_pairs`,
   `dispatch.rs`'s `scoped`).

2. **`onix-core/src/lcs.rs`'s `find_longest_match` / `get_matching_blocks`
   mutants.** These either force a non-terminating loop (reported as a
   timeout) or produce a wrong-but-terminating result that the surrounding
   source comments prove is equivalent or non-actionable. This is the only
   place a *viable* mutant is not caught.

Everything else is caught. Where a run has reported a viable mutant "missed"
outside `lcs.rs` (for example `diff/array.rs:82`'s LCS-vs-positional
threshold), applying that mutation and running `cargo test -p onix-core`
directly makes the suite fail — i.e. it is caught, and the "missed" label was
a build-caching artifact. `onix-cli`'s only non-caught mutants are the two
`Default`-substitutions above, which cannot compile; every viable `onix-cli`
mutant is caught.

## A representative run (serial `make mutants`, this tree)

443 mutants tested: 416 caught, 9 missed, 6 timeouts, 12 unviable.

- **Missed (9):** eight in `lcs.rs`'s `get_matching_blocks` (equivalent /
  non-actionable), plus `diff/array.rs:82` (`> 1` → `>= 1` in
  `lcs_or_positional_array_diff`), which is in fact caught by the suite — see
  above.
- **Timeouts (6):** all in `lcs.rs` (`find_longest_match` /
  `get_matching_blocks`).
- **Unviable (12):** the `Default`-substitution mutants of kind (1) above,
  including `args.rs:32`/`args.rs:76` and `distance.rs:32`/`distance.rs:38`;
  each cannot compile (no usable `Default` impl), so no test can exercise it.

Future work that touches this logic should re-run `make mutants` and confirm
that no *viable* mutant survives outside `lcs.rs`'s documented equivalent set.
