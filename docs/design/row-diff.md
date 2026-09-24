# Keyed row diff

Three passes over two Arrow tables matched by key, so the full decoded
tables never sit in memory at once (`crates/onix-arrow/src/row_diff.rs`).

## Algorithm

1. **Hash pass.** Streams each side batch by batch, hashing every
   row's key columns and non-key columns into one 128-bit hash each
   (`hash_cell`). Set arithmetic on the two `(key_hash, row_hash)`
   lists then classifies every key: left-only (removed), right-only
   (added), both with differing row hashes (changed), both equal
   (unchanged, never materialized), or repeated on a side (a
   duplicate key).
2. **Materialize pass.** Re-reads each side, filtering to rows whose
   key landed in the added/removed sets, plus one row per duplicate
   key; a kept selection copies any buffer it shares with its input
   batch (`unshared`).
3. **Cell pass.** Pairs the changed set's rows by key hash and
   compares every common non-key column cell by cell (`diff_cells`).

Single-threaded, every pass re-reads both sides. The parallel path
indexes the left once (`KeyIndex`), reads the right once — tallying
each row against the indexed left instead of keeping its own hashes
(`classify_indexed`) — and spills changed value rows by key-hash
partition to anonymous temporary IPC files (`RightFuse`); the left is
then re-read and materialized in the same pass (`reread_left`), and
the cell pass compares and renders one partition at a time across the
workers (`diff_cells_streaming`), finding each column's changed cells
in one mask (`changed_mask`) that equals the per-cell decision. Every partitioning is by key hash
and every reduction is order-independent or restored to batch order,
so parallel output is byte-identical to the single-threaded path.

## Hashing

Row identity is one keyed 128-bit SipHash-1-3 (`siphasher`), drawn
once per diff from the OS; both sides share the key, so their hashes
are comparable. Because the key is secret and random per diff, the
row-matching table cannot be forced into collisions by chosen input,
and no unkeyed content hash table sits on this default (no-flag)
path. Two distinct keys colliding to the same 128-bit hash, the only
way the diff can misclassify, has probability on the order of
`n² / 2¹²⁸`.

## Value semantics

Cell hashing matches `onix-core`'s scalar comparison except for NaN,
which folds to one canonical form here (unlike `onix-core`, which
refuses NaN at conversion), because the renderer cannot show two NaN
payloads apart: integers and integral floats within `±2⁵³` fold to
one integer form, other floats hash by bit pattern, decimals hash by
exact value with trailing zeros removed, timestamps/times/durations
normalize to nanoseconds, and a null is a distinct value equal only
to another null (`IS DISTINCT FROM`).

## Per-cell changes

`diff_cells` reports one output row per differing cell — the key
columns, `column`, `old_value`, `new_value`, `change` — for every row
whose `hash_cell` contribution differs between the two matched rows.
A cell is `became_null`/`became_non_null` when exactly one side is
null; `type_changed` when both are non-null and not losslessly
comparable — the two sides' `value_domain`s differ, both are
timestamps of differing zone-awareness, or both are intervals of a
different variant; otherwise `value_changed`. A lossless type change
(`Int32`→`Int64`, a float-width change, a time/duration unit change,
a decimal-scale change) hashes equal and reports nothing unless the
value itself also differs.

`old_value`/`new_value` are a canonical string rendering through
`arrow_cast::display`, null for a null cell: numbers of differing
width render at the wider type (an `f32` `0.1` shows as
`0.10000000149011612` against an `f64` `0.1`), an aware timestamp
renders its UTC instant with its zone appended, a decimal renders at
its native scale, a string renders verbatim, a duration renders as an
ISO 8601 `PT<seconds>S` string computed from its raw value (never the
Arrow formatter, whose duration path can emit `<invalid>` while still
succeeding), and a cross-variant interval renders with its variant
appended. A `value_changed` record whose two renderings are
nonetheless equal is `TableDiffError::EqualRenderings`
(`check_distinct_renderings`), never a silent row. There is no typed
old/new column: a long-format table mixes every compared column's
type in one column, so a single typed column cannot represent them
and the string rendering is the uniform form.

## Depth safety

`is_hashable` and `value_domain` recurse through a dictionary value
type; both are bounded by the `MAX_NESTING_DEPTH` check
`diff_schemas` runs before any row is read, so `hash_cell` itself
never recurses.
