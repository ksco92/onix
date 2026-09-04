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
├── expected.json  # json.loads(DeepDiff(t1, t2, verbose_level=2, **kwargs).to_json()),
│                  # re-dumped with sort_keys=True
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
| `$set` | `set` | reserved |
| `$frozenset` | `frozenset` | reserved |
| `$datetime` | `datetime.datetime` | reserved |
| `$date` | `datetime.date` | reserved |

So `{"$tuple": [1, 2]}` is the tuple `(1, 2)`, and `[{"$tuple": []}]` is a list
holding the empty tuple. **Any other object is plain data**, including one that has a
reserved key alongside others (`{"$tuple": [1], "x": 2}` is a two-key dict). The
reserved names are claimed all at once, before their types are supported, so a
fixture can never use one as an ordinary dict key and then change meaning later; a
decoder that meets a tag it cannot decode yet fails loudly.

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
as the one-key dict it literally is; each of those paths has a test pinning that.

`crates/onix-core/tests/golden.rs` reads every case directory present here
(there is no separate hand-maintained case list), runs
`onix_core::diff_with_options` with each case's own `options.json`, and
asserts the resulting report's canonical JSON (parsed `serde_json::Value`,
so object key order doesn't matter — array order and values do) equals
`expected.json` exactly, with one documented exception (see "Known DeepDiff
quirks" below). It runs as part of the normal `cargo test` / `make check`.

## Pinned versions

- **Python:** 3.13 (installed on demand by `uv`)
- **deepdiff:** `9.1.0` exactly (pinned in `scripts/gen_goldens.py`'s inline
  script metadata, resolved from PyPI's latest `8.x`+ line; see that file's `# /// script` header)

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
`crates/onix-core/src/diff/mod.rs`'s "List diffing" module doc for the
full spec these pin down.

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

**`ignore_order=True` (`ignore_order_*` cases):** pure shuffle, shuffle
plus a changed/added/removed value, duplicate-multiplicity invisibility,
nested-dict pairing, list-in-dict-in-list, mixed type changes, `[1]` vs
`[1.0]` (a real `type_changes` here, unlike the ordered LCS path's `{}`),
bool-vs-int never hash-equal, one-sided all-added/all-removed, the
`cutoff_intersection_for_pairs=0.7` gate on both sides, a worked
asymmetric-tie-break example, and index-drift `new_path` (both on a
real finding and confirmed absent on added/removed). Plus 20 seeded-random
fuzz cases (`_generate_ignore_order_fuzz_cases`). See
`crates/onix-core/src/ignore_order/mod.rs`'s module doc for the full,
source-cited spec this implements; `scripts/differential_fuzz.py` is a
separate, larger-scale (thousands of cases) fuzzer run during development,
not part of this fixed corpus.

## Known DeepDiff quirks

- **Key quoting does not escape anything.** `DeepDiff`'s path rendering
  never escapes backslashes, control characters, or a wrapping-quote
  character embedded in a key — unlike Python `repr()`, which `onix-core`'s
  `path::quote_key` originally (incorrectly) mimicked. The exact rule
  `quote_key` now implements (confirmed against `deepdiff==9.1.0` by
  `key_single_quote`/`key_double_quote`/`key_both_quotes`/`key_backslash`/
  `key_control_chars`) lives in that function's own doc
  (`crates/onix-core/src/path.rs`) — not duplicated here.

- **A key can make path rendering collide, in both tools.** `path_rendering_collision`:
  a dict key whose own text contains `']['`-shaped syntax (e.g. `p'"]["q'`)
  renders identically to an unrelated, differently-nested path (e.g.
  `{"p'": {"q'": ...}}`). `DeepDiff`'s own `to_json()` collapses this to one
  entry too (its output is a string-keyed dict), so onix following suit is
  correct, faithful behavior, not a bug. What is **not** chased: *which*
  finding survives the collapse. `DeepDiff`'s survivor depends on its
  Python dict's original key insertion order; `onix` traverses
  `serde_json::Map`'s keys in alphabetical order (no `preserve_order`
  feature), an unrelated ordering — reproducing `DeepDiff`'s exact choice
  would mean threading original JSON key order through the whole engine for
  a vanishingly rare edge case. `crates/onix-core/tests/golden.rs` checks
  this case with its own dedicated test (no panic, valid `DeepDiff`-shaped
  output, onix's own deterministic survivor) instead of exact-matching
  `expected.json`; see that test and `crate::report`'s module doc for the
  full mechanics (structural- vs. rendered-path keying).

- **`[1]` vs `[1.0]` inside a list diffs to nothing at all.** This is
  *not* a divergence — it is `DeepDiff`'s own real, faithfully-reproduced
  behavior, surprising as it looks: the list-LCS matcher's notion of
  "equal" is Python's `==` (which treats `1 == 1.0 == True`), not
  `DeepDiff`'s usual type-aware scalar comparison, and a matched `'equal'`
  opcode is never diffed further — so `DeepDiff([1], [1.0])` really is
  `{}`, unlike the *exact same pair* inside a dict (`{"a": 1}` vs
  `{"a": 1.0}`), which is a `type_changes` as usual. See
  `crates/onix-core/src/lcs.rs`'s `ScalarKey`/`find_longest_match` doc for
  the mechanics and `list_lcs_int_vs_float_single_matches_via_python_equality`
  for the golden case.

- **A hashable tuple can inherit another tuple's hash.** Under `ignore_order`,
  `DeepDiff([(1,)], [(1.0,)])` is **empty** and `DeepDiff([(1,), (1.0,)], [])` reports
  a single removal, because `DeepHash` keys its cache by the object itself and shares
  one cache across a whole run: a tuple that is Python-equal to one hashed earlier
  inherits its digest, while a tuple holding a list or a dict is unhashable and keeps
  its own. Which member of an equality class is hashed first is therefore observable,
  and reproduced — see the `ignore_order_tuple_digest_*` cases and the "Tuple digests"
  section of `crates/onix-core/src/ignore_order/memo.rs` for the full mechanism.
  **`frozenset` is hashable too**, so the same applies to it when set support lands.

- **A `namedtuple` is not diffed as a tuple; `deepdiff_rs` refuses it.** Real
  `DeepDiff` walks a `namedtuple`'s *fields* (`root[0].x`, with the class name as
  the reported type), because it inspects `_fields`/`_asdict`. This MVP has no
  field-walking conversion, and diffing one as a plain tuple would silently return
  index paths and the type name `tuple` instead — so the conversion raises
  `TypeError` naming the class, like any other unsupported type.
  `crates/onix-py/tests/test_tuples.py` asserts both the refusal and the real
  tool's own output. No golden case uses a `namedtuple`: the corpus's fixtures are
  JSON files, and the tagged encoding above deliberately has no tag for "an
  arbitrary class".

- **`to_dict()` reports a `type_changes` entry's types as names, not classes.**
  Real `DeepDiff` puts the type objects themselves (`<class 'tuple'>`) in
  `to_dict()`; `deepdiff_rs` puts the same names its `to_json()` uses (`"tuple"`).
  Values are unaffected — a tuple comes back as a tuple. See
  `crates/onix-py/src/deepdiff.rs`'s `to_dict` doc.

- **List-LCS numeric matching is exact only within `2^53`.** The matcher's
  cross-type equality (previous bullet) normalizes any integral value —
  including a `bool`, an `int`, or a `float` with no fractional part — to
  one shared bucket key so `1`/`1.0`/`true` all match each other, but only
  does so exactly for magnitudes an `f64` can represent every integer up
  to precisely (`2^53`, `9_007_199_254_740_992`); beyond that, two
  otherwise-equal large numbers compare by `f64` bit pattern instead of by
  exact value. Real Python performs exact arbitrary-precision int/float
  comparison here even for huge numbers — an accepted, narrow, documented
  limitation of this port rather than a chased-down divergence (no golden
  case exercises it; the benchmark fixtures' integers stay well under
  this bound).

`crate::diff::object_diff` (the ordinary dict-vs-dict diff, used
identically whether or not `ignore_order` is set) implements `DeepDiff`'s
`threshold_to_diff_deeper=0.33` (`_diff_dict`, diff.py): a dict-vs-dict
comparison whose key overlap (intersection / union) is below `0.33`
collapses into a single wholesale `values_changed` (old/new value the
whole dict) instead of recursing key by key, at every nesting level
including the root. Repro: `{"a": 1, "b": 2, "c": 3}` vs
`{"d": 4, "e": 5, "f": 6}` — one `values_changed` at the dict's own path.
See the `threshold_collapse_*` golden cases for the boundary (exactly
`0.33` does not collapse; just below does), nesting, and dict-in-list
coverage, and `ignore_order_nested_low_overlap_dict_pairing` /
`ignore_order_threshold_collapse_paired_dict` for this collapse surfacing
inside a paired `ignore_order` subtree.
`crate::ignore_order::count_object_diff_leaves` applies the identical rule
for the `ignore_order` *distance* computation, and
`crate::ignore_order::count_array_diff_leaves`'s own trial sub-diff for a
nested array pair now measures a nested dict-vs-dict candidate through
this same real `object_diff` collapse, so no inflated leaf count can flip
a pairing decision.

Every other divergence found while building the corpus was fixed in `onix-core` to match
`DeepDiff` exactly. The two path-rendering exceptions above and the
list-LCS `2^53` limitation are the only accepted, documented exceptions —
`ignore_order`'s own differential-fuzz testing (thousands of cases across
both a general-purpose and a nested-low-overlap-dict-biased generator, see
`scripts/differential_fuzz.py`) found zero *other* unexplained
divergences.
