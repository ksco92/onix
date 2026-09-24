# `ignore_order` list matching

`ignore_order_array_diff` matches two lists' elements independent of position, for one
list level (`a` = old, `b` = new).

## Hash

Each item reduces to a canonical equivalence key: type-tagged scalars,
order/count-insensitive nested containers, a custom object by class identity plus its
attributes, and an unconvertible value or class attribute by object identity (`keyed`).
Each list keeps only its distinct keys, first-occurrence order; a repeated value's other
occurrences are invisible in the output, and a matched key is never revisited.

## Pair, distance and walk

The added/removed key sets are the keys present on only one side, each preserving that
side's first-occurrence order. Pairing is gated:
`(added + removed) / (distinct_a + distinct_b + 1) > 0.7`, counting distinct hashes, not
raw list length; over the gate, pairing is skipped for raw per-hash add/remove. When
engaged, matching is greedy and not globally optimal: it ranks candidates by a
structural/numeric distance (never equality, memoized by structural identity so a
distinct container pair costs one trial regardless of how many candidates embed it) and
resolves them without backtracking. Walking `hashes_added` then remaining
`hashes_removed`: a paired key gets a real recursive diff against its partner, keyed at
the removed side's index; an unpaired key is a plain add/remove at its own index.

## Bounds

Pairing is `O(N²)` in unpaired elements per list level, with no `max_passes`/
`max_diffs` cutoff. The distance memo removes the exponential blowup across nesting
levels but leaves a polynomial cost in depth, in both time and memory (one memo entry
per distinct container pair): a few-KB input nested many hundreds of levels deep still
costs seconds and hundreds of MB, all under the default `max_depth`.

## Depth safety

Item hashing and the distance fallback recurse natively, unlike the rest of this
crate's traversal; every item is validated against the shared depth budget before
hashing. The distance fallback's nested-array trial restarts at depth 0 with the
remaining budget as its own `max_depth` (see `rough_distance`'s doc).
