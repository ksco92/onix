"""Differential fuzz test: onix's Python bindings vs real DeepDiff on live objects.

Runs through the actual `deepdiff_rs.DeepDiff` class (not the fast JSON-string
path), so this exercises the Python-object-to-`Value` conversion layer itself,
not just the diff engine underneath it.

Seven batches, each of at least `SEED_COUNT` seeded cases run twice (ordered
and `ignore_order=True`): the JSON-shaped types; the same plus tuples, as
containers in their own right and as elements of lists, dicts and other
tuples; the same plus sets and frozensets, likewise; the same plus naive and
aware datetimes and dates anywhere in a nested value; flat, tightly clustered
calendar lists, which put maximum pressure on difflib alignment and
`ignore_order` pairing because near-identical candidates make every tie-break
observable; dict-wrapped calendar values against strings of themselves, which
is the shape the `str()` coercion decides; and, at `COMBINED_SEED_COUNT`
cases, the full alphabet drawn together in one generator run — tuples, sets,
frozensets, datetimes and dates all able to appear at any depth, including a
calendar value as a bare or nested set item (issue #21's own combination).
Every batch compares `to_json()` (canonically, i.e. parsed, since neither
tool promises a key order) *and* `to_dict()` by `==`, the comparison that can
see a tuple, a set, a `datetime` or a `date` where the JSON one cannot.

The three calendar batches and the combined batch run under a pinned
`TZ=UTC` — see `utc_timezone`.

The set and combined batches make two allowances, both for DeepDiff's own
dependence on the process's set iteration order (see
`tests/golden/README.md`):

- Anything that came out of a set is compared order-insensitively, since
  DeepDiff emits it in hash order and onix in its own canonical order.
- A case whose *DeepDiff* answer is itself order-dependent is skipped, and
  detected mechanically rather than guessed at: the same pair is diffed a
  second time with every set rebuilt from its members in reverse, and if
  DeepDiff disagrees with itself the case is one onix deliberately answers
  deterministically instead. Anything DeepDiff answers stably, onix must
  match.
- A further, narrower class is recognized the same way: a real divergence
  DeepDiff answers *stably* (so the `_reverse_sets` check above does not
  catch it) rather than order-dependent instability —
  `_is_known_set_sequence_coercion_divergence` (`tests/golden/README.md`'s
  "Set iteration order" section, its `` `list(a_set) == some_list` `` point),
  reachable through any batch's values once a set/frozenset can pair against
  a list/tuple under `ignore_order`, not only the set batch's own.

Every generator function that walks an *existing* `set`/`frozenset`'s members
(as opposed to building a fresh one) does so through `_deterministic_members`,
never Python's own `for item in a_set`: the live set's iteration order is
hash-bucket order, randomized per process for a `str` member
(`PYTHONHASHSEED`), and each member visited consumes one `rng` draw — so an
unordered walk would make the *generated case itself* depend on the process's
hash seed despite `rng` being seeded deterministically, silently breaking the
"same seed, same case" contract every batch here relies on. This was the root
cause of an intermittent failure in the combined-alphabet batch (seed
6000169): the generated `(a, b)` pair genuinely differed between process
runs, not `onix` or DeepDiff.

Real DeepDiff's own `to_json()` raises on a report holding a `frozenset`
value, where onix serializes it as an array; such a case is compared through
`to_dict()` alone.
"""

import datetime
import json
import random
import time
from collections.abc import Iterator
from typing import Final

import pytest
from deepdiff import DeepDiff as RealDeepDiff
from golden_tags import JSON_DEFAULT_MAPPING, canonical_set_order

from deepdiff_rs import DeepDiff as OnixDeepDiff

type JsonValue = (
    dict[str, "JsonValue"]
    | list["JsonValue"]
    | tuple["JsonValue", ...]
    | datetime.datetime
    | datetime.date
    | str
    | int
    | float
    | bool
    | None
)

DICT_KEYS: Final[list[str]] = ["a", "b", "c", "d", "e"]
SCALARS: Final[list[JsonValue]] = [
    None, True, False, 0, 1, -1, 2, 3, 0.0, 1.5, -2.25, "x", "y", "z", "",
]

# Comfortably over the >=500-case target; each case also runs twice (once
# ordered, once ignore_order=True), so this is 2x SEED_COUNT diffs per batch.
SEED_COUNT: Final[int] = 300

# The tuple and set batches draw from disjoint seed ranges, so the batches are
# independent corpora rather than the same shapes with extra types sprinkled in.
TUPLE_SEED_BASE: Final[int] = 1_000_000
SET_SEED_BASE: Final[int] = 5_000_000

# How often the set batch turns a generated sequence into a set or a frozenset.
SET_PROBABILITY: Final[float] = 0.4

# ...and so do the two calendar batches.
CALENDAR_SEED_BASE: Final[int] = 2_000_000
CLUSTERED_SEED_BASE: Final[int] = 3_000_000
STRINGIFIED_SEED_BASE: Final[int] = 4_000_000

# The combined batch (issue #21): tuples, sets, frozensets, datetimes and dates drawn
# together in one generator run, its own seed range, and its own (larger) count — the
# ">=500 seeded cases" the issue's combined-goldens requirement asks for, at fuzz scale.
COMBINED_SEED_BASE: Final[int] = 6_000_000
COMBINED_SEED_COUNT: Final[int] = 500

# The calendar batch's leaves: naive and aware datetimes across a bounded range,
# plus bare dates. The offsets deliberately include one that is not a whole
# number of minutes (which widens `isoformat()`'s suffix to `+HH:MM:SS`) and the
# extremes Python permits.
CALENDAR_EPOCH: Final[datetime.datetime] = datetime.datetime(2015, 1, 1)
CALENDAR_SPAN_SECONDS: Final[int] = 15 * 365 * 86400
UTC_OFFSETS: Final[list[int]] = [
    0, 3600, -3600, 5 * 3600 + 1800, -18000, 1830, -1830, 86399, -86399,
]
MICROSECONDS: Final[list[int]] = [0, 1, 123456, 999999]

# How often a calendar leaf is a date rather than a datetime, how often a
# datetime is naive rather than aware, and how often a calendar batch leaf is a
# calendar value at all rather than a plain scalar.
DATE_PROBABILITY: Final[float] = 0.25
NAIVE_PROBABILITY: Final[float] = 0.4
CALENDAR_LEAF_PROBABILITY: Final[float] = 0.6

# Two edits that only the tuple batch applies, both aimed at shapes this slice
# turns on and that kind-preserving mutation alone can never produce: flipping
# a sequence between list and tuple while keeping its items, and re-typing a
# number inside a tuple within Python's `1 == 1.0 == True` family (which is
# what makes DeepHash's cache hand two tuples the same digest). Kept
# low-probability so a case still usually differs in more ordinary ways too.
KIND_FLIP_PROBABILITY: Final[float] = 0.15
RETYPE_PROBABILITY: Final[float] = 0.25

# How often the calendar batch re-writes a datetime at another UTC offset,
# keeping the instant (see `_calendar_edge_mutations`).
OFFSET_SHIFT_PROBABILITY: Final[float] = 0.3

# How often the calendar batch replaces a calendar leaf with a *string* of
# itself. Half take `str()` (which `model.py`'s `new_t1 = new_type(change.t1)`
# reproduces, so a `type_changes` delta omits its new value and the pair stays
# within the pairing cutoff) and half take `isoformat()` (which it does not,
# for a datetime). Without this, no generated case ever puts a calendar value
# next to a string equal to its own `str()`, which is the exact shape a wrong
# `coerce_to_python_str` gets wrong.
STRINGIFY_PROBABILITY: Final[float] = 0.2

# The clustered batch's own knobs: a two-day window (so every value is a close
# candidate for every other), and a coin flip that makes an aware value the
# exact same instant as the naive one it was drawn from.
CLUSTER_EPOCH: Final[datetime.datetime] = datetime.datetime(2024, 1, 1)
CLUSTER_SPAN_HOURS: Final[int] = 48
SAME_INSTANT_TWIN_PROBABILITY: Final[float] = 0.5


def _deterministic_members(value: set[object] | frozenset[object]) -> list[object]:
    """
    Order a live `set`/`frozenset`'s members independently of `PYTHONHASHSEED`.

    A `set`/`frozenset` literal's own iteration order is Python hash-bucket
    order, which for a `str` member is randomized per process
    (`PYTHONHASHSEED`). Every generator function below that walks an
    existing set's members in a list comprehension consumes `rng` once per
    member, in iteration order -- so an unordered `for member in value` here
    would make the *sequence* of RNG draws (and therefore the mutated result)
    depend on the process's hash seed, breaking the "same seed, same case"
    contract every other batch in this file relies on, even though `rng`
    itself is seeded deterministically. `golden_tags.canonical_set_order` is
    a purely structural sort (no hashing), so it is stable across processes.

    :param value: The set or frozenset to order.
    :return: Its members, in onix's canonical set order.
    """
    return canonical_set_order(value)


def _gen_scalar(rng: random.Random, scalars: list[JsonValue] | None = None) -> JsonValue:
    """
    Pick a random scalar.

    :param rng: Seeded RNG.
    :param scalars: Alphabet to draw from; defaults to the module `SCALARS`.
    :return: A random scalar value.
    """
    return rng.choice(SCALARS if scalars is None else scalars)


def _gen_calendar(rng: random.Random) -> JsonValue:
    """
    Pick a random `date`, or a random naive or aware `datetime`.

    :param rng: Seeded RNG.
    :return: A calendar value, or a plain scalar for the rest of the alphabet.
    """
    if rng.random() >= CALENDAR_LEAF_PROBABILITY:
        return _gen_scalar(rng)

    value = CALENDAR_EPOCH + datetime.timedelta(
        seconds=rng.randrange(CALENDAR_SPAN_SECONDS), microseconds=rng.choice(MICROSECONDS)
    )

    if rng.random() < DATE_PROBABILITY:
        return value.date()

    if rng.random() < NAIVE_PROBABILITY:
        return value

    offset = rng.choice(UTC_OFFSETS)

    return value.replace(tzinfo=datetime.timezone(datetime.timedelta(seconds=offset)))


def _gen_value(
    rng: random.Random,
    depth: int,
    scalars: list[JsonValue] | None = None,
    tuples: bool = False,
    calendar: bool = False,
) -> JsonValue:
    """
    Generate a random JSON-shaped value, nesting up to `depth` levels.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget.
    :param scalars: Scalar alphabet to draw leaves from; defaults to `SCALARS`.
    :param tuples: Whether half of the generated sequences are tuples rather
        than lists. The RNG is only consulted for this when it is set, so the
        `False` corpus is exactly the one that existed before tuples were
        supported.
    :param calendar: Whether leaves may be datetimes and dates. As with
        `tuples`, the RNG is only consulted for this when it is set, so the
        two pre-existing corpora are bit-for-bit the ones they were.
    :return: A random value built from the MVP-supported types only.
    """
    if depth <= 0:
        return _gen_calendar(rng) if calendar else _gen_scalar(rng, scalars)

    kind = rng.random()

    # The tuple corpus leans harder on sequences (fewer bare scalars), so that
    # tuples actually show up in most cases rather than a minority of them.
    if kind < (0.3 if tuples else 0.5):
        return _gen_calendar(rng) if calendar else _gen_scalar(rng, scalars)

    if kind < 0.75:
        length = rng.randint(0, 4)
        items = [_gen_value(rng, depth - 1, scalars, tuples, calendar) for _ in range(length)]

        return tuple(items) if tuples and rng.random() < 0.5 else items

    keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))
    return {key: _gen_value(rng, depth - 1, scalars, tuples, calendar) for key in keys}


def _mutate(
    rng: random.Random, value: JsonValue, tuples: bool = False, calendar: bool = False
) -> JsonValue:
    """
    Build a related-but-different copy of `value` (shuffle + selective mutation).

    A sequence keeps its own kind: a mutated tuple is still a tuple, so a
    case's two sides differ in contents rather than in container type (which
    would make every case a single type change).

    :param rng: Seeded RNG.
    :param value: The value to derive a mutated copy from.
    :param tuples: Whether replacement values may themselves be tuples.
    :param calendar: Whether replacement values may be datetimes and dates.
    :return: A structurally related, partially mutated copy.
    """
    if isinstance(value, (list, tuple)):
        mutated = list(value)
        rng.shuffle(mutated)

        for index in range(len(mutated)):
            if rng.random() < 0.3:
                mutated[index] = _gen_value(rng, 2, tuples=tuples, calendar=calendar)

        return tuple(mutated) if isinstance(value, tuple) else mutated

    if isinstance(value, dict):
        mutated = dict(value)

        for key in list(mutated):
            if rng.random() < 0.3:
                mutated[key] = _gen_value(rng, 2, tuples=tuples, calendar=calendar)

        if rng.random() < 0.3:
            mutated[rng.choice(DICT_KEYS)] = _gen_value(rng, 2, tuples=tuples, calendar=calendar)

        return mutated

    return _gen_value(rng, 2, tuples=tuples, calendar=calendar)


def _generate_case(
    seed: int, tuples: bool = False, calendar: bool = False
) -> tuple[JsonValue, JsonValue]:
    """
    Generate one seeded `(a, b)` pair.

    :param seed: The seed driving this case's RNG.
    :param tuples: Whether the pair may contain tuples.
    :param calendar: Whether the pair may contain datetimes and dates.
    :return: A related-but-different `(a, b)` pair.
    """
    rng = random.Random(seed)
    a = _gen_value(rng, 3, tuples=tuples, calendar=calendar)
    b = _mutate(rng, a, tuples=tuples, calendar=calendar)

    if tuples:
        b = _tuple_edge_mutations(rng, b)

    if calendar:
        b = _calendar_edge_mutations(rng, b)

    return a, b


def _calendar_edge_mutations(rng: random.Random, value: object) -> object:
    """
    Apply the two calendar-specific edits: an offset shift and a stringify.

    Both make shapes ordinary mutation cannot. The offset shift produces two
    datetimes that are a *different* wall clock but the *same* moment, which
    DeepDiff reports as no difference at all — and, when one side is naive and
    the other aware, are not even Python-equal, so they reach the difflib
    `'replace'` and `ignore_order` pairing paths rather than matching outright.
    The stringify (see `STRINGIFY_PROBABILITY`) puts a calendar value next to a
    string of itself, which is what exercises the `str()` coercion the
    `type_changes` delta shape depends on. The `set`/`frozenset` branch exists
    for the combined batch (issue #21): the JSON-shaped and tuple batches never
    produce one, so it is simply never taken there.

    :param rng: Seeded RNG.
    :param value: The value to edit.
    :return: The edited value.
    """
    if isinstance(value, (set, frozenset)):
        members = [_calendar_edge_mutations(rng, item) for item in _deterministic_members(value)]

        return frozenset(members) if isinstance(value, frozenset) else set(members)

    if isinstance(value, (list, tuple)):
        items = [_calendar_edge_mutations(rng, item) for item in value]

        return tuple(items) if isinstance(value, tuple) else items

    if isinstance(value, dict):
        return {key: _calendar_edge_mutations(rng, item) for key, item in value.items()}

    if isinstance(value, datetime.date) and rng.random() < STRINGIFY_PROBABILITY:
        return str(value) if rng.random() < 0.5 else value.isoformat()

    if isinstance(value, datetime.datetime) and rng.random() < OFFSET_SHIFT_PROBABILITY:
        offset = rng.choice(UTC_OFFSETS)
        shifted = value.replace(tzinfo=datetime.timezone.utc) if value.tzinfo is None else value

        return shifted.astimezone(datetime.timezone(datetime.timedelta(seconds=offset)))

    return value


def _retype_number(rng: random.Random, value: JsonValue) -> JsonValue:
    """
    Re-type a number within Python's numeric equality family, keeping its value.

    :param rng: Seeded RNG.
    :param value: The number to re-type.
    :return: An equal value of a different type (`1` -> `1.0` or `True`), or
        `value` unchanged when no equal re-typing exists.
    """
    if isinstance(value, bool):
        return int(value) if rng.random() < 0.5 else float(value)

    if isinstance(value, int):
        if value in (0, 1) and rng.random() < 0.5:
            return bool(value)

        return float(value)

    if isinstance(value, float) and value.is_integer() and abs(value) < 2**53:
        return int(value) if rng.random() < 0.5 else bool(value) if value in (0.0, 1.0) else int(value)

    return value


def _tuple_edge_mutations(rng: random.Random, value: JsonValue, in_tuple: bool = False) -> JsonValue:
    """
    Apply the two tuple-specific edits recursively (see KIND_FLIP_PROBABILITY).

    :param rng: Seeded RNG.
    :param value: The value to edit.
    :param in_tuple: Whether `value` sits inside a tuple, which is where a
        numeric re-type is worth making.
    :return: The edited value.
    """
    if isinstance(value, (list, tuple)):
        as_tuple = isinstance(value, tuple)
        items = [_tuple_edge_mutations(rng, item, in_tuple=as_tuple) for item in value]

        if rng.random() < KIND_FLIP_PROBABILITY:
            as_tuple = not as_tuple

        return tuple(items) if as_tuple else items

    if isinstance(value, dict):
        return {key: _tuple_edge_mutations(rng, item) for key, item in value.items()}

    if in_tuple and isinstance(value, (bool, int, float)) and rng.random() < RETYPE_PROBABILITY:
        return _retype_number(rng, value)

    return value


def _normalize_types(value: JsonValue | type) -> JsonValue:
    """
    Replace any Python type object in a report with its name.

    Real DeepDiff's `to_dict()` reports a `type_changes` entry's `old_type`/
    `new_type` as the type objects themselves, where `deepdiff_rs` reports the
    names its `to_json()` uses (`"tuple"`, `"list"`, ...). That one difference
    is a documented gap of this MVP, so it is normalized away here rather than
    swamping every other comparison this test exists to make.

    :param value: A report, or any part of one.
    :return: The same value with type objects replaced by their names.
    """
    if isinstance(value, dict):
        return {key: _normalize_types(item) for key, item in value.items()}

    if isinstance(value, list):
        return [_normalize_types(item) for item in value]

    if isinstance(value, tuple):
        return tuple(_normalize_types(item) for item in value)

    if isinstance(value, type):
        return value.__name__

    return value


def _diverges(a: JsonValue, b: JsonValue, ignore_order: bool) -> tuple[JsonValue, JsonValue] | None:
    """
    Diff `a`/`b` with both engines and return both reports if they disagree.

    Both renderings are compared: the JSON one (canonically, by parsing) and
    the dict one, which is the only one that can see a tuple.

    :param a: The first value.
    :param b: The second value.
    :param ignore_order: Whether to diff with `ignore_order=True`.
    :return: `(expected, actual)` if they diverge, else `None`.
    """
    real = RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2)
    onix = OnixDeepDiff(a, b, ignore_order=ignore_order)

    # The mapping only matters for the calendar batch, where a report can carry
    # a `date` that DeepDiff's stock `to_json()` refuses to serialize; it is a
    # no-op for every other value. See `scripts/golden_tags.py`.
    expected_json = json.loads(real.to_json(default_mapping=JSON_DEFAULT_MAPPING))
    actual_json = json.loads(onix.to_json())

    if actual_json != expected_json:
        return expected_json, actual_json

    expected_dict = _normalize_types(real.to_dict())
    actual_dict = _normalize_types(onix.to_dict())

    if actual_dict != expected_dict:
        return expected_dict, actual_dict

    return None


def _run_batch(
    seeds: range, tuples: bool, calendar: bool = False
) -> list[tuple[int, bool, JsonValue, JsonValue, JsonValue, JsonValue]]:
    """
    Run one batch of seeded cases through both engines, ordered and ignore_order.

    :param seeds: The seeds to generate cases from.
    :param tuples: Whether the generated cases may contain tuples.
    :param calendar: Whether the generated cases may contain datetimes and dates.
    :return: One entry per diverging (seed, ignore_order) combination.
    """
    mismatches = []

    for seed in seeds:
        a, b = _generate_case(seed, tuples=tuples, calendar=calendar)

        for ignore_order in (False, True):
            divergence = _diverges(a, b, ignore_order)

            if divergence is not None:
                expected, actual = divergence
                mismatches.append((seed, ignore_order, a, b, expected, actual))

    return mismatches


def test_differential_fuzz_matches_real_deepdiff_ordered_and_ignore_order() -> None:
    """Runs SEED_COUNT seeded cases through both engines, both ordered and ignore_order=True."""
    mismatches = _run_batch(range(SEED_COUNT), tuples=False)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} fuzz cases diverged from real DeepDiff "
        f"(showing up to 3): {mismatches[:3]}"
    )


def test_differential_fuzz_with_tuples_matches_real_deepdiff() -> None:
    """Runs a second SEED_COUNT-case batch whose values also contain tuples."""
    seeds = range(TUPLE_SEED_BASE, TUPLE_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, tuples=True)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} tuple fuzz cases diverged from real DeepDiff "
        f"(showing up to 3): {mismatches[:3]}"
    )


def _gen_clustered_calendar(rng: random.Random) -> JsonValue:
    """
    Pick one calendar value from a deliberately tiny window.

    :param rng: Seeded RNG.
    :return: A `date`, or a naive or aware `datetime` within `CLUSTER_SPAN_HOURS`.
    """
    if rng.random() < DATE_PROBABILITY:
        return (CLUSTER_EPOCH + datetime.timedelta(days=rng.randrange(6))).date()

    value = CLUSTER_EPOCH + datetime.timedelta(
        hours=rng.randrange(CLUSTER_SPAN_HOURS), microseconds=rng.choice([0, 1])
    )

    if rng.random() < NAIVE_PROBABILITY:
        return value

    offset = rng.choice(UTC_OFFSETS)

    if rng.random() < SAME_INSTANT_TWIN_PROBABILITY:
        # The same moment as the naive value above, written at `offset` — a
        # pair Python's `==` rejects but DeepDiff's comparison accepts.
        value += datetime.timedelta(seconds=offset)

    return value.replace(tzinfo=datetime.timezone(datetime.timedelta(seconds=offset)))


def _generate_stringified_calendar_case(seed: int) -> tuple[JsonValue, JsonValue]:
    """
    Generate one seeded case of dict-wrapped calendar values against strings of them.

    Both halves of the shape matter. The **dict wrapper** is what makes a pair
    close enough to pair at all: two bare scalars have a rough length of 2
    between them, so even a `diff_length` of 1 lands on 0.5, over the 0.3
    pairing cutoff, while inside a one-key dict the same difference is 1/6.
    The **string** is `str()` of the value half the time (which `model.py`'s
    `new_t1 = new_type(change.t1)` reproduces, so the delta omits its new
    value and the pair qualifies) and `isoformat()` the other half (which for
    a datetime it does not). Getting that coercion wrong flips a
    `type_changes` into an unrelated add plus remove, and no other generator
    here produces a string equal to a sibling calendar value's `str()`.

    :param seed: The seed driving this case's RNG.
    :return: A related-but-different pair of lists of one-key dicts.
    """
    rng = random.Random(seed)
    values = [_gen_calendar(rng) for _ in range(rng.randint(1, 5))]
    a: list[JsonValue] = [{rng.choice(DICT_KEYS): value} for value in values]
    b: list[JsonValue] = []

    for entry in a:
        (key, value), = entry.items()

        if not isinstance(value, datetime.date):
            b.append({key: value})
        elif rng.random() < 0.5:
            b.append({key: str(value)})
        else:
            b.append({key: value.isoformat()})

    rng.shuffle(b)

    return a, b


def _generate_clustered_case(seed: int) -> tuple[JsonValue, JsonValue]:
    """
    Generate one seeded flat-calendar-list `(a, b)` pair.

    :param seed: The seed driving this case's RNG.
    :return: A related-but-different pair of flat lists.
    """
    rng = random.Random(seed)
    a = [_gen_clustered_calendar(rng) for _ in range(rng.randint(0, 7))]
    b = list(a)
    rng.shuffle(b)

    for index in range(len(b)):
        if rng.random() < 0.4:
            b[index] = _gen_clustered_calendar(rng)

    b = [_calendar_edge_mutations(rng, item) for item in b]

    if b and rng.random() < 0.3:
        b.pop(rng.randrange(len(b)))

    if rng.random() < 0.3:
        b.append(_gen_clustered_calendar(rng))

    return a, b


@pytest.fixture
def utc_timezone(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """
    Pin the process timezone to UTC for the duration of one test.

    Real DeepDiff ranks `ignore_order` pairing candidates with a call to
    `datetime.timestamp()`, which reads a *naive* datetime in the process's
    local timezone, so its pairing choice for a list mixing naive and aware
    datetimes is machine-dependent. onix reads a naive value as UTC
    everywhere; the two agree exactly once the process timezone is UTC. The
    rationale in full lives in `distance_family`'s doc
    (`crates/onix-core/src/ignore_order/distance.rs`).

    :param monkeypatch: pytest's environment patcher, which restores `TZ`.
    :return: Nothing; this is a setup/teardown fixture.
    """
    monkeypatch.setenv("TZ", "UTC")
    time.tzset()

    yield

    # `monkeypatch` restores the environment variable itself; libc still has to
    # be told to re-read it.
    time.tzset()


def test_differential_fuzz_with_calendar_values_matches_real_deepdiff(
    utc_timezone: None,
) -> None:
    """
    Run a third SEED_COUNT-case batch whose values also contain datetimes and dates.

    :param utc_timezone: Pins `TZ=UTC` — see that fixture for why it is needed.
    """
    seeds = range(CALENDAR_SEED_BASE, CALENDAR_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, tuples=False, calendar=True)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} calendar fuzz cases diverged from real "
        f"DeepDiff (showing up to 3): {mismatches[:3]}"
    )


def test_differential_fuzz_with_clustered_calendar_lists_matches_real_deepdiff(
    utc_timezone: None,
) -> None:
    """
    Run a fourth batch of flat, tightly clustered calendar lists.

    :param utc_timezone: Pins `TZ=UTC` — see that fixture for why it is needed.
        This batch is the one that actually depends on it: near-identical
        candidates make DeepDiff's own naive-`timestamp()` reading observable.
    """
    mismatches = []

    for seed in range(CLUSTERED_SEED_BASE, CLUSTERED_SEED_BASE + SEED_COUNT):
        a, b = _generate_clustered_case(seed)

        for ignore_order in (False, True):
            divergence = _diverges(a, b, ignore_order)

            if divergence is not None:
                expected, actual = divergence
                mismatches.append((seed, ignore_order, a, b, expected, actual))

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} clustered calendar fuzz cases diverged from "
        f"real DeepDiff (showing up to 3): {mismatches[:3]}"
    )


def test_differential_fuzz_with_stringified_calendar_values_matches_real_deepdiff(
    utc_timezone: None,
) -> None:
    """
    Run a fifth batch pairing dict-wrapped calendar values against strings of them.

    :param utc_timezone: Pins `TZ=UTC` — see that fixture.
    """
    mismatches = []

    for seed in range(STRINGIFIED_SEED_BASE, STRINGIFIED_SEED_BASE + SEED_COUNT):
        a, b = _generate_stringified_calendar_case(seed)

        for ignore_order in (False, True):
            divergence = _diverges(a, b, ignore_order)

            if divergence is not None:
                expected, actual = divergence
                mismatches.append((seed, ignore_order, a, b, expected, actual))

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} stringified calendar fuzz cases diverged from "
        f"real DeepDiff (showing up to 3): {mismatches[:3]}"
    )


def _gen_hashable(rng: random.Random, depth: int) -> object:
    """
    Generate a value a Python set can hold: a scalar, a tuple of them, or a frozenset.

    A set member is *rendered into* its finding's path, and rendering a
    frozenset means rendering its members in some order — DeepDiff's is
    Python's hash order, onix's is canonical, and no order-insensitive
    comparison can reconcile the two inside a single opaque path string. A
    frozenset generated here is therefore capped at one member, where the two
    orders provably coincide; the difference itself is pinned by name in
    ``test_sets.py``. Tuples are unrestricted, their order being positional in
    both tools.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget.
    :return: A hashable value.
    """
    if depth <= 0 or rng.random() < 0.55:
        return _gen_scalar(rng)

    if rng.random() < 0.5:
        return frozenset([_gen_hashable(rng, depth - 1)] if rng.random() < 0.75 else [])

    return tuple(_gen_hashable(rng, depth - 1) for _ in range(rng.randint(0, 3)))


def _gen_set_value(rng: random.Random, depth: int) -> object:
    """
    Generate a random value whose sequences are sometimes sets or frozensets.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget.
    :return: A random value built from the supported types, sets included.
    """
    if depth <= 0:
        return _gen_scalar(rng)

    kind = rng.random()

    if kind < 0.3:
        return _gen_scalar(rng)

    if kind < 0.8:
        if rng.random() < SET_PROBABILITY:
            members = [_gen_hashable(rng, depth - 1) for _ in range(rng.randint(0, 4))]

            return frozenset(members) if rng.random() < 0.4 else set(members)

        items = [_gen_set_value(rng, depth - 1) for _ in range(rng.randint(0, 4))]

        return tuple(items) if rng.random() < 0.3 else items

    keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))

    return {key: _gen_set_value(rng, depth - 1) for key in keys}


def _mutate_set_value(rng: random.Random, value: object) -> object:
    """
    Build a related-but-different copy of a set-batch value, keeping each container's kind.

    :param rng: Seeded RNG.
    :param value: The value to derive a mutated copy from.
    :return: A structurally related, partially mutated copy.
    """
    if isinstance(value, (set, frozenset)):
        members = [
            _gen_hashable(rng, 2) if rng.random() < 0.4 else member
            for member in _deterministic_members(value)
        ]

        if rng.random() < 0.3:
            members.append(_gen_hashable(rng, 2))

        return frozenset(members) if isinstance(value, frozenset) else set(members)

    if isinstance(value, (list, tuple)):
        mutated = [
            _gen_set_value(rng, 2) if rng.random() < 0.3 else _mutate_set_value(rng, item)
            for item in value
        ]

        return tuple(mutated) if isinstance(value, tuple) else mutated

    if isinstance(value, dict):
        mutated = {
            key: _gen_set_value(rng, 2) if rng.random() < 0.3 else item
            for key, item in value.items()
        }

        if rng.random() < 0.3:
            mutated[rng.choice(DICT_KEYS)] = _gen_set_value(rng, 2)

        return mutated

    return _gen_set_value(rng, 2)


def _set_edge_mutations(rng: random.Random, value: object, in_set: bool = False) -> object:
    """
    Apply the two set-specific edits recursively, mirroring `_tuple_edge_mutations`.

    Both target shapes ordinary kind-preserving mutation can never reach, and both are
    where a set's *iteration order* becomes load-bearing rather than cosmetic: flipping
    a container between the four kinds while keeping its members (which is what makes a
    `list(a_set) == some_list` coercion decide a pairing), and re-typing a number inside
    a set member within Python's `1 == 1.0 == True` family (which is what makes DeepHash
    hand two members one digest, so that *which* member was iterated first decides what
    gets reported).

    Whatever it is handed, this returns a hashable value whenever `in_set` is set: a
    list or a set cannot be a set member, so inside a set those kinds become their
    `tuple`/`frozenset` counterparts and a flip swaps only those two.

    :param rng: Seeded RNG.
    :param value: The value to edit.
    :param in_set: Whether `value` is itself a set member.
    :return: The edited value, hashable whenever `in_set` is set.
    """
    if isinstance(value, (set, frozenset, list, tuple)):
        # A set's members are set members, and hashability is transitive, so
        # `in_set` both starts at a set and carries down through it.
        member_in_set = in_set or isinstance(value, (set, frozenset))
        source = _deterministic_members(value) if isinstance(value, (set, frozenset)) else value
        members = [_set_edge_mutations(rng, item, in_set=member_in_set) for item in source]
        target = _HASHABLE_KIND[type(value)] if in_set else type(value)

        if rng.random() < KIND_FLIP_PROBABILITY:
            flipped = _FLIPPED_IN_SET[target] if in_set else _FLIPPED[target]

            # Outside a set, a flip *into* one is only possible when every
            # member happens to be hashable (a dict member forbids it).
            if flipped not in (set, frozenset) or all(map(_is_hashable, members)):
                target = flipped

        if member_in_set and target is frozenset and len(members) > 1:
            # Same cap, and same reason, as `_gen_hashable`'s: a multi-member
            # frozenset inside a set member renders its own members into an
            # opaque path string.
            target = tuple

        return list(members) if target is list else target(members)

    if isinstance(value, dict):
        return {key: _set_edge_mutations(rng, item) for key, item in value.items()}

    if in_set and isinstance(value, (bool, int, float)) and rng.random() < RETYPE_PROBABILITY:
        return _retype_number(rng, value)

    return value


def _is_hashable(value: object) -> bool:
    """
    Report whether `value` could be a set member.

    :param value: Any value.
    :return: True if `hash(value)` succeeds.
    """
    try:
        hash(value)
    except TypeError:
        return False

    return True


# The hashable counterpart of each container kind, for a value that has to be
# usable as a set member.
_HASHABLE_KIND: Final[dict[type, type]] = {
    set: frozenset,
    frozenset: frozenset,
    list: tuple,
    tuple: tuple,
}


# Outside a set, a flip swaps each kind with its unhashable/hashable
# counterpart, so every flip crosses the boundary `list(a_set)`-style coercion
# sits on. Inside one, only the two hashable kinds are available.
_FLIPPED: Final[dict[type, type]] = {set: list, list: set, frozenset: tuple, tuple: frozenset}


_FLIPPED_IN_SET: Final[dict[type, type]] = {frozenset: tuple, tuple: frozenset}


def _normalize_set_categories(report: dict[str, object]) -> dict[str, object]:
    """
    Sort the two set categories, whose order real DeepDiff draws from Python hash order.

    :param report: A report as either engine's `to_dict()` or parsed `to_json()` gives it.
    :return: The same report with both set categories as sorted lists.
    """
    return {
        key: sorted(value) if key in {"set_item_added", "set_item_removed"} else value
        for key, value in report.items()
    }


def _reverse_sets(value: object) -> object:
    """
    Rebuild every set and frozenset in `value` from its members in reverse.

    Python inserts a set literal's members in written order, and insertion order can
    change where a collision lands, so this often (not always) gives a set the same
    members in a different iteration order.

    :param value: The value to rebuild.
    :return: A copy with every set rebuilt.
    """
    if isinstance(value, (set, frozenset)):
        members = [_reverse_sets(member) for member in reversed(list(value))]

        return frozenset(members) if isinstance(value, frozenset) else set(members)

    if isinstance(value, (list, tuple)):
        members = [_reverse_sets(item) for item in value]

        return tuple(members) if isinstance(value, tuple) else members

    if isinstance(value, dict):
        return {key: _reverse_sets(item) for key, item in value.items()}

    return value


def _deepdiff_answer(a: object, b: object, ignore_order: bool) -> object:
    """
    Real DeepDiff's own answer for one pair, normalized the way the comparison reads it.

    :param a: The first value.
    :param b: The second value.
    :param ignore_order: Whether to diff with `ignore_order=True`.
    :return: The normalized `to_dict()` report.
    """
    real = RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2)

    return _normalize_set_categories(_normalize_types(real.to_dict()))


# The container kinds `_is_known_set_sequence_coercion_divergence` treats as a
# reachable "coerced" pairing under `ignore_order` -- see that function's doc.
_SET_KINDS: Final[frozenset[str]] = frozenset({"set", "frozenset"})
_SEQUENCE_KINDS: Final[frozenset[str]] = frozenset({"list", "tuple"})


def _is_known_set_sequence_coercion_divergence(expected: object, actual: object) -> bool:
    """
    Whether `expected` differs from `actual` only by the already-documented
    "`list(a_set) == some_list`" class in `tests/golden/README.md`'s "Set
    iteration order" section -- reachable through any batch whose values can
    pair a set/frozenset against a list/tuple under `ignore_order`, calendar
    values included; this is not specific to them.

    Once `ignore_order` pairs a set against a sequence, real DeepDiff decides
    whether to report the pair as `values_changed` (coercion "reproduced" the
    new value) or `type_changes` (it did not) by comparing `list(the_set)`,
    in the set's own Python hash-iteration order, against the sequence *as
    given* -- so which of two equally-valid ordered forms of the same list
    keeps a `type_changes` depends on the process. onix answers this
    deterministically by membership instead (`crate::ignore_order::distance`'s
    `unordered_python_eq`), which decides the *pairing distance*, not the
    report shape: a genuine `Value` kind mismatch is always `type_changes` in
    onix, never folded into `values_changed`, regardless of coercion. See
    `test_a_set_versus_a_list_is_a_type_change_whatever_the_order` in
    `test_sets.py`, which pins this same class as onix's own output for a
    plain (non-calendar) pair.

    :param expected: Real DeepDiff's normalized `to_dict()`.
    :param actual: onix's normalized `to_dict()`.
    :return: `True` if the only divergence is of this class.
    """
    if not isinstance(expected, dict) or not isinstance(actual, dict):
        return False

    expected_vc = dict(expected.get("values_changed", {}))
    actual_tc = dict(actual.get("type_changes", {}))

    for path, entry in list(expected_vc.items()):
        tc_entry = actual_tc.get(path)

        if not isinstance(entry, dict) or not isinstance(tc_entry, dict):
            continue

        kinds = {tc_entry.get("old_type"), tc_entry.get("new_type")}
        if not (kinds & _SET_KINDS and kinds & _SEQUENCE_KINDS):
            continue
        if tc_entry.get("old_value") != entry.get("old_value"):
            continue
        if tc_entry.get("new_value") != entry.get("new_value"):
            continue

        del expected_vc[path]
        del actual_tc[path]

    remaining_expected = dict(expected)
    remaining_actual = dict(actual)

    if expected_vc:
        remaining_expected["values_changed"] = expected_vc
    else:
        remaining_expected.pop("values_changed", None)

    if actual_tc:
        remaining_actual["type_changes"] = actual_tc
    else:
        remaining_actual.pop("type_changes", None)

    return remaining_expected == remaining_actual


def _diverges_with_sets(a: object, b: object, ignore_order: bool) -> tuple[object, object] | None:
    """
    Diff `a`/`b` with both engines, tolerating two accepted, distinct kinds of
    non-divergence -- see this module's own doc for the full mechanism each
    reproduces:

    - `DeepDiff` disagreeing with itself once its sets are rebuilt in another
      order (`_reverse_sets`, order-dependent): a case only onix answers
      deterministically.
    - The pre-existing, *deterministic* `` `list(a_set) == some_list` ``
      class (`_is_known_set_sequence_coercion_divergence`): a real,
      stably-reproducible divergence documented in
      `tests/golden/README.md`'s "Set iteration order" section, not order
      instability.

    :param a: The first value.
    :param b: The second value.
    :param ignore_order: Whether to diff with `ignore_order=True`.
    :return: `(expected, actual)` if they diverge, else `None`.
    """
    real = RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2)
    onix = OnixDeepDiff(a, b, ignore_order=ignore_order)

    expected_dict = _normalize_set_categories(_normalize_types(real.to_dict()))
    actual_dict = _normalize_set_categories(_normalize_types(onix.to_dict()))

    if actual_dict != expected_dict:
        # DeepDiff disagreeing with itself once its sets are rebuilt in
        # another order is the documented class onix answers deterministically
        # instead, not a divergence to chase.
        if expected_dict != _deepdiff_answer(_reverse_sets(a), _reverse_sets(b), ignore_order):
            return None

        # The pre-existing "list(a_set) == some_list" class (Set iteration
        # order, the `list(a_set) == some_list` point), reachable through any
        # batch's values, not only sets': see
        # `_is_known_set_sequence_coercion_divergence`.
        if _is_known_set_sequence_coercion_divergence(expected_dict, actual_dict):
            return None

        return expected_dict, actual_dict

    try:
        expected_json = json.loads(real.to_json())
    except TypeError:
        # DeepDiff cannot serialize a frozenset value at all; onix can, so
        # there is nothing to compare the JSON rendering against here.
        return None

    actual_json = _as_set_insensitive(_normalize_set_categories(json.loads(onix.to_json())))
    expected_json = _as_set_insensitive(_normalize_set_categories(expected_json))

    if actual_json != expected_json:
        return expected_json, actual_json

    return None


def _as_set_insensitive(value: JsonValue) -> JsonValue:
    """
    Sort every array in a parsed report, so a set-derived one compares order-free.

    Only used for the set batch's JSON comparison; the `to_dict()` comparison beside it
    is exact and sees real `set` objects, so list and tuple order is still checked (a
    Python `set` compares by membership, a `list` does not).

    :param value: A parsed report, or any part of one.
    :return: The same value with every array sorted by its canonical JSON text.
    """
    if isinstance(value, dict):
        return {key: _as_set_insensitive(item) for key, item in value.items()}

    if isinstance(value, list):
        return sorted(
            (_as_set_insensitive(item) for item in value),
            key=lambda item: json.dumps(item, sort_keys=True),
        )

    return value


def test_differential_fuzz_with_sets_matches_real_deepdiff() -> None:
    """Runs a third SEED_COUNT-case batch whose values also contain sets and frozensets."""
    mismatches = []

    for seed in range(SET_SEED_BASE, SET_SEED_BASE + SEED_COUNT):
        rng = random.Random(seed)
        a = _gen_set_value(rng, 3)
        b = _set_edge_mutations(rng, _mutate_set_value(rng, a))

        for ignore_order in (False, True):
            divergence = _diverges_with_sets(a, b, ignore_order)

            if divergence is not None:
                expected, actual = divergence
                mismatches.append((seed, ignore_order, a, b, expected, actual))

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} set fuzz cases diverged from real DeepDiff "
        f"(showing up to 3): {mismatches[:3]}"
    )


def _gen_combined_hashable(rng: random.Random, depth: int) -> object:
    """
    Generate a set member drawing from the full supported alphabet, calendar values included.

    Identical in shape to `_gen_hashable`, but its leaves are `_gen_calendar`'s
    alphabet (scalars plus datetimes and dates) rather than bare scalars, and a
    frozenset is capped at one member for the identical reason `_gen_hashable`'s
    own doc gives — a multi-member frozenset's rendering inside a set item's path
    depends on iteration order, which the two tools do not share.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget.
    :return: A hashable value, possibly a `datetime` or a `date`.
    """
    if depth <= 0 or rng.random() < 0.55:
        return _gen_calendar(rng)

    if rng.random() < 0.5:
        return frozenset(
            [_gen_combined_hashable(rng, depth - 1)] if rng.random() < 0.75 else []
        )

    return tuple(_gen_combined_hashable(rng, depth - 1) for _ in range(rng.randint(0, 3)))


def _gen_combined_value(rng: random.Random, depth: int) -> object:
    """
    Generate a value drawing from the full supported alphabet in one generator run.

    Lists, dicts, tuples, sets, frozensets, datetimes and dates can all appear
    together at any depth — issue #21's "combined goldens" requirement, at fuzz
    scale: mirrors `_gen_set_value`, with `_gen_calendar` in place of `_gen_scalar`
    for its leaves.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget.
    :return: A random value built from the full supported alphabet.
    """
    if depth <= 0:
        return _gen_calendar(rng)

    kind = rng.random()

    if kind < 0.25:
        return _gen_calendar(rng)

    if kind < 0.75:
        if rng.random() < SET_PROBABILITY:
            members = [_gen_combined_hashable(rng, depth - 1) for _ in range(rng.randint(0, 4))]

            return frozenset(members) if rng.random() < 0.4 else set(members)

        items = [_gen_combined_value(rng, depth - 1) for _ in range(rng.randint(0, 4))]

        return tuple(items) if rng.random() < 0.3 else items

    keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))

    return {key: _gen_combined_value(rng, depth - 1) for key in keys}


def _mutate_combined_value(rng: random.Random, value: object) -> object:
    """
    Build a related-but-different copy of a combined-batch value.

    Mirrors `_mutate_set_value`, drawing replacement members from the combined
    alphabet (`_gen_combined_hashable`/`_gen_combined_value`) so a mutation can
    turn a plain scalar into a calendar value and back, not only reshuffle ones
    already present.

    :param rng: Seeded RNG.
    :param value: The value to derive a mutated copy from.
    :return: A structurally related, partially mutated copy.
    """
    if isinstance(value, (set, frozenset)):
        members = [
            _gen_combined_hashable(rng, 2) if rng.random() < 0.4 else member
            for member in _deterministic_members(value)
        ]

        if rng.random() < 0.3:
            members.append(_gen_combined_hashable(rng, 2))

        return frozenset(members) if isinstance(value, frozenset) else set(members)

    if isinstance(value, (list, tuple)):
        mutated = [
            _gen_combined_value(rng, 2)
            if rng.random() < 0.3
            else _mutate_combined_value(rng, item)
            for item in value
        ]

        return tuple(mutated) if isinstance(value, tuple) else mutated

    if isinstance(value, dict):
        mutated = {
            key: _gen_combined_value(rng, 2) if rng.random() < 0.3 else item
            for key, item in value.items()
        }

        if rng.random() < 0.3:
            mutated[rng.choice(DICT_KEYS)] = _gen_combined_value(rng, 2)

        return mutated

    return _gen_combined_value(rng, 2)


def _generate_combined_case(seed: int) -> tuple[object, object]:
    """
    Generate one seeded `(a, b)` pair drawing from the full supported alphabet.

    Stacks every edge-mutation pass this module has for one type in isolation
    (`_set_edge_mutations`'s kind flips and numeric retyping,
    `_calendar_edge_mutations`'s offset shifts and stringify) onto a value that
    can hold a tuple, a set, a frozenset, a datetime and a date all at once —
    issue #21's combined-goldens requirement, at fuzz scale instead of by hand.

    :param seed: The seed driving this case's RNG.
    :return: A related-but-different `(a, b)` pair.
    """
    rng = random.Random(seed)
    a = _gen_combined_value(rng, 3)
    b = _mutate_combined_value(rng, a)
    b = _set_edge_mutations(rng, b)
    b = _calendar_edge_mutations(rng, b)

    return a, b


def test_differential_fuzz_with_the_combined_alphabet_matches_real_deepdiff(
    utc_timezone: None,
) -> None:
    """
    Run a seventh, >=500-case batch drawing the full alphabet in one generator.

    Tuples, sets, frozensets, datetimes and dates all appear together at any
    depth — including a calendar value as a bare or nested set member, the one
    combination #18-#20 left out and #21 closes.

    :param utc_timezone: Pins `TZ=UTC` — see that fixture.
    """
    mismatches = []

    for seed in range(COMBINED_SEED_BASE, COMBINED_SEED_BASE + COMBINED_SEED_COUNT):
        a, b = _generate_combined_case(seed)

        for ignore_order in (False, True):
            divergence = _diverges_with_sets(a, b, ignore_order)

            if divergence is not None:
                expected, actual = divergence
                mismatches.append((seed, ignore_order, a, b, expected, actual))

    assert not mismatches, (
        f"{len(mismatches)} of {COMBINED_SEED_COUNT * 2} combined-alphabet fuzz cases diverged "
        f"from real DeepDiff (showing up to 3): {mismatches[:3]}"
    )
