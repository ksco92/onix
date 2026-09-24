# Value model

## Stack safety

`Value` nests through boxed slices (`Array`/`Tuple`), `SetItems`
(`Set`/`FrozenSet`), and `Object`'s entries. `Drop` and structural
`PartialEq` are iterative, an explicit heap work-stack, so teardown
and deep comparison use `O(1)` native stack regardless of nesting
depth. `SetItems::new`'s canonical ordering (`canonical_cmp`) is
iterative for the same reason: a set is built during conversion,
before any depth guard has run. `Debug` and `Clone` stay recursive:
`Debug` is debug/test-only, and the diff engine only ever clones a
value already validated by the combined path-plus-value depth guard
in `crate::diff`, so `Clone` recursion is bounded by `max_depth`. A
caller that clones untrusted input outside that guard rejects
over-deep input first with `crate::exceeds_depth`.

## Subclasses

Every `Value` variant that can carry a subclass name wraps its
payload in `Typed`, except `SetItems`/`Object`, which carry an
equivalent `type_name` field instead. A subclass instance keeps its
source class name for a `type_changes` finding, while comparing,
hashing, and rendering exactly like its base type everywhere else;
`diff_at` (`crate::diff::dispatch`) is the one place that reads the
class name before recursing.

## Calendar types

`Date` (`datetime.date`) and `DateTime` (`datetime.datetime`, naive
or fixed UTC offset) hold plain wall-clock fields; the only
arithmetic is civil-date/day-number conversion (Howard Hinnant's
`days_from_civil`/`civil_from_days`). `DateTime` compares by instant:
an aware value normalizes through its own offset, a naive value is
stamped as UTC, matching `datetime_normalize`. A `Date` never equals
a `DateTime`. `Date`, `DateTime`, and `Time` render through
`isoformat()` (microseconds only when non-zero, an offset suffix
only when the value is aware); `TimeDelta` renders through
`str(timedelta)`, since `DeepDiff` has no `isoformat()` for it.

`Time` (`datetime.time`) shares `DateTime`'s wall-clock/offset fields
minus the calendar date; unlike `DateTime`, its equality is a plain
`!=` with no naive-as-UTC normalization — a naive value never equals
an aware one, and two aware values compare by an offset-adjusted
micros-of-day.

`TimeDelta` (`datetime.timedelta`) stores Python's own normalized
`(days, seconds, microseconds)` triple rather than a flattened
microsecond count, which would overflow `i64` at Python's extreme
`days=999_999_999`; it always compares and hashes by that exact
value, with no naive/aware split.

Under `ignore_order`, `Date`/`DateTime`/`TimeDelta` hash exactly, but
`Time` hashes by `(hour*60+minute)*60+second`, dropping the
microsecond and any offset, matching `DeepHash`'s own truncation — so
two times equal only to the second hash-match under `ignore_order`
even when plain `==` calls them different.
