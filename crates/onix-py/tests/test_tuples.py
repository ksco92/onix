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


def test_namedtuple_is_diffed_by_position_not_by_field() -> None:
    """
    Documented MVP limitation: a namedtuple converts through the tuple path.

    Real DeepDiff walks a namedtuple's fields (``root[0].x``) and names its
    class as the type; onix treats it as the tuple it is a subclass of, so the
    values compared are the same but the paths (and type names) differ. See
    ``crates/onix-py/src/convert.rs``'s module doc and ``tests/golden/README.md``.
    """
    point = collections.namedtuple("Point", "x")
    a, b = (point(1),), (point(2),)

    assert DeepDiff(a, b).to_dict() == {
        "values_changed": {"root[0][0]": {"new_value": 2, "old_value": 1}}
    }
    assert RealDeepDiff(a, b, verbose_level=2).to_dict() == {
        "values_changed": {"root[0].x": {"new_value": 2, "old_value": 1}}
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
