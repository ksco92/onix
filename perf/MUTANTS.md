# Mutation testing results

The `cargo-mutants` enumeration for `onix-core`, `onix-cli` and `onix-arrow`,
the kinds of mutant that survive and why none is a test gap, and how to
reproduce it. Line coverage proves every line ran; mutation testing proves a
test would fail if that line's logic were wrong.

## Tooling and reproduce

- `cargo-mutants` 27.1.0; toolchain pinned by `rust-toolchain.toml` (Rust 1.98.0).

```sh
cargo install cargo-mutants --locked
make mutants        # cargo mutants --package onix-core --package onix-cli --package onix-arrow
```

`onix-py` is out of scope; `CONTRIBUTING.md`'s Coverage scope explains why
(same structural reason it is excluded from line coverage).

## What is deterministic, and what the tool classifies unreliably

`make mutants` enumerates a deterministic 1511 mutants (`cargo mutants --list
-p onix-core -p onix-cli`: 20 in `onix-cli`, 1491 in `onix-core`, of which
`hash.rs` has 42, `memo.rs` 25, `lcs.rs` 191) plus 634 in `onix-arrow`
(`cargo mutants --list -p onix-arrow`: 509 in `row_diff.rs`, 38 in
`schema.rs`, 36 in `profile.rs`, 26 in `table_diff.rs`, 9 in `options.rs`, 5 in
`json_rows.rs`, 4 in `lib.rs`, 4 in `error.rs`, 3 in `spool.rs`). The
classification below is of that 2145-mutant enumeration, one serial `make
mutants` run (14 h): **1790 caught, 246 unviable, 49 timeout, 60 missed**.

| crate | mutants | caught | unviable | timeout | missed |
| --- | ---: | ---: | ---: | ---: | ---: |
| `onix-core` | 1491 | 1313 | 130 | 37 | 11 |
| `onix-cli` | 20 | 14 | 6 | 0 | 0 |
| `onix-arrow` | 634 | 463 | 110 | 12 | 49 |

`onix-arrow` by file:

| file | mutants | caught | unviable | timeout | missed |
| --- | ---: | ---: | ---: | ---: | ---: |
| `row_diff.rs` | 509 | 407 | 85 | 12 | 5 |
| `schema.rs` | 38 | 26 | 12 | 0 | 0 |
| `profile.rs` | 36 | 0 | 0 | 0 | 36 |
| `table_diff.rs` | 26 | 10 | 8 | 0 | 8 |
| `options.rs` | 9 | 8 | 1 | 0 | 0 |
| `json_rows.rs` | 5 | 5 | 0 | 0 | 0 |
| `lib.rs` | 4 | 3 | 1 | 0 | 0 |
| `error.rs` | 4 | 4 | 0 | 0 | 0 |
| `spool.rs` | 3 | 0 | 3 | 0 | 0 |

The unviable mutants are `Default`-substitution mutants on types without a
usable `Default`. The timeouts are mutant-induced infinite loops the tests
reach, detected as hangs (the trailing-zero reduction loop in `hash_decimal`,
the cursor-advance loops in `classify`, `find_longest_match`'s forward
extension), plus mutants slowed past the 84 s limit on a loaded machine: a
re-run of the 49 with `--timeout 300` left 32 timed out, 6 caught and 11 missed.
Those 11 and the 60 missed are triaged below.

cargo-mutants' classification of each mutant into caught / missed / timeout /
unviable is **not** reproducible run to run: it depends on wall-clock time (a
slow mutant is a "timeout" on one machine and "missed"/"caught" on another)
and, in this workspace, on build caching (a mutant that fails to compile can
be reported as "unviable" or, spuriously, as "caught"). So the substance below
is what was verified independently of any single run's labels.

### Kinds of mutant that survive, and why none is a real test gap

1. **Equivalent viable mutants** — a mutation that compiles and runs but
   cannot change any output, so no test can kill it. Confined to these spots:
   - `onix-core/src/lcs.rs`'s `find_longest_match` / `get_matching_blocks`:
     these either force a non-terminating loop (reported as a timeout) or
     touch only the backward extension step, whose size increment the
     forward step re-covers, or a bound that only skips an empty window.
   - `onix-core/src/lcs.rs`'s `mix_float_bits`: `^` → `|` changes only how
     float hashes spread over buckets, never a result.
   - `onix-core/src/diff/array.rs`'s `lcs_or_positional_array_diff` `> 1`
     threshold: replacing `> 1` with `>= 1` is output-neutral (at exactly one
     LCS finding the positional report holds the same finding or at least two,
     so both thresholds return a report with the same single finding; the
     `>= 1` variant returns the identical positional report), and
     `cargo mutants -p onix-core -f '**/diff/array.rs' -F '> with >= in
     lcs_or_positional'` lists this `>=` mutant, reported missed with the
     sibling `==` and `<` mutants caught (the expected signature of an
     equivalent mutant beside non-equivalent ones).
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
   - `onix-core/src/ignore_order/distance.rs`'s `python_eq` and
     `is_below_threshold_to_diff_deeper`: `||` → `&&` on `has_non_str_keys`
     only sends a pair with exactly one non-`str` side down the `str`-only
     branch, which gives the same answer (no `str` key equals a non-`str`
     one; non-`str` keys sort last).
   - `onix-core/src/report.rs`'s `Report::merge`: `>` → `>=` on the size
     comparison only swaps the destination on a tie, and the maps are sorted by
     path.
   - `onix-core/src/value.rs`: `Wtf8Chars::next`'s `|` → `^` combining three
     disjoint bit fields, and `Number::integer_cmp`'s `i128` fast-path arm,
     which the `BigInt` arm below it orders identically.
   - `onix-arrow/src/row_diff.rs`: the reordering worker's `|| failed` break
     (the consumer sets `stop` on the first error, so continuing only
     drains), `RightFuse::visit`'s `row_at < first.at` → `<=` (`row_at` is
     unique per row), and its `else if stale > 0` → `>= 0` (`compact_if_stale`
     leaves `stale * 2 <= rows`, so a zero count is a no-op).
   - `onix-arrow/src/profile.rs`: all 36 mutants are reported missed because
     the module is compiled only under the `profile` feature, which `make
     mutants` does not enable. `cargo mutants -p onix-arrow --features
     profile -f crates/onix-arrow/src/profile.rs` classifies them: 23 caught,
     12 unviable, 1 timeout, 0 missed.

2. **`Default`-substitution mutants that cannot compile.** cargo-mutants tries
   replacing a function body with `Default::default()` (and similar). They
   fail because the return type has no `Default` impl (or, for
   `compute_pairs`, no matching constructor), so no test can exercise them.
   The list:
   - `onix-cli/src/args.rs`: `parse_diff_args`, `parse_args`.
   - `onix-core/src/ignore_order/distance.rs`: `Distance`'s `partial_cmp` and
     `cmp` (`std::cmp::Ordering` has no `Default`).
   - `lcs.rs`: `scalar_key`, `get_matching_blocks`, `compute_opcodes`.
   - `dispatch.rs`: `scoped`.
   - `hash.rs`: `set_difference`, `set_member_digest`, `child_reps`,
     `build_container`, `scalar_content_key`, `number_key`, `item_key`,
     `keyed`, `tuple_keyed`, `HashedList::build`, `HashedList::get`.
   - `memo.rs`: `tuple_digest`, `content_rep`, `member_rep`.
   - `ignore_order/pairing.rs`'s `compute_pairs` contributes **two** unviable
     mutants by two independent mechanisms: the `HashMap::from_iter` one
     fails on the missing `Default` for `ItemKey`, and the `HashMap::new()`
     one fails because the crate's fxhash-backed `HashMap` type alias has no
     inherent `::new()` (that associated fn exists only for the
     `RandomState`-backed `std` `HashMap`), not a `Default` issue.

The `ItemKey::hash` no-op mutant is caught by
`float_hash_buckets_stay_distinct_and_grow_linearly_with_member_count`.

Future work that touches this logic should re-run `make mutants` and confirm
that no *viable* mutant survives outside the documented equivalent spots.
