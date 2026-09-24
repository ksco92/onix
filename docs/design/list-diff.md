# List diffing: scalar-list LCS matching

`array_diff` chooses between plain index-aligned comparison and an
LCS/`difflib`-style match for one pair of lists.

## Condition and candidate selection

The LCS path applies only when every element of both lists is a JSON
scalar (null, bool, number, string); a dict or a nested list anywhere in
either list disqualifies the whole comparison back to index-aligned. When
it applies, compute the LCS match first: at most one finding, use it as
is; otherwise also compute the index-aligned result and keep whichever
has fewer total findings, favoring index-aligned on an exact tie.

## Opcode-to-finding mapping

- An `equal` opcode block is never diffed further, not even for a type
  check.
- A `delete` opcode reports each element as `iterable_item_removed` keyed
  by its old-side index.
- An `insert` opcode reports each element as `iterable_item_added` keyed
  by its new-side index.
- A `replace` opcode pairs its two ranges position by position: a shared
  position becomes `values_changed`/`type_changes` keyed by the old-side
  index; a position present on only one side becomes an added/removed
  finding on that side's own index. The two ranges never share a matching
  element pair, so every paired position is guaranteed to differ.

## Matching rules

- `autojunk` is disabled: no popular-element exclusion applies.
- The LCS match's own equality is Python's `==`: `1 == 1.0 == True` and
  `0 == 0.0 == False` compare equal regardless of type, so an `equal`
  opcode can hide a type difference every other comparison in this engine
  would report.
- A finding's path renders from the old-side index by default; a
  `new_path` is added when the new-side index differs, which only happens
  inside a `replace` opcode once an earlier insert/delete has shifted the
  offsets.
- `iterable_item_moved` is unreachable: it would require a `replace`
  opcode's two ranges to share a matching pair, which never happens.
