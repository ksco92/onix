"""Differential fuzz test: onix's Python bindings vs real DeepDiff on live objects.

Runs through `deepdiff_rs.DeepDiff`, exercising the Python-object-to-`Value`
conversion layer. Fifteen batches of seeded cases run twice (ordered and
`ignore_order=True`), comparing `to_json()` (parsed) and `to_dict()`; the
custom-object batch compares `to_json()` alone and the enum and class-attribute
batches the report structure alone, since DeepDiff renders a whole object from
other views. The big-integer batch (issue #65) draws its big ints as bare scalars
only, never inside a tuple/set, so it stays on the arbitrary-precision property
under test rather than surfacing the pre-existing container-hashing divergence a
biased alphabet inside a hashable container would otherwise trigger.
"""

from __future__ import annotations

import collections
import datetime
import enum
import json
import random
import time
from collections.abc import Callable, Iterator
from typing import Final, Union

import pytest
from conftest import _normalize_types, require_deepdiff

require_deepdiff()

from deepdiff import DeepDiff as RealDeepDiff
from golden_tags import JSON_DEFAULT_MAPPING, canonical_set_order

from deepdiff_rs import DeepDiff as OnixDeepDiff

JsonValue = Union[
    dict[str, "JsonValue"],
    list["JsonValue"],
    tuple["JsonValue", ...],
    datetime.datetime,
    datetime.date,
    str,
    int,
    float,
    bool,
    None,
]

DICT_KEYS: Final[list[str]] = ["a", "b", "c", "d", "e"]
SCALARS: Final[list[JsonValue]] = [
    None, True, False, 0, 1, -1, 2, 3, 0.0, 1.5, -2.25, "x", "y", "z", "",
]

BIG_INT_SEED_BASE: Final[int] = 11_000_000
BIG_INT_SCALARS: Final[list[JsonValue]] = [
    *SCALARS,
    2**64, 2**64 + 1, -(2**64), 2**70, -(2**70),
    2**100, 2**100 + 1, -(2**100), 10**30, 2**200, -(2**200),
]

SEED_COUNT: Final[int] = 300

# Disjoint seed ranges keep the tuple and set batches independent corpora.
TUPLE_SEED_BASE: Final[int] = 1_000_000
SET_SEED_BASE: Final[int] = 5_000_000

SET_PROBABILITY: Final[float] = 0.4

CALENDAR_SEED_BASE: Final[int] = 2_000_000
CLUSTERED_SEED_BASE: Final[int] = 3_000_000
STRINGIFIED_SEED_BASE: Final[int] = 4_000_000

# COMBINED_SEED_COUNT satisfies issue #21's >=500-case requirement.
COMBINED_SEED_BASE: Final[int] = 6_000_000
COMBINED_SEED_COUNT: Final[int] = 500

MULTILINE_SEED_BASE: Final[int] = 7_000_000

MULTILINE_STRINGS: Final[list[str]] = [
    "a\nb", "a\nc", "c\nd", "line1\nline2", "line1\nline3",
    "x\ny\nz", "x\nY\nz", "\nlead\nmore", "trail\nend\n", "a\n\nb",
    "a\r\nb\r\nc", "a\r\nb\r\nd", "mixed\rCR\nLF", "para sep end\ntail",
    "one\ntwo\nthree\nfour\nfive\nsix\nseven",
    "one\ntwo\nthree\nCHANGED\nfive\nsix\nseven",
    "same\nsame\nsame\nsame", "same\ndiff\nsame\nsame",
    "", "single-line", "another single line",
]
MULTILINE_ALPHABET: Final[list[JsonValue]] = [
    *MULTILINE_STRINGS, None, True, False, 0, 1, 1.5,
]

DICT_KEY_SEED_BASE: Final[int] = 8_000_000

SUBCLASS_KEY_SEED_BASE: Final[int] = 9_000_000
SUBCLASS_KEY_SEED_COUNT: Final[int] = 150

NON_STR_KEY_PROBABILITY: Final[float] = 0.4

# Not itself a tuple: a dict key may not nest one (see convert.rs's module doc).
NON_STR_KEY_TUPLE_LEAVES: Final[list[JsonValue]] = [1, "x", True, None, 2.5]

# issue #59. Never generates a set/frozenset: real DeepDiff crashes hashing a
# lone surrogate (see tests/golden/README.md).
SURROGATE_SEED_BASE: Final[int] = 10_000_000

SURROGATE_STRINGS: Final[list[str]] = [
    "\udc80", "\udc81", "\ud800", "\udbff", "\udfff",
    "a\udc80", "\udc80b", "a\udc80b", "x\udc80y\udc81z",
    "\udc80\udc81", "café\udc80", "", "plain", "another",
]
SURROGATE_ALPHABET: Final[list[JsonValue]] = [
    *SURROGATE_STRINGS, None, True, False, 0, 1, 1.5,
]

# UTC_OFFSETS includes a non-whole-minute offset (widens isoformat()'s suffix
# to +HH:MM:SS) and Python's extremes.
CALENDAR_EPOCH: Final[datetime.datetime] = datetime.datetime(2015, 1, 1)
CALENDAR_SPAN_SECONDS: Final[int] = 15 * 365 * 86400
UTC_OFFSETS: Final[list[int]] = [
    0, 3600, -3600, 5 * 3600 + 1800, -18000, 1830, -1830, 86399, -86399,
]
MICROSECONDS: Final[list[int]] = [0, 1, 123456, 999999]

DATE_PROBABILITY: Final[float] = 0.25
NAIVE_PROBABILITY: Final[float] = 0.4
CALENDAR_LEAF_PROBABILITY: Final[float] = 0.6

TIME_PROBABILITY: Final[float] = 0.15
TIMEDELTA_PROBABILITY: Final[float] = 0.15

# Tuple-only: a kind flip (list<->tuple) and a numeric re-type within Python's
# `1 == 1.0 == True` family, which is what makes DeepHash's cache hand two
# tuples the same digest.
KIND_FLIP_PROBABILITY: Final[float] = 0.15
RETYPE_PROBABILITY: Final[float] = 0.25

OFFSET_SHIFT_PROBABILITY: Final[float] = 0.3

# Half take str() (which model.py's new_t1 = new_type(change.t1) reproduces,
# keeping the pair within the type_changes pairing cutoff) and half take
# isoformat() (which it does not).
STRINGIFY_PROBABILITY: Final[float] = 0.2

CLUSTER_EPOCH: Final[datetime.datetime] = datetime.datetime(2024, 1, 1)
CLUSTER_SPAN_HOURS: Final[int] = 48
SAME_INSTANT_TWIN_PROBABILITY: Final[float] = 0.5


def _deterministic_members(value: set[object] | frozenset[object]) -> list[object]:
    """Order a live set/frozenset's members deterministically, independent of `PYTHONHASHSEED`.

    Each member consumes one `rng` draw in iteration order, so callers use this instead of a
    plain loop to keep a given seed reproducible.
    """
    return canonical_set_order(value)


def _gen_scalar(rng: random.Random, scalars: list[JsonValue] | None = None) -> JsonValue:
    """Pick a random scalar."""
    return rng.choice(SCALARS if scalars is None else scalars)


def _gen_calendar(rng: random.Random) -> JsonValue:
    """Pick a random `date`, `time`, `timedelta`, or naive/aware `datetime`."""
    if rng.random() >= CALENDAR_LEAF_PROBABILITY:
        return _gen_scalar(rng)

    if rng.random() < TIMEDELTA_PROBABILITY:
        return datetime.timedelta(
            seconds=rng.randrange(-CALENDAR_SPAN_SECONDS, CALENDAR_SPAN_SECONDS),
            microseconds=rng.choice(MICROSECONDS),
        )

    if rng.random() < TIME_PROBABILITY:
        seconds_of_day = rng.randrange(86_400)
        wall_clock = datetime.time(
            seconds_of_day // 3600,
            seconds_of_day // 60 % 60,
            seconds_of_day % 60,
            rng.choice(MICROSECONDS),
        )

        if rng.random() < NAIVE_PROBABILITY:
            return wall_clock

        offset = rng.choice(UTC_OFFSETS)

        return wall_clock.replace(tzinfo=datetime.timezone(datetime.timedelta(seconds=offset)))

    value = CALENDAR_EPOCH + datetime.timedelta(
        seconds=rng.randrange(CALENDAR_SPAN_SECONDS), microseconds=rng.choice(MICROSECONDS)
    )

    if rng.random() < DATE_PROBABILITY:
        return value.date()

    if rng.random() < NAIVE_PROBABILITY:
        return value

    offset = rng.choice(UTC_OFFSETS)

    return value.replace(tzinfo=datetime.timezone(datetime.timedelta(seconds=offset)))


def _gen_non_str_dict_key(rng: random.Random) -> JsonValue:
    """Pick a random non-`str` dict key: a scalar, `datetime`/`date`, or a `tuple` (issue #62)."""
    kind = rng.random()

    if kind < 0.2:
        return rng.randint(-5, 5)

    if kind < 0.35:
        return rng.choice([True, False])

    if kind < 0.5:
        return rng.choice([0.5, 1.5, -2.5, 3.0])

    if kind < 0.6:
        return None

    value = CALENDAR_EPOCH + datetime.timedelta(seconds=rng.randrange(CALENDAR_SPAN_SECONDS))

    if kind < 0.8:
        return value if kind < 0.7 else value.date()

    # Never the empty tuple: a real, narrow DeepDiff bug, not reproduced --
    # see tests/golden/README.md's empty-tuple-key section.
    length = rng.randint(1, 2)

    return tuple(rng.choice(NON_STR_KEY_TUPLE_LEAVES) for _ in range(length))


def _gen_dict_key(rng: random.Random, dict_keys: bool) -> JsonValue:
    """Pick a random dict key: a `str`, or (when `dict_keys` is set) another type `DeepDiff` accepts."""
    if dict_keys and rng.random() < NON_STR_KEY_PROBABILITY:
        return _gen_non_str_dict_key(rng)

    return rng.choice(DICT_KEYS)


def _gen_value(
    rng: random.Random,
    depth: int,
    scalars: list[JsonValue] | None = None,
    tuples: bool = False,
    calendar: bool = False,
    dict_keys: bool = False,
) -> JsonValue:
    """Generate a random JSON-shaped value, nesting up to `depth` levels."""
    if depth <= 0:
        return _gen_calendar(rng) if calendar else _gen_scalar(rng, scalars)

    kind = rng.random()

    # The tuple corpus leans harder on sequences (fewer bare scalars), so that
    # tuples actually show up in most cases rather than a minority of them.
    if kind < (0.3 if tuples else 0.5):
        return _gen_calendar(rng) if calendar else _gen_scalar(rng, scalars)

    if kind < 0.75:
        length = rng.randint(0, 4)
        items = [
            _gen_value(rng, depth - 1, scalars, tuples, calendar, dict_keys)
            for _ in range(length)
        ]

        return tuple(items) if tuples and rng.random() < 0.5 else items

    if dict_keys:
        count = rng.randint(0, len(DICT_KEYS))
        keys = list(dict.fromkeys(_gen_dict_key(rng, dict_keys) for _ in range(count)))
    else:
        keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))

    return {
        key: _gen_value(rng, depth - 1, scalars, tuples, calendar, dict_keys) for key in keys
    }


def _mutate(
    rng: random.Random,
    value: JsonValue,
    tuples: bool = False,
    calendar: bool = False,
    dict_keys: bool = False,
    scalars: list[JsonValue] | None = None,
) -> JsonValue:
    """Build a related-but-different copy of `value` via shuffle and selective mutation."""
    if isinstance(value, (list, tuple)):
        mutated = list(value)
        rng.shuffle(mutated)

        for index in range(len(mutated)):
            if rng.random() < 0.3:
                mutated[index] = _gen_value(
                    rng, 2, tuples=tuples, calendar=calendar, dict_keys=dict_keys, scalars=scalars
                )

        return tuple(mutated) if isinstance(value, tuple) else mutated

    if isinstance(value, dict):
        mutated = dict(value)

        for key in list(mutated):
            if rng.random() < 0.3:
                mutated[key] = _gen_value(
                    rng, 2, tuples=tuples, calendar=calendar, dict_keys=dict_keys, scalars=scalars
                )

        if rng.random() < 0.3:
            new_key = _gen_dict_key(rng, dict_keys) if dict_keys else rng.choice(DICT_KEYS)
            mutated[new_key] = _gen_value(
                rng, 2, tuples=tuples, calendar=calendar, dict_keys=dict_keys, scalars=scalars
            )

        return mutated

    return _gen_value(rng, 2, tuples=tuples, calendar=calendar, dict_keys=dict_keys, scalars=scalars)


def _generate_case(
    seed: int,
    tuples: bool = False,
    calendar: bool = False,
    dict_keys: bool = False,
    scalars: list[JsonValue] | None = None,
) -> tuple[JsonValue, JsonValue]:
    """Generate one seeded `(a, b)` pair."""
    rng = random.Random(seed)
    a = _gen_value(rng, 3, scalars=scalars, tuples=tuples, calendar=calendar, dict_keys=dict_keys)
    b = _mutate(rng, a, tuples=tuples, calendar=calendar, dict_keys=dict_keys, scalars=scalars)

    if tuples:
        b = _tuple_edge_mutations(rng, b)

    if calendar:
        b = _calendar_edge_mutations(rng, b)

    return a, b


def _calendar_edge_mutations(rng: random.Random, value: object) -> object:
    """Apply the two calendar-specific edits: an offset shift and a stringify."""
    if isinstance(value, (set, frozenset)):
        members = [_calendar_edge_mutations(rng, item) for item in _deterministic_members(value)]

        return frozenset(members) if isinstance(value, frozenset) else set(members)

    if isinstance(value, (list, tuple)):
        items = [_calendar_edge_mutations(rng, item) for item in value]

        return tuple(items) if isinstance(value, tuple) else items

    if isinstance(value, dict):
        return {key: _calendar_edge_mutations(rng, item) for key, item in value.items()}

    if isinstance(value, (datetime.time, datetime.timedelta)) and rng.random() < STRINGIFY_PROBABILITY:
        return str(value)

    if isinstance(value, datetime.date) and rng.random() < STRINGIFY_PROBABILITY:
        return str(value) if rng.random() < 0.5 else value.isoformat()

    if isinstance(value, datetime.datetime) and rng.random() < OFFSET_SHIFT_PROBABILITY:
        offset = rng.choice(UTC_OFFSETS)
        shifted = value.replace(tzinfo=datetime.timezone.utc) if value.tzinfo is None else value

        return shifted.astimezone(datetime.timezone(datetime.timedelta(seconds=offset)))

    return value


def _retype_number(rng: random.Random, value: JsonValue) -> JsonValue:
    """Re-type a number within Python's numeric equality family, keeping its value."""
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
    """Apply the two tuple-specific edits recursively: a kind flip and a numeric re-type."""
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


def _diverges(a: JsonValue, b: JsonValue, ignore_order: bool) -> tuple[JsonValue, JsonValue] | None:
    """Diff `a`/`b` with both engines and return both reports if they disagree."""
    real = RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2)
    onix = OnixDeepDiff(a, b, ignore_order=ignore_order)

    # The mapping only matters for the calendar batch, where a report can carry
    # a `date` that DeepDiff's stock `to_json()` refuses to serialize; it is a
    # no-op for every other value. See `scripts/golden_tags.py`. A report
    # that carries a raw `frozenset` value, or a *nested* dict value keyed by
    # a `datetime`/`date`/`tuple` (issue #62 -- a dict key any deeper than
    # the top-level path segment has no json.dumps rule at all, unlike
    # `int`/`bool`/`float`/`None`, which DeepDiff's own `to_json()` already
    # stringifies), makes `to_json()` raise `TypeError` outright; both are
    # real DeepDiff crashes, so the comparison falls back to `to_dict()`
    # alone rather than treating a crash as a divergence to report.
    try:
        expected_json = json.loads(real.to_json(default_mapping=JSON_DEFAULT_MAPPING))
        real_to_json_crashed = False
    except TypeError:
        real_to_json_crashed = True

    if not real_to_json_crashed:
        actual_json = json.loads(onix.to_json())

        if actual_json != expected_json:
            return expected_json, actual_json

    expected_dict = _normalize_types(real.to_dict())
    actual_dict = _normalize_types(onix.to_dict())

    if actual_dict != expected_dict:
        return expected_dict, actual_dict

    return None


def _run_batch(
    seeds: range,
    tuples: bool = False,
    calendar: bool = False,
    dict_keys: bool = False,
    case_fn: Callable[[int], tuple[JsonValue, JsonValue]] | None = None,
    diverge_fn: Callable[[JsonValue, JsonValue, bool], tuple[JsonValue, JsonValue] | None] = _diverges,
) -> list[tuple[int, bool, JsonValue, JsonValue, JsonValue, JsonValue]]:
    """Run one batch of seeded cases through both engines, ordered and `ignore_order`."""
    build_case = case_fn or (
        lambda seed: _generate_case(seed, tuples=tuples, calendar=calendar, dict_keys=dict_keys)
    )
    mismatches = []

    for seed in seeds:
        a, b = build_case(seed)

        for ignore_order in (False, True):
            divergence = diverge_fn(a, b, ignore_order)

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


def test_differential_fuzz_with_big_integers_matches_real_deepdiff() -> None:
    """Runs a SEED_COUNT-case batch whose leaves include arbitrary-precision ints (issue #65)."""
    seeds = range(BIG_INT_SEED_BASE, BIG_INT_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, case_fn=lambda seed: _generate_case(seed, scalars=BIG_INT_SCALARS))

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} big-integer fuzz cases diverged from real "
        f"DeepDiff (showing up to 3): {mismatches[:3]}"
    )


def _gen_clustered_calendar(rng: random.Random) -> JsonValue:
    """Pick one calendar value from a tiny window."""
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
    """Generate one seeded case of dict-wrapped calendar values against strings of them.

    Wrapped in a one-key dict because two bare scalars sit above the 0.3 ignore_order pairing cutoff even for a one-character
    difference, while the same difference inside a one-key dict sits below it -- bare scalars would never pair.
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
    """Generate one seeded flat-calendar-list `(a, b)` pair."""
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
    """Pin the process timezone to UTC for the duration of one test."""
    monkeypatch.setenv("TZ", "UTC")
    time.tzset()

    yield

    # `monkeypatch` restores the environment variable itself; libc still has to
    # be told to re-read it.
    time.tzset()


def test_differential_fuzz_with_calendar_values_matches_real_deepdiff(
    utc_timezone: None,
) -> None:
    """Run a third SEED_COUNT-case batch whose values also contain datetimes and dates."""
    seeds = range(CALENDAR_SEED_BASE, CALENDAR_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, tuples=False, calendar=True)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} calendar fuzz cases diverged from real "
        f"DeepDiff (showing up to 3): {mismatches[:3]}"
    )


def test_differential_fuzz_with_clustered_calendar_lists_matches_real_deepdiff(
    utc_timezone: None,
) -> None:
    """Run a fourth batch of flat, tightly clustered calendar lists."""
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
    """Run a fifth batch pairing dict-wrapped calendar values against strings of them."""
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
    """Generate a value a Python set can hold: a scalar, a tuple of them, or a frozenset.

    The frozenset is capped at one member, unlike the tuple branch: a set member renders into the finding's path, DeepDiff's in
    hash order and onix's in canonical order, and only a single member makes the two coincide.
    """
    if depth <= 0 or rng.random() < 0.55:
        return _gen_scalar(rng)

    if rng.random() < 0.5:
        return frozenset([_gen_hashable(rng, depth - 1)] if rng.random() < 0.75 else [])

    return tuple(_gen_hashable(rng, depth - 1) for _ in range(rng.randint(0, 3)))


def _gen_set_value(rng: random.Random, depth: int) -> object:
    """Generate a random value whose sequences are sometimes sets or frozensets."""
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
    """Build a related-but-different copy of a set-batch value, keeping each container's kind."""
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
    """Apply the two set-specific edits recursively: a kind flip and a numeric re-type."""
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
    """Report whether `value` could be a set member."""
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
    """Sort the two set categories, whose order real DeepDiff draws from Python hash order."""
    return {
        key: sorted(value) if key in {"set_item_added", "set_item_removed"} else value
        for key, value in report.items()
    }


def _reverse_sets(value: object) -> object:
    """Rebuild every set and frozenset in `value` from its members in reverse."""
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
    """Real DeepDiff's own answer for one pair, normalized the way the comparison reads it."""
    real = RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2)

    return _normalize_set_categories(_normalize_types(real.to_dict()))


# The container kinds `_is_known_set_sequence_coercion_divergence` treats as a
# reachable "coerced" pairing under `ignore_order` -- see that function's doc.
_SET_KINDS: Final[frozenset[str]] = frozenset({"set", "frozenset"})
_SEQUENCE_KINDS: Final[frozenset[str]] = frozenset({"list", "tuple"})


def _is_known_set_sequence_coercion_divergence(expected: object, actual: object) -> bool:
    """Whether `expected` differs from `actual` only by the documented `list(a_set) == some_list` coercion class."""
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
    """Diff `a`/`b` with both engines, tolerating two documented classes of non-divergence."""
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
    """Sort every array in a parsed report, so a set-derived one compares order-free."""
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
    """Generate a set member drawing from the full supported alphabet, calendar values included.

    Caps its frozenset at one member for the same reason `_gen_hashable` does.
    """
    if depth <= 0 or rng.random() < 0.55:
        return _gen_calendar(rng)

    if rng.random() < 0.5:
        return frozenset(
            [_gen_combined_hashable(rng, depth - 1)] if rng.random() < 0.75 else []
        )

    return tuple(_gen_combined_hashable(rng, depth - 1) for _ in range(rng.randint(0, 3)))


def _gen_combined_value(rng: random.Random, depth: int) -> object:
    """Generate a value drawing from the full supported alphabet in one generator run (issue #21)."""
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
    """Build a related-but-different copy of a combined-batch value."""
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
    """Generate one seeded `(a, b)` pair drawing from the full supported alphabet (issue #21)."""
    rng = random.Random(seed)
    a = _gen_combined_value(rng, 3)
    b = _mutate_combined_value(rng, a)
    b = _set_edge_mutations(rng, b)
    b = _calendar_edge_mutations(rng, b)

    return a, b


def test_differential_fuzz_with_the_combined_alphabet_matches_real_deepdiff(
    utc_timezone: None,
) -> None:
    """Run a seventh, >=500-case batch drawing the full alphabet in one generator (issue #21)."""
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


def _gen_multiline_leaf(rng: random.Random) -> JsonValue:
    """Pick a leaf from the multi-line alphabet."""
    return rng.choice(MULTILINE_ALPHABET)


def _gen_multiline_value(rng: random.Random, depth: int) -> JsonValue:
    """Generate a random value whose leaves are drawn from the multi-line alphabet."""
    if depth <= 0:
        return _gen_multiline_leaf(rng)

    kind = rng.random()

    if kind < 0.5:
        return _gen_multiline_leaf(rng)

    if kind < 0.8:
        length = rng.randint(0, 4)

        return [_gen_multiline_value(rng, depth - 1) for _ in range(length)]

    keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))

    return {key: _gen_multiline_value(rng, depth - 1) for key in keys}


def _mutate_multiline_value(rng: random.Random, value: JsonValue) -> JsonValue:
    """Build a related-but-different copy, replacing leaves from the same alphabet."""
    if isinstance(value, list):
        mutated = list(value)
        rng.shuffle(mutated)

        for index in range(len(mutated)):
            if rng.random() < 0.4:
                mutated[index] = _gen_multiline_value(rng, 2)

        return mutated

    if isinstance(value, dict):
        mutated = dict(value)

        for key in list(mutated):
            if rng.random() < 0.4:
                mutated[key] = _gen_multiline_value(rng, 2)

        if rng.random() < 0.3:
            mutated[rng.choice(DICT_KEYS)] = _gen_multiline_value(rng, 2)

        return mutated

    return _gen_multiline_value(rng, 2)


def test_differential_fuzz_with_multiline_strings_matches_real_deepdiff() -> None:
    """Run an eighth SEED_COUNT-case batch whose leaves are often multi-line strings (issue #28)."""
    mismatches = []

    for seed in range(MULTILINE_SEED_BASE, MULTILINE_SEED_BASE + SEED_COUNT):
        rng = random.Random(seed)
        a = _gen_multiline_value(rng, 3)
        b = _mutate_multiline_value(rng, a)

        for ignore_order in (False, True):
            divergence = _diverges(a, b, ignore_order)

            if divergence is not None:
                expected, actual = divergence
                mismatches.append((seed, ignore_order, a, b, expected, actual))

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} multi-line string fuzz cases diverged from "
        f"real DeepDiff (showing up to 3): {mismatches[:3]}"
    )


def test_differential_fuzz_with_non_str_dict_keys_matches_real_deepdiff() -> None:
    """Run a ninth SEED_COUNT-case batch whose dicts may carry non-`str` keys (issue #62)."""
    seeds = range(DICT_KEY_SEED_BASE, DICT_KEY_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, tuples=True, calendar=True, dict_keys=True)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} dict-key fuzz cases diverged from real DeepDiff "
        f"(showing up to 3): {mismatches[:3]}"
    )


_NamedPoint = collections.namedtuple("_NamedPoint", "x y")


class _TupleSub(tuple):
    """A plain `tuple` subclass, not a `namedtuple`, for the subclass dict-key batch below."""


class _DateTimeSub(datetime.datetime):
    """A plain `datetime` subclass, e.g. pandas' `Timestamp`, for the batch below."""


class _DateSub(datetime.date):
    """A plain `date` subclass, for the batch below."""


def _gen_subclass_key(rng: random.Random) -> object:
    """Pick a random subclass dict key: `namedtuple`, `tuple`, `datetime`, or `date` (issue #64 follow-up)."""
    kind = rng.random()

    if kind < 0.25:
        return _NamedPoint(rng.randint(-5, 5), rng.randint(-5, 5))

    if kind < 0.5:
        return _TupleSub((rng.randint(-5, 5), rng.randint(-5, 5)))

    value = CALENDAR_EPOCH + datetime.timedelta(seconds=rng.randrange(CALENDAR_SPAN_SECONDS))

    if kind < 0.75:
        return _DateTimeSub(
            value.year, value.month, value.day, value.hour, value.minute, value.second
        )

    return _DateSub(value.year, value.month, value.day)


def _base_twin(key: object) -> object:
    """The plain-base-type twin of a subclass key `_gen_subclass_key` produced, same value."""
    if isinstance(key, datetime.datetime):
        return datetime.datetime(
            key.year, key.month, key.day, key.hour, key.minute, key.second
        )
    if isinstance(key, datetime.date):
        return datetime.date(key.year, key.month, key.day)
    return tuple(key)


def _mutated_twin(key: object, rng: random.Random) -> object:
    """A plain-base-type twin of a subclass key with a *different* value."""
    if isinstance(key, datetime.datetime):
        return datetime.datetime(
            key.year, key.month, key.day, key.hour, key.minute, (key.second + 1) % 60
        )
    if isinstance(key, datetime.date):
        return key + datetime.timedelta(days=1)
    return (*key[:-1], key[-1] + 1)


def _generate_subclass_key_case(seed: int) -> tuple[dict[object, JsonValue], dict[object, JsonValue]]:
    """Build one `(a, b)` dict pair keyed by a subclass instance and its matching-or-not base-type twin."""
    rng = random.Random(seed)
    key_a = _gen_subclass_key(rng)
    key_b = _base_twin(key_a) if rng.random() < 0.5 else _mutated_twin(key_a, rng)
    value = rng.choice(["x", "y", 1, 2.5, None, True])

    return {key_a: value}, {key_b: value}


def test_differential_fuzz_with_subclass_dict_keys_matches_real_deepdiff() -> None:
    """Run a tenth batch whose dicts carry a subclass key against its base-type twin (issue #64's dict-key follow-up)."""
    seeds = range(SUBCLASS_KEY_SEED_BASE, SUBCLASS_KEY_SEED_BASE + SUBCLASS_KEY_SEED_COUNT)
    mismatches = _run_batch(seeds, case_fn=_generate_subclass_key_case)

    assert not mismatches, (
        f"{len(mismatches)} of {SUBCLASS_KEY_SEED_COUNT * 2} subclass dict-key fuzz cases "
        f"diverged from real DeepDiff (showing up to 3): {mismatches[:3]}"
    )


def _gen_surrogate_leaf(rng: random.Random) -> JsonValue:
    """Pick a leaf from the surrogate alphabet."""
    return rng.choice(SURROGATE_ALPHABET)


def _gen_surrogate_value(rng: random.Random, depth: int) -> JsonValue:
    """Generate a random value whose leaves are drawn from the surrogate alphabet."""
    if depth <= 0:
        return _gen_surrogate_leaf(rng)

    kind = rng.random()

    if kind < 0.5:
        return _gen_surrogate_leaf(rng)

    if kind < 0.8:
        length = rng.randint(0, 4)

        return [_gen_surrogate_value(rng, depth - 1) for _ in range(length)]

    keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))

    return {key: _gen_surrogate_value(rng, depth - 1) for key in keys}


def _mutate_surrogate_value(rng: random.Random, value: JsonValue) -> JsonValue:
    """Build a related-but-different copy, replacing leaves from the same alphabet."""
    if isinstance(value, list):
        mutated = list(value)
        rng.shuffle(mutated)

        for index in range(len(mutated)):
            if rng.random() < 0.4:
                mutated[index] = _gen_surrogate_value(rng, 2)

        return mutated

    if isinstance(value, dict):
        mutated = dict(value)

        for key in list(mutated):
            if rng.random() < 0.4:
                mutated[key] = _gen_surrogate_value(rng, 2)

        if rng.random() < 0.3:
            mutated[rng.choice(DICT_KEYS)] = _gen_surrogate_value(rng, 2)

        return mutated

    return _gen_surrogate_value(rng, 2)


def _mutate_surrogate_keys(rng: random.Random, value: JsonValue) -> JsonValue:
    """Additionally replace some dict keys with a surrogate-bearing string (issue #59)."""
    if isinstance(value, dict):
        retagged: dict[str, JsonValue] = {}

        for key, item in value.items():
            new_key = rng.choice(SURROGATE_STRINGS) if rng.random() < 0.2 else key
            retagged[new_key] = _mutate_surrogate_keys(rng, item)

        return retagged

    if isinstance(value, list):
        return [_mutate_surrogate_keys(rng, item) for item in value]

    return value


def test_differential_fuzz_with_surrogate_strings_matches_real_deepdiff() -> None:
    """Run an eleventh SEED_COUNT-case batch whose leaves and dict keys often hold a lone surrogate code point (issue #59)."""
    mismatches = []

    for seed in range(SURROGATE_SEED_BASE, SURROGATE_SEED_BASE + SEED_COUNT):
        rng = random.Random(seed)
        a = _gen_surrogate_value(rng, 3)
        a = _mutate_surrogate_keys(rng, a)
        b = _mutate_surrogate_value(rng, a)
        b = _mutate_surrogate_keys(rng, b)

        divergence = _diverges(a, b, ignore_order=False)

        if divergence is not None:
            expected, actual = divergence
            mismatches.append((seed, a, b, expected, actual))

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT} surrogate-string fuzz cases diverged from "
        f"real DeepDiff (showing up to 3): {mismatches[:3]}"
    )


# The custom-object batches (issue #66) draw attribute values from bare
# scalars, nested objects, lists and dicts, never a tuple or set, to stay off
# the container-hashing divergence.
OBJECT_SEED_BASE: Final[int] = 12_000_000
ENUM_OBJECT_SEED_BASE: Final[int] = 13_000_000
CLASS_ATTRIBUTE_SEED_BASE: Final[int] = 14_000_000
_OBJECT_ATTR_NAMES: Final[list[str]] = ["p", "q", "r", "s"]


def _set_attributes(self: object, **attrs: object) -> None:
    """Set each keyword argument as an attribute: the shared constructor of the object batch's classes."""
    for key, value in attrs.items():
        setattr(self, key, value)


class _Shade(enum.Enum):
    RED = 1
    GREEN = 2
    BLUE = "blue"


_OBJECT_CLASSES: Final[list[type]] = [
    type(name, (), {"__init__": _set_attributes}) for name in ("_Obj1", "_Obj2", "_Obj3")
]


def _gen_object_attr(rng: random.Random, depth: int) -> object:
    """Generate one attribute value: a scalar, or with depth budget a nested object, list or dict."""
    roll = rng.random()
    if depth > 0 and roll < 0.12:
        return _gen_object(rng, depth - 1)
    if depth > 0 and roll < 0.24:
        return [_gen_object_attr(rng, depth - 1) for _ in range(rng.randint(0, 3))]
    if depth > 0 and roll < 0.34:
        return {
            key: _gen_object_attr(rng, depth - 1)
            for key in rng.sample(DICT_KEYS, rng.randint(0, 2))
        }
    return _gen_scalar(rng)


def _gen_object(rng: random.Random, depth: int) -> object:
    """Generate one instance of a random pool class with a random subset of attributes."""
    cls = rng.choice(_OBJECT_CLASSES)
    names = rng.sample(_OBJECT_ATTR_NAMES, rng.randint(0, len(_OBJECT_ATTR_NAMES)))
    return cls(**{name: _gen_object_attr(rng, depth) for name in names})


def _gen_object_top(rng: random.Random, depth: int) -> object:
    """Generate a top-level value: a bare object, or a list, dict or tuple of them."""
    roll = rng.random()
    if roll < 0.55 or depth <= 0:
        return _gen_object(rng, depth)
    if roll < 0.75:
        return [_gen_object_top(rng, depth - 1) for _ in range(rng.randint(0, 4))]
    if roll < 0.9:
        return {key: _gen_object_top(rng, depth - 1) for key in rng.sample(DICT_KEYS, rng.randint(0, 3))}
    return tuple(_gen_object_top(rng, depth - 1) for _ in range(rng.randint(0, 3)))


def _mutate_object(rng: random.Random, obj: object) -> object:
    """Rebuild `obj` with attributes changed, added or removed, or its class swapped."""
    cls = rng.choice(_OBJECT_CLASSES) if rng.random() < 0.15 else type(obj)
    attrs = dict(vars(obj))
    if attrs and rng.random() < 0.3:
        del attrs[rng.choice(list(attrs))]
    if rng.random() < 0.3:
        attrs[rng.choice(_OBJECT_ATTR_NAMES)] = _gen_scalar(rng)
    for key in list(attrs):
        if rng.random() < 0.4:
            attrs[key] = _mutate_object_value(rng, attrs[key])
    return cls(**attrs)


def _mutate_object_value(rng: random.Random, value: object) -> object:
    """Recursively mutate a value drawn from the object batch."""
    if isinstance(value, tuple(_OBJECT_CLASSES)):
        return _mutate_object(rng, value)
    if isinstance(value, list):
        shuffled = list(value)
        rng.shuffle(shuffled)
        return [_mutate_object_value(rng, item) for item in shuffled]
    if isinstance(value, dict):
        return {key: _mutate_object_value(rng, item) for key, item in value.items()}
    if isinstance(value, tuple):
        return tuple(_mutate_object_value(rng, item) for item in value)
    return _gen_scalar(rng) if rng.random() < 0.5 else value


def _generate_object_case(seed: int) -> tuple[object, object]:
    """Generate one custom-object case: a graph and a mutation of it, or an unrelated graph."""
    rng = random.Random(seed)
    a = _gen_object_top(rng, 3)
    b = _mutate_object_value(rng, a) if rng.random() < 0.9 else _gen_object_top(rng, 3)
    return a, b


def _object_diverges(a: object, b: object, ignore_order: bool) -> tuple[object, object] | None:
    """Diff `a`/`b` with both engines and return both parsed `to_json()` reports if they disagree."""
    expected = json.loads(RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2).to_json())
    actual = json.loads(OnixDeepDiff(a, b, ignore_order=ignore_order).to_json())
    return None if expected == actual else (expected, actual)


def test_differential_fuzz_with_custom_objects_matches_real_deepdiff() -> None:
    """Runs a SEED_COUNT-case batch of custom-object graphs, ordered and ignore_order=True (issue #66)."""
    seeds = range(OBJECT_SEED_BASE, OBJECT_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, case_fn=_generate_object_case, diverge_fn=_object_diverges)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} custom-object fuzz cases diverged from real "
        f"DeepDiff (showing up to 3): {mismatches[:3]}"
    )


def _gen_enum_object_item(rng: random.Random) -> object:
    """Generate a bare Enum member, or a pool-class instance whose attributes are bare scalars or members."""
    if rng.random() < 0.3:
        return rng.choice(list(_Shade))
    names = rng.sample(_OBJECT_ATTR_NAMES, rng.randint(0, len(_OBJECT_ATTR_NAMES)))
    return rng.choice(_OBJECT_CLASSES)(
        **{name: rng.choice(list(_Shade)) if rng.random() < 0.4 else _gen_scalar(rng) for name in names}
    )


def _generate_enum_object_case(seed: int) -> tuple[list[object], list[object]]:
    """Generate a list of objects and Enum members, and a copy with items replaced, dropped or added."""
    rng = random.Random(seed)
    a = [_gen_enum_object_item(rng) for _ in range(rng.randint(0, 5))]
    b = [item if rng.random() < 0.5 else _gen_enum_object_item(rng) for item in a if rng.random() < 0.9]
    b += [_gen_enum_object_item(rng) for _ in range(rng.randint(0, 2))]
    rng.shuffle(b)
    return a, b


def _type_name(value: object) -> object:
    """A `type_changes` type as its name: DeepDiff reports the type, onix the name."""
    return getattr(value, "__name__", value)


def _report_structure(report: dict) -> dict[str, object]:
    """A `to_dict()` report reduced to its categories, paths and `type_changes` type names."""
    return {
        category: sorted(
            (path, _type_name(entry["old_type"]), _type_name(entry["new_type"])) for path, entry in entries.items()
        )
        if category == "type_changes"
        else sorted(entries)
        for category, entries in report.items()
    }


def _structure_diverges(a: object, b: object, ignore_order: bool) -> tuple[object, object] | None:
    """Diff `a`/`b` with both engines and return both report structures if they disagree."""
    expected = _report_structure(RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2).to_dict())
    actual = _report_structure(OnixDeepDiff(a, b, ignore_order=ignore_order).to_dict())
    return None if expected == actual else (expected, actual)


def test_differential_fuzz_with_enum_members_and_objects_matches_real_deepdiff() -> None:
    """Runs a SEED_COUNT-case batch of lists mixing objects and Enum members, ordered and ignore_order=True."""
    seeds = range(ENUM_OBJECT_SEED_BASE, ENUM_OBJECT_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, case_fn=_generate_enum_object_case, diverge_fn=_structure_diverges)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} enum-and-object fuzz cases diverged from real "
        f"DeepDiff (showing up to 3): {mismatches[:3]}"
    )


_DEFAULT_CLASSES: Final[list[type]] = [
    type(f"_Default{i}", (), {"d": default, "__init__": _set_attributes})
    for i, default in enumerate([1, "a", [1, 2], {"k": 1}])
]


def _gen_default_item(rng: random.Random) -> object:
    """Generate an instance of a class with a `d` default, shadowing it half the time."""
    attrs: dict[str, object] = {"x": rng.randint(0, 3)}
    if rng.random() < 0.5:
        attrs["d"] = rng.choice([1, 2, "a", "b", [1, 2], [1, 3], {"k": 1}, {"k": 2}])
    return rng.choice(_DEFAULT_CLASSES)(**attrs)


def _generate_class_attribute_case(seed: int) -> tuple[list[object], list[object]]:
    """Generate two lists of instances whose class-attribute defaults are shadowed or shared."""
    rng = random.Random(seed)
    return (
        [_gen_default_item(rng) for _ in range(rng.randint(0, 4))],
        [_gen_default_item(rng) for _ in range(rng.randint(0, 4))],
    )


def test_differential_fuzz_with_class_attribute_defaults_matches_real_deepdiff() -> None:
    """Runs a SEED_COUNT-case batch of instances shadowing or sharing class defaults, ordered and ignore_order=True."""
    seeds = range(CLASS_ATTRIBUTE_SEED_BASE, CLASS_ATTRIBUTE_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, case_fn=_generate_class_attribute_case, diverge_fn=_structure_diverges)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} class-attribute fuzz cases diverged from real "
        f"DeepDiff (showing up to 3): {mismatches[:3]}"
    )
