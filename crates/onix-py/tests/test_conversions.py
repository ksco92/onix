"""Conversion-error tests: every documented MVP-unsupported-input path.

Covers `deepdiff_rs.DeepDiff`'s Python-object-to-`Value` conversion (see
`crates/onix-py/src/convert.rs`'s module doc for the authoritative
conversion table this pins) and `deepdiff_rs.diff_json`'s JSON-parse error
path.
"""

import collections
import datetime
import math

import pytest

from deepdiff_rs import DeepDiff, diff_json


# int range


def test_int_within_i64_range_is_accepted() -> None:
    """A large-but-in-range negative int (fits i64) converts without error."""
    diff = DeepDiff(-(2**63), 0)
    assert diff.to_dict()["values_changed"]["root"]["old_value"] == -(2**63)


def test_int_within_u64_range_is_accepted() -> None:
    """A large-but-in-range positive int (fits u64, beyond i64::MAX) converts without error."""
    diff = DeepDiff(2**64 - 1, 0)
    assert diff.to_dict()["values_changed"]["root"]["old_value"] == 2**64 - 1


def test_int_beyond_u64_max_raises_value_error() -> None:
    """An int beyond u64::MAX raises ValueError naming the MVP limitation."""
    with pytest.raises(ValueError, match="arbitrary-precision integers"):
        DeepDiff(2**64, 0)


def test_int_below_i64_min_raises_value_error() -> None:
    """An int below i64::MIN raises ValueError naming the MVP limitation."""
    with pytest.raises(ValueError, match="arbitrary-precision integers"):
        DeepDiff(-(2**63) - 1, 0)


# float finiteness


def test_nan_float_raises_value_error() -> None:
    """A NaN float raises ValueError (JSON has no representation for it)."""
    with pytest.raises(ValueError, match="NaN and infinite"):
        DeepDiff(math.nan, 0.0)


def test_positive_infinity_raises_value_error() -> None:
    """A positive-infinity float raises ValueError."""
    with pytest.raises(ValueError, match="NaN and infinite"):
        DeepDiff(math.inf, 0.0)


def test_negative_infinity_raises_value_error() -> None:
    """A negative-infinity float raises ValueError."""
    with pytest.raises(ValueError, match="NaN and infinite"):
        DeepDiff(-math.inf, 0.0)


def test_finite_float_is_accepted() -> None:
    """An ordinary finite float converts without error."""
    diff = DeepDiff(1.5, 2.5)
    assert diff.to_dict()["values_changed"]["root"]["old_value"] == 1.5


# non-str dict keys (see crates/onix-core/src/value.rs's `ObjectKey`)


def test_int_dict_key_is_accepted_and_diffed() -> None:
    """An int dict key converts and diffs like real DeepDiff, at its own repr'd path."""
    diff = DeepDiff({1: "a"}, {1: "b"})
    assert diff.to_dict() == {"values_changed": {"root[1]": {"new_value": "b", "old_value": "a"}}}


def test_bool_none_and_float_dict_keys_are_accepted() -> None:
    """`bool`, `None`, and `float` dict keys all convert, each at its own repr'd path.

    Both sides share enough keys ("z" plus every eventually-shared new one) to stay above
    DeepDiff's own `threshold_to_diff_deeper` ratio, so this reports one entry per key
    instead of collapsing to a single wholesale `values_changed` on the whole dict."""
    diff = DeepDiff(
        {"z": 0, True: 1, None: 2, 1.5: 3},
        {"z": 0, True: 10, None: 20, 1.5: 30},
    )
    assert diff.to_dict() == {
        "values_changed": {
            "root[True]": {"new_value": 10, "old_value": 1},
            "root[None]": {"new_value": 20, "old_value": 2},
            "root[1.5]": {"new_value": 30, "old_value": 3},
        }
    }


def test_datetime_and_date_dict_keys_are_accepted() -> None:
    """A `datetime`/`date` dict key converts, at a path rendered via Python's own `repr()`.

    Above the `threshold_to_diff_deeper` ratio — see the bool/None/float test's own note."""
    dt = datetime.datetime(2024, 1, 1, 10, 30)
    d = datetime.date(2024, 1, 1)
    diff = DeepDiff({"z": 0, dt: "a", d: "b"}, {"z": 0, dt: "a2", d: "b2"})
    assert diff.to_dict() == {
        "values_changed": {
            "root[datetime.datetime(2024, 1, 1, 10, 30)]": {
                "new_value": "a2",
                "old_value": "a",
            },
            "root[datetime.date(2024, 1, 1)]": {"new_value": "b2", "old_value": "b"},
        }
    }


def test_tuple_dict_key_is_accepted_and_splits_the_path_per_element() -> None:
    """A tuple dict key of scalars converts; the path splits into one subscript per element,
    matching real `DeepDiff` (not `root[(1, 2)]`)."""
    diff = DeepDiff({}, {(1, 2): "x"})
    assert diff.to_dict() == {"dictionary_item_added": {"root[1][2]": "x"}}


def test_int_and_float_dict_keys_match_by_python_equality() -> None:
    """`1` and `1.0` are the same *key* to `DeepDiff` (Python `dict` equality), so this is a
    values_changed at the shared key, not an added+removed pair."""
    diff = DeepDiff({1: "a"}, {1.0: "b"})
    assert diff.to_dict() == {"values_changed": {"root[1.0]": {"new_value": "b", "old_value": "a"}}}


def test_to_dict_returns_the_original_non_str_key_object_in_a_nested_value() -> None:
    """`to_dict()` hands back a reported *value* that is itself a dict with its real, non-`str`
    key objects intact (an `int`, not `"1"`) — unlike `to_json()`, which must stringify it."""
    diff = DeepDiff({}, {"a": {1: "x"}})
    nested = diff.to_dict()["dictionary_item_added"]["root['a']"]
    assert nested == {1: "x"}
    (key,) = nested.keys()
    assert isinstance(key, int) and not isinstance(key, bool)


def test_complex_dict_key_raises_type_error() -> None:
    """A dict key of a type outside the accepted set raises TypeError naming it."""
    with pytest.raises(TypeError, match="complex"):
        DeepDiff({complex(1, 2): "a"}, {})


def test_tuple_dict_key_containing_a_nested_tuple_is_rejected() -> None:
    """A tuple dict key may not itself nest another tuple — only the scalar kinds it wraps."""
    with pytest.raises(TypeError, match=r"tuple at root"):
        DeepDiff({}, {((1, 2), 3): "x"})


def test_namedtuple_dict_key_is_rejected() -> None:
    """A tuple *subclass* key is refused, like a tuple subclass value (see the module doc)."""
    point = collections.namedtuple("Point", "x y")
    with pytest.raises(TypeError, match="Point"):
        DeepDiff({}, {point(1, 2): "x"})


def test_tuple_is_accepted_and_diffed_positionally() -> None:
    """A tuple converts and diffs element by element, like real DeepDiff."""
    diff = DeepDiff((1, 2, 3), (1, 2, 4))
    assert diff.to_dict() == {"values_changed": {"root[2]": {"new_value": 4, "old_value": 3}}}


def test_namedtuple_raises_type_error_naming_the_class() -> None:
    """A namedtuple is not a plain tuple to DeepDiff (it walks fields), so it is refused."""
    point = collections.namedtuple("Point", "x y")

    with pytest.raises(TypeError, match="Point"):
        DeepDiff((point(1, 2),), (point(1, 3),))


def test_set_converts_and_diffs() -> None:
    """A set is supported: it diffs into the two set categories (see test_sets.py)."""
    assert DeepDiff({1, 2}, {1, 3}).to_dict() == {
        "set_item_added": ["root[3]"],
        "set_item_removed": ["root[2]"],
    }


def test_frozenset_converts_and_diffs() -> None:
    """A frozenset is supported too, and stays distinct from a set."""
    assert DeepDiff(frozenset({1, 2}), frozenset({1, 3})).to_dict() == {
        "set_item_added": ["root[3]"],
        "set_item_removed": ["root[2]"],
    }


def test_unhashable_set_member_raises_type_error_naming_the_set() -> None:
    """A member no Python set can normally hold is refused, reporting the set's own path."""

    class HashableDict(dict):
        __hash__ = object.__hash__

    with pytest.raises(TypeError, match=r"HashableDict at root\[<set member>\]"):
        DeepDiff({HashableDict()}, {1})


def test_datetime_is_accepted_and_compared_by_instant() -> None:
    """A datetime converts and diffs, reporting the pair normalized to UTC."""
    diff = DeepDiff(datetime.datetime(2024, 1, 1, 10), datetime.datetime(2024, 1, 2, 10))

    assert diff.to_dict() == {
        "values_changed": {
            "root": {
                "old_value": datetime.datetime(2024, 1, 1, 10, tzinfo=datetime.timezone.utc),
                "new_value": datetime.datetime(2024, 1, 2, 10, tzinfo=datetime.timezone.utc),
            }
        }
    }


def test_date_is_accepted_and_compared_by_value() -> None:
    """A date converts and diffs, reporting real date objects."""
    diff = DeepDiff(datetime.date(2024, 1, 1), datetime.date(2024, 1, 2))

    assert diff.to_dict() == {
        "values_changed": {
            "root": {"old_value": datetime.date(2024, 1, 1), "new_value": datetime.date(2024, 1, 2)}
        }
    }


def test_time_is_accepted_and_reports_the_raw_pair() -> None:
    """A time converts and diffs; unlike a datetime, the report carries the raw pair."""
    diff = DeepDiff(datetime.time(10), datetime.time(11))

    assert diff.to_dict() == {
        "values_changed": {
            "root": {"old_value": datetime.time(10), "new_value": datetime.time(11)}
        }
    }


def test_timedelta_is_accepted_and_reports_real_timedelta_objects() -> None:
    """A timedelta converts and diffs, reporting real timedelta objects."""
    diff = DeepDiff(datetime.timedelta(days=1), datetime.timedelta(days=2))

    assert diff.to_dict() == {
        "values_changed": {
            "root": {
                "old_value": datetime.timedelta(days=1),
                "new_value": datetime.timedelta(days=2),
            }
        }
    }


def test_time_subclass_raises_type_error_naming_the_class() -> None:
    """DeepDiff reports a value under its own type name, so a `time` subclass is refused."""

    class Clock(datetime.time):
        pass

    with pytest.raises(TypeError, match="Clock"):
        DeepDiff(Clock(10), datetime.time(10))


def test_timedelta_subclass_raises_type_error_naming_the_class() -> None:
    """The same rule for a `timedelta` subclass."""

    class Duration(datetime.timedelta):
        pass

    with pytest.raises(TypeError, match="Duration"):
        DeepDiff(Duration(days=1), datetime.timedelta(days=1))


def test_datetime_subclass_raises_type_error_naming_the_class() -> None:
    """DeepDiff reports a value under its own type name, so a subclass is refused."""

    class Stamp(datetime.datetime):
        pass

    with pytest.raises(TypeError, match="Stamp"):
        DeepDiff(Stamp(2024, 1, 1), datetime.datetime(2024, 1, 1))


def test_date_subclass_raises_type_error_naming_the_class() -> None:
    """The same rule for a `date` subclass, which the exact cast also refuses."""

    class Day(datetime.date):
        pass

    with pytest.raises(TypeError, match="Day"):
        DeepDiff(Day(2024, 1, 1), datetime.date(2024, 1, 1))


def test_sub_second_utc_offset_raises_value_error() -> None:
    """A tzinfo whose utcoffset() carries microseconds is out of the value model."""
    tz = datetime.timezone(datetime.timedelta(seconds=1800, microseconds=5))

    with pytest.raises(ValueError, match="whole number of seconds"):
        DeepDiff(datetime.datetime(2024, 1, 1, tzinfo=tz), datetime.datetime(2024, 1, 2))


def test_custom_object_raises_type_error() -> None:
    """An arbitrary custom object raises TypeError naming its class."""

    class Custom:
        pass

    with pytest.raises(TypeError, match="Custom"):
        DeepDiff(Custom(), Custom())


def test_unsupported_type_is_reported_even_when_nested() -> None:
    """An unsupported type nested inside an otherwise-supported dict raises with its exact path."""
    with pytest.raises(TypeError, match=r"complex at root\['a'\]\['b'\]\[1\]"):
        DeepDiff({"a": {"b": [1, 1j]}}, {"a": {"b": [1, 2j]}})


def test_unsupported_type_nested_in_a_tuple_reports_its_path() -> None:
    """A tuple is walked like a list, so an unsupported element inside one reports its index."""
    with pytest.raises(TypeError, match=r"complex at root\['a'\]\[1\]"):
        DeepDiff({"a": (1, 1j)}, {"a": (1, 2j)})


def test_unsupported_type_at_root_reports_bare_root_path() -> None:
    """A top-level unsupported value reports the bare `root` path."""
    with pytest.raises(TypeError, match=r"complex at root;"):
        DeepDiff(1j, 2j)


def test_unsupported_dict_key_error_reports_path_to_the_dict() -> None:
    """An unsupported dict key error reports the path to the dict containing it, not just the
    key's type — the key itself has no path segment of its own to report."""
    with pytest.raises(TypeError, match=r"complex at root\['a'\]"):
        DeepDiff({"a": {complex(1, 2): "x"}}, {"a": {complex(1, 2): "y"}})


# lone (unpaired) surrogates: legal in a Python str, not representable as UTF-8; see
# tests/golden/README.md for the documented divergence from real DeepDiff.


def test_lone_surrogate_value_raises_value_error() -> None:
    """A lone surrogate value raises ValueError instead of silently comparing equal."""
    with pytest.raises(ValueError, match=r"str at root contains a lone"):
        DeepDiff("\udc80", "\udc81")


def test_distinct_lone_surrogates_both_raise_the_same_way() -> None:
    """A different lone surrogate pair is refused identically, not silently equated."""
    with pytest.raises(ValueError, match=r"str at root contains a lone"):
        DeepDiff("\udc81", "\udc82")


def test_identical_lone_surrogate_value_still_raises() -> None:
    """An identical pair still raises, even though real DeepDiff reports no change for it."""
    with pytest.raises(ValueError, match=r"str at root contains a lone"):
        DeepDiff("\udc80", "\udc80")


def test_identical_lone_surrogate_dict_key_still_raises() -> None:
    """The same holds for a dict key equal on both sides: conversion still validates it."""
    with pytest.raises(ValueError, match=r"dict key at root\['a'\] contains a lone"):
        DeepDiff({"a": {"\udc80": 1}}, {"a": {"\udc80": 1}})


def test_identical_lone_surrogate_set_item_still_raises() -> None:
    """The same holds for a set member equal on both sides: real DeepDiff would still crash."""
    with pytest.raises(ValueError, match=r"str at root\[<set member>\] contains a lone"):
        DeepDiff({"\udc80"}, {"\udc80"})


def test_lone_surrogate_nested_in_list_reports_its_path() -> None:
    """The error names the exact path, like every other conversion error in this module."""
    with pytest.raises(ValueError, match=r"str at root\['a'\]\[1\] contains a lone"):
        DeepDiff({"a": [1, "\udc80"]}, {"a": [1, "ok"]})


def test_lone_surrogate_dict_key_raises_value_error_naming_the_dict() -> None:
    """A lone surrogate dict key raises ValueError naming the containing dict's path."""
    with pytest.raises(ValueError, match=r"dict key at root\['a'\] contains a lone"):
        DeepDiff({"a": {"\udc80": 1}}, {"a": {"ok": 1}})


def test_lone_surrogate_set_item_raises_value_error() -> None:
    """A lone surrogate set member raises ValueError; real DeepDiff crashes hashing one instead."""
    with pytest.raises(ValueError, match=r"str at root\[<set member>\] contains a lone"):
        DeepDiff({"\udc80"}, {"ok"})


def test_lone_surrogate_tuple_item_raises_value_error() -> None:
    """A lone surrogate inside a tuple raises ValueError naming its index."""
    with pytest.raises(ValueError, match=r"str at root\[0\] contains a lone"):
        DeepDiff(("\udc80",), ("ok",))


def test_non_bmp_character_is_accepted() -> None:
    """A genuine non-BMP character converts fine: only an unpaired surrogate is refused."""
    diff = DeepDiff("😀", "😁")
    assert diff.to_dict()["values_changed"]["root"] == {"new_value": "😁", "old_value": "😀"}


# diff_json's own error path (JSON parsing, not Python-object conversion)


def test_diff_json_invalid_json_raises_value_error() -> None:
    """Malformed JSON text raises ValueError naming which argument failed."""
    with pytest.raises(ValueError, match='"b"'):
        diff_json("{}", "not json")


def test_diff_json_valid_input_round_trips() -> None:
    """Sanity check: diff_json parses, diffs, and serializes valid JSON."""
    result = diff_json('{"a": 1}', '{"a": 2}')
    assert result == '{"values_changed":{"root[\'a\']":{"new_value":2,"old_value":1}}}'
