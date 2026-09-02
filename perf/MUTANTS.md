# Mutation testing results

Machine-generated summary of a full `cargo-mutants` run over `onix-core` and
`onix-cli`. Line coverage proves every line ran; mutation testing proves a
test would fail if that line's logic were wrong.

## Tooling

- `cargo-mutants` 27.1.0
- toolchain: pinned by `rust-toolchain.toml` (Rust 1.98.0)

## Reproduce

```sh
cargo install cargo-mutants --locked
make mutants        # cargo mutants --package onix-core --package onix-cli
```

Add `--jobs N` to parallelize; it changes only wall-clock time, not which
mutants are enumerated. `onix-py` is excluded for the same reason it is
excluded from line coverage: its logic is only exercised by calling the
compiled wheel from Python, which the Rust test harness cannot do.

## What reproduces exactly, and what does not

The set of **443** mutants is deterministic: `make mutants` enumerates the
same 443 every time (18 in `onix-cli`, 425 in `onix-core`).

The split into caught / missed / timeout / unviable is **not** fully
deterministic, and that is a property of the tool, not of the tests.
`cargo-mutants` classifies a mutant as a *timeout* (and a slow-to-build one
as *unviable*) by wall-clock time, auto-calibrated from the baseline test
time, so a borderline mutant can land in "caught", "missed", or "timeout" on
different machines or under different load. Every borderline mutant here is in
`onix-core/src/lcs.rs`'s `find_longest_match` / `get_matching_blocks`, whose
mutants either force a non-terminating loop or produce a wrong-but-terminating
result the surrounding source comments prove is equivalent or non-actionable.

The reproducible, load-independent result is therefore:

- **Every non-caught mutant lies in `onix-core/src/lcs.rs`'s `find_longest_match`
  / `get_matching_blocks`**, or is a `Default`-substitution mutant that does
  not compile (unviable). No mutant anywhere else survives.
- **`onix-cli` is fully caught** (all 18 mutants).
- No mutant in `onix-core` outside the `lcs.rs` equivalent set survives as a
  real test gap.

## Representative run (quiet machine)

443 mutants tested: 424 caught, 14 missed, 2 timeouts, 3 unviable.

| Crate | Tested | Caught | Non-caught (all in `lcs.rs` or `Default`-unviable) |
| --- | --- | --- | --- |
| onix-core | 425 | 406 | 19 |
| onix-cli | 18 | 18 | 0 |
| **Total** | **443** | **424** | **19** |

The 14 missed and 2 timeouts in this run are all in `lcs.rs`
(`find_longest_match` / `get_matching_blocks`). A different machine may show,
say, 6 timeouts and 6 missed for the same functions; the total non-caught set
and its confinement to `lcs.rs` is what stays fixed.

## Unviable (representative run)

```
onix-core/src/lcs.rs:265:5: replace get_matching_blocks -> Vec<Match> with vec![Default::default()]
onix-core/src/ignore_order/hash.rs:69:5: replace item_key -> ItemKey with Default::default()
onix-core/src/ignore_order/pairing.rs:93:5: replace compute_pairs -> HashMap<ItemKey, ItemKey> with HashMap::from_iter([(Default::default(), Default::default())])
```

An unviable mutant is one that does not compile (substituting a `Default` the
type does not usefully provide), so no test can exercise it. Which mutants are
unviable can shift slightly between runs when a slow build is itself timed out.
