"""`datetime.time` behavior of the `deepdiff_rs.DeepDiff` class, against real DeepDiff.

Two things this file pins that the golden corpus cannot:

- the `time` **superset**. Real DeepDiff's `to_json()` has no serializer for
  `datetime.time` and raises `TypeError` on a report holding one, so a time case's
  agreement with the real tool has to be asserted on `to_dict()` — which is exactly
  what these tests do, alongside onix's own `time.isoformat()` JSON rendering.
- the confirmed `ignore_order` hashing quirk (`DeepHash` reduces a `time` to whole
  seconds-of-day, dropping the microsecond and offset entirely) against real
  DeepDiff's own output, not just onix's internal model.
"""

import datetime

import pytest
from conftest import _normalize_types
from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff as OnixDeepDiff

UTC = datetime.timezone.utc
PLUS_TWO = datetime.timezone(datetime.timedelta(hours=2))
MINUS_FIVE = datetime.timezone(datetime.timedelta(hours=-5))


@pytest.mark.parametrize(
    ("a", "b"),
    [
        (datetime.time(10, 30), datetime.time(12, 0)),
        (datetime.time(10, 30), datetime.datetime(2024, 1, 1, 10, 30)),
        (datetime.time(10, 30), datetime.date(2024, 1, 1)),
        ({}, {"t": datetime.time(10, 30)}),
        ({"t": datetime.time(10, 30)}, {}),
        ([datetime.time(10, 30)], [datetime.time(10, 30), datetime.time(12, 0)]),
        (datetime.time(10, 30), "10:30:00"),
        # Naive is never equal to aware, unlike a datetime.
        (datetime.time(10, 0), datetime.time(10, 0, tzinfo=UTC)),
        # Two aware values at the same offset-adjusted instant ARE equal.
        (datetime.time(10, 0, tzinfo=UTC), datetime.time(12, 0, tzinfo=PLUS_TWO)),
    ],
)
def test_time_cases_match_real_deepdiffs_to_dict(a: object, b: object) -> None:
    """
    Assert onix agrees with real DeepDiff on a case whose `to_json()` DeepDiff refuses.

    :param a: The first value.
    :param b: The second value.
    """
    expected = _normalize_types(RealDeepDiff(a, b, verbose_level=2).to_dict())

    assert _normalize_types(OnixDeepDiff(a, b).to_dict()) == expected


def test_real_deepdiff_still_cannot_serialize_a_time() -> None:
    """The superset this file exists for is real: DeepDiff's own to_json() raises."""
    with pytest.raises(TypeError, match="JSON-serialize"):
        RealDeepDiff(datetime.time(10, 30), datetime.time(12, 0)).to_json()


def test_onix_renders_a_time_as_its_isoformat() -> None:
    """onix's documented superset: the same bytes `time.isoformat()` gives."""
    diff = OnixDeepDiff(datetime.time(10, 30), datetime.time(12, 0))

    assert diff.to_json() == (
        '{"values_changed":{"root":{"new_value":"12:00:00","old_value":"10:30:00"}}}'
    )


def test_a_time_pair_reports_the_raw_values_never_normalized() -> None:
    """Unlike a datetime, `_diff_time` never normalizes, so this carries the raw pair."""
    changed = OnixDeepDiff(
        datetime.time(10, 0, tzinfo=MINUS_FIVE),
        datetime.time(11, 0, tzinfo=MINUS_FIVE),
    )

    assert changed.to_dict() == {
        "values_changed": {
            "root": {
                "old_value": datetime.time(10, 0, tzinfo=MINUS_FIVE),
                "new_value": datetime.time(11, 0, tzinfo=MINUS_FIVE),
            }
        }
    }


def test_a_naive_time_comes_back_naive() -> None:
    """A raw-category naive time keeps its `tzinfo=None`, matching DeepDiff."""
    result = OnixDeepDiff({}, {"t": datetime.time(10, 30)}).to_dict()

    assert result["dictionary_item_added"]["root['t']"].tzinfo is None


@pytest.mark.parametrize(
    "offset_seconds",
    [0, 1830, -1830, 5 * 3600 + 1800, -(5 * 3600 + 1800), 86399, -86399],
)
def test_offsets_render_the_way_python_isoformat_does(offset_seconds: int) -> None:
    """
    Assert onix's raw offset suffix is byte-identical to Python's own `isoformat()`.

    :param offset_seconds: The fixed UTC offset to render.
    """
    value = datetime.time(10, 0, tzinfo=datetime.timezone(datetime.timedelta(seconds=offset_seconds)))

    assert OnixDeepDiff({}, {"t": value}).to_json() == (
        '{"dictionary_item_added":{"root[\'t\']":"' + value.isoformat() + '"}}'
    )
    assert OnixDeepDiff({}, {"t": value}).to_dict()["dictionary_item_added"]["root['t']"] == value


@pytest.mark.parametrize("microsecond", [0, 1, 999999])
def test_microseconds_render_only_when_non_zero(microsecond: int) -> None:
    """
    Assert the `.ffffff` suffix appears exactly when Python's `isoformat()` shows it.

    :param microsecond: The microsecond field to render.
    """
    value = datetime.time(10, 0, 0, microsecond)

    assert OnixDeepDiff({}, {"t": value}).to_json() == (
        '{"dictionary_item_added":{"root[\'t\']":"' + value.isoformat() + '"}}'
    )


def test_ignore_order_hash_truncation_drops_microsecond_and_offset() -> None:
    """
    `DeepHash` reduces a `time` to whole seconds-of-day before hashing.

    A microsecond-only or an offset-only difference hash-matches under `ignore_order`
    even though the ordinary comparison calls the pair different — real, confirmed
    `DeepDiff` behavior, matched exactly here.
    """
    micros_only = (
        [datetime.time(10, 30, 0, 123_456), "anchor"],
        ["anchor", datetime.time(10, 30, 0, 999_999)],
    )
    offset_only = (
        [datetime.time(10, 30), "anchor"],
        ["anchor", datetime.time(10, 30, tzinfo=PLUS_TWO)],
    )

    for a, b in (micros_only, offset_only):
        real = RealDeepDiff(a, b, ignore_order=True, verbose_level=2).to_dict()
        onix = OnixDeepDiff(a, b, ignore_order=True).to_dict()

        assert real == {}
        assert onix == {}

    # The ordinary (non-ignore_order) comparison of the same values does NOT
    # truncate: the pair is reported.
    assert OnixDeepDiff(datetime.time(10, 30, 0, 123_456), datetime.time(10, 30, 0, 999_999))
