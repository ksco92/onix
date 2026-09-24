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
depths add: at most `max_depth` levels combined, never two
independent `max_depth` budgets. A flat `max_depth` for the value
regardless of `d` would let both together reach roughly `2 *
max_depth` levels; the shared budget instead bounds the combined
depth by `max_depth`.

## Equal inputs of any depth

Before recursing, the whole `a`/`b` pair is compared once with an
iterative, heap-stack equality check: two fully-equal inputs diff to
an empty `Report` at any depth, regardless of `max_depth`. This runs
once, at the top, not per key, so an equal subtree under an unrelated
shallower difference can still trip `MaxDepthExceeded`.

## Safety contract

The traversal recurses at most `max_depth` levels; each level costs a
small constant number of native frames (`diff_at` plus one dispatch
function, `object_diff` or `array_diff`), so the worst case is
roughly `2 * max_depth` frames — about 4,000 bytes/level for nested
lists in a debug build (the measured worst-case shape), per
`crates/onix-core/examples/stack_frame_cost.rs`; `guard.rs` (in
`onix-py`) rounds this up to size its worker stack. Every site that
clones a whole value into a `Report` calls
`check_value_depth`/`check_map_depth` first, so no value over the
combined budget is ever cloned; `Value`'s `Clone` then recurses no
deeper than that budget, and `Report::to_json_value` renders those
same already-bounded values through `Value::to_serde_json`'s own
unguarded recursion safely for the same reason. `Value`'s `Drop` is
iterative at any depth and outside this budget entirely.
`ignore_order`'s hashing and distance fallback recurse natively too,
checked against this same budget before they run — see
`docs/design/ignore-order.md`'s "Depth safety" section. The worst
case on adversarial input is a clean `MaxDepthExceeded`, never a
stack overflow.
