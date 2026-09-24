# `ignore_order` list matching

`ignore_order_array_diff` matches two lists' elements independent of
position, for one list level (`a` = old, `b` = new).

## Hash

Each item is reduced to a canonical equivalence key (type-tagged numbers,
order/count-insensitive nested containers). Each list keeps only its
distinct keys, in first-occurrence order; a repeated value's other
occurrences are invisible in the output. A key present on both sides is
matched and never revisited.

## Pair

The added/removed key sets are the keys present on only one side, each
preserving that side's first-occurrence order. Pairing is gated:
`(added + removed) / (distinct_a + distinct_b + 1) > 0.7` disables
pairing and falls back to raw per-hash add/remove; the denominator counts
distinct hashes, not raw list length. When pairing is engaged, matching
is greedy and not globally optimal: it resolves candidate pairs in ranked
order without backtracking.

## Distance

Distance ranks candidate pairs during pairing; it is a structural/numeric
measure between two values, never an equality check. A container
candidate pair's distance goes through a shared memo keyed by structural
identity, so a distinct container pair's distance is computed once
regardless of how many candidates embed it.

## Walk

Walking `hashes_added` in order, then remaining `hashes_removed`: a
paired added key gets a real recursive diff against its removed partner,
keyed at the removed side's index; an unpaired added key is a plain add
at its own index; an unpaired removed key left over after pairing is a
plain remove at its own index.

## Depth safety

Item hashing and the distance fallback both recurse natively, unlike the
rest of this crate's traversal. Every item is validated against the
shared traversal/value depth budget before any hashing happens.
