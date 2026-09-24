# `ignore_order` list matching

`ignore_order_array_diff` matches two lists' elements independent of
position, for one list level (`a` = old, `b` = new).

## Hash

Each item reduces to a canonical equivalence key: type-tagged scalars,
order/count-insensitive nested containers, a custom object by class
name plus its attributes, and an unconvertible value or class
attribute by object identity (`keyed`). Each list keeps only its
distinct keys, first-occurrence order; a repeated value's other
occurrences are invisible in the output, and a matched key is never
revisited.

## Pair

The added/removed key sets are the keys present on only one side, each
preserving that side's first-occurrence order. Pairing is gated:
`(added + removed) / (distinct_a + distinct_b + 1) > 0.7`, counting
distinct hashes, not raw list length; over the gate, pairing is skipped
for raw per-hash add/remove. When engaged, matching is greedy and not
globally optimal: it resolves candidates in ranked distance order
without backtracking.

## Distance

Distance ranks candidate pairs; it is a structural/numeric measure
between two values, never an equality check. A container candidate
pair's distance is memoized by structural identity, so a distinct
container pair costs one trial regardless of how many candidates
embed it.

## Walk

Walking `hashes_added` in order, then remaining `hashes_removed`: a
paired key gets a real recursive diff against its partner, keyed at
the removed side's index; an unpaired key is a plain add/remove at
its own index.

## Bounds

Pairing is `O(N²)` in unpaired elements per list level, with no
`max_passes`/`max_diffs` cutoff. The distance memo turns the
exponential cost across nesting levels into a polynomial one in time
and memory, with one entry per distinct container pair.

## Depth safety

Item hashing and the distance fallback recurse natively, unlike the
rest of this crate's traversal; every item is validated against the
shared depth budget before hashing. The distance fallback's
nested-array trial restarts at depth 0 with the remaining budget as
its own `max_depth` (see `rough_distance`'s doc).
