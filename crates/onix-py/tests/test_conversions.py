"""Conversion tests: every documented MVP-unsupported-input path, plus subclass acceptance.

Covers `deepdiff_rs.DeepDiff`'s Python-object-to-`Value` conversion (see
`crates/onix-py/src/convert.rs`'s module doc for the authoritative
conversion table this pins) and `deepdiff_rs.diff_json`'s JSON-parse error
path.
"""

import collections
import datetime
import json
import math

import pytest
from conftest import _normalize_types, require_deepdiff

require_deepdiff()

from deepdiff import DeepDiff as RealDeepDiff

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


def test_int_beyond_u64_max_is_accepted_and_kept_exact() -> None:
    """An int beyond u64::MAX converts and round-trips through to_dict() exactly."""
    diff = DeepDiff(2**100, 2**100 + 1)
    assert diff.to_dict() == {
        "values_changed": {"root": {"new_value": 2**100 + 1, "old_value": 2**100}}
    }


def test_int_below_i64_min_is_accepted_and_kept_exact() -> None:
    """An int below i64::MIN converts and keeps its exact negative value."""
    diff = DeepDiff(-(2**100), -(2**100) - 1)
    assert diff.to_dict()["values_changed"]["root"]["old_value"] == -(2**100)


def test_big_int_2_64_boundary_converts() -> None:
    """2**64, one past u64::MAX, converts and compares equal to itself (empty diff)."""
    assert DeepDiff(2**64, 2**64).to_dict() == {}


def test_big_int_matches_real_deepdiff_across_the_value_model() -> None:
    """A spread of big-int shapes matches real DeepDiff (values, type change, dict key, set)."""
    for a, b in [
        (2**100, 2**100 + 1),
        (-(2**100), 2**100),
        (10**20, 1e20),  # int-vs-float type change even though Python-equal
        ({"k": 2**80}, {"k": 2**80 + 7}),
        ({2**70: "a"}, {2**70: "b"}),  # big int as a dict key
        ([1, 2**100, 3], [3, 2**100, 1]),  # ignore_order shuffle (below)
    ]:
        assert _normalize_types(DeepDiff(a, b).to_dict()) == _normalize_types(
            RealDeepDiff(a, b, verbose_level=2).to_dict()
        ), f"mismatch for {a!r} vs {b!r}"


def test_big_int_under_ignore_order_matches_real_deepdiff() -> None:
    """Big ints pair by hash and by numeric distance under ignore_order, like real DeepDiff."""
    for a, b in [
        ([1, 2**100, 3], [3, 2**100, 1]),
        ([2**100], [2**100 + 1]),
        ([2**100], [1e100]),
    ]:
        assert _normalize_types(DeepDiff(a, b, ignore_order=True).to_dict()) == _normalize_types(
            RealDeepDiff(a, b, ignore_order=True, verbose_level=2).to_dict()
        ), f"mismatch for {a!r} vs {b!r}"


def test_big_int_to_json_emits_full_digits_and_reads_back() -> None:
    """to_json() writes a big int's full decimal digits, which json.loads parses back exactly."""
    diff = DeepDiff(2**100, 2**100 + 1)
    text = diff.to_json()
    # Full digits present verbatim (not a truncated/exponential float).
    assert str(2**100) in text
    assert str(2**100 + 1) in text
    # Python's own json reader recovers the exact integers.
    parsed = json.loads(text)
    assert parsed["values_changed"]["root"] == {
        "old_value": 2**100,
        "new_value": 2**100 + 1,
    }


def test_big_int_in_set_matches_real_deepdiff() -> None:
    """A big int inside a set diffs like real DeepDiff (add/remove by exact value)."""
    a, b = {1, 2**70}, {1, 2**71}
    assert _normalize_types(DeepDiff(a, b).to_dict()) == _normalize_types(
        RealDeepDiff(a, b, verbose_level=2).to_dict()
    )


# A big int's exact value is read from PyLong through `int`'s own *unbound*
# `bit_length`/`to_bytes` (see `exact_big_int` in crates/onix-py/src/convert.rs),
# so an `int` subclass cannot make onix compare a value other than the one it
# actually holds — no matter which method it overrides (`__str__`, `__index__`,
# `bit_length`, or `to_bytes` itself), because onix never calls the instance's
# own method. Each subclass below overrides a different one to a wrong answer
# and the true value must still survive.


class _LyingStr(int):
    """An `int` subclass whose `__str__` reports a value it does not hold."""

    def __str__(self: "_LyingStr") -> str:
        """Return a constant unrelated to the true value."""
        return "1"


class _MoneyInt(int):
    """An `int` subclass whose `__str__` is a non-numeric formatted string."""

    def __str__(self: "_MoneyInt") -> str:
        """Return a currency-formatted string, not a parseable integer."""
        return f"${int(self)}"


class _LyingBitLength(int):
    """An `int` subclass whose `bit_length` under-reports, which would truncate a naive read."""

    def bit_length(self: "_LyingBitLength") -> int:
        """Return 1, far below the true bit length."""
        return 1


class _LyingToBytes(int):
    """An `int` subclass whose `to_bytes` returns bytes for a different value."""

    def to_bytes(self: "_LyingToBytes", *_args: object, **_kwargs: object) -> bytes:
        """Return the two's-complement bytes of 1, not of the true value."""
        return (1).to_bytes(1, "little", signed=True)


@pytest.mark.parametrize("cls", [_LyingStr, _LyingBitLength, _LyingToBytes, _MoneyInt])
def test_big_int_subclass_overriding_a_read_method_compares_by_true_value(cls: type) -> None:
    """
    A subclass overriding any method the read might use cannot change the compared value.

    onix reads through ``int``'s own unbound ``bit_length``/``to_bytes``, so an
    override of ``__str__``, ``bit_length``, or ``to_bytes`` (and the non-numeric
    ``Money`` ``__str__``, which a decimal read would fail to parse) is bypassed
    and the true value survives.

    :param cls: The lying ``int`` subclass under test.
    """
    diff = DeepDiff(cls(10**30), cls(10**31))
    assert diff.to_dict() == {
        "values_changed": {"root": {"new_value": 10**31, "old_value": 10**30}}
    }


def test_big_int_subclass_negative_value_keeps_its_sign() -> None:
    """A negative big int under a lying subclass keeps its exact signed value."""
    diff = DeepDiff(_LyingStr(-(10**30)), _LyingStr(-(10**31)))
    assert diff.to_dict() == {
        "values_changed": {"root": {"new_value": -(10**31), "old_value": -(10**30)}}
    }


def test_big_int_subclass_lying_str_equal_values_report_nothing() -> None:
    """Two big ints of equal true value report nothing, even when __str__ lies about them."""
    assert DeepDiff(_LyingStr(10**30), _LyingStr(10**30)).to_dict() == {}


def test_big_int_subclass_lying_str_as_dict_key_still_diffs() -> None:
    """A lying __str__ on a big-int dict key does not hide the key's change (true value used)."""
    a, b = {_LyingStr(10**30): "a"}, {_LyingStr(10**31): "b"}
    assert _normalize_types(DeepDiff(a, b).to_dict()) == _normalize_types(
        RealDeepDiff(a, b, verbose_level=2).to_dict()
    )


def test_int_beyond_str_digit_cap_diffs_against_live_deepdiff() -> None:
    """An int past CPython's 4,300-digit int<->str cap diffs like live DeepDiff (bytes, no cap)."""
    old = 10**5000  # 5,001 digits, well past sys.get_int_max_str_digits()'s default 4,300
    assert _normalize_types(DeepDiff(old, old + 1).to_dict()) == _normalize_types(
        RealDeepDiff(old, old + 1, verbose_level=2).to_dict()
    )


def test_ten_thousand_digit_int_round_trips_through_to_dict() -> None:
    """A 10,000-digit int round-trips through to_dict() as its exact value (bytes read/write)."""
    old = 10**9999  # 10,000 digits
    result = DeepDiff(old, old + 1).to_dict()
    assert result["values_changed"]["root"]["old_value"] == old
    assert result["values_changed"]["root"]["new_value"] == old + 1


# float finiteness: non-finite floats convert; see test_non_finite.py.


def test_nan_float_is_accepted() -> None:
    """A NaN float converts without error (see test_non_finite.py)."""
    diff = DeepDiff(math.nan, 0.0)
    assert math.isnan(diff.to_dict()["values_changed"]["root"]["old_value"])


def test_positive_infinity_is_accepted() -> None:
    """A positive-infinity float converts without error."""
    diff = DeepDiff(math.inf, 0.0)
    assert diff.to_dict()["values_changed"]["root"]["old_value"] == math.inf


def test_negative_infinity_is_accepted() -> None:
    """A negative-infinity float converts without error."""
    diff = DeepDiff(-math.inf, 0.0)
    assert diff.to_dict()["values_changed"]["root"]["old_value"] == -math.inf


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
    """`bool`, `None`, and `float` dict keys all convert, each at its own repr'd path."""
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
    """A `datetime`/`date` dict key converts, at a path rendered via Python's own `repr()`."""
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


def test_namedtuple_dict_key_matches_a_plain_tuple_key_by_value() -> None:
    """A `namedtuple` key is accepted and matches a plain tuple key with the same elements,
    unlike a `namedtuple` *value* — `DeepDiff`'s dict-key matching is plain Python `==`/`hash`
    and never consults `type(obj)`, so no class name is tracked for a key at all."""
    point = collections.namedtuple("Point", "x y")
    a = {point(1, 2): "a"}
    b = {(1, 2): "a"}
    expected = RealDeepDiff(a, b, verbose_level=2).to_dict()
    assert DeepDiff(a, b).to_dict() == expected == {}


def test_tuple_subclass_dict_key_matches_and_mismatches_by_value() -> None:
    """A `tuple` subclass key (not a `namedtuple`) is accepted the same way, matching or not
    purely by element value."""

    class MyTuple(tuple):
        pass

    matching_a, matching_b = {MyTuple((1, 2)): "a"}, {(1, 2): "a"}
    expected_match = RealDeepDiff(matching_a, matching_b, verbose_level=2).to_dict()
    assert DeepDiff(matching_a, matching_b).to_dict() == expected_match == {}

    diff_a, diff_b = {MyTuple((1, 2)): "a"}, {(1, 3): "a"}
    expected_diff = RealDeepDiff(diff_a, diff_b, verbose_level=2).to_dict()
    assert DeepDiff(diff_a, diff_b).to_dict() == expected_diff
    assert expected_diff  # a different key: not the {} a matching key would give


def test_a_key_subclass_with_overridden_equality_matches_structurally_not_by_python_eq() -> None:
    """
    A documented nuance, not a bug: `onix` matches a subclass key by its base type's
    *value*, never by an overridden `__eq__`/`__hash__` — that is custom-object territory,
    out of this MVP's scope (see `crates/onix-py/src/convert.rs`'s `classify_dict_key` doc).

    Real `DeepDiff` uses the key's own (overridden) equality, so two keys this class calls
    equal collapse into one shared key there (`values_changed` at the surviving key's path);
    `onix` sees two structurally different keys and reports the whole dict changed instead.
    """

    class AlwaysEqual(tuple):
        def __eq__(self, other: object) -> bool:
            return True

        def __hash__(self) -> int:
            return 0

    a = {AlwaysEqual((1, 2)): "v1"}
    b = {AlwaysEqual((3, 4)): "v2"}

    assert RealDeepDiff(a, b, verbose_level=2).to_dict() == {
        "values_changed": {"root[3][4]": {"new_value": "v2", "old_value": "v1"}}
    }
    assert DeepDiff(a, b).to_dict() == {
        "values_changed": {"root": {"new_value": {(3, 4): "v2"}, "old_value": {(1, 2): "v1"}}}
    }


def test_datetime_subclass_dict_key_matches_and_mismatches_by_value() -> None:
    """A `datetime`/`date` subclass key (e.g. pandas `Timestamp`) is accepted and matches a
    plain key with the same instant, or not, purely by value — never by class."""

    class Stamp(datetime.datetime):
        pass

    matching_a, matching_b = {Stamp(2024, 1, 1): "a"}, {datetime.datetime(2024, 1, 1): "a"}
    expected_match = RealDeepDiff(matching_a, matching_b, verbose_level=2).to_dict()
    assert DeepDiff(matching_a, matching_b).to_dict() == expected_match == {}

    diff_a = {Stamp(2024, 1, 1): "a"}
    diff_b = {datetime.datetime(2024, 1, 2): "a"}
    expected_diff = _normalize_types(RealDeepDiff(diff_a, diff_b, verbose_level=2).to_dict())
    assert _normalize_types(DeepDiff(diff_a, diff_b).to_dict()) == expected_diff


def test_tuple_is_accepted_and_diffed_positionally() -> None:
    """A tuple converts and diffs element by element, like real DeepDiff."""
    diff = DeepDiff((1, 2, 3), (1, 2, 4))
    assert diff.to_dict() == {"values_changed": {"root[2]": {"new_value": 4, "old_value": 3}}}


def test_a_list_subclass_is_accepted_and_compares_as_a_list() -> None:
    """A `list` subclass diffs like a plain list, and reports its own name in a type change."""

    class MyList(list):
        pass

    same_type = DeepDiff(MyList([1, 2]), MyList([1, 3]))
    assert same_type.to_dict() == {"values_changed": {"root[1]": {"new_value": 3, "old_value": 2}}}

    cross_type = DeepDiff(MyList([1, 2]), [1, 2])
    entry = cross_type.to_dict()["type_changes"]["root"]
    assert entry == {
        "old_type": "MyList",
        "new_type": "list",
        "old_value": [1, 2],
        "new_value": [1, 2],
    }

    real = RealDeepDiff(MyList([1, 2]), [1, 2], verbose_level=2).to_dict()["type_changes"]["root"]
    assert real["old_type"] is MyList
    assert real["new_type"] is list


def test_a_dict_subclass_is_accepted_and_compares_as_a_dict() -> None:
    """The same rule holds for `dict`."""

    class MyDict(dict):
        pass

    same_type = DeepDiff(MyDict(a=1), MyDict(a=2))
    assert same_type.to_dict() == {
        "values_changed": {"root['a']": {"new_value": 2, "old_value": 1}}
    }

    cross_type = DeepDiff(MyDict(a=1), {"a": 1})
    entry = cross_type.to_dict()["type_changes"]["root"]
    assert entry == {
        "old_type": "MyDict",
        "new_type": "dict",
        "old_value": {"a": 1},
        "new_value": {"a": 1},
    }


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


def test_a_time_subclass_is_accepted_and_compared_by_value() -> None:
    """A `time` subclass diffs like a plain `time`, and reports its own name in a type change."""

    class Clock(datetime.time):
        pass

    same_class = DeepDiff(Clock(10), Clock(11))
    assert same_class.to_dict() == {
        "values_changed": {"root": {"old_value": Clock(10), "new_value": Clock(11)}}
    }

    cross_type = DeepDiff(Clock(10), datetime.time(10))
    expected = _normalize_types(
        RealDeepDiff(Clock(10), datetime.time(10), verbose_level=2).to_dict()
    )
    assert _normalize_types(cross_type.to_dict()) == expected
    assert expected["type_changes"]["root"]["old_type"] == "Clock"
    assert expected["type_changes"]["root"]["new_type"] == "time"


def test_a_timedelta_subclass_is_accepted_and_compared_by_value() -> None:
    """The same rule for a `timedelta` subclass."""

    class Duration(datetime.timedelta):
        pass

    same_class = DeepDiff(Duration(days=1), Duration(days=2))
    assert same_class.to_dict() == {
        "values_changed": {
            "root": {"old_value": Duration(days=1), "new_value": Duration(days=2)}
        }
    }

    cross_type = DeepDiff(Duration(days=1), datetime.timedelta(days=1))
    expected = _normalize_types(
        RealDeepDiff(Duration(days=1), datetime.timedelta(days=1), verbose_level=2).to_dict()
    )
    assert _normalize_types(cross_type.to_dict()) == expected
    assert expected["type_changes"]["root"]["old_type"] == "Duration"
    assert expected["type_changes"]["root"]["new_type"] == "timedelta"


def test_sub_second_utc_offset_raises_value_error() -> None:
    """A tzinfo whose utcoffset() carries microseconds is out of the value model."""
    tz = datetime.timezone(datetime.timedelta(seconds=1800, microseconds=5))

    with pytest.raises(ValueError, match="whole number of seconds"):
        DeepDiff(datetime.datetime(2024, 1, 1, tzinfo=tz), datetime.datetime(2024, 1, 2))


def test_custom_object_diffs_by_its_attributes() -> None:
    """A custom object diffs by its attributes (issue #66), matching DeepDiff's `_diff_obj`."""

    class Custom:
        def __init__(self, x: int, y: int) -> None:
            self.x = x
            self.y = y

    assert json.loads(DeepDiff(Custom(1, 2), Custom(1, 3)).to_json()) == {
        "values_changed": {"root.y": {"new_value": 3, "old_value": 2}}
    }


def test_two_different_classes_report_a_type_change() -> None:
    """Two instances of different classes are a `type_changes`, named by class (issue #66)."""

    class A:
        def __init__(self) -> None:
            self.x = 1

    class B:
        def __init__(self) -> None:
            self.x = 1

    report = json.loads(DeepDiff(A(), B()).to_json())
    assert report["type_changes"]["root"]["old_type"] == "A"
    assert report["type_changes"]["root"]["new_type"] == "B"


def test_a_custom_object_nested_in_a_dict_reports_its_attribute_path() -> None:
    """A custom object nested inside a dict keeps the dict subscript then a dotted attribute (issue #66)."""

    class Custom:
        def __init__(self, x: int) -> None:
            self.x = x

    assert json.loads(DeepDiff({"a": {"b": Custom(1)}}, {"a": {"b": Custom(2)}}).to_json()) == {
        "values_changed": {"root['a']['b'].x": {"new_value": 2, "old_value": 1}}
    }


def test_unsupported_dict_key_error_reports_path_to_the_dict() -> None:
    """An unsupported dict key error reports the path to the dict containing it, not just the
    key's type — the key itself has no path segment of its own to report."""
    with pytest.raises(TypeError, match=r"complex at root\['a'\]"):
        DeepDiff({"a": {complex(1, 2): "x"}}, {"a": {complex(1, 2): "y"}})


# lone (unpaired) surrogates: legal in a Python str, not representable as UTF-8.
# Compared and reported like any other str, matching DeepDiff's plain `==` — see
# tests/golden/README.md for the one accepted divergence (hashing one as a set member).
# `to_json()` is compared canonically (parsed, like test_differential_fuzz.py's own
# comparison, since neither tool promises identical whitespace) against live
# `deepdiff==9.1.0`, and `to_dict()` structurally — the golden-fixture corpus (plain JSON
# files) cannot hold this content at all, see tests/golden/README.md for why.


def test_lone_surrogate_equal_pair_reports_no_change() -> None:
    """An equal pair compares equal, matching DeepDiff's plain `==` (no hashing involved)."""
    diff = DeepDiff("a\udc80b", "a\udc80b")
    assert not diff
    assert diff.to_json() == "{}"
    real = RealDeepDiff("a\udc80b", "a\udc80b", verbose_level=2)
    assert not real


def test_lone_surrogate_differing_pair_matches_real_deepdiff() -> None:
    """A differing pair reports a values_changed entry, matching real DeepDiff's."""
    diff = DeepDiff("a\udc80b", "a\udc80c")
    real = RealDeepDiff("a\udc80b", "a\udc80c", verbose_level=2)
    assert json.loads(diff.to_json()) == json.loads(real.to_json())
    assert diff.to_dict() == real.to_dict()


def test_lone_surrogate_dict_key_matches_real_deepdiff() -> None:
    """A dict key holding a surrogate is diffed and its path rendered like real DeepDiff's."""
    diff = DeepDiff({"a\udc80b": 1}, {"a\udc80b": 2})
    real = RealDeepDiff({"a\udc80b": 1}, {"a\udc80b": 2}, verbose_level=2)
    assert json.loads(diff.to_json()) == json.loads(real.to_json())
    assert diff.to_dict() == real.to_dict()


def test_lone_surrogate_dict_key_added_matches_real_deepdiff() -> None:
    """A newly added dict key holding a surrogate reports dictionary_item_added correctly."""
    diff = DeepDiff({}, {"\udc80": 1})
    real = RealDeepDiff({}, {"\udc80": 1}, verbose_level=2)
    assert json.loads(diff.to_json()) == json.loads(real.to_json())
    assert diff.to_dict() == real.to_dict()


def test_lone_surrogate_in_list_matches_real_deepdiff() -> None:
    """A surrogate-holding string nested in a list reports its path like real DeepDiff's."""
    diff = DeepDiff({"a": [1, "\udc80"]}, {"a": [1, "\udc81"]})
    real = RealDeepDiff({"a": [1, "\udc80"]}, {"a": [1, "\udc81"]}, verbose_level=2)
    assert json.loads(diff.to_json()) == json.loads(real.to_json())
    assert diff.to_dict() == real.to_dict()


def test_lone_surrogate_in_tuple_matches_real_deepdiff() -> None:
    """A surrogate-holding string inside a tuple reports its index like real DeepDiff's."""
    diff = DeepDiff(("\udc80",), ("\udc81",))
    real = RealDeepDiff(("\udc80",), ("\udc81",), verbose_level=2)
    assert json.loads(diff.to_json()) == json.loads(real.to_json())
    assert diff.to_dict() == real.to_dict()


def test_lone_surrogate_set_item_added_and_removed() -> None:
    """A set holding a surrogate member diffs deterministically; real DeepDiff crashes hashing one."""
    diff = DeepDiff({"\udc80"}, {"\udc81"})
    assert diff.to_dict() == {
        "set_item_added": ["root['\\udc81']"],
        "set_item_removed": ["root['\\udc80']"],
    }
    with pytest.raises(UnicodeEncodeError):
        RealDeepDiff({"\udc80"}, {"\udc81"})


def test_lone_surrogate_frozenset_item_matches_deterministic_behavior() -> None:
    """The same deterministic-hashing divergence holds for a frozenset member."""
    diff = DeepDiff(frozenset({"\udc80"}), frozenset({"\udc80", "\udc81"}))
    assert diff.to_dict() == {"set_item_added": ["root['\\udc81']"]}
    with pytest.raises(UnicodeEncodeError):
        RealDeepDiff(frozenset({"\udc80"}), frozenset({"\udc80", "\udc81"}))


def test_lone_surrogate_under_ignore_order_diffs_deterministically_even_outside_a_set() -> None:
    """`ignore_order=True` hashes every value (DeepHash), not just a set's members.

    Real DeepDiff crashes with UnicodeEncodeError the moment a surrogate
    appears anywhere in the tree once ignore_order=True, even in a plain
    list with no set involved at all; onix diffs deterministically either way.
    """
    diff = DeepDiff({"a": ["x\udc80"]}, {"a": ["y\udc81"]}, ignore_order=True)
    assert diff.to_dict() == {
        "values_changed": {"root['a'][0]": {"new_value": "y\udc81", "old_value": "x\udc80"}}
    }
    with pytest.raises(UnicodeEncodeError):
        RealDeepDiff({"a": ["x\udc80"]}, {"a": ["y\udc81"]}, ignore_order=True)


def test_lone_surrogate_path_rendering_matches_real_deepdiff() -> None:
    """A top-level dict key holding a surrogate renders its path like real DeepDiff's."""
    diff = DeepDiff({"\udc80": 1}, {"\udc80": 2})
    real = RealDeepDiff({"\udc80": 1}, {"\udc80": 2}, verbose_level=2)
    assert json.loads(diff.to_json()) == json.loads(real.to_json())
    assert list(diff.to_dict()["values_changed"]) == list(real.to_dict()["values_changed"])


def test_lone_surrogate_high_and_low_surrogate_values() -> None:
    """Both surrogate halves (high 0xD800-0xDBFF and low 0xDC00-0xDFFF) round-trip correctly."""
    for code_point in (0xD800, 0xDBFF, 0xDC00, 0xDFFF):
        s = chr(code_point)
        diff = DeepDiff(s, s + "x")
        real = RealDeepDiff(s, s + "x", verbose_level=2)
        assert json.loads(diff.to_json()) == json.loads(real.to_json())
        assert diff.to_dict() == real.to_dict()


def test_non_bmp_character_is_accepted() -> None:
    """A genuine non-BMP character converts fine; only an unpaired surrogate needed this feature."""
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


# --- Object fallback: types DeepDiff routes elsewhere must raise, not {} (issue #66) ---


import array as _array  # noqa: E402
import decimal  # noqa: E402
import enum as _enum  # noqa: E402
import fractions  # noqa: E402
import io as _io  # noqa: E402
import ipaddress  # noqa: E402
import uuid  # noqa: E402


class _IterOnly:
    """An object that is iterable but carries attributes -- DeepDiff iterates it."""

    def __init__(self, items: object) -> None:
        self.items = items

    def __iter__(self) -> object:
        return iter(self.items)


class _Color(_enum.Enum):
    RED = 1
    GREEN = 2


@pytest.mark.parametrize(
    "value_factory",
    [
        lambda: b"hello",
        lambda: bytearray(b"hi"),
        lambda: range(3),
        lambda: 1 + 2j,
        object,
        lambda: memoryview(b"a"),
        lambda: _io.BytesIO(b"a"),
        lambda: _array.array("i", [1]),
        lambda: collections.deque([1]),
        lambda: decimal.Decimal("1.5"),
        lambda: fractions.Fraction(1, 2),
        lambda: uuid.UUID(int=1),
        lambda: ipaddress.ip_address("1.1.1.1"),
        lambda: _IterOnly([1, 2]),
        lambda: int,  # a class object
    ],
    ids=[
        "bytes", "bytearray", "range", "complex", "object", "memoryview",
        "BytesIO", "array", "deque", "decimal", "fraction", "uuid",
        "ipaddress", "iter_only", "class_object",
    ],
)
def test_types_deepdiff_routes_elsewhere_raise_rather_than_reporting_empty(value_factory) -> None:
    """
    A value DeepDiff sends to a handler onix lacks (an iterable, an attribute-less
    builtin, a class object) raises TypeError, never silently reports {} for two
    unequal values -- the invariant the object fallback must keep (issue #66).
    """
    a, b = value_factory(), value_factory()
    with pytest.raises(TypeError):
        DeepDiff(a, b)


def test_bytes_does_not_silently_compare_equal() -> None:
    """The headline defect: two different bytes must not report {} (a false negative)."""
    with pytest.raises(TypeError):
        DeepDiff(b"hello", b"world")


def test_a_dict_subclass_and_a_same_named_object_are_a_type_change_like_deepdiff() -> None:
    """A dict subclass and a custom object sharing a __name__ are a type_changes (issue #66)."""
    foo_dict = type("Foo", (dict,), {})
    foo_obj = type("Foo", (), {})

    for x_a, x_b in ((1, 1), (1, 2)):
        a = foo_dict({"x": x_a})
        b = foo_obj()
        b.x = x_b
        assert json.loads(DeepDiff(a, b).to_json()) == json.loads(RealDeepDiff(a, b, verbose_level=2).to_json())
        assert "type_changes" in json.loads(DeepDiff(a, b).to_json())


def test_same_named_classes_from_different_modules_are_a_type_change_like_deepdiff() -> None:
    """Two objects whose classes share a __name__ but differ by module are a type_changes (issue #66)."""
    cls_a = type("User", (), {})
    cls_a.__module__ = "package_a.models"
    cls_b = type("User", (), {})
    cls_b.__module__ = "package_b.models"

    for i_a, i_b in ((1, 1), (1, 2)):
        a = cls_a()
        a.i = i_a
        b = cls_b()
        b.i = i_b
        assert json.loads(DeepDiff(a, b).to_json()) == json.loads(RealDeepDiff(a, b, verbose_level=2).to_json())
        assert "type_changes" in json.loads(DeepDiff(a, b).to_json())


def test_same_class_objects_diff_by_attribute_not_type_change() -> None:
    """The control: two instances of one class diff by attribute, not type_changes (issue #66)."""

    class Same:
        def __init__(self, i: int) -> None:
            self.i = i

    assert json.loads(DeepDiff(Same(1), Same(2)).to_json()) == {
        "values_changed": {"root.i": {"new_value": 2, "old_value": 1}}
    }


def test_a_property_raising_non_attribute_error_propagates_at_the_path() -> None:
    """A @property getter raising ValueError propagates as ValueError, never swallowed (issue #66)."""

    class HasBadProperty:
        def __init__(self, x: int) -> None:
            self.x = x

        @property
        def bad(self) -> int:
            raise ValueError("boom")

    with pytest.raises(ValueError, match="boom"):
        DeepDiff(HasBadProperty(1), HasBadProperty(2))


def test_a_property_raising_attribute_error_is_skipped() -> None:
    """
    A @property raising AttributeError leaves that attribute out, where DeepDiff
    marks the whole object 'unprocessed' (a category onix does not have) -- a
    documented nuance (see tests/golden/README.md), pinned here.
    """

    class HasLazyProperty:
        def __init__(self, x: int) -> None:
            self.x = x

        @property
        def lazy(self) -> int:
            raise AttributeError("not yet")

    assert json.loads(DeepDiff(HasLazyProperty(1), HasLazyProperty(2)).to_json()) == {
        "values_changed": {"root.x": {"new_value": 2, "old_value": 1}}
    }


def test_a_property_mutating_the_instance_dict_does_not_panic() -> None:
    """A @property that mutates __dict__ mid-walk must not panic pyo3's dict iterator (issue #66)."""

    class SelfMutating:
        def __init__(self, x: int) -> None:
            self.x = x

        @property
        def sneaky(self) -> int:
            self.__dict__["injected"] = 99
            return 1

    # Completes without a PanicException; the snapshot copy makes the walk safe.
    assert "values_changed" in json.loads(DeepDiff(SelfMutating(1), SelfMutating(2)).to_json())


# --- Object attribute enumeration strategies match DeepDiff (issue #66 item 19) ---


def _canonical(a: object, b: object) -> tuple[object, object]:
    """Both engines' to_json, parsed, for a live-DeepDiff equality assertion."""
    return json.loads(DeepDiff(a, b).to_json()), json.loads(RealDeepDiff(a, b, verbose_level=2).to_json())


def test_slots_only_object_matches_deepdiff() -> None:
    """A slots-only class is diffed by its slot values, matching `_dict_from_slots` (issue #66)."""

    class Slots:
        __slots__ = ("a", "b")

        def __init__(self, a: int, b: int) -> None:
            self.a = a
            self.b = b

    onix, real = _canonical(Slots(1, 2), Slots(1, 3))
    assert onix == real == {"values_changed": {"root.b": {"new_value": 3, "old_value": 2}}}


def test_mixed_dict_and_slots_object_matches_deepdiff() -> None:
    """A class mixing __slots__ and __dict__ is diffed across both, matching DeepDiff (issue #66)."""

    class Mixed:
        __slots__ = ("a", "__dict__")

        def __init__(self, a: int, b: int) -> None:
            self.a = a  # slot
            self.b = b  # __dict__

    onix, real = _canonical(Mixed(1, 2), Mixed(1, 3))
    assert onix == real


def test_dataclass_with_default_factory_matches_deepdiff() -> None:
    """A dataclass (including a default_factory list field) is diffed by its attributes (issue #66)."""
    import dataclasses

    @dataclasses.dataclass
    class Rec:
        x: int
        tags: list = dataclasses.field(default_factory=list)

    onix, real = _canonical(Rec(1, ["a"]), Rec(2, ["a", "b"]))
    assert onix == real


def test_name_mangled_private_attribute_matches_deepdiff() -> None:
    """A name-mangled `_Cls__x` attribute is kept and diffed, matching detailed__dict__ (issue #66)."""

    class Mangled:
        def __init__(self, v: int) -> None:
            self.__secret = v  # stored as _Mangled__secret

    onix, real = _canonical(Mangled(1), Mangled(2))
    assert onix == real
    assert "root._Mangled__secret" in onix["values_changed"]


def test_property_and_class_attribute_object_matches_deepdiff() -> None:
    """An unchanged @property and class attribute stay out of the diff, matching DeepDiff (issue #66)."""

    class WithComputed:
        kls = "shared"

        def __init__(self, x: int) -> None:
            self.x = x

        @property
        def doubled(self) -> int:
            return self.x * 10

    # x changes; the property (equal-shaped) and class attribute do not, so only
    # `root.x` and the property's derived `root.doubled` (which tracks x) appear
    # -- both engines agree.
    onix, real = _canonical(WithComputed(1), WithComputed(2))
    assert onix == real


def test_enum_member_matches_deepdiff_via_name_and_value() -> None:
    """An Enum member is diffed by its name/value, matching DeepDiff's _diff_enum (issue #66)."""
    onix = json.loads(DeepDiff(_Color.RED, _Color.GREEN).to_json())
    real = json.loads(RealDeepDiff(_Color.RED, _Color.GREEN, verbose_level=2).to_json())
    assert onix == real == {
        "values_changed": {
            "root.name": {"new_value": "GREEN", "old_value": "RED"},
            "root.value": {"new_value": 2, "old_value": 1},
        }
    }
