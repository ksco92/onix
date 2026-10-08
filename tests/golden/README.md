# Golden corpus

This directory is the compatibility test corpus: `onix`'s report is
byte-identical (canonical JSON) to `DeepDiff`'s `to_json()` output at
`verbose_level=2`, on a hand-designed corpus of small, diverse cases.

## Layout

Each subdirectory is one case:

```
tests/golden/<case_name>/
├── a.json         # t1, as fed to both DeepDiff and onix
├── b.json         # t2
├── expected.json  # DeepDiff(t1, t2, verbose_level=2, **kwargs) rendered
│                  # through golden_tags.canonical_report (see "JSON
│                  # supersets"), re-dumped with sort_keys=True
└── options.json   # {"ignore_order": bool}: DiffOptions for the case;
                   # kwargs above mirrors it (currently the only option
                   # this corpus varies)
```

`crates/onix-core/tests/golden.rs` reads every case directory present here
(there is no separate hand-maintained case list), runs
`onix_core::diff_with_options` with each case's own `options.json`, and asserts
the resulting report's canonical JSON (parsed `serde_json::Value`, so object key
order doesn't matter — array order and values do) equals `expected.json`
exactly, except the cases documented below. It runs as part of the normal
`cargo test` / `make check`.

## Values JSON cannot express: the tagged encoding

DeepDiff diffs Python objects, and several of the types it handles have no JSON
literal. A case that needs one writes it as a **tagged object**: a JSON object
with **exactly one** key, and that key one of the reserved names below.

| Tag | Decodes to | Status |
| --- | --- | --- |
| `$tuple` | `tuple` | supported |
| `$set` | `set` | supported |
| `$frozenset` | `frozenset` | supported |
| `$datetime` | `datetime.datetime` | supported (ISO 8601 string, offset optional) |
| `$date` | `datetime.date` | supported (ISO 8601 string) |
| `$time` | `datetime.time` | supported (ISO 8601 string, offset optional) |
| `$timedelta` | `datetime.timedelta` | supported (`{"days": D, "seconds": S, "microseconds": U}`) |
| `$dict` | `dict` with a non-`str` key | supported (list of `[key, value]` pairs) |
| `$bigint` | `int` beyond `i64`/`u64` | supported (exact decimal digits as a string) |
| `$object` | a custom object | supported (`{"class": "<name>", "attrs": {…}}`) |

So `{"$tuple": [1, 2]}` is the tuple `(1, 2)`, `{"$set": [1, 2]}` is the set
`{1, 2}`, `[{"$tuple": []}]` is a list holding the empty tuple,
`{"$datetime": "2024-01-01T10:00:00+02:00"}` is that aware datetime,
`{"$date": "2024-01-01"}` is that date and `{"$time": "10:00:00+02:00"}` is that
aware time. The three calendar-string tags carry what `isoformat()` produces and
`fromisoformat()` reads back, with the UTC offset present only for an aware
value. `$timedelta` carries Python's own already-normalized
`(days, seconds, microseconds)` triple as an object instead of one ISO string,
because a single flattened microsecond count overflows even a 64-bit integer at
Python's own extreme `days=999_999_999` (see `onix_core::datetime::TimeDelta`'s
own doc). `$dict` is the one tag whose payload is a list of pairs rather than a
list of items or a bare string: a plain JSON object can only ever have `str`
keys, so a `dict` with any other key kind (`int`, `bool`, `float`, `None`,
`datetime`, `date`, or a `tuple` of those) has no other shape to write it in —
`{"$dict": [[1, "x"], [true, "y"]]}` is `{1: "x", True: "y"}`. Each pair's key
is itself tagged where its type needs it (a `$tuple`/`$datetime`/`$date` key
encodes like a value of that type would). **Any other object is plain data**,
including one that has a reserved key alongside others
(`{"$tuple": [1], "x": 2}` is a two-key dict). The reserved names are claimed
all at once, before their types are supported, so a fixture can never use one as
an ordinary dict key and then change meaning later; a decoder that meets a tag
it cannot decode yet fails loudly.

`$bigint` is the one tag for a value JSON *can* express:
`{"$bigint": "1267650600228229401496703205376"}` is `2**100`. An
arbitrary-precision integer has a JSON number literal, but this corpus's Rust
reader parses a number back through `serde_json` without its
`arbitrary_precision` feature, which collapses any integer beyond `i64`/`u64` to
the nearest `f64` — so an untagged big integer in an input file would decode
to a float, not the integer the case means. Tagging it as its exact decimal
digits keeps the input an integer for both readers. An in-range integer stays a
plain JSON number, unchanged. This is the same representation gap onix's own
value model closes (`onix_core::value::Number`'s arbitrary-precision arm); the
JSON *text* readers (`diff_json`, the CLI) share `serde_json`'s limitation and
parse such an integer as a float, so two documents whose integers differ only
past `i64`/`u64` compare equal and diff to `{}` there — stated in the README's
Known limitations and tracked in issue #92.

Because a big integer in a **report value** (a `values_changed`/`type_changes`
`old_value`/`new_value`) has the same `serde_json` gap, the golden test compares
one at `f64` resolution and the diff structure exactly; see
`collapse_bigint_tags` in `crates/onix-core/tests/golden.rs`.

The one cost of the encoding is that a dict whose *only* key is a reserved name
cannot be a fixture value. `scripts/golden_tags.py`'s `encode_tags` refuses to
write such a value rather than writing a file that would decode back into
something else.

Two implementations of the rule, one per language, cover the corpus's three
readers:

- `scripts/golden_tags.py` — the definition, used both by the generator (which
  also reads every file it writes back and checks it against the case it came
  from) and by `crates/onix-py/tests/test_golden_parity.py`.
- `crates/onix-core/tests/golden.rs` — the Rust decoder, building the engine's
  own value model.

**The product never interprets a tag.** `onix_core::Value`'s `Deserialize`,
`deepdiff_rs.diff_json`, the `DeepDiff` class, and the CLI all read
`{"$tuple": [1]}` or `{"$datetime": "2024-01-01T00:00:00"}` as the one-key dict
it is; each of those paths has a test for that.

## JSON supersets

DeepDiff's `serialization.JSON_CONVERTOR` maps `datetime.datetime` to
`isoformat()` and has **no entry for `datetime.date`**, so its own `to_json()`
raises `TypeError` on any report carrying a bare date. onix renders one as
`YYYY-MM-DD` — the same bytes `date.isoformat()` gives — a superset of
DeepDiff's output, not a divergence: passing
`default_mapping={datetime.date: datetime.date.isoformat}` to DeepDiff's own
`to_json()` makes it produce byte-identical output. That mapping is
`scripts/golden_tags.py`'s `JSON_DEFAULT_MAPPING`, which carries the `date`,
`time` and `timedelta` mappings, `canonical_report` passes when it renders each
`expected.json`, and which `crates/onix-py/tests/test_golden_parity.py` shares.
So a date-carrying golden case still has DeepDiff output as its spec.
`canonical_report` also emits set-derived values in onix's canonical order (the
"Canonical set order" point of "Set iteration order").

The same gap, for the same reason: `JSON_CONVERTOR` has no entry for
`datetime.time` or `datetime.timedelta` either, so DeepDiff's stock `to_json()`
raises `TypeError` on a report carrying either. onix renders a `time` as
`time.isoformat()`'s own bytes (the same shape a `datetime`'s own time portion
takes) and a `timedelta` as `str(timedelta)`'s — there being no
`timedelta.isoformat()` to mirror instead, `str()` is the natural, deterministic
choice.

**`frozenset` values are a superset, not a difference.** `DeepDiff`'s own
`to_json()` raises `TypeError` on a report holding a `frozenset` (e.g. the
`new_value` of a set-vs-frozenset `type_changes`); `onix` serializes it as an
array. There is no golden case: the corpus's `expected.json` cannot be generated
for it, so `test_onix_serializes_a_frozenset_where_real_deepdiff_refuses` in
`test_sets.py` asserts the behaviour.

The tests for the calendar types are `crates/onix-py/tests/test_datetimes.py`,
`test_times.py` and `test_timedeltas.py`; they compare against DeepDiff's
`to_dict()` and assert that DeepDiff's stock `to_json()` still raises for these
types.

## Pinned versions

- **Python:** 3.14 (installed on demand by `uv`, per `scripts/gen_goldens.py`'s
  inline script metadata) — pinned exactly, not just a floor: a case holding a
  `str` nested inside a tuple/frozenset set item is rendered by DeepDiff's own
  `repr()`, which escapes against *this interpreter's* Unicode table, so
  generating on any other version could bake a stale classification into the
  committed spec (see the Unicode entry below; `main()` asserts the running
  interpreter's `unicodedata.unidata_version` before writing anything).
- **deepdiff:** `9.1.0` exactly (pinned in `scripts/gen_goldens.py`'s inline
  script metadata, resolved from PyPI's latest `8.x`+ line; see that file's
  `# /// script` header)
- **Unicode:** 16.0.0 via the `unicode-general-category` crate, matching CPython
  3.14's `unicodedata` table; an older CPython escapes code points assigned
  after its own Unicode version where onix renders them literally (the `str`
  escaping in `onix_core::path` — see `crates/onix-py/tests/test_sets.py`'s
  BMP differential test).

## Regenerating

```sh
uv run scripts/gen_goldens.py
```

This is the **only** source of `expected.json` (and, for full reproducibility,
`a.json`/`b.json` too) — never hand-edit any file in this directory. Every
case is defined in `scripts/gen_goldens.py`'s `CASES` dict; add a case there and
re-run to add a new golden. The script overwrites existing case directories in
place, so a clean re-run should produce no `git diff` unless `CASES` or the
pinned `deepdiff` version changed.

## Case coverage

`scripts/gen_goldens.py`'s `CASES` is the case list; a fuzz batch is built by
the generator function its row names, and `_TIME_FUZZ_CASE_COUNT` there sets the
size of the time/`timedelta` batches. Spec pages are under `docs/design/`;
`scripts/differential_fuzz.py` is a separate, larger-scale fuzzer, not part of
this corpus.

| Glob | Pins | Spec page |
| --- | --- | --- |
| `values_changed_*`, `float_change`, `null_vs_value`, `large_integer_*` | scalar `values_changed` | — |
| `type_change_*` | `type_changes` pairings (int/float/bool/`None`/dict/list), root and depth | — |
| `dictionary_*` | `dictionary_item_added`/`removed`, together at depth, to/from an empty dict | — |
| `iterable_item_*`, `same_length_*` | `iterable_item_added`/`removed`: from/to empty, tail growth/shrink, same-length change | — |
| `nested_*` | dict-in-dict, list-in-list, dict-in-list, list-in-dict | — |
| `key_*`, `unicode_key` | key quoting: quotes, backslash, control characters, unicode | — |
| `path_rendering_collision` | the one path-rendering collision case | — |
| `dict_key_*` | non-`str` dict keys: added, removed, changed, matched, nested | — |
| `threshold_collapse_*` | `threshold_to_diff_deeper=0.33` dict-vs-dict collapse and its boundary | — |
| `big_int_*`, `negative_big_int_*`, `list_big_int_*` | integers beyond `i64`/`u64` compared and rendered by exact value | — |
| `list_lcs_*` | `difflib`-style scalar-list matching: reorder, shift, repeats, `new_path` | `list-diff.md` |
| `list_lcs_fuzz_*` | seeded random list-LCS cases (`_generate_fuzz_cases`) | `list-diff.md` |
| `tuple_*`, `ignore_order_tuple_*`, `ignore_order_unhashable_tuple_*` | positional diff, tuple versus list, hash pairing | `ignore-order.md` |
| `set_*`, `frozenset_*`, `ignore_order_set_*`, `ignore_order_unhashable_set_*` | `set_item_added`/`removed`, item rendering, set versus other containers | `value-model.md` |
| `datetime_*`, `date_*`, `list_lcs_datetime_*`, `ignore_order_date*` | instant comparison, UTC-normalized `values_changed`, `isoformat()` boundaries | `value-model.md` |
| `time_*`, `timedelta_*`, `list_lcs_time*`, `ignore_order_time*`, `set_time*` | raw `values_changed`, naive versus aware, seeded fuzz batches | `value-model.md` |
| `multiline_string_*`, `singleline_string_*`, `ignore_order_multiline_string_*` | the `diff` field `_diff_str` adds at `verbose_level=2` | — |
| `object_*`, `ignore_order_object_*` | custom objects by attributes, `Enum` members | `value-conversion.md` |
| `ignore_order_*` | shuffle, multiplicity, nested pairing, thresholds, tie-breaks | `ignore-order.md` |
| `ignore_order_fuzz_*` | seeded random cases (`_generate_ignore_order_fuzz_cases`) | `ignore-order.md` |
| `combined_*` | several value kinds in one report | — |

## Normalized versus raw datetimes

A `values_changed` produced by *comparing two datetimes* carries the pair
normalized to UTC, so `10:00-05:00` is reported as `15:00+00:00`. Every other
category carries the raw value, including the `values_changed` that `model.py`'s
`mutual_add_removes_to_become_value_changes` post-pass folds a same-path
add/remove pair into. The mechanism, with its source citations, is documented
once in `crate::diff::datetime_diff` (`crates/onix-core/src/diff/scalar.rs`).
The cases `datetime_values_changed_normalized_to_utc` and
`datetime_dictionary_item_added_reports_raw_value` cover the two sides.

Hashing splits the same way: `DeepHash._prep_datetime` normalizes, so a naive
and an aware value at one instant hash-match under `ignore_order`, while
`_prep_date` does not and formats a bare `YYYY-MM-DD`, which can never collide
with `_prep_datetime`'s `YYYY-MM-DD HH:MM:SS+00:00`.

**Fixed-offset `tzinfo` round-trip.** `to_dict()` returns an aware `datetime`
carrying a plain `datetime.timezone(timedelta(...))`, built from the offset a
`zoneinfo`/`pytz` (or any other) `tzinfo` reported *at the moment converted* —
never the original zone object. This changes nothing about the diff itself
(`DeepDiff` compares by instant and reports `values_changed` normalized to UTC
regardless — see above), only what a caller sees if they inspect `to_dict()`'s
value directly.

## Set iteration order: where onix differs

`onix` does not chase `DeepDiff` here because there is nothing stable to chase.
`DeepDiff`'s answers for sets depend on the order the *running process* happens
to iterate a Python set in — hash order, and for `str` members
`PYTHONHASHSEED`-dependent — or, for a `datetime`/`date`/tuple/frozenset set
member, on how `DeepHash` computes or caches a digest independently of Python's
own `==`. `onix` is deterministic throughout instead. Five consequences:

**Entry order.** `_diff_set` (`diff.py`) builds `set_item_added`/
`set_item_removed` from `t2_hashes - t1_hashes`, a Python set of SHA-256 hex
strings, so entry order follows those hashes. `onix` sorts entries by their
rendered path string — same findings, different order, the only one of the
five that is order-only.
`test_set_entry_order_is_sorted_where_real_deepdiff_is_hash_ordered` in
`test_sets.py` asserts onix's output (no golden case: `DeepDiff`'s own answer is
hash-order-dependent).

**Which member of an equality class wins.** `DeepHash` keys its shared cache by
`_make_hash_key(obj)` (`deephash.py`), so a Python-equal tuple or frozenset
hashed earlier in the run fixes the digest for every later Python-equal one, and
which member that is follows the process's set iteration order. `onix` hashes
each side's members in its own canonical set order, so the winner never depends
on process hash order.
`test_a_sets_report_does_not_depend_on_which_member_was_hashed_first` in
`test_sets.py` asserts onix's output (no golden case, same reason as above).

**`list(a_set) == some_list`.** `DeepDiff`'s distance computation asks whether
applying the new side's type to the old value reproduces it
(`_from_tree_type_changes`'s `include_values`, `model.py`), and for a set
against a sequence that is `list(the_set) == the_list`, answered in the set's
own iteration order — so which of two orderings of one list keeps a
`type_changes` is process-dependent. `onix` compares the two by membership in
either ordering. `test_a_set_versus_a_list_is_a_type_change_whatever_the_order`
in `test_sets.py` asserts onix's output (no golden case, same reason as above).

**A naive and an aware datetime at one instant are two Python set members, but
`DeepDiff` can report only one of them.** `_diff_set` groups members by
`DeepHash` digest, and `_prep_datetime` (`deephash.py`) normalizes every
datetime to its UTC instant before hashing, so the two land in the same bucket;
`_create_hashtable` keeps only one `{item, indexes}` entry per bucket, and which
member survives follows the set's own iteration order. `onix` stores every
structurally distinct member — identity for matching is by instant, but
`SetItems` keeps both.
`test_a_naive_and_aware_datetime_set_member_is_two_members_in_onix_one_in_deepdiff`
in `test_sets.py` asserts both the bare and the one-level-nested-in-a-tuple
case.

**A tuple or a frozenset set member matches order- and repetition-insensitively
in `DeepDiff`, not by Python `==`.** `DeepHash._prep_iterable` (`deephash.py`)
runs with `ignore_iterable_order`/`ignore_repetition` for every iterable it
hashes, so a tuple set member is affected too. `onix` compares a tuple member
positionally (`tuple.__eq__`) and a frozenset member by membership.
`test_a_tuple_set_member_matches_by_position_where_deepdiff_ignores_order_and_repetition`
in `test_sets.py` asserts both shapes.

None of the five has a golden case, since a golden fixture asserts byte parity
and these diverge. The bindings' set fuzz batch in
`crates/onix-py/tests/test_differential_fuzz.py` (see `_reverse_sets` and
`_gen_hashable`) skips a case where `DeepDiff` disagrees with itself.

**Canonical set order.** Everywhere a set's members become output — the JSON
array a set serializes to, and the members of a `frozenset` rendered inside a
`set_item_*` entry's path — `onix` emits them in one documented order, a
purely structural comparison. The rule lives on `onix_core::value::SetItems`'s
own doc; `scripts/golden_tags.py`'s `canonical_set_order` is its Python twin.

## Non-finite floats: where onix differs

For a comparison, `NaN`/`Infinity`/`-Infinity` behave like `DeepDiff`:
`NaN != NaN`, `Infinity == Infinity`, and `to_json()` renders the same bare
`NaN`/`Infinity`/`-Infinity` tokens Python's `json.dumps` writes by default
(`allow_nan=True`) rather than quoting them or raising. Under `ignore_order`,
matching agrees too: `DeepHash` digests any `NaN` by `str(obj)`
(`deephash.py::_prep_number`), which is the same three characters for every
`NaN` regardless of its sign or payload bits, so every `NaN` in a list, dict
value, or set member matches every other one (see
`crate::ignore_order::hash::deephash_float_bits`'s own doc, in
`crates/onix-core/src/ignore_order/hash.rs`). These cases are asserted in
`crates/onix-py/tests/test_non_finite.py`, comparing rendered JSON text, because
`serde_json` cannot parse a bare `NaN` token.

The divergences share one cause: object identity, which this crate's value model
carries for custom objects only. They are deterministic in every case.
`DeepDiff` returns `{}` when both sides are the same float or container object
(`t1 is t2`), while `onix` reports `values_changed`. Two bit-identical NaNs in
one set fold into one member at conversion (visible only when the set is carried
whole into a report), and `difflib`'s `b2j` matches one repeated NaN object to
itself where `onix` never matches. The mechanisms are in the docs of
`SetItems::new` (`crates/onix-core/src/value.rs`) and `ScalarKey::Nan`
(`crates/onix-core/src/lcs.rs`);
`dist_key_hash_collision_on_distinct_nans_never_becomes_equality` in
`crates/onix-core/src/ignore_order/tests.rs` asserts that a hash collision
between two distinct `NaN`s never becomes a false equality in the distance memo.

## Custom objects: where onix differs

A custom object (an instance of a user-defined class) is diffed by its
attributes, matching DeepDiff's `_diff_obj`:
`attribute_added`/`attribute_removed` and `root.attr` paths, and `type_changes`
between two classes that are not the same. onix enumerates attributes as
`_diff_obj` does — the instance `__dict__` plus the non-callable, non-dunder
names `dir()` adds (class attributes and `@property` values, read through
`getattr`), or the slot values up the MRO for a slots-only class — dropping
dunder (`__x`) names and keeping single-underscore (`_x`) and name-mangled
(`_Cls__x`) ones. An `Enum` member is read as `_diff_enum` reads it: `name` and
`value` only. Two instances of one plain class (only public instance attributes:
no `@property`, class attribute, or private) match DeepDiff byte-for-byte,
including under `ignore_order` and as list/dict values.

Class identity is the class object itself, as DeepDiff's `type(t1) != type(t2)`
compares it (the rule is stated once, on `Object::same_class` in
`crates/onix-core/src/value.rs`), plus the kind (`dict` subclass versus
attribute-diffed object). The rendered `old_type`/`new_type` stays the short
`__name__` DeepDiff shows.

Divergences, all deterministic. The first three (a whole object's serialized
value, `ignore_order` hashing, and `to_dict`) are the object-view gaps tracked
in [#99](https://github.com/ksco92/onix/issues/99), where onix keeps one
attribute view and DeepDiff uses three.

### A whole object's serialized value

When an object appears as a whole value in a report (a `type_changes`'
`old_value`/`new_value`, an object added to a list/dict, a
`threshold_to_diff_deeper` collapse), onix renders its full diffed attribute
set. DeepDiff's `to_json()` runs `serialization.json_convertor_default`, which
serializes only public `@property` values, or failing that only public
`__dict__` entries, and raises `TypeError` for a slots-only object with neither.
A class attribute is left out of both renders. For a plain class the two
coincide; they differ for a whole-value object with a `@property`, a private
(`_x`) attribute, an `Enum` member (DeepDiff renders `{}`, onix its `name` and
`value`), or a slot value on a class that also has `__dict__` (`vars(m)` is
`{}`, so DeepDiff renders the added instance as `{}` where onix renders the
slot), and for a slots-only object, where DeepDiff crashes and onix renders.

### `ignore_order` object hashing

DeepDiff's `DeepHash._prep_obj` hashes an object by its raw `__dict__` (or its
slots), never the `dir()`-derived properties and class attributes `_diff_obj`
reads. onix hashes the one attribute view it holds, so pairing can differ for an
object whose `@property` values change what its diffed view contains. Both tag
the hash with the class `__name__`, so a custom object never pairs with a plain
`dict`, while two distinct classes sharing a `__name__` share a hash bucket in
both. The pairing distance counts an object as `_get_item_length` does
(`len(obj.__dict__)`; for an `Enum` class, the `__dict__` lengths of all its
members) plus the `DeepHash` count of the `__dict__` entries onix does not
extract, so an `Enum` member holding a container pairs by a slightly different
distance.

### `to_dict()` returns attribute dicts, not the original objects

DeepDiff's `to_dict()` hands back the original instances (in a `type_changes`,
an added item); onix converts every input to its value model up front and cannot
reconstruct an instance, so `to_dict()` returns the object's attribute `dict`.
`to_json()` is the byte-parity target and is unaffected.

### A recursive object

DeepDiff's `parents_ids` skips a child whose first-side object is already on the
path from the root, so a self-referential object or a parent pointer reports
nothing for the cycle. On the first side onix reports nothing; on the second
side only, it is compared as the object it points back at, so `a.me = 5` against
`b.me = b` is a `type_changes` as in DeepDiff. A cycle a report shows renders as
`{}` where DeepDiff's `to_json()` raises on the circular reference.

### Types DeepDiff routes to a handler onix lacks

The accept-list is derived from `_diff`'s isinstance ladder. DeepDiff sends a
**number** (`complex`, `Decimal`, `Fraction`, a `numpy` scalar or
`numpy.datetime64`), an **iterable** (`bytes`, `bytearray`, `memoryview`,
`range`, a generator, `deque`, `array.array`, any `__iter__`-defining object), a
**`uuid`** or an **`ipaddress`** value to dedicated handlers before it reaches
`_diff_obj`; a class object it diffs by its class `__dict__` and a module
likewise. onix implements none of those. At the root such a value raises a
typed, path-naming `TypeError` rather than being reshaped into an object, which
for the attribute-less ones (`complex`, a bare `object()`) would otherwise
silently report `{}` for two *unequal* values.

Below the root, DeepDiff's `_diff` returns before any handler when `t1 is t2`.
onix treats a value it cannot convert as equal only to the same object, and
raises a `TypeError` naming its path wherever a report would have to show it.
Two equal but distinct unsupported objects (`Decimal("1")` built twice)
therefore raise where DeepDiff reports nothing, and an added object holding one
raises where DeepDiff renders it. A class attribute onix cannot convert
(ABCMeta's `_abc_impl`, a class-level lock) raises `TypeError` once a report
compares it with a value that shadows it. `resolve_token` and `token_error` in
`crates/onix-py/src/convert.rs` document the rest.

### Pydantic models

DeepDiff's ladder checks a `pydantic` `BaseModel` just before `Iterable` and
diffs it with `_diff_obj`, reading its fields and, from the class, its other
attributes; `DeepHash` and `_get_item_length` instead treat the model as the
iterable of `(field, value)` pairs it is. onix refuses a model, with the root
`TypeError`, or a path-naming `TypeError` below the root, where DeepDiff diffs
it.

### Refused mappings

A custom `collections.abc.Mapping` that is not a `dict` is an iterable to the
accept-list and is refused, where DeepDiff diffs it as a mapping. The same holds
for the read-only mapping a `re.Pattern` with named groups exposes as
`groupindex`: `re.compile("(?P<x>a)")` versus `re.compile("(?P<y>a)")` raises
`TypeError: ... mappingproxy at root.groupindex`, where DeepDiff reports both
`root.groupindex` and `root.pattern`. A pattern without named groups exposes a
plain `dict` there and diffs like DeepDiff.

### A property that raises

A getter raising anything other than `AttributeError` (a `ValueError`, and any
`BaseException` such as `KeyboardInterrupt`) propagates out of onix as that
Python exception, matching DeepDiff. An `AttributeError` while reading an
object's attributes (a `@property` that raises it, or an unset slot on a class
that also has `__dict__`) raises a `TypeError` naming the object's path, where
DeepDiff reports the whole object as `unprocessed`, a report category onix does
not implement.

## Known DeepDiff quirks

### Reproduced quirks

onix reproduces these byte-for-byte; the doc named in each row states the rule.

| Quirk | Where documented | Golden cases |
| --- | --- | --- |
| Key quoting escapes nothing | `onix_core::path::quote_key` | `key_*` |
| A set item is quoted by its own rule | `onix_core::path::set_item_repr` | `set_str_item_*`, `set_str_inside_tuple_item` |
| A non-`str` dict key renders via `repr()`, a `tuple` key split per element | `onix_core::path::dict_key_repr` | `dict_key_*` |
| A dict key matches across two dicts by Python `==`, not by type, and `SetOrdered.intersection` keeps `b`'s key object: `{1: "a"}` vs `{1.0: "a2"}` reports `root[1.0]` | `crate::ignore_order::match_dict_keys` | `dict_key_int_vs_float_changed_value_matches_by_python_equality` |
| `[1]` vs `[1.0]` inside a list diffs to nothing | `ScalarKey` in `crates/onix-core/src/lcs.rs` | `list_lcs_int_vs_float_single_matches_via_python_equality` |
| A hashable tuple inherits another tuple's hash under `ignore_order` | "Distance memo" in `docs/design/ignore-order.md` | `ignore_order_tuple_digest_*` |
| A `time` hashes by whole seconds-of-day under `ignore_order`, dropping microsecond and offset | `onix_core::datetime::Time::hash_seconds_of_day` | `ignore_order_time_microsecond_only_difference_hash_matches`, `ignore_order_time_offset_only_difference_hash_matches` |

The `frozenset` equivalent of the tuple-hash row is a divergence: see "Which
member of an equality class wins" under "Set iteration order". The cases
`set_tuple_item_python_equality` and `set_frozenset_item_python_equality` cover
the equality rule itself.

### Nested non-`str` dict key in `to_json()`

`to_json()` on a *nested* dict value with a non-`str` key mirrors `json.dumps`'s
key-stringification rule where Python has one, and diverges where `DeepDiff`
itself crashes. A *top-level* path segment always renders via `repr()` (the
non-`str` dict key row of Reproduced quirks); a nested dict *value* has no such
literal in JSON, so Python's `json.dumps` stringifies a
`bool`/`None`/`int`/`float` key (`"true"`/`"false"`, `"null"`, the same
shortest-round-trip form a float *value* gets) and `onix` matches exactly
(`dict_key_nested_value_stringifies_bool_none_float_keys`). A
`datetime`/`date`/`tuple` key has no such rule: `json.dumps` (and so
`DeepDiff.to_json()`) *raises* `TypeError`, so per this crate's compatibility
policy `onix` renders the same `repr()` text that key would get as a top-level
path segment instead. No golden fixture can hold this crash case; asserted in
`crates/onix-core/src/value_tests.rs`'s
`to_serde_json_stringifies_a_tuple_key_via_python_repr_where_deepdiff_would_crash`
and its `datetime` twin.

### Path-rendering collision survivor

A key can make path rendering collide, in both tools. A dict key whose own text
contains `']['`-shaped syntax (e.g. `p'"]["q'`) renders identically to an
unrelated, differently-nested path (e.g. `{"p'": {"q'": ...}}`); `DeepDiff`'s
own `to_json()` collapses this to one entry too. Which finding survives the
collapse differs: `DeepDiff`'s follows its Python dict's original key insertion
order, `onix` traverses `serde_json::Map`'s keys alphabetically (no
`preserve_order` feature). See `crate::report`'s module doc for the structural-
vs. rendered-path keying rule this follows from. The `path_rendering_collision`
golden case and `crates/onix-core/tests/golden.rs`'s dedicated test (no panic,
valid `DeepDiff`-shaped output, onix's own deterministic survivor) cover it.

### `namedtuple` positional diff

A `namedtuple` is diffed positionally, not by field. `DeepDiff` walks a
`namedtuple`'s fields too (`deephash.py::_prep_tuple`), reporting `root[0].x`
rather than `root[0][0]`. `onix` accepts a `namedtuple` as an ordinary `tuple`
subclass — carrying its class name into a `type_changes` entry like any other
tuple subclass — but diffs its contents positionally like every other tuple.
`crates/onix-py/tests/test_tuples.py` asserts both outputs side by side. No
golden case: the corpus's tagged encoding has no tag for a `namedtuple`.

### Subclasses

Every other subclass — `list`/`tuple`/`set`/`frozenset`/`dict`, and a
`datetime`/`date`/`time`/`timedelta` subclass such as pandas' `Timestamp` — is
accepted and compares as its base type, carrying its own class name into a
`type_changes` entry, matching `DeepDiff`'s `type(obj).__name__` rule (one
restriction: a `tuple`/`frozenset` subclass, including a `namedtuple`, is not
accepted as a `set` member: `onix` raises `TypeError` for such a member where
`DeepDiff` accepts it and compares it by value). See
`crates/onix-core/src/value.rs`'s `Typed` doc and
`docs/design/value-conversion.md`'s "Subclasses" section for the conversion
rules; `test_tuples.py`, `test_sets.py`, `test_datetimes.py` and
`test_conversions.py` assert this against DeepDiff. No golden case uses a
subclass, for the same tagged-encoding reason as above. A `zoneinfo`/`pytz`
`tzinfo` is the same simplification: see "Fixed-offset `tzinfo` round-trip"
under "Normalized versus raw datetimes".

### `to_dict()` type names

`to_dict()` reports a `type_changes` entry's types as names, not classes.
`DeepDiff` puts the type objects themselves (`<class 'tuple'>`) in `to_dict()`;
`onix` puts the same names its `to_json()` uses (`"tuple"`). Values are
unaffected.

### List-LCS `2^53` limit

List-LCS numeric matching is exact only within `2^53`. The matcher's cross-type
equality (the `[1]` vs `[1.0]` row of Reproduced quirks) normalizes any integral
value — `bool`, `int`, or a fraction-free `float` — to one shared bucket
key, but only exactly for magnitudes an `f64` represents every integer up to
(`2^53`, `9_007_199_254_740_992`); beyond that, two otherwise-equal large
numbers compare by `f64` bit pattern instead of exact value. Python performs
exact arbitrary-precision comparison here. No golden case exercises it.

### Naive datetime pairing and the process timezone

`ignore_order` pairing among naive datetimes depends on the process's local
timezone in `DeepDiff`, but not in `onix`: `distance.py`'s
`_get_datetime_distance` ranks a candidate pair through `datetime.timestamp()`,
which reads a naive value in the local timezone. onix reads a naive value as UTC
everywhere, so the two agree once the process timezone is UTC. The rationale is
in `distance_family`'s doc (`crates/onix-core/src/ignore_order/distance.rs`);
the `utc_timezone` fixture in `crates/onix-py/tests/test_differential_fuzz.py`
asserts the comparison. No golden case: regeneration is byte-stable under any
`TZ`.

### Datetime outside year `1..=9999`

A datetime whose UTC form leaves year `1..=9999` cannot be compared to another
datetime. Normalizing e.g. `9999-12-31T23:00-01:00` lands on year 10000, and
`astimezone(timezone.utc)` raises `OverflowError` there, so `DeepDiff` raises
rather than reporting anything; `onix` raises too, as
`onix_core::Error::DateTimeOutOfRange`, surfaced to Python as a `ValueError`
naming the path. On the ordered path both tools raise only when two datetimes
are actually compared. Under `ignore_order`, `DeepHash._prep_datetime`
normalizes every datetime it hashes, so `DeepDiff` raises even for a value
merely added, removed, or shuffled; `onix` hashes by instant and reports it raw
instead, per the compatibility policy, raising only where two datetimes are
diffed, in the report or in a candidate pair's distance.
`an_unnormalizable_datetime_under_ignore_order_is_reported_raw` in
`crates/onix-core/src/diff/tests.rs` asserts onix's side.

### Integer beyond `f64::MAX`

An integer beyond `f64::MAX` (about `2**1024`) crashes `DeepDiff` under
`ignore_order`; `onix` reports the change. `distance.py::_get_numbers_distance`
runs `float(num1)` outside its `try`, and `float(2**1024)` raises
`OverflowError`. onix reads such an integer as a saturated `f64` infinity, so
the pair's distance short-circuits and it reports `values_changed`, per the
compatibility policy. The case directory's `expected.json` has the keys
`deepdiff_raises` and `onix` instead of a report.
`ignore_order_big_int_beyond_f64_pairs_without_panicking` in `golden.rs` asserts
the `onix-core` render; `crates/onix-py/tests/test_golden_parity.py` asserts the
bindings' `to_dict()` against `decode_tags(expected["onix"])`, and
`every_deepdiff_crash_case_is_pinned` in `golden.rs` asserts every case carrying
the marker is registered.

### Lone surrogate hashing

A `str` (or a dict key) containing a lone (unpaired) surrogate code point is
accepted and compared like any other `str`, as in `DeepDiff`; `pystring_to_cstr`
in `crates/onix-py/src/convert.rs` documents the conversion. The divergence is
in hashing: as a `set`/`frozenset` member or under `ignore_order`, `onix` hashes
a lone surrogate by code point, where `DeepDiff` crashes with an unhandled
`UnicodeEncodeError` from `deephash.py`. `onix` reports deterministically, per
the compatibility policy. No golden case: the corpus's JSON writer cannot hold
this content either (`ensure_ascii=False` raises the same `UnicodeEncodeError`).
`crates/onix-py/tests/test_conversions.py` asserts the directed cases (plus a
non-BMP character converting normally) and `test_differential_fuzz.py`'s
surrogate batch runs `SEED_COUNT` generated cases through both engines.

### Empty-`tuple` dict key

An empty-`tuple` (`()`) dict key is a `DeepDiff` bug. `stringify_param`
(`model.py`) renders a `tuple` param as `']['.join(map(repr, param))`, which for
`()` is the empty string, so the path comes back Python `None` and then the
literal `"null"` dict key once `to_dict()`'s report reaches `to_json()`. `onix`
renders `root[]` instead — deterministic, and a usable path — per the
compatibility policy. The differential fuzz batch that generates `tuple` dict
keys (issue #62) excludes the empty tuple for this reason; see
`crates/onix-py/tests/test_differential_fuzz.py`'s `_gen_non_str_dict_key`.

### Non-finite `float` dict key

A non-finite `float` (`nan`/`inf`) dict key is also a `DeepDiff` bug.
`stringify_param`'s `repr()`-then-`ast.literal_eval` round trip fails on
`"nan"`/`"inf"` and silently collapses the key to `None`, the same way the
empty-tuple case above does: `DeepDiff({}, {float('nan'): 1}).to_json()` is
`{"dictionary_item_added": {"null": 1}}`. `onix` renders the key
deterministically — `root[nan]` as a path segment, and the bare
`NaN`/`Infinity`/`-Infinity` token text wherever the key is embedded in a
carried JSON object — per the same compatibility-policy choice. See
`crates/onix-py/tests/test_non_finite.py`'s
`test_non_finite_dict_key_renders_without_crashing`.

### `datetime`/`date` subclass dict key

A subclass's `repr()` looks like a constructor call, which
`literal_eval_extended` cannot parse back as a literal, so the path of a
`datetime`/`date` subclass key collapses to `None`/`"null"` through the same
`stringify_param` bug as the two cases above — not only for an added/removed
key, but for the same subclass key on both sides holding a container value with
its own internal change, since that also needs a path built past the key:
`{MyDT(...): {1, 2, 3}}` vs `{MyDT(...): {1, 2, 4}}` gives
`{'set_item_removed': ['None[3]'], 'set_item_added': ['None[4]']}`. `onix`
renders the key path deterministically either way (a `tuple`/`namedtuple` key is
unaffected — `stringify_param` renders any tuple-shaped key positionally,
never through `repr()`). See `crates/onix-py/tests/test_differential_fuzz.py`'s
`_generate_subclass_key_case`.

### Subclass dict keys

A dict key that is a `tuple`/`datetime`/`date` subclass, including a
`namedtuple`, classifies as its base type (`classify_dict_key` in
`crates/onix-py/src/convert.rs`), as `DeepDiff`'s plain-`==` key matching never
consults `type(obj)`. The divergence is an overridden `__eq__`/`__hash__`:
`DeepDiff` matches by the subclass's own equality, `onix` by the base type's
structural value. A `tuple` subclass whose `__eq__` is always `True` and
`__hash__` always `0` makes `{K((1, 2)): "v1"}` vs `{K((3, 4)): "v2"}` a
`values_changed` at `root[3][4]` for `DeepDiff` and a change of the whole dict
at `root` for `onix`.
`test_a_key_subclass_with_overridden_equality_matches_structurally_not_by_python_eq`
in `crates/onix-py/tests/test_conversions.py` and the subclass-key batch of
`test_differential_fuzz.py` assert it.