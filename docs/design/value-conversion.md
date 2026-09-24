# Value conversion

## Subclasses

`to_value` classifies a Python object by its exact type first, falling
through to a non-exact `isinstance`-style cast only on the exact match's
failure. `datetime`/`date`/`time`/`timedelta` share this order;
`datetime` (exact and subclass) runs before `date`, since every
`datetime` is also a `date`, or checking `date` first would swallow
every `datetime` too.

Only the source class *name* survives conversion, never the class
object, so a subclass instance (`list`/`tuple`/`set`/`frozenset`/`dict`,
or a `datetime`/`date`/`time`/`timedelta` subclass) cannot round-trip
through `DeepDiff.to_dict()` as itself: it renders back as the plain
base type its fields describe. A `zoneinfo`/`pytz` `tzinfo` on a
`datetime` is the same simplification, one level down — see
`tests/golden/README.md`'s "Normalized versus raw datetimes" section,
its "Fixed-offset `tzinfo` round-trip" point.

## Key interning

Converting one side of a diff threads one `onix_core::value::Builder`
through the whole walk, so a `str` object key repeated across many
objects — record-shaped data commonly repeats a handful of keys
thousands of times — costs one shared `Arc<str>` allocation rather
than one per occurrence. Each side of a diff gets its own `Builder`,
so interning is per-side, never shared across `t1`/`t2`. A key
holding a lone surrogate code point is never interned — see
`onix_core::value::Key`'s own doc for why that shape isn't worth
sharing.
