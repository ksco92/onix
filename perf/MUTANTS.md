# Mutation testing results

Summary of a full `cargo-mutants` run over `onix-core`, `onix-cli`, and
`onix-arrow`. Line coverage proves every line ran; mutation testing proves a
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
`json_rows.rs`, 4 in `lib.rs`, 4 in `error.rs`, 3 in `spool.rs`). The current
enumeration has not been classified, see #212. A standalone `cargo mutants -p
onix-arrow` on a quiet machine, run on the earlier 274-mutant `onix-arrow`
enumeration (208 in `row_diff.rs`, 38 in `schema.rs`, 17 in `table_diff.rs`, 4
in `lib.rs`, 4 in `error.rs`, 3 in `options.rs`), classified **212 caught, 52
unviable, 9 timeout, 1 missed**. The 52 unviable are `Default`-substitution
mutants on types without a usable `Default`. The 9 timeouts are
mutant-induced infinite loops the tests reach — the trailing-zero reduction
loop in `hash_decimal` (`==`/`/=` mutants) and the two cursor-advance loops in `classify` (the `<`/`==`/`+=`
mutants) — detected as hangs, not silent survivors. The 1 missed is a genuine equivalent
mutant: `row_diff.rs`'s `push_filtered` (the shared filter-and-push helper of
both the added/removed and the per-cell materialize passes) guards
`if selected.num_rows() > 0` before pushing a batch to `concat_batches`, and
`> 0 -> >= 0` only adds empty batches, which `concat_batches` ignores, so the
output is identical.

cargo-mutants' classification of each mutant into caught / missed / timeout /
unviable is **not** reproducible run to run: it depends on wall-clock time (a
slow mutant is a "timeout" on one machine and "missed"/"caught" on another)
and, in this workspace, on build caching (a mutant that fails to compile can
be reported as "unviable" or, spuriously, as "caught"). So the substance below
is what was verified independently of any single run's labels.

### Kinds of mutant that survive, and why none is a real test gap

1. **Equivalent viable mutants** — a mutation that compiles and runs but
   cannot change any output, so no test can kill it. Confined to these spots,
   each with the argument written at the source:
   - `onix-core/src/lcs.rs`'s `find_longest_match` / `get_matching_blocks`:
     these either force a non-terminating loop (reported as a timeout) or
     produce a wrong-but-terminating result the surrounding comments prove is
     equivalent or non-actionable.
   - `onix-core/src/diff/array.rs`'s `lcs_or_positional_array_diff` `> 1`
     threshold: replacing `> 1` with `>= 1` is verified output-neutral, and
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
   - `onix-core/src/ignore_order/memo.rs`'s `is_container`, mutated to always
     return `true`: it only gates whether a candidate pair's distance is
     *cached*, never what value is computed. Scalar pairs are then cached too,
     which costs a clone and a hashmap round trip each and cannot change a
     result.

2. **`Default`-substitution mutants that cannot compile.** cargo-mutants tries
   replacing a function body with `Default::default()` (and similar). Most
   fail because the return type has no `Default` impl, so the mutant does not
   compile; none can be exercised by a test. The list:
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

Every other viable mutant is caught. `onix-cli`'s only non-caught mutants are
the two uncompilable `Default`-substitutions above; every viable `onix-cli`
mutant is caught. The `ItemKey::hash` no-op mutant is caught by
`float_hash_buckets_stay_distinct_and_grow_linearly_with_member_count`.

Future work that touches this logic should re-run `make mutants` and confirm
that no *viable* mutant survives outside the five documented equivalent spots.
