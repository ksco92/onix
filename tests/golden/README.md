# Golden corpus

This directory is the correctness gate for `onix`'s compatibility claim: proof
that `onix`'s report is byte-identical (canonical JSON) to real `DeepDiff`'s
`to_json()` output at `verbose_level=2`, on a hand-designed corpus of small,
diverse cases.

## Layout

Each subdirectory is one case:

```
tests/golden/<case_name>/
├── a.json         # t1, as fed to both DeepDiff and onix
├── b.json         # t2
├── expected.json  # DeepDiff(t1, t2, verbose_level=2, **kwargs), rendered through
│                  # golden_tags.canonical_report (which passes the
│                  # JSON_DEFAULT_MAPPING a `date` case needs, and puts anything
│                  # set-derived into onix's canonical order) and re-dumped with
│                  # sort_keys=True — see "The `date` superset" and "Set
│                  # iteration order" below
└── options.json   # {"ignore_order": bool} — which DiffOptions onix diffs
                    # this case with; kwargs above mirrors it (currently the
                    # only option this corpus varies)
```

## Values JSON cannot express: the tagged encoding

DeepDiff diffs Python objects, and several of the types it handles have no JSON
literal. A case that needs one writes it as a **tagged object**: a JSON object with
**exactly one** key, and that key one of the reserved names below.

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
`{"$date": "2024-01-01"}` is that date and `{"$time": "10:00:00+02:00"}` is
that aware time. The three calendar-string tags carry exactly what
`isoformat()` produces and `fromisoformat()` reads back, with the UTC offset
present only for an aware value. `$timedelta` carries Python's own
already-normalized `(days, seconds, microseconds)` triple as an object
instead of one ISO string, because a single flattened microsecond count
overflows even a 64-bit integer at Python's own extreme `days=999_999_999`
(see `onix_core::datetime::TimeDelta`'s own doc); a set's members are always
*written* in the canonical order documented below, which is what makes a
fixture holding one byte-identical between runs. `$dict` is the one tag whose
payload is a list of pairs rather than a list of items or a bare string: a
plain JSON object can only ever have `str` keys, so a `dict` with any other
key kind (`int`, `bool`, `float`, `None`, `datetime`, `date`, or a `tuple` of
those) has no other shape to write it in — `{"$dict": [[1, "x"], [true,
"y"]]}` is `{1: "x", True: "y"}`. Each pair's key is itself tagged where its
type needs it (a `$tuple`/`$datetime`/`$date` key encodes exactly like a
value of that type would). **Any other object is plain data**, including one
that has a
reserved key alongside others (`{"$tuple": [1], "x": 2}` is a two-key dict). The
reserved names are claimed all at once, before their types are supported, so a
fixture can never use one as an ordinary dict key and then change meaning later; a
decoder that meets a tag it cannot decode yet fails loudly.

`$bigint` is the one tag for a value JSON *can* express: `{"$bigint":
"1267650600228229401496703205376"}` is `2**100`. An arbitrary-precision integer
has a perfectly good JSON number literal, but this corpus's Rust reader parses a
number back through `serde_json` without its `arbitrary_precision` feature, which
collapses any integer beyond `i64`/`u64` to the nearest `f64` — so an untagged
big integer in an input file would decode to a float, not the integer the case
means. Tagging it as its exact decimal digits keeps the input a real integer for
both readers. An in-range integer stays a plain JSON number, unchanged. This is
the same representation gap onix's own value model closes (`onix_core::value::
Number`'s arbitrary-precision arm); the JSON *text* readers (`diff_json`, the
CLI) share `serde_json`'s limitation and parse such an integer as a float, so
two documents whose integers differ only past `i64`/`u64` compare equal and
diff to `{}` there — stated in the README's Known limitations and tracked in
issue #92.

Because a big integer in a **report value** (a `values_changed`/`type_changes`
`old_value`/`new_value`) has the same `serde_json` gap, the golden test collapses
both onix's output and the `expected.json` to that nearest-`f64` resolution
before comparing (`collapse_bigint_tags` in `crates/onix-core/tests/golden.rs`).
The diff *structure* — which category, which path, the int-versus-float type
split — is still checked exactly; only a big integer's rendered *value* is
compared at `f64` resolution there. onix's exact-digit rendering is pinned
directly instead by the crate's own JSON-writer/`Number` unit tests and the
Python bindings' `to_dict()`/`to_json()` round-trip tests, which do not go
through `serde_json`.

The one cost of the encoding is that a dict whose *only* key is a reserved name
cannot be a fixture value. `scripts/golden_tags.py`'s `encode_tags` refuses to write
such a value rather than writing a file that would decode back into something else.

Two implementations of the rule, one per language, cover the corpus's three readers:

- `scripts/golden_tags.py` — the definition, used both by the generator (which also
  reads every file it writes back and checks it against the case it came from) and by
  `crates/onix-py/tests/test_golden_parity.py`.
- `crates/onix-core/tests/golden.rs` — the Rust decoder, building the engine's own
  value model.

**The product never interprets a tag.** `onix_core::Value`'s `Deserialize`,
`deepdiff_rs.diff_json`, the `DeepDiff` class, and the CLI all read `{"$tuple": [1]}`
or `{"$datetime": "2024-01-01T00:00:00"}` as the one-key dict it literally is; each
of those paths has a test pinning that.

## The `date` superset

DeepDiff's `serialization.JSON_CONVERTOR` maps `datetime.datetime` to
`isoformat()` and has **no entry for `datetime.date`**, so its own `to_json()`
raises `TypeError` on any report carrying a bare date. onix renders one as
`YYYY-MM-DD` — the same bytes `date.isoformat()` gives — which is a deliberate
superset, not a divergence: passing
`default_mapping={datetime.date: datetime.date.isoformat}` to DeepDiff's own
`to_json()` makes it produce byte-identical output, and that is exactly what
`scripts/golden_tags.py`'s `JSON_DEFAULT_MAPPING` is, shared by the generator
and by `crates/onix-py/tests/test_golden_parity.py`. So a date-carrying golden
case still has real DeepDiff output as its spec.

`crates/onix-py/tests/test_datetimes.py` additionally asserts date cases against
DeepDiff's `to_dict()` — the rendering-free comparison — and pins the fact that
DeepDiff's stock `to_json()` still raises.

## The `time`/`timedelta` superset

The same gap, for the same reason: `JSON_CONVERTOR` has no entry for
`datetime.time` or `datetime.timedelta` either, so DeepDiff's stock
`to_json()` raises `TypeError` on a report carrying either. onix renders a
`time` as `time.isoformat()`'s own bytes (the same shape a `datetime`'s
own time portion takes) and a `timedelta` as `str(timedelta)`'s — there
being no `timedelta.isoformat()` to mirror instead, `str()` is the natural,
deterministic choice. `JSON_DEFAULT_MAPPING` carries both mappings
alongside `date`'s, so a golden case holding either still has real DeepDiff
output as its spec, the identical mechanism the `date` superset uses.

`crates/onix-py/tests/test_times.py`/`test_timedeltas.py` additionally
assert cases against DeepDiff's `to_dict()` and pin the fact that DeepDiff's
stock `to_json()` still raises for both.

`crates/onix-core/tests/golden.rs` reads every case directory present here
(there is no separate hand-maintained case list), runs
`onix_core::diff_with_options` with each case's own `options.json`, and
asserts the resulting report's canonical JSON (parsed `serde_json::Value`,
so object key order doesn't matter — array order and values do) equals
`expected.json` exactly, with one documented exception (see "Known DeepDiff
quirks" below). It runs as part of the normal `cargo test` / `make check`.

## Pinned versions

- **Python:** 3.14 (installed on demand by `uv`, per `scripts/gen_goldens.py`'s
  inline script metadata) — pinned exactly, not just a floor: a case holding
  a `str` nested inside a tuple/frozenset set item is rendered by real
  DeepDiff's own `repr()`, which escapes against *this interpreter's*
  Unicode table, so generating on any other version could bake a stale
  classification into the committed spec (see the Unicode entry below;
  `main()` asserts the running interpreter's `unicodedata.unidata_version`
  before writing anything).
- **deepdiff:** `9.1.0` exactly (pinned in `scripts/gen_goldens.py`'s inline
  script metadata, resolved from PyPI's latest `8.x`+ line; see that file's `# /// script` header)
- **Unicode:** 16.0.0 via the `unicode-general-category` crate, matching
  CPython 3.14's `unicodedata` table; an older CPython escapes code points
  assigned after its own Unicode version where onix renders them literally
  (`onix_core::path::python_repr`'s `str` escaping — see
  `crates/onix-py/tests/test_sets.py`'s BMP differential test).

## Regenerating

```sh
uv run scripts/gen_goldens.py
```

This is the **only** source of `expected.json` (and, for full
reproducibility, `a.json`/`b.json` too) — never hand-edit any file in this
directory. Every case is defined in `scripts/gen_goldens.py`'s `CASES` dict;
add a case there and re-run to add a new golden. The script overwrites
existing case directories in place, so a clean re-run should produce no `git
diff` unless `CASES` or the pinned `deepdiff` version changed.

## Case coverage

The corpus exercises: `values_changed`, `type_changes` (including
int/float/bool/`None`/dict/list pairings, at the root and at depth),
`dictionary_item_added`/`removed` (including both firing together at depth,
and to/from an empty dict), `iterable_item_added`/`removed` (including
to/from an empty list, tail growth/shrink keyed by absolute original index,
and a same-length element change), every nesting combination
(dict-in-dict, list-in-list, dict-in-list, list-in-dict), unicode keys, key
quoting/escaping edge cases (single quote, double quote, both, a literal
backslash, and control characters), the `threshold_to_diff_deeper=0.33`
dict-vs-dict collapse (`threshold_collapse_*` cases: root and nested,
the boundary at exactly `0.33` vs just below it, dict-in-list, deeply
nested collapsed values, and alongside an unrelated `type_changes`), and
one adversarial path-rendering collision (see below).

**List-compat cases (`list_lcs_*`):** `DeepDiff`'s LCS/`difflib`-style
list matching for all-scalar lists — the reorder repro (a reorder producing an
add+remove instead of three `values_changed`), a same-length replace, a
mid-list insert/delete, a shifted list, repeated elements, disqualification
by an unhashable (dict or nested-list) element, mixed scalar kinds, the
`[1]`/`[1.0]` cross-type hashability finding (matches as `'equal'`, reports
nothing), the `new_path` field on an index-drifted `values_changed`/
`type_changes` (including on a mixed-type pair, and with unicode strings),
the "keep the smaller, ties favor index-aligned" count comparison, and the
`autojunk=False` finding at ≥200 items. See
`docs/design/list-diff.md` for the full spec these pin down.

**Set cases (`set_*`, `frozenset_*`, `ignore_order_set_*`,
`ignore_order_frozenset_*`, `ignore_order_unhashable_set_*`):** a set diffs into
the two categories no other type produces, `set_item_added`/`set_item_removed`,
whose entries are paths ending in the item itself rather than a path-keyed
value; at the root, nested in a dict, to and from an empty set, and with several
items on each side. A set *versus* a `frozenset`, a `list` or a `dict` is a
`type_changes` at the container itself. One case per item kind pins the
rendering rule (`onix_core::path::set_item_repr`'s own doc has it, and it is
not `quote_key`'s): `None`, `bool`, `int`, `float`
(including `1e+16`, `1e-05` and `-0.0`), `str` (plain, with a single quote, with
a double quote), `tuple` (nested, and holding a `str` — which *is* escaped,
unlike a top-level one), and `frozenset` (empty and non-empty). Membership is Python's own
`==` with bare numbers kept type-distinct, so `{1}` vs `{1.0}` and `{True}` vs
`{1}` are each a removal plus an addition, while `{(1,)}` vs `{(1.0,)}` and
`{frozenset({1})}` vs `{frozenset({1.0})}` are empty. A tuple and a frozenset
holding the same members are not Python-equal, so they never collide either.
Under `ignore_order` a set diffs identically (a set has no order to ignore),
never hash-matches another container kind, and pairs with another set by
distance like any other item. See "Set iteration order" below for the three
places this rule is deliberately more deterministic than `DeepDiff`'s own.

**Tuple cases (`tuple_*`, `ignore_order_tuple_*`, `ignore_order_unhashable_tuple_*`):** a tuple diffs positionally
exactly like a list (element change, length change, tuples of dicts, tuples in
tuples, and the same difflib match — including its `new_path` — for all-scalar
contents), while a tuple *versus* a list is a `type_changes` at the container
itself, at the root and nested in a dict, including the empty-vs-empty pair. A
tuple element disqualifies its list from the difflib match the way a nested list
does. Under `ignore_order`, a tuple is hash-paired like a list, a nested tuple
hashes order-insensitively, and a tuple never hash-matches a list with the same
items (DeepHash carries the type), which surfaces as a `type_changes` on the
paired items rather than nothing at all. The `ignore_order_tuple_digest_*`
cases pin `DeepHash`'s shared-cache collision (see "Known DeepDiff quirks"
below): which numeric type a hashable tuple holds stops mattering once a
Python-equal tuple has been hashed, while an unhashable one keeps its own
digest.

**Datetime and date cases (`datetime_*`, `date_*`, `list_lcs_datetime_*`,
`ignore_order_date*`):** a datetime pair compares by *instant* with a naive
value read as UTC, and a changed pair is reported **normalized to UTC** while
every other category keeps the **raw** value (see "Normalized versus raw"
below). The `isoformat()` rendering boundaries are pinned too: microseconds
only when non-zero, an offset suffix only when aware, widening to `+HH:MM:SS`
for an offset that is not a whole number of minutes. A `date` compares by
value, is never equal to a `datetime` at the same midnight, and is reported
under the type name `date`. A list of either takes the difflib path, since
both are in DeepDiff's `helper.basic_types`; the cases cover a naive/aware
same-instant pair reaching a `'replace'` opcode and reporting nothing, an
aware pair matching as `'equal'` outright, and `new_path` on an index-drifted
pair. Under `ignore_order`, they cover a naive and an aware value at one
instant hash-matching, a paired change carrying `new_path`, unpaired items
reported raw, a date and a datetime never hash-matching but still pairing by
distance, and a calendar value pairing with a string of itself (which is what
the `str()` coercion in the delta shape decides).

**Time and timedelta cases (`time_*`, `timedelta_*`, `list_lcs_time_*`,
`list_lcs_timedelta_*`, `ignore_order_time_*`, `ignore_order_timedelta_*`,
`set_time_*`, `set_timedelta_*`):** unlike a `datetime`, real `_diff_time`
(the function DeepDiff uses for `time`, `date` **and** `timedelta` alike)
never normalizes, so a `time`/`timedelta` `values_changed` always carries
the **raw** pair. A naive `time` is **never** equal to an aware one — no
"read naive as UTC" rule applies — while two aware values compare by an
offset-adjusted micros-of-day at full microsecond precision. A `timedelta`
compares by its exact `(days, seconds, microseconds)`. Both are in
DeepDiff's `helper.basic_types`, so a list of either takes the difflib
path; because a naive/aware `time` pair is never Python-equal, it reaches a
`'replace'` opcode and, unlike the analogous `datetime` case, **is**
reported (`list_lcs_time_naive_vs_aware_reports_a_change` — the one place
`time` and `datetime` genuinely diverge in this corpus). Under
`ignore_order`, `time` hashes through a genuine, confirmed `DeepHash`
quirk: `time_to_seconds` drops **both** the microsecond and any offset
before hashing, so a microsecond-only or an offset-only difference
hash-matches even though the ordinary comparison calls the pair different
(`ignore_order_time_microsecond_only_difference_hash_matches`,
`ignore_order_time_offset_only_difference_hash_matches`); `timedelta`
hashes exactly, with no such truncation
(`ignore_order_timedelta_exact_hashing_no_truncation`). Neither type ever
pairs with a number, and `time` never pairs with a `date`/`datetime`
(`TYPES_TO_DIST_FUNC` never `isinstance`-matches a `time` against either).
Fifteen seeded fuzz cases each cover the ordered
(`list_lcs_time_timedelta_fuzz_seed_*`) and `ignore_order`
(`ignore_order_time_timedelta_fuzz_seed_*`) paths.
`docs/design/value-model.md`'s "Calendar types" section is the full spec.

**Multi-line string cases (`multiline_string_*`,
`ignore_order_multiline_string_*`):** at `verbose_level=2` DeepDiff adds a
`diff` field (a `difflib.unified_diff` of the two strings) to a
`values_changed` entry whenever both values are strings and one contains a
newline (`_diff_str`). These pin: the field at the root and at a nested dict
path; a newline on the old side only and on the new side only both triggering
it; a plain single-line change getting **no** field; `splitlines()`
boundaries, including `\r\n` as one boundary and a bare `\r` (and other
Unicode boundaries) splitting only once a literal `\n` has triggered the
field; leading and trailing newlines (a leading newline is a blank first
line, a trailing one is dropped by `splitlines()`); two strings differing
**only** by a trailing newline getting a `values_changed` but no `diff`
(the line lists are equal); identical multi-line strings reporting no entry
at all; two far-apart changes splitting into two hunks (`n=3` context); the
autojunk heuristic at 250+ lines (which `unified_diff` enables, unlike the
ordered-list path) and its greedy backward extension over a purged popular
run; and the `ignore_order` route, where a paired change reaches `_diff_str`
and carries `new_path`. The field is emitted only where DeepDiff runs
`_diff_str`: the mutual-add-remove merge and the threshold-collapse paths
deliberately omit it. See `crates/onix-core/src/unified_diff.rs`.

**`ignore_order=True` (`ignore_order_*` cases):** pure shuffle, shuffle
plus a changed/added/removed value, duplicate-multiplicity invisibility,
nested-dict pairing, list-in-dict-in-list, mixed type changes, `[1]` vs
`[1.0]` (a real `type_changes` here, unlike the ordered LCS path's `{}`),
bool-vs-int never hash-equal, one-sided all-added/all-removed, the
`cutoff_intersection_for_pairs=0.7` gate on both sides, a worked
asymmetric-tie-break example, and index-drift `new_path` (both on a
real finding and confirmed absent on added/removed). Plus 20 seeded-random
fuzz cases (`_generate_ignore_order_fuzz_cases`). See
`docs/design/ignore-order.md` for the full spec this implements;
`scripts/differential_fuzz.py` is a separate, larger-scale (thousands of
cases) fuzzer run during development, not part of this fixed corpus.

## Normalized versus raw datetimes

A `values_changed` produced by *comparing two datetimes* carries the pair
normalized to UTC, so `10:00-05:00` is reported as `15:00+00:00`. Every other
category carries the raw value, including the `values_changed` that
`model.py`'s `mutual_add_removes_to_become_value_changes` post-pass folds a
same-path add/remove pair into. The mechanism, with its source citations,
is documented once in `crate::diff::datetime_diff`
(`crates/onix-core/src/diff/scalar.rs`).
`datetime_values_changed_normalized_to_utc` and
`datetime_dictionary_item_added_reports_raw_value` pin the two sides.

Hashing splits the same way: `DeepHash._prep_datetime` normalizes, so a naive
and an aware value at one instant hash-match under `ignore_order`, while
`_prep_date` does not and formats a bare `YYYY-MM-DD`, which can never collide
with `_prep_datetime`'s `YYYY-MM-DD HH:MM:SS+00:00`.

**Fixed-offset `tzinfo` round-trip.** `to_dict()` returns an aware `datetime`
carrying a plain `datetime.timezone(timedelta(...))`, built from the offset a
`zoneinfo`/`pytz` (or any other) `tzinfo` reported *at the moment converted* —
never the original zone object. This changes nothing about the diff itself
(`DeepDiff` compares by instant and reports `values_changed` normalized to UTC
regardless — see above), only what a caller sees if they inspect
`to_dict()`'s value directly. See `crates/onix-py/src/convert.rs`'s module
doc.

## Set iteration order: where onix is deliberately different

This is the one place `onix` does not chase `DeepDiff`, and the reason is that
there is nothing stable to chase. `DeepDiff`'s answers for sets depend on the
order the *running process* happens to iterate a Python set in — hash order,
and for `str` members `PYTHONHASHSEED`-dependent — or, for a
`datetime`/`date`/tuple/frozenset set member, on how `DeepHash` computes or
caches a digest independently of Python's own `==`. `onix` is deterministic
throughout instead (owner-approved, 2026-09-03). Five consequences, each
confirmed against `deepdiff==9.1.0`:

**Entry order.** `_diff_set` (`diff.py`) builds `set_item_added`/
`set_item_removed` from `t2_hashes - t1_hashes`, a Python set of SHA-256 hex
strings, so entry order follows those hashes. `onix` sorts entries by their
rendered path string — same findings, different order, the only one of the
five that is order-only. `test_set_entry_order_is_sorted_where_real_deepdiff_is_hash_ordered`
in `test_sets.py` pins onix's output (no golden case: `DeepDiff`'s own answer
is hash-order-dependent).

**Which member of an equality class wins.** `DeepHash` keys its shared cache
by `_make_hash_key(obj)` (`deephash.py`), so a Python-equal tuple or
frozenset hashed earlier in the run fixes the digest for every later
Python-equal one, and which member that is follows the process's set
iteration order. `onix` hashes each side's members in its own canonical set
order, so the winner never depends on process hash order.
`test_a_sets_report_does_not_depend_on_which_member_was_hashed_first` in
`test_sets.py` pins onix's output (no golden case, same reason as above).

**`list(a_set) == some_list`.** `DeepDiff`'s distance computation asks
whether applying the new side's type to the old value reproduces it
(`_from_tree_type_changes`'s `include_values`, `model.py`), and for a set
against a sequence that is `list(the_set) == the_list`, answered in the
set's own iteration order — so which of two orderings of one list keeps a
`type_changes` is process-dependent. `onix` compares the two by membership in
either ordering. `test_a_set_versus_a_list_is_a_type_change_whatever_the_order`
in `test_sets.py` pins onix's output (no golden case, same reason as above).

**A naive and an aware datetime at one instant are two Python set members,
but `DeepDiff` can report only one of them.** `_diff_set` groups members by
`DeepHash` digest, and `_prep_datetime` (`deephash.py`) normalizes every
datetime to its UTC instant before hashing, so the two land in the same
bucket; `_create_hashtable` keeps only one `{item, indexes}` entry per
bucket, and which member survives follows the set's own iteration order.
`onix` stores every structurally distinct member — identity for matching is
by instant, but `SetItems` keeps both.
`test_a_naive_and_aware_datetime_set_member_is_two_members_in_onix_one_in_deepdiff`
in `test_sets.py` pins both the bare and the one-level-nested-in-a-tuple
case.

**A tuple or a frozenset set member matches order- and
repetition-insensitively in real `DeepDiff`, not by Python `==`.**
`DeepHash._prep_iterable` (`deephash.py`) runs with `ignore_iterable_order`/
`ignore_repetition` for every iterable it hashes, so a tuple set member is
affected too. `onix` compares a tuple member positionally (`tuple.__eq__`)
and a frozenset member by membership.
`test_a_tuple_set_member_matches_by_position_where_deepdiff_ignores_order_and_repetition`
in `test_sets.py` pins both shapes.

Only "Entry order", "Which member of an equality class wins" and
`` `list(a_set) == some_list` `` are hash-order-dependent in real `DeepDiff`;
none of the five has a golden case, since a golden fixture pins byte parity
and these are deliberate divergences instead. The bindings' set fuzz batch
mechanically re-diffs each pair with every set rebuilt from its members in
reverse and skips a case where `DeepDiff` disagrees with itself; it caps a
nested `frozenset` inside a set item at one member, because a multi-member
one renders inside the entry's opaque path string with no positional key to
check.

**Canonical set order.** Everywhere a set's members become output — the JSON
array a set serializes to, and the members of a `frozenset` rendered inside a
`set_item_*` entry's path — `onix` emits them in one documented order, a
purely structural comparison. The rule lives on `onix_core::value::SetItems`'s
own doc; `scripts/golden_tags.py`'s `canonical_set_order` is its Python twin.

**`frozenset` values are a superset, not a difference.** `DeepDiff`'s own
`to_json()` raises `TypeError` on a report holding a `frozenset` (e.g. the
`new_value` of a set-vs-frozenset `type_changes`); `onix` serializes it as an
array. No golden case (the corpus's `expected.json` could not be generated);
`test_onix_serializes_a_frozenset_where_real_deepdiff_refuses` in
`test_sets.py` pins it.

## Non-finite floats: where onix is deliberately different

For a comparison, `NaN`/`Infinity`/`-Infinity` behave exactly like real
`DeepDiff`, verified against `deepdiff==9.1.0`: `NaN != NaN`,
`Infinity == Infinity`, and `to_json()` renders the same bare
`NaN`/`Infinity`/`-Infinity` tokens Python's `json.dumps` writes by default
(`allow_nan=True`) rather than quoting them or raising. Under `ignore_order`,
matching agrees too: `DeepHash` digests any `NaN` by `str(obj)`
(`deephash.py::_prep_number`), which is the same three characters for every
`NaN` regardless of its sign or payload bits, so every `NaN` in a list, dict
value, or set member matches every other one; `onix`'s `ItemKey::Float`
reproduces that by collapsing every `NaN` onto one shared key (see
`crate::ignore_order::hash::deephash_float_bits`'s own doc, in
`crates/onix-core/src/ignore_order/hash.rs`), so this is not a golden gap —
these cases could be goldens, except that `serde_json` (which the golden
pipeline's `expected.json` round-trips through) cannot parse the bare `NaN`
token back out of JSON text at all; they are pinned instead in
`crates/onix-py/tests/test_non_finite.py`, comparing rendered JSON text
directly (sidestepping `NaN != NaN` in the comparison itself).

The one real divergence, in every case deterministic and traceable to the
same cause: this crate's value model carries Python object identity for custom
objects only, and real `DeepDiff`/`CPython` also use it for a float or a
container as a shortcut that lets one `NaN` match itself where two
independently-obtained `NaN`s would not:

```text
DeepDiff(nan, nan) where both sides are literally the same object (t1 is t2)
  -> {} : DeepDiff's own diff() short-circuits on t1 is t2 before comparing
DeepDiff(nan, nan) with two distinct NaN objects -> values_changed
onix -> values_changed always for a float or a container holding one:
  DeepDiff({"a": nan_list}, {"a": nan_list}) with one shared [nan] list is {},
  onix reports values_changed root['a'][0]
onix -> {} for the same custom object on both sides, like DeepDiff:
  DeepDiff({"a": o}, {"a": o}) with o = O2(float("nan"))
```

```text
{nan_a, nan_b}  (two distinct NaN objects, same bits) carried whole as an
  added/removed *value* rather than diffed member by member, e.g.
  DeepDiff({'a': 1}, {'a': 1, 'b': {nan_a, nan_b}})
  -> dictionary_item_added: {"root['b']": {nan, nan}}, the real, untouched
     2-member set (hash(nan) collides, but the identity-then-equality probe
     finds neither `is` nor `==`, so a real Python set keeps both)
onix -> SetItems::new (crate::value) canonicalizes at conversion time, before
  the diff runs: two bit-identical NaNs fold into one canonical member, so
  the carried set has one member, not two — deterministic (same bits always
  dedup the same way, regardless of insertion order), but a real divergence
  from this specific real-Python set. `_diff_set` itself (comparing two sets
  against each other, member by member, with or without `ignore_order`)
  already reduces to one entry per content digest on *both* tools' side —
  see the "which member of an equality class wins" point above — so this
  divergence is visible only when the set survives whole into the report,
  never in the set_item_added/set_item_removed categories themselves.
```

```text
A list holding the SAME NaN object twice, compared against a shifted copy
  -> difflib's b2j index (a dict) matches that position to itself via the
     identity-before-equality probe CPython dict/list internals use
onix -> never matches: `ScalarKey::Nan` keys by the compact Value node's own
  address (crate::lcs::python_scalar_key, crates/onix-core/src/lcs.rs), which
  is fresh per occurrence by construction — the same posture DeepDiff takes
  for two independently-obtained NaN objects, just applied uniformly
```

`crates/onix-core/src/value.rs`'s `SetItems::new` doc and
`crates/onix-core/src/lcs.rs`'s `ScalarKey::Nan` doc have the full mechanism
for the second and third points; `crates/onix-core/src/ignore_order/tests.rs`'s
`dist_key_hash_collision_on_distinct_nans_never_becomes_equality` pins that a
hash collision between two distinct `NaN`s (deliberate, matching `DeepHash`)
never becomes a false equality in the distance memo.

## Custom objects: where onix is deliberately different

A custom object (an instance of a user-defined class) is diffed by its
attributes, matching DeepDiff's `_diff_obj`: `attribute_added`/`attribute_removed`
and `root.attr` paths, and `type_changes` between two classes that are not the
same. onix enumerates attributes exactly as `_diff_obj` does — the instance
`__dict__` plus the non-callable, non-dunder names `dir()` adds (class attributes
and `@property` values, read through `getattr`), or the slot values up the MRO
for a slots-only class — dropping dunder (`__x`) names and keeping
single-underscore (`_x`) and name-mangled (`_Cls__x`) ones. An `Enum` member is
read as `_diff_enum` reads it: `name` and `value` only. Two instances of one
plain class (only public instance attributes: no `@property`, class attribute, or
private) match DeepDiff byte-for-byte, including under `ignore_order` and as
list/dict values.

Class identity is the class object itself, as DeepDiff's `type(t1) != type(t2)`
compares it (the rule is stated once, on `Object::same_class` in
`crates/onix-core/src/value.rs`), plus the kind (`dict` subclass versus
attribute-diffed object). The rendered `old_type`/`new_type` stays the short
`__name__` DeepDiff shows.

Divergences, all deterministic. The first three (a whole object's serialized
value, `ignore_order` hashing, and `to_dict`) are the object-view gaps tracked in
[#99](https://github.com/ksco92/onix/issues/99), where onix keeps one attribute
view and DeepDiff uses three.

### A whole object's serialized value

When an object appears as a whole value in a report (a `type_changes`'
`old_value`/`new_value`, an object added to a list/dict, a
`threshold_to_diff_deeper` collapse), onix renders its full diffed attribute set.
DeepDiff's `to_json()` instead runs `serialization.json_convertor_default`, which
serializes **only** public `@property` values, or failing that only public
`__dict__` entries, and raises `TypeError` for a slots-only object with neither.
A class attribute is left out of both renders. For a plain class the two
coincide; they differ only for a whole-value object with one of these four
triggers: a `@property`, a private (`_x`) attribute, an `Enum` member (DeepDiff renders `{}`, onix its `name` and
`value`), or a **slot value on a class that also has `__dict__`** — for
`class SlotBase: __slots__ = ("p",)` and `class Mixed(SlotBase)` with `p` set,
`vars(m)` is `{}`, so DeepDiff renders an added `Mixed` as `{}` while onix
renders `{"p": "x"}` — and, separately, for a slots-only object, where DeepDiff
crashes and onix renders. onix's value is self-consistent (the value shown equals
the value diffed) and total.

### `ignore_order` object hashing

DeepDiff's `DeepHash._prep_obj` hashes an object by its raw `__dict__` (or its
slots), never the `dir()`-derived properties and class attributes `_diff_obj`
reads. onix hashes the one attribute view it holds (the diffed one), so pairing
can differ for an object whose `@property` values change what its diffed view
contains; a class attribute is the same for every instance of a class, and the
rough length leaves it out as `DeepHash` does. Both tag the hash with the class `__name__`, so a custom
object never pairs with a plain `dict`, while two distinct classes sharing a
`__name__` share a hash bucket in both. The pairing distance counts an object
the way DeepDiff's `_get_item_length` does (`len(obj.__dict__)`, and for an
`Enum` class the `__dict__` lengths of all its members) and adds the `DeepHash`
count of the `__dict__` entries onix does not extract; an `Enum` member's own
attributes each count as one node there, so one holding a container pairs by a
slightly different distance.

### `to_dict()` returns attribute dicts, not the original objects

DeepDiff's `to_dict()` hands back the original instances (in a `type_changes`, an
added item); onix converts every input to its value model up front and cannot
reconstruct an instance, so `to_dict()` returns the object's attribute `dict`.
`to_json()` is the byte-parity target and is unaffected.

### A recursive object

DeepDiff's `parents_ids` skips a child whose first-side object is already on the
path from the root, so a self-referential object or a parent pointer reports
nothing for the cycle. onix holds such a child as a cycle token: on the first side
it reports nothing; on the second side only, it is compared as the object it
points back at, so `a.me = 5` against `b.me = b` is a `type_changes` as in
DeepDiff. A cycle token a report shows renders as `{}` where DeepDiff's
`to_json()` raises on the circular reference.

### Types DeepDiff routes to a handler onix lacks

The accept-list is derived from `_diff`'s isinstance ladder. DeepDiff sends a
**number** (`complex`, `Decimal`, `Fraction`, a `numpy` scalar or
`numpy.datetime64` — `_diff_numbers`, or `_diff_booleans` for `numpy.bool_`), an **iterable** (`bytes`, `bytearray`, `memoryview`,
`range`, a generator, `deque`, `array.array`, any `__iter__`-defining object —
`_diff_str`/`_diff_iterable`), a **`uuid`** (`_diff_uuids`), an **`ipaddress`**
value (`_diff_ipranges`) to dedicated handlers before ever reaching `_diff_obj`; a class object it diffs by its class `__dict__` and a module
likewise. onix implements none of those. At the root such a value raises a
typed, path-naming `TypeError` rather than being reshaped into an object — which
for the attribute-less ones (`complex`, a bare `object()`) would otherwise
silently report `{}` for two *unequal* values.

Below the root, DeepDiff's `_diff` returns before any handler when `t1 is t2`, so
a value both sides share is never diffed. onix skips two custom objects converted
from the identical Python object the same way, and holds a value it cannot
convert as an identity token, equal only to the same object. A class attribute is
a token until a report compares it with a value that shadows it; it is then
converted once for the diff (a class-level `Enum` member, config object or
`re.Pattern`), so the shadowing value compares against the default's value. A
class attribute onix cannot convert (ABCMeta's `_abc_impl`, a class-level lock)
raises `TypeError` there, and one nested past `max_depth` on its own raises
`MaxDepthError`. An `Exception` while reading an object's attributes (a property
that raises) keeps the object with its instance `__dict__`, equal only to itself
and hashed under `ignore_order` by that `__dict__` as `DeepHash` hashes it: it
raises its error only where a walk compares it with an object of its class, as
DeepDiff does, and a report showing it whole renders that `__dict__` (DeepDiff's
render shows only its public entries, tracked in #99). An `Exception` while
converting what the attributes hold (an unsupported dict key, a value nested past
`max_depth`) makes the innermost object being converted a token that raises
wherever a report compares or shows it. The same object on both sides reports
nothing either way; `KeyboardInterrupt` and `SystemExit` raise at once. A whole object in a report leaves every class
attribute out, as DeepDiff's `to_json()` render does. A token raises a
`TypeError` naming its path wherever a report would have to show it: as the
compared value of a finding, or as an instance attribute of an object in the
report. Two equal but distinct unsupported objects (`Decimal("1")` built twice)
therefore raise where DeepDiff reports nothing, and an added object holding one
raises where DeepDiff renders it.

### Pydantic models

DeepDiff's ladder checks a `pydantic` `BaseModel` just before `Iterable` and diffs
it with `_diff_obj`, reading its fields and, from the class, its other attributes;
`DeepHash` and `_get_item_length` instead treat the model as the iterable of
`(field, value)` pairs it is. onix refuses a model, with the root `TypeError` or an
identity token below the root, where DeepDiff diffs it.

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
`BaseException` such as `KeyboardInterrupt`) propagates out of onix as that Python
exception, matching DeepDiff. An `AttributeError` while reading an object's
attributes (a `@property` that raises it, or an unset slot on a class that also
has `__dict__`) raises a `TypeError` naming the object's path, where DeepDiff
reports the whole object as `unprocessed`, a report category onix does not
implement.

A `namedtuple` is unaffected by all of the above: it is a `tuple` subclass and
diffs positionally (`root[1]`, not `root.field`) — the pre-existing, separately
documented divergence, unchanged here.

### Subclasses

A dict key that is a `tuple`/`datetime`/`date` subclass, including a
`namedtuple`, classifies as its base type: `classify_dict_key`
(`crates/onix-py/src/convert.rs`) tracks no class name for a key, matching
`DeepDiff`'s own key matching (`_diff_dict`'s `t2_keys & t1_keys`, plain
Python `==`/`hash()`, `diff.py`), which never consults `type(obj)`. Where
the two diverge is an overridden `__eq__`/`__hash__`: `DeepDiff` matches by
the subclass's own equality, `onix` matches by the base type's structural
value — confirmed live, a `tuple` subclass whose `__eq__` always returns
`True` and `__hash__` is always `0` makes `{K((1, 2)): "v1"}` vs
`{K((3, 4)): "v2"}` a `values_changed` at `root[3][4]` for `DeepDiff`,
where `onix` reports the whole dict changed at `root` instead. See
`crates/onix-py/tests/test_conversions.py`'s
`test_a_key_subclass_with_overridden_equality_matches_structurally_not_by_python_eq`
and the differential fuzz batch
(`test_differential_fuzz_with_subclass_dict_keys_matches_real_deepdiff`).

## Known DeepDiff quirks

- **Key quoting does not escape anything.** `DeepDiff`'s path rendering never
  escapes a backslash, a control character, or an embedded quote character,
  unlike Python `repr()`. The exact rule, confirmed against
  `deepdiff==9.1.0`, lives in `onix_core`'s `path::quote_key` doc
  (`crates/onix-core/src/path.rs`); the `key_single_quote`/
  `key_double_quote`/`key_both_quotes`/`key_backslash`/`key_control_chars`
  golden cases pin it.

- **A set item is quoted by a different rule again**, in
  `onix_core::path::set_item_repr`'s own doc, which also carries the
  upstream code it reproduces
  (`model.py::TextResult._from_tree_set_item_added_or_removed`); the
  `set_str_item_*` and `set_str_inside_tuple_item` golden cases pin it.

- **A non-`str` dict key's path renders via `repr()`, with a `tuple` key
  split into one bracket group per element.** `DeepDiff`'s
  `ChildRelationship.stringify_param` (`model.py`) special-cases exactly
  `tuple`: `isinstance(param, tuple)` renders `']['.join(map(repr, param))`,
  so `(1, 2)` produces `root[1][2]`, never `root[(1, 2)]`; every other
  non-`str` key renders as plain `repr(param)` — confirmed against real
  `deepdiff==9.1.0`. See `onix_core::path::dict_key_repr`'s doc; the
  `dict_key_*` golden cases cover each kind.

- **A dict key matches across two dicts by Python `==`, not by type.**
  `DeepDiff`'s `_diff_dict` (`diff.py`) builds its added/removed/shared key
  sets with `t2_keys & t1_keys` (`SetOrdered` intersection, itself Python
  `==`/`hash()`), so `1`, `1.0`, and `True` are the same key between two
  dicts even though `onix`'s own `ObjectKey` keeps them structurally
  distinct everywhere else — confirmed: `DeepDiff({1: "a"}, {1.0: "a"})` is
  `{}`. `SetOrdered.intersection` also keeps `t2`'s (`b`'s) key object on a
  match, so `{1: "a"}` vs `{1.0: "a2"}` reports `root[1.0]`, `b`'s form,
  never `a`'s. `onix` reproduces both halves in
  `crate::ignore_order::match_dict_keys`, used by
  `crate::diff::object::object_diff_mixed` whenever either side has a
  non-`str` key. `dict_key_int_vs_float_same_value_matches_by_python_equality`,
  `dict_key_int_vs_float_changed_value_matches_by_python_equality`, and
  `dict_key_bool_vs_int_matches_by_python_equality` pin it.

- **`to_json()` on a *nested* dict value with a non-`str` key mirrors
  `json.dumps`'s key-stringification rule where Python has one, and
  diverges where `DeepDiff` itself crashes.** A *top-level* path segment
  always renders via `repr()` (the bullet above); a nested dict *value* has
  no such literal in JSON, so Python's `json.dumps` stringifies a
  `bool`/`None`/`int`/`float` key (`"true"`/`"false"`, `"null"`, the same
  shortest-round-trip form a float *value* gets) and `onix` matches exactly
  (`dict_key_nested_value_stringifies_bool_none_float_keys`). A
  `datetime`/`date`/`tuple` key has no such rule: `json.dumps` (and so
  `DeepDiff.to_json()`) *raises* `TypeError` — confirmed against real
  `deepdiff==9.1.0` — so per this crate's compatibility policy `onix`
  renders the same `repr()` text that key would get as a top-level path
  segment instead. No golden fixture can hold this crash case; pinned in
  `crates/onix-core/src/value_tests.rs`'s
  `to_serde_json_stringifies_a_tuple_key_via_python_repr_where_deepdiff_would_crash`
  and its `datetime` twin.

- **A key can make path rendering collide, in both tools.** A dict key
  whose own text contains `']['`-shaped syntax (e.g. `p'"]["q'`) renders
  identically to an unrelated, differently-nested path (e.g.
  `{"p'": {"q'": ...}}`); `DeepDiff`'s own `to_json()` collapses this to one
  entry too. Which finding survives the collapse differs: `DeepDiff`'s
  follows its Python dict's original key insertion order, `onix` traverses
  `serde_json::Map`'s keys alphabetically (no `preserve_order` feature). See
  `crate::report`'s module doc for the structural- vs. rendered-path keying
  rule this follows from. The `path_rendering_collision` golden case and
  `crates/onix-core/tests/golden.rs`'s dedicated test (no panic, valid
  `DeepDiff`-shaped output, onix's own deterministic survivor) pin it.

- **`[1]` vs `[1.0]` inside a list diffs to nothing at all.** This is real
  `DeepDiff` behavior, faithfully reproduced, not a divergence: the
  list-LCS matcher's notion of "equal" is Python's `==` (`1 == 1.0 ==
  True`), not `DeepDiff`'s usual type-aware scalar comparison, and a matched
  `'equal'` opcode is never diffed further — so `DeepDiff([1], [1.0])` is
  `{}`, unlike the same pair inside a dict (`{"a": 1}` vs `{"a": 1.0}`),
  which is a `type_changes` as usual. See `crates/onix-core/src/lcs.rs`'s
  `ScalarKey`/`find_longest_match` doc;
  `list_lcs_int_vs_float_single_matches_via_python_equality` pins it.

- **A hashable tuple can inherit another tuple's hash.** Under
  `ignore_order`, `DeepHash` keys its shared cache by the object itself
  across the whole run, so a tuple Python-equal to one hashed earlier
  inherits its digest: `DeepDiff([(1,)], [(1.0,)])` is empty and
  `DeepDiff([(1,), (1.0,)], [])` reports a single removal, of whichever
  member is hashed first. `onix` reproduces the rule the cache implements —
  Python `==` with bare numbers type-wrapped — deterministically; see the
  `ignore_order_tuple_digest_*` cases and `docs/design/ignore-order.md`'s
  "Distance memo" section for the mechanism. `frozenset` is hashable too and
  `DeepDiff` caches one the same way (`[frozenset({1}), frozenset({1.0})]`
  vs `[]` reports a single removal); `onix` does not reproduce that survivor
  choice — see "Set iteration order"'s "Which member of an equality class
  wins" point, and `ignore_order_unhashable_set_never_collides` for the
  `set` case, where a `set` is unhashable in both tools — but does match the
  equality rule itself (`set_tuple_item_python_equality`,
  `set_frozenset_item_python_equality`).

- **A `namedtuple` is diffed positionally, not by field.** `DeepDiff` walks
  a `namedtuple`'s fields too (`deephash.py::_prep_tuple`), reporting
  `root[0].x` rather than `root[0][0]`. `onix` accepts a `namedtuple` as an
  ordinary `tuple` subclass — carrying its class name into a `type_changes`
  entry like any other tuple subclass — but diffs its contents positionally
  like every other tuple: a permanent divergence, not an approximation.
  `crates/onix-py/tests/test_tuples.py` asserts both outputs side by side.
  No golden case: the corpus's tagged encoding has no tag for a
  `namedtuple`.

- **Every other subclass** — `list`/`tuple`/`set`/`frozenset`/`dict`, and a
  `datetime`/`date`/`time`/`timedelta` subclass such as pandas' `Timestamp`
  — **is accepted and compares exactly as its base type**, carrying its own
  class name into a `type_changes` entry, matching `DeepDiff`'s
  `type(obj).__name__` rule exactly (one restriction: a `tuple`/`frozenset`
  subclass, including a `namedtuple`, is not accepted as a `set` member).
  See `crates/onix-core/src/value.rs`'s `Typed` doc and
  `crates/onix-py/src/convert.rs`'s module doc for the conversion rules;
  `test_tuples.py`, `test_sets.py`, `test_datetimes.py` and
  `test_conversions.py` assert this against the real tool. No golden case
  uses a subclass, for the same tagged-encoding reason as above.

- **`to_dict()` reports a `type_changes` entry's types as names, not
  classes.** Real `DeepDiff` puts the type objects themselves (`<class
  'tuple'>`) in `to_dict()`; `onix` puts the same names its `to_json()` uses
  (`"tuple"`). Values are unaffected. See
  `crates/onix-py/src/deepdiff.rs`'s `to_dict` doc.

- **List-LCS numeric matching is exact only within `2^53`.** The matcher's
  cross-type equality (previous bullet) normalizes any integral value —
  `bool`, `int`, or a fraction-free `float` — to one shared bucket key, but
  only exactly for magnitudes an `f64` represents every integer up to
  (`2^53`, `9_007_199_254_740_992`); beyond that, two otherwise-equal large
  numbers compare by `f64` bit pattern instead of exact value. Real Python
  performs exact arbitrary-precision comparison here. An accepted, narrow
  limitation of this port; no golden case exercises it.

- **`ignore_order` pairing among naive datetimes depends on the process's
  local timezone in `DeepDiff`, but not in `onix`.** `distance.py`'s
  `_get_datetime_distance` ranks a candidate pair through
  `datetime.timestamp()`, which reads a naive value in the local timezone,
  so real `DeepDiff`'s pairing for a list mixing naive and aware datetimes
  is machine-dependent. `onix` has no timezone database and reads a naive
  value as UTC everywhere, matching `datetime_normalize`. The two agree
  exactly once the process timezone is UTC. See `distance_family`'s doc
  (`crates/onix-core/src/ignore_order/distance.rs`); the `utc_timezone`
  fixture in `crates/onix-py/tests/test_differential_fuzz.py` pins the
  comparison. No golden case: regeneration is byte-stable under any `TZ`.

- **A datetime whose UTC form leaves year `1..=9999` cannot be compared to
  another datetime.** Normalizing e.g. `9999-12-31T23:00-01:00` lands on
  year 10000, and real `astimezone(timezone.utc)` raises `OverflowError:
  date value out of range` there, so `DeepDiff` raises rather than
  reporting anything; `onix` raises too, as
  `onix_core::Error::DateTimeOutOfRange`, surfaced to Python as a
  `ValueError` naming the path. On the ordered path only `_diff_datetime`
  normalizes, so both tools raise only when two datetimes are actually
  compared. Under `ignore_order`, `DeepHash._prep_datetime` normalizes
  every datetime it hashes, so real `DeepDiff` raises even for a value
  merely added, removed, or shuffled; `onix` hashes by instant and reports
  it raw instead, per the compatibility policy.
  `an_unnormalizable_datetime_under_ignore_order_is_reported_raw` in
  `crates/onix-core/src/diff/tests.rs` pins onix's side.

- **An integer beyond `f64::MAX` (about `2**1024`) crashes `DeepDiff` under
  `ignore_order`; `onix` reports the change.**
  `distance.py::_get_numbers_distance` runs `float(num1)` *outside* its
  `try`, and `float(2**1024)` raises `OverflowError: int too large to
  convert to float` — confirmed against `deepdiff==9.1.0`. `onix` reads
  such an integer as a saturated `f64` infinity (`num-bigint`'s `to_f64`),
  so the pair's distance short-circuits and it reports `values_changed`,
  per the compatibility policy. A crash-class case directory carries the
  usual `a.json`/`b.json`/`options.json` plus a two-key `expected.json`
  (`deepdiff_raises`: the exception `DeepDiff` raises; `onix`: onix's own
  report, big integers `$bigint`-tagged); both readers detect a crash case
  by the presence of the `deepdiff_raises` key.
  `ignore_order_big_int_beyond_f64_pairs_without_panicking` in `golden.rs`
  pins the `onix-core` render (which has no exact big-int form through
  `serde_json`); `crates/onix-py/tests/test_golden_parity.py` asserts the
  bindings' byte-exact `to_dict()` against `decode_tags(expected["onix"])`.
  `golden.rs`'s `every_deepdiff_crash_case_is_pinned` asserts every case
  carrying the marker is registered.

- **A `time` hashes by whole seconds-of-day under `ignore_order`, dropping
  the microsecond and any offset entirely.** `DeepHash._prep_datetime`
  (reused for `time` via `helper.times = (datetime.datetime, datetime.time,
  np_datetime64)`) calls `datetime_normalize`, which for a `time` returns
  only `time_to_seconds(obj)` (`(hour*60+minute)*60+second`), never reading
  `utcoffset()` — live-confirmed against `deepdiff==9.1.0`: a
  microsecond-only or an offset-only difference both report `{}` in a list
  under `ignore_order=True`. A real, faithfully-reproduced `DeepHash`
  quirk, not a port bug; a `timedelta` has no analogous quirk (`_prep_number`
  hashes it exactly). See `onix_core::ignore_order::hash`'s
  `hash_seconds_of_day` doc; `ignore_order_time_microsecond_only_difference_hash_matches`
  and `ignore_order_time_offset_only_difference_hash_matches` pin it.

- **A `str` (or a dict key) containing a lone (unpaired) surrogate code
  point is accepted and compared like any other `str`, matching real
  `DeepDiff`'s plain Python `==`.** `crates/onix-py/src/convert.rs`'s
  `pystring_to_cstr` reads the zero-copy UTF-8 path first and, only on that
  borrow's failure, falls back to `str.encode('utf-8', 'surrogatepass')`
  (the CPython idiom that yields WTF-8 bytes, `onix_core::value::Str`);
  equality, ordering, path rendering, and `to_dict()`'s reconstruction all
  follow from that split. The one accepted divergence is **hashing** one —
  a `set`/`frozenset` member, or any value once `ignore_order=True`: real
  `DeepDiff` crashes with an unhandled `UnicodeEncodeError` from
  `deephash.py`; `onix` hashes by code point and reports deterministically
  instead, per the compatibility policy. No golden case: the corpus's JSON
  writer cannot hold this content either (`ensure_ascii=False` raises the
  same `UnicodeEncodeError`). `crates/onix-py/tests/test_conversions.py`
  pins the directed cases (plus a genuine non-BMP character converting
  normally) and `test_differential_fuzz.py`'s surrogate batch runs
  `SEED_COUNT` generated cases through both engines.

`crate::diff::object_diff` (the ordinary dict-vs-dict diff, used identically
whether or not `ignore_order` is set) implements `DeepDiff`'s
`threshold_to_diff_deeper=0.33` (`_diff_dict`, `diff.py`): a dict-vs-dict
comparison whose key overlap (intersection / union) is below `0.33`
collapses into a single wholesale `values_changed` instead of recursing key
by key, at every nesting level including the root. See the
`threshold_collapse_*` golden cases for the boundary, nesting, and
dict-in-list coverage, and `ignore_order_nested_low_overlap_dict_pairing` /
`ignore_order_threshold_collapse_paired_dict` for the same collapse
surfacing inside a paired `ignore_order` subtree.
`crate::ignore_order::count_object_diff_leaves` and
`count_array_diff_leaves` apply the identical rule for the `ignore_order`
distance computation.

**An empty-`tuple` (`()`) dict key is a real, narrow `DeepDiff` bug, not
reproduced.** `stringify_param` (`model.py`) renders a `tuple` param as
`']['.join(map(repr, param))`, which for `()` is the empty string, so the
path comes back Python `None` and then the literal `"null"` dict key once
`to_dict()`'s report reaches `to_json()`. `onix` renders `root[]` instead —
deterministic, and at least a real path — per the compatibility policy. The
differential fuzz batch that generates `tuple` dict keys (issue #62)
excludes the empty tuple for this reason; see
`crates/onix-py/tests/test_differential_fuzz.py`'s `_gen_non_str_dict_key`.

**A non-finite `float` (`nan`/`inf`) dict key is also a real, narrow
`DeepDiff` bug, not reproduced.** `stringify_param`'s `repr()`-then-
`ast.literal_eval` round trip fails on `"nan"`/`"inf"` and silently
collapses the key to `None`, the same way the empty-tuple case above does —
confirmed live: `DeepDiff({}, {float('nan'): 1}).to_json()` is
`{"dictionary_item_added": {"null": 1}}`. `onix` renders the key
deterministically — `root[nan]` as a path segment, and the bare
`NaN`/`Infinity`/`-Infinity` token text wherever the key is embedded in a
carried JSON object — per the same compatibility-policy choice. See
`crates/onix-py/tests/test_non_finite.py`'s
`test_non_finite_dict_key_renders_without_crashing`.

**A `datetime`/`date` subclass dict key that `DeepDiff` must render as part
of any path segment is the same `stringify_param` bug again, not
reproduced.** A subclass's `repr()` looks like a constructor call, which
`literal_eval_extended` cannot parse back as a literal either, so the path
collapses to `None`/`"null"` the same way the two cases above do — not
only for an added/removed key, but for the same subclass key on both sides
holding a container value with its own internal change, since that also
needs a path built past the key: confirmed live, `{MyDT(...): {1, 2, 3}}`
vs `{MyDT(...): {1, 2, 4}}` gives `{'set_item_removed': ['None[3]'],
'set_item_added': ['None[4]']}`. `onix` renders the real key path
deterministically either way (a `tuple`/`namedtuple` key is unaffected —
`stringify_param` renders any tuple-shaped key positionally, never through
`repr()`). See `crates/onix-py/tests/test_differential_fuzz.py`'s
`_generate_subclass_key_case`.

Every other divergence found while building the corpus was fixed in
`onix-core` to match `DeepDiff` exactly. The path-rendering collision
exception, the multi-member nested-`frozenset`-rendering exception (both
above), the three set-iteration-order differences, the list-LCS `2^53`
limitation, the naive-datetime pairing timezone above, the `time`
seconds-of-day hashing quirk under `ignore_order` above, the
non-finite-float object-identity divergence documented under "Non-finite
floats" above, the lone-surrogate hashing divergence above, the
empty-tuple-key, non-finite-float-key and subclass-key repr bugs above, the
overridden-`__eq__`/`__hash__` key-subclass nuance in "Subclasses" above,
and the Unicode-version `str`-repr divergence documented under "Pinned
versions" above are the only accepted, documented exceptions —
`ignore_order`'s own differential-fuzz testing (thousands of cases across
both a general-purpose and a nested-low-overlap-dict-biased generator, see
`scripts/differential_fuzz.py`) found zero *other* unexplained divergences.
