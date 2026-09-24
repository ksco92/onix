"""The tagged JSON encoding the golden corpus uses for values JSON cannot express.

A JSON object with exactly one key drawn from :data:`RESERVED_TAGS` decodes to the
corresponding Python object (a container type JSON has no literal for, an
out-of-range ``int``, or a custom object); any other JSON object decodes to a plain
``dict``. ``$timedelta`` carries Python's own normalized ``{days, seconds,
microseconds}`` triple, not a flattened microsecond count, which overflows an
``i64`` at Python's own extreme (``days=999_999_999``). Corpus tooling only: onix's own
parse paths never interpret these tags; ``crates/onix-core/tests/golden.rs`` implements the same rule.
"""

from __future__ import annotations

import datetime
import enum
import json
from collections.abc import Callable
from typing import Final, Protocol, Union

TUPLE_TAG: Final[str] = "$tuple"
SET_TAG: Final[str] = "$set"
FROZENSET_TAG: Final[str] = "$frozenset"
DATETIME_TAG: Final[str] = "$datetime"
DATE_TAG: Final[str] = "$date"
TIME_TAG: Final[str] = "$time"
TIMEDELTA_TAG: Final[str] = "$timedelta"
DICT_TAG: Final[str] = "$dict"
BIGINT_TAG: Final[str] = "$bigint"
OBJECT_TAG: Final[str] = "$object"

# Every tag name the encoding reserves, so a fixture can never use one as an ordinary dict key.
RESERVED_TAGS: Final[frozenset[str]] = frozenset(
    {
        TUPLE_TAG,
        SET_TAG,
        FROZENSET_TAG,
        DATETIME_TAG,
        DATE_TAG,
        TIME_TAG,
        TIMEDELTA_TAG,
        DICT_TAG,
        BIGINT_TAG,
        OBJECT_TAG,
    }
)

# The inclusive `int` range JSON round-trips without loss; an `int` outside it is
# tagged as `$bigint` since onix's serde_json reader has no arbitrary-precision int form.
_I64_MIN: Final[int] = -(2**63)
_U64_MAX: Final[int] = 2**64 - 1

# The key kinds a dict may hold, mirroring `onix_core::value::ObjectKey`'s non-`str` case.
DictKey = Union[
    str,
    int,
    float,
    bool,
    None,
    datetime.datetime,
    datetime.date,
    tuple[Union[str, int, float, bool, None, datetime.datetime, datetime.date], ...],
]

# DeepDiff's own `to_json()` raises on a `date`/`time`/`timedelta`; this mapping renders
# them the way onix does, so `to_json(default_mapping=...)` stays real DeepDiff's own output.
_Renderable = Union[datetime.date, datetime.time, datetime.timedelta]

JSON_DEFAULT_MAPPING: Final[dict[type, Callable[[_Renderable], str]]] = {
    datetime.date: datetime.date.isoformat,
    datetime.time: datetime.time.isoformat,
    datetime.timedelta: str,
}

# A JSON-shaped value, plus the Python types the tags decode to.
TaggedValue = Union[
    dict[DictKey, "TaggedValue"],
    list["TaggedValue"],
    tuple["TaggedValue", ...],
    set["SetMember"],
    frozenset["SetMember"],
    datetime.datetime,
    datetime.date,
    datetime.time,
    datetime.timedelta,
    str,
    int,
    float,
    bool,
    None,
]

# What a Python set can hold: hashable values only, so no dict, list or set.
SetMember = Union[
    tuple["SetMember", ...],
    frozenset["SetMember"],
    datetime.datetime,
    datetime.date,
    datetime.time,
    datetime.timedelta,
    str,
    int,
    float,
    bool,
    None,
]


class GoldenObject:
    """A custom-object marker a golden ``CASE`` writes in place of a live user-class instance."""

    def __init__(self, class_name: str, attrs: dict[str, "TaggedValue"]) -> None:
        self.class_name = class_name
        self.attrs = attrs

    def __eq__(self, other: object) -> bool:
        # Also equal to a decoded live instance carrying the same `__dict__`.
        if isinstance(other, GoldenObject):
            return self.class_name == other.class_name and self.attrs == other.attrs
        if type(other) in _OBJECT_CLASSES.values():
            return type(other).__name__ == self.class_name and vars(other) == self.attrs
        return NotImplemented

    __hash__ = None  # type: ignore[assignment]  # a mutable marker is never hashed


class GoldenEnum(GoldenObject):
    """An ``Enum`` member as a top-level golden input: written as the ``name`` and ``value`` ``_diff_enum`` reads."""

    def __init__(self, member: enum.Enum) -> None:
        super().__init__(type(member).__name__, {"name": member.name, "value": member.value})
        self.member = member


# Classes created for `$object` tags, cached by name so two instances of the same
# class share one `type` object; a fresh class per instance would spuriously
# `type_changes` every same-class diff, since `DeepDiff` compares `type(t1) != type(t2)`.
_OBJECT_CLASSES: dict[str, type] = {}


def _object_class(class_name: str) -> type:
    """Return the cached plain, attribute-only class named `class_name`."""
    cls = _OBJECT_CLASSES.get(class_name)
    if cls is None:
        cls = type(class_name, (), {})
        _OBJECT_CLASSES[class_name] = cls
    return cls


def _make_object(class_name: str, attrs: dict[str, "TaggedValue"]) -> object:
    """Build a live instance of the cached class `class_name` carrying `attrs`."""
    cls = _object_class(class_name)
    obj = cls.__new__(cls)
    for key, value in attrs.items():
        setattr(obj, key, value)
    return obj


def _sole_tag(value: dict[str, TaggedValue]) -> str | None:
    """Return the reserved tag `value` is an encoding of, or ``None`` if it is plain data."""
    if len(value) != 1:
        return None

    key = next(iter(value))

    return key if key in RESERVED_TAGS else None


def encode_tags(value: TaggedValue) -> TaggedValue:
    """
    Encode a Python value into its JSON-writable tagged form.

    :raises ValueError: If a plain dict's only key is itself a reserved tag name.
    """
    if isinstance(value, GoldenObject):
        return {
            OBJECT_TAG: {
                "class": value.class_name,
                "attrs": {key: encode_tags(item) for key, item in value.attrs.items()},
            }
        }

    if isinstance(value, tuple):
        return {TUPLE_TAG: [encode_tags(item) for item in value]}

    # `bool` is an `int` subclass but has its own JSON literal.
    if isinstance(value, int) and not isinstance(value, bool) and not (_I64_MIN <= value <= _U64_MAX):
        return {BIGINT_TAG: str(value)}

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

    # Written in onix's canonical set order, not the live set's PYTHONHASHSEED-dependent one.
    if isinstance(value, frozenset):
        return {FROZENSET_TAG: [encode_tags(item) for item in canonical_set_order(value)]}

    if isinstance(value, set):
        return {SET_TAG: [encode_tags(item) for item in canonical_set_order(value)]}

    if isinstance(value, list):
        return [encode_tags(item) for item in value]

    if isinstance(value, dict):
        # A non-`str` key forces the `$dict` pair-list form for every key in the dict.
        if not all(isinstance(key, str) for key in value):
            return {DICT_TAG: [[encode_tags(key), encode_tags(item)] for key, item in value.items()]}

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

    :raises NotImplementedError: If the value carries a reserved tag no decoder supports yet.
    """
    if isinstance(value, list):
        return [decode_tags(item) for item in value]

    if isinstance(value, dict):
        tag = _sole_tag(value)

        if tag == TUPLE_TAG:
            return tuple(decode_tags(item) for item in value[tag])

        if tag == BIGINT_TAG:
            return int(str(value[tag]))

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

        if tag == DICT_TAG:
            pairs = value[tag]
            if not isinstance(pairs, list):
                raise TypeError(f"the {DICT_TAG!r} tag's payload must be a list of pairs")
            return {decode_tags(key): decode_tags(item) for key, item in pairs}

        if tag == OBJECT_TAG:
            payload = value[tag]
            if not isinstance(payload, dict):
                raise TypeError(f"the {OBJECT_TAG!r} tag's payload must be an object")
            attrs = payload["attrs"]
            if not isinstance(attrs, dict):
                raise TypeError(f"the {OBJECT_TAG!r} tag's attrs must be an object")
            return _make_object(
                str(payload["class"]),
                {str(key): decode_tags(item) for key, item in attrs.items()},
            )

        if tag is not None:
            raise NotImplementedError(f"the {tag!r} tag is reserved but not decodable yet")

        return {key: decode_tags(item) for key, item in value.items()}

    return value


# The two report categories whose entries are bare path strings, not path-keyed values.
SET_CATEGORIES: Final[frozenset[str]] = frozenset({"set_item_added", "set_item_removed"})


class OnixReport(Protocol):
    """The one method :func:`sorted_set_categories` needs from an onix report."""

    def to_json(self) -> str:
        """Render the report as a JSON string."""


class RealReport(Protocol):
    """The two methods :func:`canonical_report` needs from a real DeepDiff report."""

    def to_json(self, default_mapping: dict[type, Callable[[datetime.date], str]]) -> str:
        """Render the report as a JSON string, using `default_mapping` for types it cannot render itself."""

    def to_dict(self) -> dict[str, object]:
        """Render the report as native Python objects, with real ``set``/``frozenset`` still in place."""


def canonical_report(diff: RealReport) -> TaggedValue:
    """
    Render one real DeepDiff report as the JSON spec onix must match.

    Reorders only the two set categories and every set-derived array into
    :func:`canonical_set_order`, since a Python set's own iteration order is
    PYTHONHASHSEED-dependent; everything else stays exactly as ``to_json()`` wrote it.
    """
    # `JSON_DEFAULT_MAPPING` lets a `date`-carrying case render; stock `to_json()` raises on one.
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
    """Render one onix report, sorting only the two set categories (its arrays are already canonical)."""
    return {
        category: sorted(entries) if category in SET_CATEGORIES else entries
        for category, entries in json.loads(diff.to_json()).items()
    }


def _canonical_value(as_object: object, as_json: TaggedValue) -> TaggedValue:
    """Reorder every set-derived array inside one report entry; leave everything else alone."""
    if isinstance(as_object, (set, frozenset)) and isinstance(as_json, list):
        paired = sorted(zip(as_object, as_json), key=lambda pair: _order_key(pair[0]))

        return [_canonical_value(member, element) for member, element in paired]

    if isinstance(as_object, (list, tuple)) and isinstance(as_json, list):
        return [
            _canonical_value(member, element) for member, element in zip(as_object, as_json)
        ]

    if isinstance(as_object, dict) and isinstance(as_json, dict):
        # Paired positionally: a non-`str` key is stringified by `to_json()`, so it
        # can never look itself up in `as_json` by identity.
        return dict(
            zip(
                as_json.keys(),
                (
                    _canonical_value(value, json_value)
                    for value, json_value in zip(as_object.values(), as_json.values())
                ),
            )
        )

    return as_json


def canonical_set_order(members: object) -> list[SetMember]:
    """Sort a set's members into onix's canonical order, the Python twin of ``onix_core::value::SetItems``."""
    return sorted(members, key=_order_key)


def _order_key(value: object) -> tuple[object, ...]:
    """
    Build the sort key :func:`canonical_set_order` compares by.

    :raises TypeError: If `value` is of a kind no set can hold.
    """
    if value is None:
        return (0,)

    # `bool` before `int`: every bool is an int in Python.
    if isinstance(value, bool):
        return (1, value)

    if isinstance(value, int):
        return (2, value)

    if isinstance(value, float):
        # Folds -0.0 into +0.0, matching `number_cmp` in crates/onix-core/src/value.rs.
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
    """Build a `datetime`'s ordering key: UTC instant, then awareness, then raw offset (naive read as UTC)."""
    offset = value.utcoffset() or datetime.timedelta()
    naive = value.replace(tzinfo=None) - offset
    epoch = datetime.datetime(1970, 1, 1)
    delta = naive - epoch
    micros = (delta.days * 86_400 + delta.seconds) * 1_000_000 + delta.microseconds

    return (micros, value.tzinfo is not None, int(offset.total_seconds()))


def _time_sort_key(value: datetime.time) -> tuple[bool, int, int]:
    """Build a `time`'s ordering key: naive first, then offset-adjusted micros-of-day, then raw offset."""
    offset = value.utcoffset()
    wall_micros = (
        (value.hour * 3600 + value.minute * 60 + value.second) * 1_000_000 + value.microsecond
    )
    offset_seconds = int(offset.total_seconds()) if offset is not None else 0

    return (offset is not None, wall_micros - offset_seconds * 1_000_000, offset_seconds)
