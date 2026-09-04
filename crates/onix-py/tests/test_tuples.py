"""Tuple support at the bindings boundary: what `to_dict()` hands back, and what it does not.

The diff *results* for tuples are pinned by the golden corpus (``tuple_*`` and
``ignore_order_tuple_*`` cases, checked against real DeepDiff by
``test_golden_parity.py``). This file covers the parts that live only in the
bindings: that a report crossing back into Python carries real ``tuple``
objects wherever DeepDiff's own ``to_dict()`` does, that ``to_json()`` still
shows them as arrays, and the two documented edges (namedtuples, and the
golden corpus's tagged encoding never being interpreted by the product).
"""

import collections
import json

import pytest
from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff, diff_json


def test_to_dict_returns_a_tuple_for_an_added_dict_value() -> None:
    """A whole tuple added under a dict key comes back as a tuple, not a list."""
    diff = DeepDiff({}, {"s": (1, 2)})

    assert diff.to_dict() == {"dictionary_item_added": {"root['s']": (1, 2)}}
    assert diff.to_json() == '{"dictionary_item_added":{"root[\'s\']":[1,2]}}'


def test_to_dict_returns_tuples_nested_inside_dicts_and_lists() -> None:
    """Tuple-ness survives at any depth inside a reported value, including inside a tuple."""
    diff = DeepDiff({}, {"k": [{"t": (1, (2, 3))}]})

    assert diff.to_dict() == {"dictionary_item_added": {"root['k']": [{"t": (1, (2, 3))}]}}


def test_to_dict_returns_tuples_for_old_and_new_values_of_a_type_change() -> None:
    """A tuple-vs-list type change reports the tuple side as a tuple and the list side as a list."""
    diff = DeepDiff((1, 2), [1, 2])
    entry = diff.to_dict()["type_changes"]["root"]

    assert entry["old_value"] == (1, 2)
    assert entry["new_value"] == [1, 2]
    assert entry["old_type"] == "tuple"
    assert entry["new_type"] == "list"


def test_to_dict_returns_a_tuple_for_a_removed_iterable_item() -> None:
    """The iterable_item_removed category preserves tuple-ness too."""
    diff = DeepDiff([3, (1, 2)], [3])

    assert diff.to_dict() == {"iterable_item_removed": {"root[1]": (1, 2)}}


def test_to_dict_matches_real_deepdiff_on_tuple_values() -> None:
    """The values real DeepDiff hands back for these reports are the ones onix hands back."""
    for a, b in (({}, {"s": (1, 2)}), ([3, (1, 2)], [3]), ({}, {"k": [{"t": (1, (2, 3))}]})):
        expected = RealDeepDiff(a, b, verbose_level=2).to_dict()
        assert DeepDiff(a, b).to_dict() == expected, (a, b)


def test_namedtuple_is_refused_rather_than_diffed_as_a_plain_tuple() -> None:
    """
    A namedtuple is a tuple subclass, but DeepDiff diffs it by field, not by index.

    Real DeepDiff reports ``root[0].x`` and names the class as the type; this
    MVP has no field-walking conversion, and returning index paths instead
    would be a silent disagreement, so the conversion refuses it by name. The
    real tool's own output is asserted here so the gap stays visible.
    """
    point = collections.namedtuple("Point", "x")
    a, b = (point(1),), (point(2),)

    with pytest.raises(TypeError, match="Point"):
        DeepDiff(a, b)

    assert RealDeepDiff(a, b, verbose_level=2).to_dict() == {
        "values_changed": {"root[0].x": {"new_value": 2, "old_value": 1}}
    }


def test_a_tuple_subclass_is_refused_too() -> None:
    """
    Every tuple subclass is refused, not just namedtuples.

    DeepDiff reports each value under its own ``type(obj).__name__``, so a
    subclass is never a plain tuple there: it reports a type change where
    diffing the subclass as a tuple would report nothing at all.
    """

    class Pair(tuple):
        pass

    with pytest.raises(TypeError, match="Pair"):
        DeepDiff(Pair((1, 2)), (1, 2))

    with pytest.raises(TypeError, match=r"Pair at root\['a'\]"):
        DeepDiff({"a": Pair((1, 2))}, {"a": (1, 2)})

    assert RealDeepDiff(Pair((1, 2)), (1, 2), verbose_level=2).to_dict() == {
        "type_changes": {
            "root": {
                "old_type": Pair,
                "new_type": tuple,
                "old_value": Pair((1, 2)),
                "new_value": (1, 2),
            }
        }
    }


def test_the_golden_corpus_tag_is_ordinary_data_to_the_product() -> None:
    """`$tuple` is a fixture-file convention only: both entry points read it as a dict key."""
    tagged = '{"$tuple": [1]}'

    assert diff_json(tagged, '{"$tuple": [2]}') == (
        '{"values_changed":{"root[\'$tuple\'][0]":{"new_value":2,"old_value":1}}}'
    )
    assert DeepDiff({"$tuple": [1]}, {"$tuple": [2]}).to_dict() == {
        "values_changed": {"root['$tuple'][0]": {"new_value": 2, "old_value": 1}}
    }


def test_python_equal_tuples_match_under_ignore_order_like_real_deepdiff() -> None:
    """DeepHash's cache makes a hashable tuple inherit an earlier Python-equal one's digest."""
    for a, b in (
        ([(1,)], [(1.0,)]),
        ([(True,)], [(1,)]),
        ([("a", 1)], [("a", 1.0)]),
        ([{"k": (1,)}], [{"k": (1.0,)}]),
    ):
        expected = json.loads(RealDeepDiff(a, b, ignore_order=True, verbose_level=2).to_json())
        actual = json.loads(DeepDiff(a, b, ignore_order=True).to_json())
        assert actual == expected == {}, (a, b, actual)


def test_an_unhashable_tuple_does_not_inherit_a_digest() -> None:
    """A tuple holding a list cannot be a dict key, so it keeps its own type-strict digest."""
    a, b = [(1, [1])], [(1.0, [1])]

    expected = json.loads(RealDeepDiff(a, b, ignore_order=True, verbose_level=2).to_json())
    actual = json.loads(DeepDiff(a, b, ignore_order=True).to_json())

    assert actual == expected
    assert "type_changes" in actual


def test_a_tuple_pairs_with_a_python_equal_list_as_a_type_change() -> None:
    """`list((1,)) == [1.0]`, so the pair stays within DeepDiff's pairing cutoff."""
    a, b = [(1,)], [[1.0]]

    expected = json.loads(RealDeepDiff(a, b, ignore_order=True, verbose_level=2).to_json())
    actual = json.loads(DeepDiff(a, b, ignore_order=True).to_json())

    assert actual == expected
    assert actual["type_changes"]["root[0]"]["old_type"] == "tuple"
