# Depth-budget proof

`diff_at` bounds path depth and cloned-value depth against one shared
`max_depth` budget, never two independent ones.

## Depth and value budget

The root pair is depth `0`; each recursed key or leaf finding adds one
to its child's depth, and `check_traversal_depth` rejects a path
depth past `max_depth`. At a finding whose path sits at depth `d`, its
cloned value (measured standalone, root at depth `0`) may nest no
deeper than `max_depth - d`; `check_value_depth`/`check_map_depth`
enforce it via `deeper_than`/`map_deeper_than`, both iterative.

## Why shared, not doubled

The traversal reaching a finding and the native `Clone` recording its
value run on one call stack with no unwinding between, so their
frames add. A flat `max_depth` for the value regardless of `d` would
let both together cost roughly `2 * max_depth` frames; the shared
budget bounds combined native stack by `max_depth` instead.

## Equal inputs of any depth

Before recursing, the whole `a`/`b` pair is compared once with an
iterative, heap-stack equality check: two fully-equal inputs diff to
an empty `Report` at any depth, regardless of `max_depth`. This runs
once, at the top, not per key, so an equal subtree under an unrelated
shallower difference can still trip `MaxDepthExceeded`.

## Safety contract

No native recursion this budget guards — traversal, `Value`'s
`Clone`, `Report`'s `Drop`, `Report::to_json_value` — exceeds
`O(max_depth)` frames; worst case is a clean `MaxDepthExceeded`, never
a stack overflow. `ignore_order`'s hashing and distance fallback
recurse natively too, checked against this same budget first — see
`docs/design/ignore-order.md`'s "Depth safety" section.
