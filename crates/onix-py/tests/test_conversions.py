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


# unsupported dict keys and types


def test_non_str_dict_key_raises_type_error() -> None:
    """A dict with a non-str key raises TypeError naming the key's type."""
    with pytest.raises(TypeError, match="int"):
        DeepDiff({1: "a"}, {1: "b"})


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


def test_non_str_dict_key_error_reports_path_to_the_dict() -> None:
    """A non-str dict key error reports the path to the dict containing it, not just the type."""
    with pytest.raises(TypeError, match=r"int at root\['a'\]"):
        DeepDiff({"a": {1: "x"}}, {"a": {1: "y"}})


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
