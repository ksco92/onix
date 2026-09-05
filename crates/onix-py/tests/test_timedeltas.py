"""`datetime.timedelta` behavior of the `deepdiff_rs.DeepDiff` class, against real DeepDiff.

Two things this file pins that the golden corpus cannot:

- the `timedelta` **superset**. Real DeepDiff's `to_json()` has no serializer for
  `datetime.timedelta` and raises `TypeError` on a report holding one, so a
  timedelta case's agreement with the real tool has to be asserted on `to_dict()` —
  which is exactly what these tests do, alongside onix's own `str(timedelta)` JSON
  rendering.
- that, unlike `time`, a `timedelta` hashes EXACTLY under `ignore_order` (no
  truncation), against real DeepDiff's own output.
"""

import datetime

import pytest
from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff as OnixDeepDiff


def _normalize_types(value: object) -> object:
    """
    Replace any Python type object in a report with its name.

    :param value: A report, or any part of one.
    :return: The same value with type objects replaced by their names.
    """
    if isinstance(value, dict):
        return {key: _normalize_types(item) for key, item in value.items()}

    if isinstance(value, list):
        return [_normalize_types(item) for item in value]

    if isinstance(value, type):
        return value.__name__

    return value


@pytest.mark.parametrize(
    ("a", "b"),
    [
        (datetime.timedelta(days=1, seconds=3600), datetime.timedelta(days=2)),
        (datetime.timedelta(0), datetime.timedelta(seconds=-1)),
        (datetime.timedelta(seconds=1), 1),
        ({}, {"d": datetime.timedelta(seconds=1)}),
        ({"d": datetime.timedelta(seconds=1)}, {}),
        (
            [datetime.timedelta(seconds=1)],
            [datetime.timedelta(seconds=1), datetime.timedelta(seconds=2)],
        ),
        (datetime.timedelta(seconds=1), "0:00:01"),
        (datetime.timedelta(microseconds=1), datetime.timedelta(0)),
    ],
)
def test_timedelta_cases_match_real_deepdiffs_to_dict(a: object, b: object) -> None:
    """
    Assert onix agrees with real DeepDiff on a case whose `to_json()` DeepDiff refuses.

    :param a: The first value.
    :param b: The second value.
    """
    expected = _normalize_types(RealDeepDiff(a, b, verbose_level=2).to_dict())

    assert _normalize_types(OnixDeepDiff(a, b).to_dict()) == expected


def test_real_deepdiff_still_cannot_serialize_a_timedelta() -> None:
    """The superset this file exists for is real: DeepDiff's own to_json() raises."""
    with pytest.raises(TypeError, match="JSON-serialize"):
        RealDeepDiff(datetime.timedelta(seconds=1), datetime.timedelta(seconds=2)).to_json()


def test_onix_renders_a_timedelta_as_its_python_str() -> None:
    """onix's documented superset: the same bytes `str(timedelta)` gives."""
    diff = OnixDeepDiff(
        datetime.timedelta(days=1, seconds=3600), datetime.timedelta(days=2)
    )

    assert diff.to_json() == (
        '{"values_changed":{"root":{"new_value":"2 days, 0:00:00",'
        '"old_value":"1 day, 1:00:00"}}}'
    )


@pytest.mark.parametrize(
    "value",
    [
        datetime.timedelta(0),
        datetime.timedelta(seconds=1),
        datetime.timedelta(days=1),
        datetime.timedelta(days=-1),
        datetime.timedelta(days=2, seconds=11_045, microseconds=6),
        datetime.timedelta(microseconds=1),
        datetime.timedelta(days=-999_999_999),
        datetime.timedelta(days=999_999_999, hours=23, minutes=59, seconds=59, microseconds=999_999),
    ],
)
def test_rendering_matches_pythons_own_str_at_every_boundary(value: datetime.timedelta) -> None:
    """
    Assert onix's `str()` rendering is byte-identical to Python's own at documented boundaries.

    :param value: The duration to render.
    """
    assert OnixDeepDiff({}, {"d": value}).to_json() == (
        '{"dictionary_item_added":{"root[\'d\']":"' + str(value) + '"}}'
    )


def test_ignore_order_hashes_exactly_with_no_truncation() -> None:
    """Unlike `time`, a one-second difference never hash-matches under `ignore_order`."""
    a = [datetime.timedelta(seconds=1), "anchor"]
    b = ["anchor", datetime.timedelta(seconds=2)]

    real = RealDeepDiff(a, b, ignore_order=True, verbose_level=2).to_dict()
    onix = OnixDeepDiff(a, b, ignore_order=True).to_dict()

    assert real
    assert bool(onix) == bool(real)
