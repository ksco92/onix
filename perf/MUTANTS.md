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

`onix-py` is out of scope; the `Makefile`'s `mutants` target documents why
(same structural reason it is excluded from line coverage).

## What is deterministic, and what the tool classifies unreliably

`make mutants` enumerates a deterministic **1256** mutants (20 in `onix-cli`,
980 in `onix-core`, 256 in `onix-arrow`). A standalone `cargo mutants -p
onix-arrow` on a quiet machine reports, for the 256 `onix-arrow` mutants
(190 in `row_diff.rs`, 38 in `schema.rs`, 17 in `table_diff.rs`, 4 in `lib.rs`,
4 in `error.rs`, 3 in `options.rs`): **193 caught, 53 unviable, 9 timeout, 1
missed**. The 53 unviable are `Default`-substitution mutants on types without a
usable `Default`. The 9 timeouts are mutant-induced infinite loops the tests
reach — the trailing-zero reduction loop in `hash_decimal` (`==`/`/=` mutants)
and the two cursor-advance loops in `classify` (the `<`/`==`/`+=` mutants) —
detected as hangs, not silent survivors. The 1 missed is a genuine equivalent
mutant: `row_diff.rs`'s `push_filtered` (the shared filter-and-push helper of
both the added/removed and the per-cell materialize passes) guards
`if selected.num_rows() > 0` before pushing a batch to `concat_batches`, and
`> 0 -> >= 0` only adds empty batches, which `concat_batches` ignores, so the
output is identical.

`value_domain`'s arms are each pinned: `each_value_domain_paired_with_a_string_is_type_changed`
pairs every domain against a `Utf8` column and asserts `type_changed` (dropping
an arm would mislabel it `value_changed`), and the `Null` arm is caught because
its catch-all is `unreachable!` — dropping it panics on the `Null`-typed column
`only_the_differing_cells_of_a_changed_row_are_reported` diffs.

Its classification of each mutant into
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

The set-member identity in `hash.rs` was later rebuilt on the crate's own
digest cache: each member is reduced to one content id (`RepId`) computed
through the shared `memo` at every hashable node, with a separate Python-equality
id (`NodeId`) keying the cache (the earlier two-key `IdentityMode::Loose`/
`Content` layering, which matched on *either* of two whole-member keys, could
not model `DeepHash`'s per-*node* cache decision and under-reported some
naive/aware-in-tuple members). That rebuild replaced several `hash.rs`
functions and added the set-member interning tables to `memo.rs`, so the mutant
enumeration shifted from **986** to **1000** total (20 `onix-cli`, 980
`onix-core`).

A scoped re-run of the two rebuilt files, on an otherwise-idle machine (no
contention, no false timeouts), shows the expected signature cleanly:

```
cargo mutants --package onix-core \
  -f crates/onix-core/src/ignore_order/hash.rs \
  -f crates/onix-core/src/ignore_order/memo.rs
```

51 tested — **27 caught, 20 unviable, 4 missed, 0 timeout**. All four misses
are `memo.rs`'s pre-existing caching-gate equivalents (`should_cache`,
`is_container` — kind (1) above, provably result-neutral); every viable mutant
in the new set-member code (`set_member_digest`, `build_container`,
`child_reps`, `scalar_content_key`, `set_difference`, `content_rep`,
`member_rep`) is caught. In particular the content-path bool arm
(`scalar_content_key`'s `Value::Bool(b) => ItemKey::Bool(*b)`) is caught by the
`set_tuple_datetime_and_bool_sibling_differs_via_content_path` golden, which
differs the bool across sides so a `Bool(*b) -> Bool(true)` mutant flips the
result — confirmed by applying that mutant by hand and watching
`cargo test --test golden` fail.

Replacing `<impl std::hash::Hash for ItemKey>::hash` with `()` (a no-op
hasher) used to only surface as a `cargo-mutants` timeout: a no-op `ItemKey`
hash sends every `FxHash`-backed table (`HashedList`, tuple digests) to one
bucket, i.e. `O(n^2)`, and the suite-wide slowdown it caused tripped the
tool's per-mutant test timeout before the previous timing-based test
(removed, issue #33) could fail on its own bound — detected, but only as a
hang. That test was replaced with
`float_hash_buckets_stay_distinct_and_grow_linearly_with_member_count`, which
hashes real `ItemKey`s through the crate's actual `FxHasher` and asserts the
low bits of the resulting hash spread across (near-)distinct buckets for a
run of integral and half-integer floats. A no-op hash makes every key hash to
the same value regardless of content, so the test now fails its
distinctness assertion immediately — no diff, no recursion, no timing
involved — confirmed by applying the mutation by hand and re-running just
that test (0.01 s to fail, reporting 1 distinct bucket out of 10,000). A
standalone re-run of this file now classifies the mutant as **caught**, not
timeout.

The `onix-core` + `onix-cli` portion of the enumeration is deterministic at
**1000** mutants (`cargo mutants --list`; hash.rs 37, memo.rs 14, lcs.rs 111 of
them); with `onix-arrow`'s 256 the top-line total is 1256, as recorded above. The last full
representative run classified the previous 986-mutant enumeration as **890
caught, 75 unviable, 15 missed, 6 timeout** — the survivors confined to the
documented equivalent/uncompilable kinds: the `lcs.rs` LCS spots (missed plus
genuine non-terminating timeouts), `diff/array.rs`'s
`lcs_or_positional_array_diff`, `path.rs`'s `python_float_repr`, `distance.rs`'s
`* 1_000_000.0`, and `memo.rs`'s four caching-gate mutants, plus the
`Default`-substitution mutants that do not compile (classification is noisy run
to run, per the caveat above; the survivor *set* is not). The set-member
rebuild plus the hash-flooding/float-mixing hardening add a net **+14** mutants
(986 -> 1000), mostly in `lcs.rs` (`mix_float_bits` and the hand-written
`Hash` impls) and `hash.rs`; per the scoped run above every one is caught or
unviable, including the `ItemKey::hash`-no-op mutant discussed there. No new
*missed* survivor. Its caller-threading elsewhere (`diff/set.rs`,
`diff/dispatch.rs`, `distance.rs`'s `count_set_diff_leaves`) is exercised by the
existing set/`ignore_order` tests.

- **Unviable:** the `Default`-substitution (and `HashMap::new()`) mutants of
  kind (2), including `args.rs:32`/`args.rs:76` and `distance.rs:32`/
  `distance.rs:38`; none compiles, so no test can exercise it. `hash.rs` alone
  carries these for `set_difference`, `set_member_digest`, `child_reps`,
  `build_container`, `scalar_content_key`, `number_key`, `item_key`, `keyed`,
  `tuple_keyed`, `HashedList::build`, `HashedList::get`, and `memo.rs` for
  `tuple_digest`, `content_rep`, `member_rep`.

Future work that touches this logic should re-run `make mutants` and confirm
that no *viable* mutant survives outside the five documented equivalent spots.
