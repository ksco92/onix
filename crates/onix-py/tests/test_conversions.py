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


def test_set_raises_type_error() -> None:
    """A set (unsupported in this MVP) raises TypeError naming `set`."""
    with pytest.raises(TypeError, match="set"):
        DeepDiff({1, 2}, {1, 3})


def test_frozenset_raises_type_error() -> None:
    """A frozenset (unsupported in this MVP) raises TypeError naming `frozenset`."""
    with pytest.raises(TypeError, match="frozenset"):
        DeepDiff(frozenset({1, 2}), frozenset({1, 3}))


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


def test_time_raises_type_error() -> None:
    """A time (unsupported in this MVP) raises TypeError naming the type."""
    with pytest.raises(TypeError, match=r"diffing: time at root"):
        DeepDiff(datetime.time(10), datetime.time(11))


def test_timedelta_raises_type_error() -> None:
    """A timedelta (unsupported in this MVP) raises TypeError naming the type."""
    with pytest.raises(TypeError, match=r"diffing: timedelta at root"):
        DeepDiff(datetime.timedelta(days=1), datetime.timedelta(days=2))


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
    with pytest.raises(TypeError, match=r"set at root\['a'\]\['b'\]\[1\]"):
        DeepDiff({"a": {"b": [1, {1, 2}]}}, {"a": {"b": [1, {1, 3}]}})


def test_unsupported_type_nested_in_a_tuple_reports_its_path() -> None:
    """A tuple is walked like a list, so an unsupported element inside one reports its index."""
    with pytest.raises(TypeError, match=r"set at root\['a'\]\[1\]"):
        DeepDiff({"a": (1, {1, 2})}, {"a": (1, {1, 3})})


def test_unsupported_type_at_root_reports_bare_root_path() -> None:
    """A top-level unsupported value reports the bare `root` path."""
    with pytest.raises(TypeError, match=r"set at root;"):
        DeepDiff({1, 2}, {1, 3})


def test_non_str_dict_key_error_reports_path_to_the_dict() -> None:
    """A non-str dict key error reports the path to the dict containing it, not just the type."""
    with pytest.raises(TypeError, match=r"int at root\['a'\]"):
        DeepDiff({"a": {1: "x"}}, {"a": {1: "y"}})


# diff_json's own error path (JSON parsing, not Python-object conversion)


def test_diff_json_invalid_json_raises_value_error() -> None:
    """Malformed JSON text raises ValueError naming which argument failed."""
    with pytest.raises(ValueError, match='"b"'):
        diff_json("{}", "not json")


def test_diff_json_valid_input_round_trips() -> None:
    """Sanity check: diff_json parses, diffs, and serializes valid JSON."""
    result = diff_json('{"a": 1}', '{"a": 2}')
    assert result == '{"values_changed":{"root[\'a\']":{"new_value":2,"old_value":1}}}'
