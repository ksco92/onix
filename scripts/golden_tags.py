"""The tagged JSON encoding the golden corpus uses for Python values JSON cannot express.

A golden case's ``a.json``/``b.json`` are plain JSON files, but the values they stand
for are Python objects, and several of the types DeepDiff diffs (``tuple``, ``set``,
``frozenset``, ``datetime``, ``date``, ``time``, ``timedelta``) have no JSON literal. This
module defines the one encoding that closes that gap, shared by every reader of the corpus:

- A JSON object with **exactly one** key, and that key one of :data:`RESERVED_TAGS`, is a
  tagged value and decodes to the corresponding Python object.
- **Any other** JSON object is plain data and decodes to a ``dict``, recursively.

So ``{"$tuple": [1, 2]}`` is the tuple ``(1, 2)``, ``{"$datetime": "2024-01-01T10:00:00+02:00"}``
is that aware ``datetime``, ``{"$date": "2024-01-01"}`` that ``date`` and ``{"$time":
"10:00:00+02:00"}`` that aware ``time``, while ``{"$tuple": [1], "x": 2}`` and ``{"other": 1}``
are ordinary dicts. The three calendar tags carry an ISO 8601 string — exactly what
``isoformat()`` produces and ``fromisoformat()`` reads back, with the UTC offset present only
for an aware value. ``$timedelta`` carries Python's own already-normalized ``{"days": D,
"seconds": S, "microseconds": U}`` triple instead of a single number: a flattened total-
microsecond count overflows even a 64-bit integer at Python's own extreme
``days=999_999_999`` (see ``onix_core::datetime::TimeDelta``'s own doc), where the three
components never do. The cost of the encoding is that a dict whose only
key is literally one of the reserved names cannot be written as a golden fixture;
:func:`encode_tags` refuses such a value rather than writing a file that would decode
back into something else.

This is corpus tooling only. onix's own parse paths (``onix_core::Value``'s
``Deserialize``, ``deepdiff_rs.diff_json``, the CLI) never interpret these names — a
tagged object is an ordinary dict to all of them, which the test suites pin down.

The Rust reader (``crates/onix-core/tests/golden.rs``) implements the identical rule
against the same fixtures.
"""

import datetime
import json
from collections.abc import Callable
from typing import Final, Protocol

TUPLE_TAG: Final[str] = "$tuple"
SET_TAG: Final[str] = "$set"
FROZENSET_TAG: Final[str] = "$frozenset"
DATETIME_TAG: Final[str] = "$datetime"
DATE_TAG: Final[str] = "$date"
TIME_TAG: Final[str] = "$time"
TIMEDELTA_TAG: Final[str] = "$timedelta"

# Every tag name the encoding reserves. All seven are implemented; the list is still
# fixed here so a fixture can never use one as an ordinary dict key, and so all three
# readers agree on the full set.
RESERVED_TAGS: Final[frozenset[str]] = frozenset(
    {TUPLE_TAG, SET_TAG, FROZENSET_TAG, DATETIME_TAG, DATE_TAG, TIME_TAG, TIMEDELTA_TAG}
)

# DeepDiff's own `to_json()` cannot serialize a `date`, `time` or `timedelta` at all:
# `serialization.JSON_CONVERTOR` has an entry for `datetime.datetime` (`isoformat()`) and none
# for the other three, so a report carrying one raises TypeError. onix renders a `date`/`time`
# as `isoformat()`'s own bytes and a `timedelta` as `str()`'s — documented supersets (see
# tests/golden/README.md) — and passing this mapping to DeepDiff's own
# `to_json(default_mapping=...)` makes it produce exactly the same bytes, so a golden case
# holding any of the three still has real DeepDiff output as its spec.
type _Renderable = datetime.date | datetime.time | datetime.timedelta

JSON_DEFAULT_MAPPING: Final[dict[type, Callable[[_Renderable], str]]] = {
    datetime.date: datetime.date.isoformat,
    datetime.time: datetime.time.isoformat,
    datetime.timedelta: str,
}

# A JSON-shaped value, plus the Python types the tags decode to. Named instead of
# `typing.Any` per the python-coding-guide's ban on `Any`.
type TaggedValue = (
    dict[str, "TaggedValue"]
    | list["TaggedValue"]
    | tuple["TaggedValue", ...]
    | set["SetMember"]
    | frozenset["SetMember"]
    | datetime.datetime
    | datetime.date
    | datetime.time
    | datetime.timedelta
    | str
    | int
    | float
    | bool
    | None
)

# What a Python set can hold: hashable values only, so no dict, list or set.
type SetMember = (
    tuple["SetMember", ...]
    | frozenset["SetMember"]
    | datetime.datetime
    | datetime.date
    | datetime.time
    | datetime.timedelta
    | str
    | int
    | float
    | bool
    | None
)


def _sole_tag(value: dict[str, TaggedValue]) -> str | None:
    """
    Return the reserved tag `value` is an encoding of, or ``None`` if it is plain data.

    :param value: A decoded JSON object.
    :return: The single reserved key, or ``None``.
    """
    if len(value) != 1:
        return None

    key = next(iter(value))

    return key if key in RESERVED_TAGS else None


def encode_tags(value: TaggedValue) -> TaggedValue:
    """
    Encode a Python value into its JSON-writable tagged form.

    :param value: The value to encode; tuples, datetimes and dates become tagged objects,
        everything else is rebuilt unchanged.
    :raises ValueError: If a plain dict would encode to something a decoder would read
        back as a tagged value (its only key is a reserved name).
    :return: A value containing only JSON-expressible types.
    """
    if isinstance(value, tuple):
        return {TUPLE_TAG: [encode_tags(item) for item in value]}

    # `datetime` is a `date` subclass, so it must be tested first.
    if isinstance(value, datetime.datetime):
        return {DATETIME_TAG: value.isoformat()}

    if isinstance(value, datetime.date):
        return {DATE_TAG: value.isoformat()}

    if isinstance(value, datetime.time):
        return {TIME_TAG: value.isoformat()}

    if isinstance(value, datetime.timedelta):
        return {
            TIMEDELTA_TAG: {
                "days": value.days,
                "seconds": value.seconds,
                "microseconds": value.microseconds,
            }
        }

    # Written in onix's canonical set order rather than the live set's own iteration
    # order, which is hash order and, for `str` members, PYTHONHASHSEED-dependent. onix
    # never depends on a set's order, so the fixture does not have to record one — and
    # writing the canonical order is what makes the file byte-identical between runs.
    if isinstance(value, frozenset):
        return {FROZENSET_TAG: [encode_tags(item) for item in canonical_set_order(value)]}

    if isinstance(value, set):
        return {SET_TAG: [encode_tags(item) for item in canonical_set_order(value)]}

    if isinstance(value, list):
        return [encode_tags(item) for item in value]

    if isinstance(value, dict):
        if _sole_tag(value) is not None:
            raise ValueError(
                f"cannot encode a dict whose only key is the reserved tag {next(iter(value))!r}: "
                "it would decode back as a tagged value, not as a dict"
            )

        return {key: encode_tags(item) for key, item in value.items()}

    return value


def decode_tags(value: TaggedValue) -> TaggedValue:
    """
    Decode a parsed JSON value, turning tagged objects into their Python counterparts.

    :param value: A value parsed from a golden fixture file.
    :raises NotImplementedError: If the value carries a reserved tag no decoder supports
        yet (the corpus must not use one before its slice lands).
    :return: The Python value the fixture stands for.
    """
    if isinstance(value, list):
        return [decode_tags(item) for item in value]

    if isinstance(value, dict):
        tag = _sole_tag(value)

        if tag == TUPLE_TAG:
            return tuple(decode_tags(item) for item in value[tag])

        if tag == SET_TAG:
            return {decode_tags(item) for item in value[tag]}

        if tag == FROZENSET_TAG:
            return frozenset(decode_tags(item) for item in value[tag])

        if tag == DATETIME_TAG:
            return datetime.datetime.fromisoformat(str(value[tag]))

        if tag == DATE_TAG:
            return datetime.date.fromisoformat(str(value[tag]))

        if tag == TIME_TAG:
            return datetime.time.fromisoformat(str(value[tag]))

        if tag == TIMEDELTA_TAG:
            payload = value[tag]
            if not isinstance(payload, dict):
                raise TypeError(f"the {TIMEDELTA_TAG!r} tag's payload must be an object")
            return datetime.timedelta(
                days=int(payload["days"]),
                seconds=int(payload["seconds"]),
                microseconds=int(payload["microseconds"]),
            )

        if tag is not None:
            raise NotImplementedError(f"the {tag!r} tag is reserved but not decodable yet")

        return {key: decode_tags(item) for key, item in value.items()}

    return value


# The two report categories whose entries are bare path strings rather than
# path-keyed values (see :func:`canonical_report`).
SET_CATEGORIES: Final[frozenset[str]] = frozenset({"set_item_added", "set_item_removed"})


class OnixReport(Protocol):
    """The one method :func:`sorted_set_categories` needs from an onix report."""

    def to_json(self) -> str:
        """
        Render the report as a JSON string.

        :return: The JSON text.
        """


class RealReport(Protocol):
    """The two methods :func:`canonical_report` needs from a real DeepDiff report."""

    def to_json(self, default_mapping: dict[type, Callable[[datetime.date], str]]) -> str:
        """
        Render the report as a JSON string.

        :param default_mapping: Serializers for the types DeepDiff cannot render itself.
        :return: The JSON text.
        """

    def to_dict(self) -> dict[str, object]:
        """
        Render the report as native Python objects.

        :return: The report, with real ``set``/``frozenset`` objects still in place.
        """


def canonical_report(diff: RealReport) -> TaggedValue:
    """
    Render one **real DeepDiff** report as the JSON spec onix must match.

    ``to_json()`` is the spec for everything except the *order* of anything that came
    out of a Python set, which follows hash order and, for ``str`` members,
    ``PYTHONHASHSEED``. Exactly two things are reordered here, and nothing else is
    touched:

    - the two set categories, whose entries are path strings, are sorted; and
    - every JSON array that stands for a set value (found by walking ``to_dict()``,
      which still holds the real ``set`` objects, alongside the parsed JSON) is
      reordered into :func:`canonical_set_order`, onix's own documented order.

    Reordering pairs each JSON element with its Python member by iterating the set once
    more: ``to_json()`` serialized it by the same single iteration, so ``zip`` lines the
    two up exactly. Every value in the result is therefore still DeepDiff's own. Use
    :func:`sorted_set_categories` for onix's own report, whose arrays are canonical
    already and would be scrambled by that pairing.

    :param diff: A real DeepDiff instance.
    :return: The parsed, canonically ordered report.
    """
    # `JSON_DEFAULT_MAPPING` is what lets a `date`-carrying case be rendered at all:
    # DeepDiff's stock `to_json()` raises TypeError on one.
    parsed = json.loads(diff.to_json(default_mapping=JSON_DEFAULT_MAPPING))
    as_objects = diff.to_dict()

    return {
        category: (
            sorted(entries)
            if category in SET_CATEGORIES
            else {
                path: _canonical_value(as_objects[category][path], entry)
                for path, entry in entries.items()
            }
        )
        for category, entries in parsed.items()
    }


def sorted_set_categories(diff: OnixReport) -> TaggedValue:
    """
    Render one **onix** report, sorting only the two set categories.

    onix already emits every set value in :func:`canonical_set_order`; only the two
    categories are left in the structural order the report stores them in.

    :param diff: An onix report object.
    :return: The parsed report, with both set categories sorted.
    """
    return {
        category: sorted(entries) if category in SET_CATEGORIES else entries
        for category, entries in json.loads(diff.to_json()).items()
    }


def _canonical_value(as_object: object, as_json: TaggedValue) -> TaggedValue:
    """
    Reorder every set-derived array inside one report entry; leave everything else alone.

    :param as_object: The same subtree as DeepDiff's own ``to_dict()`` holds it, still
        carrying real ``set``/``frozenset`` objects.
    :param as_json: That subtree parsed back from ``to_json()``.
    :return: `as_json` with each set-derived array in canonical order.
    """
    if isinstance(as_object, (set, frozenset)) and isinstance(as_json, list):
        paired = sorted(zip(as_object, as_json), key=lambda pair: _order_key(pair[0]))

        return [_canonical_value(member, element) for member, element in paired]

    if isinstance(as_object, (list, tuple)) and isinstance(as_json, list):
        return [
            _canonical_value(member, element) for member, element in zip(as_object, as_json)
        ]

    if isinstance(as_object, dict) and isinstance(as_json, dict):
        return {
            key: _canonical_value(value, as_json[key])
            for key, value in as_object.items()
            if key in as_json
        }

    return as_json


def canonical_set_order(members: object) -> list[SetMember]:
    """
    Sort a set's members into onix's canonical set order.

    This is the Python twin of ``onix_core::value::SetItems``'s own ordering, whose doc
    is the definition of the rule. Two points it is easy to get wrong here: ``bool`` is
    ranked before ``int`` even though every Python bool *is* an int, and ``float``
    comparison folds ``-0.0`` into ``0.0`` before ordering, matching Python's own
    equality -- see ``number_cmp`` in ``crates/onix-core/src/value.rs``.

    :param members: Any iterable of set members.
    :return: The members in canonical order.
    """
    return sorted(members, key=_order_key)


def _order_key(value: object) -> tuple[object, ...]:
    """
    Build the sort key :func:`canonical_set_order` compares by.

    :param value: Any value.
    :raises TypeError: If `value` is of a kind no set can hold.
    :return: A tuple ordering `value` against any other by kind, then by value.
    """
    if value is None:
        return (0,)

    # `bool` before `int`: every bool is an int in Python.
    if isinstance(value, bool):
        return (1, value)

    if isinstance(value, int):
        return (2, value)

    if isinstance(value, float):
        # Folds -0.0 into +0.0 before ordering -- see `number_cmp` in
        # crates/onix-core/src/value.rs.
        return (3, value + 0.0)

    if isinstance(value, str):
        return (4, value)

    if isinstance(value, tuple):
        return (5, [_order_key(item) for item in value])

    if isinstance(value, frozenset):
        return (6, [_order_key(item) for item in canonical_set_order(value)])

    if isinstance(value, list):
        return (7, [_order_key(item) for item in value])

    if isinstance(value, set):
        return (8, [_order_key(item) for item in canonical_set_order(value)])

    if isinstance(value, dict):
        return (9, [(key, _order_key(item)) for key, item in sorted(value.items())])

    # `datetime` before `date`: every `datetime` is a `date` in Python.
    if isinstance(value, datetime.datetime):
        return (10, _datetime_instant(value))

    if isinstance(value, datetime.date):
        return (11, value.toordinal())

    if isinstance(value, datetime.time):
        return (12, _time_sort_key(value))

    if isinstance(value, datetime.timedelta):
        return (13, (value.days, value.seconds, value.microseconds))

    raise TypeError(f"no canonical order defined for {type(value).__name__}")


def _datetime_instant(value: datetime.datetime) -> tuple[int, bool, int]:
    """
    Build a `datetime`'s ordering key: its UTC instant, then whether it is aware.

    The Python twin of ``onix_core::datetime::DateTime::instant`` plus
    ``crate::value``'s aware/naive tie-break: a naive value is read as UTC (matching
    ``datetime_normalize``'s default), so this ranks by microseconds since the epoch
    with the offset already applied, then by awareness (naive first) and finally by the
    raw offset — the same order two datetimes at one instant fall back on in
    ``onix_core::value::canonical_cmp``.

    :param value: A `datetime`, naive or aware.
    :return: A tuple ordering `value` against any other `datetime` the same way onix does.
    """
    offset = value.utcoffset() or datetime.timedelta()
    naive = value.replace(tzinfo=None) - offset
    epoch = datetime.datetime(1970, 1, 1)
    delta = naive - epoch
    micros = (delta.days * 86_400 + delta.seconds) * 1_000_000 + delta.microseconds

    return (micros, value.tzinfo is not None, int(offset.total_seconds()))


def _time_sort_key(value: datetime.time) -> tuple[bool, int, int]:
    """
    Build a `time`'s ordering key: naive first, then by the offset-adjusted
    micros-of-day, then by the raw offset.

    The Python twin of ``onix_core::datetime::Time::sort_instant`` plus
    ``crate::value::canonical_cmp``'s own tie-break for `Time` -- unlike
    :func:`_datetime_instant`, a naive value is NOT read as if it were UTC
    (real `time.__eq__` never does that; see `crate::datetime`'s module doc),
    so its own micros-of-day is used unadjusted.

    :param value: A `time`, naive or aware.
    :return: A tuple ordering `value` against any other `time` the same way onix does.
    """
    offset = value.utcoffset()
    wall_micros = (
        (value.hour * 3600 + value.minute * 60 + value.second) * 1_000_000 + value.microsecond
    )
    offset_seconds = int(offset.total_seconds()) if offset is not None else 0

    return (offset is not None, wall_micros - offset_seconds * 1_000_000, offset_seconds)
