# Mutation testing results

The `cargo-mutants` enumeration for `onix-core`, `onix-cli` and `onix-arrow`,
the survivors and why each is a test gap or an equivalent spot, and how to
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
(`cargo mutants --list -p onix-arrow`). The classification below is of that
2145-mutant enumeration, one `make mutants` run with cargo-mutants 27.1.0 from
2026-10-08 to 2026-10-09: 1790 caught, 246 unviable, 49 timeout, 60 missed.

| crate / file | mutants | caught | unviable | timeout | missed |
| --- | ---: | ---: | ---: | ---: | ---: |
| `onix-core` | 1491 | 1313 | 130 | 37 | 11 |
| `onix-cli` | 20 | 14 | 6 | 0 | 0 |
| `onix-arrow` | 634 | 463 | 110 | 12 | 49 |
| &nbsp;&nbsp;`row_diff.rs` | 509 | 407 | 85 | 12 | 5 |
| &nbsp;&nbsp;`schema.rs` | 38 | 26 | 12 | 0 | 0 |
| &nbsp;&nbsp;`profile.rs` | 36 | 0 | 0 | 0 | 36 |
| &nbsp;&nbsp;`table_diff.rs` | 26 | 10 | 8 | 0 | 8 |
| &nbsp;&nbsp;`options.rs` | 9 | 8 | 1 | 0 | 0 |
| &nbsp;&nbsp;`json_rows.rs` | 5 | 5 | 0 | 0 | 0 |
| &nbsp;&nbsp;`lib.rs` | 4 | 3 | 1 | 0 | 0 |
| &nbsp;&nbsp;`error.rs` | 4 | 4 | 0 | 0 | 0 |
| &nbsp;&nbsp;`spool.rs` | 3 | 0 | 3 | 0 | 0 |

The unviable mutants are `Default`-substitution mutants on types without a
usable `Default`. The 49 timeouts, classified again at `--timeout 300`: 32
timeout, 6 caught, 11 missed (the missed are among the survivors below). The 32
timeout at `row_diff.rs` (`hash_decimal`, `classify` x8, `consume_reordered`,
`classify_partition`), `lcs.rs` (`find_longest_match` x5, `get_matching_blocks`
x2), `report.rs` (`Report::merge`), `value.rs` (`Value::eq`, `structural_eq` x5),
`hash.rs` (`DistKey::eq` x2), `memo.rs` (`caching_enabled`, `get`, `put`,
`is_container`) and `pairing.rs` (`compute_pairs`).

cargo-mutants' classification of each mutant into caught / missed / timeout /
unviable is **not** reproducible run to run: it depends on wall-clock time (a
slow mutant is a "timeout" on one machine and "missed"/"caught" on another)
and, in this workspace, on build caching (a mutant that fails to compile can
be reported as "unviable" or, spuriously, as "caught"). So the substance below
is what was verified independently of any single run's labels.

### Survivors: test gaps, equivalent spots, and `profile.rs`

The 60 missed: ten were test gaps, killed by tests added in the same change
(`table_diff.rs`'s row-cap sum x8, `row_diff.rs:1081` x2) and likewise the
`lcs.rs` non-empty-block guards and the two `has_non_str_keys` sites in
`object_diff` and `count_object_diff_leaves`; the 36 in `profile.rs` are not
mutants of compiled code (below); the rest are the equivalent spots.

`profile.rs` is compiled only under the `profile` feature, which `make mutants`
does not enable. `cargo mutants -p onix-arrow --features profile -f
crates/onix-arrow/src/profile.rs` classifies its 36: 23 caught, 12 unviable, 1
timeout, 0 missed.

1. **Equivalent viable mutants** — a mutation that compiles and runs but
   cannot change any output, so no test can kill it. Confined to these spots:
   - `lcs.rs`: `find_longest_match` backward-extension `+=` → `*=` and
     `get_matching_blocks` window bounds; argument at the site.
   - `lcs.rs`: `mix_float_bits`, `^` → `|`; argument at the site.
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
   - `distance.rs`: `python_eq`, `||` → `&&`; argument at the site.
   - `distance.rs`: `is_below_threshold_to_diff_deeper`, `||` → `&&`; argument
     at the site.
   - `report.rs`: `Report::merge`, `>` → `>=`; argument at the site.
   - `value.rs`: `Wtf8Chars::next`, `|` → `^`; argument at the site.
   - `value.rs`: `Number::integer_cmp`, deleted `i128` arm; argument at the site.
   - `row_diff.rs`: the reordering worker's `|| failed` break, `||` → `&&`;
     argument at the site.
   - `row_diff.rs`: `RightFuse::visit`, `row_at < first.at` → `<=`; argument at
     the site.
   - `row_diff.rs`: `RightFuse::visit`, `else if stale > 0` → `>= 0`; argument
     at the site.

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
