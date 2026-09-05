"""Datetime and date behavior of the `deepdiff_rs.DeepDiff` class, against real DeepDiff.

Two things this file pins that the golden corpus cannot:

- the `date` **superset**. Real DeepDiff's `to_json()` has no serializer for
  `datetime.date` and raises `TypeError` on a report holding one, so a date case's
  agreement with the real tool has to be asserted on `to_dict()` — which is exactly
  what these tests do, alongside onix's own `YYYY-MM-DD` JSON rendering.
- the Python objects `to_dict()` hands back, including the documented
  `zoneinfo`-to-fixed-offset round trip.
"""

import datetime
import zoneinfo

import pytest
from conftest import _normalize_types
from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff as OnixDeepDiff
from deepdiff_rs import diff_json

UTC = datetime.timezone.utc
PLUS_TWO = datetime.timezone(datetime.timedelta(hours=2))
MINUS_FIVE = datetime.timezone(datetime.timedelta(hours=-5))


@pytest.mark.parametrize(
    ("a", "b"),
    [
        (datetime.date(2024, 1, 1), datetime.date(2024, 1, 2)),
        (datetime.date(2024, 1, 1), datetime.datetime(2024, 1, 1)),
        (datetime.datetime(2024, 1, 1), datetime.date(2024, 1, 1)),
        ({}, {"d": datetime.date(2024, 1, 1)}),
        ({"d": datetime.date(2024, 1, 1)}, {}),
        ([datetime.date(2024, 1, 1)], [datetime.date(2024, 1, 1), datetime.date(2024, 2, 1)]),
        (
            {"d": datetime.date(2024, 1, 1), "t": datetime.datetime(2024, 1, 1, 10)},
            {"d": datetime.date(2024, 3, 5), "t": datetime.datetime(2024, 1, 2, 10)},
        ),
        (datetime.date(2024, 1, 1), "2024-01-01"),
    ],
)
def test_date_cases_match_real_deepdiffs_to_dict(a: object, b: object) -> None:
    """
    Assert onix agrees with real DeepDiff on a case whose `to_json()` DeepDiff refuses.

    :param a: The first value.
    :param b: The second value.
    """
    expected = _normalize_types(RealDeepDiff(a, b, verbose_level=2).to_dict())

    assert _normalize_types(OnixDeepDiff(a, b).to_dict()) == expected


def test_real_deepdiff_still_cannot_serialize_a_date() -> None:
    """The superset this file exists for is real: DeepDiff's own to_json() raises."""
    with pytest.raises(TypeError, match="JSON-serialize"):
        RealDeepDiff(datetime.date(2024, 1, 1), datetime.date(2024, 1, 2)).to_json()


def test_onix_renders_a_date_as_an_iso_calendar_day() -> None:
    """onix's documented superset: `YYYY-MM-DD`, the same bytes `date.isoformat()` gives."""
    diff = OnixDeepDiff(datetime.date(2024, 1, 1), datetime.date(2024, 1, 2))

    assert diff.to_json() == (
        '{"values_changed":{"root":{"new_value":"2024-01-02","old_value":"2024-01-01"}}}'
    )


def test_values_changed_returns_utc_aware_datetimes_while_other_categories_stay_raw() -> None:
    """A datetime pair is normalized to UTC; every other category keeps the raw value."""
    changed = OnixDeepDiff(
        datetime.datetime(2024, 1, 1, 10, tzinfo=MINUS_FIVE),
        datetime.datetime(2024, 1, 2, 10, tzinfo=MINUS_FIVE),
    )
    added = OnixDeepDiff({}, {"t": datetime.datetime(2024, 1, 1, 10, tzinfo=MINUS_FIVE)})

    assert changed.to_dict() == {
        "values_changed": {
            "root": {
                "old_value": datetime.datetime(2024, 1, 1, 15, tzinfo=UTC),
                "new_value": datetime.datetime(2024, 1, 2, 15, tzinfo=UTC),
            }
        }
    }
    assert added.to_dict() == {
        "dictionary_item_added": {
            "root['t']": datetime.datetime(2024, 1, 1, 10, tzinfo=MINUS_FIVE)
        }
    }


def test_a_naive_datetime_comes_back_naive() -> None:
    """A raw-category naive datetime keeps its `tzinfo=None`, matching DeepDiff."""
    result = OnixDeepDiff({}, {"t": datetime.datetime(2024, 1, 1, 10)}).to_dict()

    assert result["dictionary_item_added"]["root['t']"].tzinfo is None


def test_a_zoneinfo_datetime_round_trips_as_a_fixed_offset_timezone() -> None:
    """
    A named zone comes back as the fixed offset it was in force at, not as the zone.

    Documented in README and `crates/onix-py/src/convert.rs`: onix stores a datetime's
    UTC offset, not its `tzinfo` object. Nothing about the diff itself changes, since
    DeepDiff compares datetimes by instant.
    """
    madrid = datetime.datetime(2024, 7, 1, 12, tzinfo=zoneinfo.ZoneInfo("Europe/Madrid"))
    result = OnixDeepDiff({}, {"t": madrid}).to_dict()
    round_tripped = result["dictionary_item_added"]["root['t']"]

    assert round_tripped == madrid
    assert round_tripped.tzinfo == datetime.timezone(datetime.timedelta(hours=2))
    assert not isinstance(round_tripped.tzinfo, zoneinfo.ZoneInfo)
    # And it diffs against the same instant written any other way as equal.
    assert not OnixDeepDiff(madrid, datetime.datetime(2024, 7, 1, 10, tzinfo=UTC))


@pytest.mark.parametrize(
    "offset_seconds",
    [0, 1830, -1830, 5 * 3600 + 1800, -(5 * 3600 + 1800), 86399, -86399],
)
def test_offsets_render_the_way_python_isoformat_does(offset_seconds: int) -> None:
    """
    Assert onix's raw offset suffix is byte-identical to Python's own `isoformat()`.

    :param offset_seconds: The fixed UTC offset to render.
    """
    value = datetime.datetime(
        2024, 1, 1, 10, tzinfo=datetime.timezone(datetime.timedelta(seconds=offset_seconds))
    )
    rendered = OnixDeepDiff({}, {"t": value}).to_dict()

    assert OnixDeepDiff({}, {"t": value}).to_json() == (
        '{"dictionary_item_added":{"root[\'t\']":"' + value.isoformat() + '"}}'
    )
    assert rendered["dictionary_item_added"]["root['t']"] == value


@pytest.mark.parametrize("microsecond", [0, 1, 999999])
def test_microseconds_render_only_when_non_zero(microsecond: int) -> None:
    """
    Assert the `.ffffff` suffix appears exactly when Python's `isoformat()` shows it.

    :param microsecond: The microsecond field to render.
    """
    value = datetime.datetime(2024, 1, 1, 10, 0, 0, microsecond)

    assert OnixDeepDiff({}, {"t": value}).to_json() == (
        '{"dictionary_item_added":{"root[\'t\']":"' + value.isoformat() + '"}}'
    )


def test_a_tagged_datetime_object_is_ordinary_data_to_diff_json() -> None:
    """The golden corpus's `$datetime` tag is test tooling; the product never reads it."""
    assert diff_json('{"$datetime": "2024-01-01T00:00:00"}', '{"$datetime": "2024-01-02"}') == (
        '{"values_changed":{"root[\'$datetime\']":'
        '{"new_value":"2024-01-02","old_value":"2024-01-01T00:00:00"}}}'
    )


# The `1..=9999` normalization boundary


EXTREME = datetime.datetime(9999, 12, 31, 23, tzinfo=datetime.timezone(datetime.timedelta(hours=-1)))


def test_comparing_two_datetimes_that_cannot_normalize_raises_naming_the_path() -> None:
    """A pair whose UTC form leaves year 1..=9999 raises, as real DeepDiff does."""
    with pytest.raises(ValueError, match=r"root\['t'\]"):
        OnixDeepDiff({"t": EXTREME}, {"t": datetime.datetime(2024, 1, 1)})

    with pytest.raises(OverflowError, match="date value out of range"):
        RealDeepDiff({"t": EXTREME}, {"t": datetime.datetime(2024, 1, 1)}, verbose_level=2)


def test_an_unnormalizable_datetime_is_fine_when_it_is_never_compared_to_another() -> None:
    """DeepDiff normalizes only inside `_diff_datetime`, so these two report normally."""
    added = {"t": EXTREME}

    assert _normalize_types(OnixDeepDiff({}, added).to_dict()) == _normalize_types(
        RealDeepDiff({}, added, verbose_level=2).to_dict()
    )
    assert _normalize_types(OnixDeepDiff(EXTREME, 5).to_dict()) == _normalize_types(
        RealDeepDiff(EXTREME, 5, verbose_level=2).to_dict()
    )


def test_a_calendar_value_pairs_with_its_own_str_under_ignore_order() -> None:
    """`str(datetime)`/`str(date)` are reproducible by coercion, so the pair pairs."""
    for value in (datetime.datetime(2024, 1, 1), datetime.date(2024, 1, 1)):
        a = [{"a": value}]
        b = [{"a": str(value)}]
        expected = _normalize_types(
            RealDeepDiff(a, b, ignore_order=True, verbose_level=2).to_dict()
        )

        assert _normalize_types(OnixDeepDiff(a, b, ignore_order=True).to_dict()) == expected
        assert "type_changes" in expected
