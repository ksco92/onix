# Mutation testing results

Machine-generated summary of a full `cargo-mutants` run over `onix-core` and
`onix-cli`. Line coverage proves every line ran; mutation testing proves a
test would fail if that line's logic were wrong. Every number here comes from
the run described under "Reproduce"; none is hand-written.

## Tooling

- `cargo-mutants` 27.1.0
- toolchain: pinned by `rust-toolchain.toml` (Rust 1.98.0)

## Reproduce

```sh
cargo install cargo-mutants --locked
cargo mutants --package onix-core --package onix-cli --jobs 6
```

`onix-py` is excluded for the same reason it is excluded from line coverage:
its logic is only exercised by calling the compiled wheel from Python, which
the Rust test harness cannot do.

## Totals

443 mutants tested: 429 caught, 6 missed, 2 unviable, 6 timeouts.

| Crate | Tested | Caught | Missed | Timeout | Unviable |
| --- | --- | --- | --- | --- | --- |
| onix-core | 425 | 412 | 6 | 6 | 1 |
| onix-cli | 18 | 17 | 0 | 0 | 1 |
| **Total** | **443** | **429** | **6** | **6** | **2** |

## Caught, by source file

| File | Caught |
| --- | --- |
| onix-core/src/ignore_order/distance.rs | 120 |
| onix-core/src/lcs.rs | 88 |
| onix-core/src/diff/dispatch.rs | 44 |
| onix-core/src/report.rs | 43 |
| onix-core/src/diff/array.rs | 38 |
| onix-core/src/diff/scalar.rs | 19 |
| onix-core/src/ignore_order/mod.rs | 17 |
| onix-core/src/ignore_order/fxhash.rs | 13 |
| onix-cli/src/args.rs | 10 |
| onix-core/src/diff/object.rs | 8 |
| onix-core/src/ignore_order/hash.rs | 6 |
| onix-core/src/ignore_order/pairing.rs | 6 |
| onix-cli/src/run.rs | 6 |
| onix-core/src/path.rs | 4 |
| onix-core/src/diff/options.rs | 3 |
| onix-core/src/lib.rs | 2 |
| onix-core/src/error.rs | 1 |
| onix-cli/src/main.rs | 1 |

## Missed (6) — all in `onix-core/src/lcs.rs`

```
lcs.rs:280:37: replace < with <= in get_matching_blocks
lcs.rs:283:24: replace + with - in get_matching_blocks
lcs.rs:283:37: replace < with == in get_matching_blocks
lcs.rs:283:37: replace < with > in get_matching_blocks
lcs.rs:283:37: replace < with <= in get_matching_blocks
lcs.rs:283:43: replace && with || in get_matching_blocks
```

## Timeouts (6) — all in `onix-core/src/lcs.rs`

```
lcs.rs:220:5: replace find_longest_match -> (usize, usize, usize) with (1, 0, 1)
lcs.rs:220:5: replace find_longest_match -> (usize, usize, usize) with (1, 1, 1)
lcs.rs:220:5: replace find_longest_match -> (usize, usize, usize) with (1, 1, 0)
lcs.rs:224:27: replace + with - in find_longest_match
lcs.rs:224:27: replace + with * in find_longest_match
lcs.rs:228:28: replace < with == in find_longest_match
```

A timeout is `cargo-mutants`' category for a mutant that forces a
non-terminating loop rather than a wrong-but-terminating result. The missed
and timeout mutants are confined to `lcs.rs`'s `get_matching_blocks` and
`find_longest_match`; the surrounding source comments carry the argument for
why each is equivalent or non-actionable.

## Unviable (2)

```
onix-cli/src/args.rs:32:5: replace parse_diff_args -> Result<DiffArgs, String> with Ok(Default::default())
onix-core/src/ignore_order/distance.rs:32:9: replace <impl PartialOrd for Distance>::partial_cmp -> Option<Ordering> with Some(Default::default())
```

An unviable mutant is one that does not compile (here, substituting a
`Default` the type does not usefully provide), so no test can exercise it.
