"""Set and frozenset support at the bindings boundary, and its documented divergences.

The diff *results* for sets are pinned by the golden corpus (``set_*``,
``frozenset_*`` and ``ignore_order_set_*`` cases, checked against real DeepDiff
by ``test_golden_parity.py``). This file covers the parts that live only in the
bindings: that a report crossing back into Python carries real ``set``/
``frozenset`` objects, that the two set categories come back as lists of path
strings, the float rendering used inside a set item's path, and the places
onix deliberately differs from real DeepDiff: the three consequences of
DeepDiff's set results depending on the process's set iteration order (see
``tests/golden/README.md``'s "Set iteration order" section), and being able to
serialize a frozenset at all. Each of those is pinned here as onix's own
output, with DeepDiff's shown alongside rather than asserted equal.
"""

import datetime
import json
import math
import random
import struct
from typing import Final

import pytest
from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff, diff_json

# Seeded float corpus size for the repr differential below, and how many are
# rendered per diff call (one call per batch keeps a million-value run fast).
FLOAT_CASE_COUNT: Final[int] = 1_000_000
FLOAT_BATCH_SIZE: Final[int] = 5_000


def test_set_items_are_reported_as_added_and_removed_paths() -> None:
    """The two set categories are lists of path strings, sorted by onix."""
    diff = DeepDiff({1, 2, 3}, {2, 3, 4})

    assert diff.to_dict() == {
        "set_item_added": ["root[4]"],
        "set_item_removed": ["root[1]"],
    }
    assert json.loads(diff.to_json()) == diff.to_dict()


def test_empty_set_categories_are_omitted() -> None:
    """A one-sided set change reports only the category that fired."""
    assert DeepDiff({1, 2}, {1, 2, 3}).to_dict() == {"set_item_added": ["root[3]"]}
    assert DeepDiff({1, 2}, {2, 1}).to_dict() == {}


def test_to_dict_returns_a_real_set_for_an_added_dict_value() -> None:
    """A whole set added under a dict key comes back as a set, not a list."""
    diff = DeepDiff({}, {"s": {1, 2}})
    added = diff.to_dict()["dictionary_item_added"]["root['s']"]

    assert added == {1, 2}
    assert isinstance(added, set)
    assert diff.to_json() == '{"dictionary_item_added":{"root[\'s\']":[1,2]}}'


def test_to_dict_returns_a_real_frozenset_nested_in_a_reported_value() -> None:
    """Frozenset-ness survives at any depth inside a reported value."""
    diff = DeepDiff({}, {"k": [{"t": frozenset({1, 2})}]})

    assert diff.to_dict() == {
        "dictionary_item_added": {"root['k']": [{"t": frozenset({1, 2})}]}
    }


def test_to_dict_reports_a_set_versus_frozenset_type_change() -> None:
    """A set-vs-frozenset pairing is a type change carrying both real objects."""
    entry = DeepDiff({1, 2}, frozenset({1, 2})).to_dict()["type_changes"]["root"]

    assert entry["old_type"] == "set"
    assert entry["new_type"] == "frozenset"
    assert isinstance(entry["old_value"], set)
    assert isinstance(entry["new_value"], frozenset)


def test_onix_serializes_a_frozenset_where_real_deepdiff_refuses() -> None:
    """Documented superset: DeepDiff's own to_json() cannot serialize a frozenset value."""
    real = RealDeepDiff({1, 2}, frozenset({1, 2}), verbose_level=2)

    with pytest.raises(TypeError, match="frozenset"):
        real.to_json()

    assert json.loads(DeepDiff({1, 2}, frozenset({1, 2})).to_json()) == {
        "type_changes": {
            "root": {
                "old_type": "set",
                "new_type": "frozenset",
                "old_value": [1, 2],
                "new_value": [1, 2],
            }
        }
    }


def test_set_entry_order_is_sorted_where_real_deepdiff_is_hash_ordered() -> None:
    """The one documented divergence: onix sorts set entries, DeepDiff hash-orders them.

    DeepDiff builds them from ``t2_hashes - t1_hashes``, a Python set of
    SHA-256 hex strings, so their order follows ``PYTHONHASHSEED``.
    """
    a, b = set(range(5)), set(range(3, 8))
    onix = DeepDiff(a, b).to_dict()
    real = RealDeepDiff(a, b, verbose_level=2).to_dict()

    assert onix["set_item_removed"] == ["root[0]", "root[1]", "root[2]"]
    assert onix["set_item_added"] == ["root[5]", "root[6]", "root[7]"]
    # Same findings, order aside — which is all real DeepDiff promises here.
    for category in ("set_item_added", "set_item_removed"):
        assert sorted(onix[category]) == sorted(real[category])


@pytest.mark.parametrize(
    ("item", "rendered"),
    [
        (None, "None"),
        (True, "True"),
        (False, "False"),
        (7, "7"),
        (-7, "-7"),
        (1.0, "1.0"),
        (1e16, "1e+16"),
        (1e-05, "1e-05"),
        (-0.0, "-0.0"),
        ("a", "'a'"),
        ("it's", "'it's'"),
        ('say "hi"', "'say \"hi\"'"),
        ((1, 2), "(1, 2)"),
        ((1,), "(1,)"),
        ((1, (2, 3)), "(1, (2, 3))"),
        (("it's",), '("it\'s",)'),
        (frozenset(), "frozenset()"),
        (frozenset({1, 2}), "frozenset({1, 2})"),
    ],
)
def test_set_item_paths_match_real_deepdiff(item: object, rendered: str) -> None:
    """Each item kind renders into the entry path exactly as real DeepDiff renders it."""
    onix = DeepDiff({item}, {"sentinel"}).to_dict()
    real = RealDeepDiff({item}, {"sentinel"}, verbose_level=2).to_dict()

    assert onix["set_item_removed"] == [f"root[{rendered}]"]
    assert onix["set_item_removed"] == list(real["set_item_removed"])


def _rendered_float_paths(values: list[float]) -> list[str]:
    """
    Render a batch of floats as set-item paths, in one diff per batch.

    :param values: Distinct finite floats.
    :return: The rendered ``set_item_removed`` entries, sorted.
    """
    return sorted(DeepDiff(set(values), {"sentinel"}).to_dict()["set_item_removed"])


def test_float_set_item_paths_break_shortest_form_ties_pythons_way() -> None:
    """The known tie values, pinned so the fast suite goes red on a `{:e}`-trusting renderer.

    Rust's own shortest form rounds each of these away from Python's ``repr``;
    they were found by the million-pattern differential below.
    """
    ties = [
        160598971591683.12,
        2113325745016023.2,
        -20243279817481.062,
        245712874376162.12,
    ]

    assert _rendered_float_paths(ties) == sorted(f"root[{value!r}]" for value in ties)


def test_float_set_item_paths_match_python_repr_over_a_million_bit_patterns() -> None:
    """Every finite float renders exactly as Python's ``repr()`` does.

    Random *bit patterns* rather than random magnitudes: repr's tie-breaking
    only bites near a decimal midpoint, and about one float in 3,800 sits
    there, so a corpus drawn from the raw 64-bit space finds them where one
    drawn from ``uniform()`` mostly does not.
    """
    rng = random.Random(20260903)
    mismatches: list[tuple[str, str]] = []
    checked = 0

    for _ in range(FLOAT_CASE_COUNT // FLOAT_BATCH_SIZE):
        batch: set[float] = set()
        while len(batch) < FLOAT_BATCH_SIZE:
            candidate = struct.unpack("<d", struct.pack("<Q", rng.getrandbits(64)))[0]
            if math.isfinite(candidate):
                batch.add(candidate)

        values = list(batch)
        checked += len(values)
        expected = sorted(f"root[{value!r}]" for value in values)
        mismatches.extend(
            pair for pair in zip(expected, _rendered_float_paths(values)) if pair[0] != pair[1]
        )

    assert checked >= FLOAT_CASE_COUNT
    assert not mismatches, (
        f"{len(mismatches)} of {checked} floats rendered differently "
        f"(showing up to 3): {mismatches[:3]}"
    )


def test_a_set_holding_an_unhashable_subclass_is_refused() -> None:
    """A list subclass that defines __hash__ can be a set member; onix refuses it by path."""

    class HashableList(list):
        __hash__ = object.__hash__

    with pytest.raises(TypeError, match=r"HashableList at root\['a'\]\[<set member>\]"):
        DeepDiff({"a": {HashableList([1])}}, {"a": {1}})


def test_an_unsupported_type_under_a_set_member_reports_no_fabricated_index() -> None:
    """A set member has no subscript, so nothing beneath one is reported as `[0]`."""
    with pytest.raises(TypeError, match=r"complex at root\['a'\]\[<set member>\]\[1\]"):
        DeepDiff({"a": {(1, 1j)}}, {"a": {1}})


def test_a_set_subclass_is_refused_like_a_tuple_subclass() -> None:
    """DeepDiff reports a subclass under its own name, so onix refuses it rather than lying."""

    class MySet(set):
        pass

    with pytest.raises(TypeError, match="MySet"):
        DeepDiff(MySet({1}), {1})

    assert RealDeepDiff(MySet({1}), {1}, verbose_level=2).to_dict()["type_changes"]["root"][
        "old_type"
    ] is MySet


def test_a_frozenset_subclass_is_refused() -> None:
    """The same exact-type rule holds for frozenset."""

    class MyFrozenSet(frozenset):
        pass

    with pytest.raises(TypeError, match="MyFrozenSet"):
        DeepDiff(MyFrozenSet({1}), frozenset({1}))


def test_set_tags_are_ordinary_dicts_to_the_json_path() -> None:
    """The corpus's `$set`/`$frozenset` tags are plain data to every product path."""
    for tag in ("$set", "$frozenset"):
        assert diff_json(f'{{"{tag}": [1]}}', f'{{"{tag}": [2]}}') == (
            f'{{"values_changed":{{"root[\'{tag}\'][0]":{{"new_value":2,"old_value":1}}}}}}'
        )


def test_a_nested_frozenset_renders_in_canonical_order() -> None:
    """Set iteration order, "Canonical set order": a frozenset rendered *inside* a path uses onix's order.

    Real DeepDiff renders it with Python's ``str()``, i.e. in the set's own
    hash order — which depends on how the set was built, and which no
    order-insensitive comparison can reconcile inside one opaque path string:

        DeepDiff({frozenset({10, 2})}, {"sentinel"})
        -> set_item_removed ["root[frozenset({10, 2})]"]

    onix renders the members in canonical (here, numeric) order however the
    frozenset was written.
    """
    for item in (frozenset({2, 10}), frozenset({10, 2})):
        assert DeepDiff({item}, {"sentinel"}).to_dict()["set_item_removed"] == [
            "root[frozenset({2, 10})]"
        ]
        # DeepDiff instead reproduces whatever `str()` gives for this object.
        assert RealDeepDiff({item}, {"sentinel"}, verbose_level=2).to_dict()[
            "set_item_removed"
        ] == [f"root[{item!s}]"]

    assert str(frozenset({10, 2})) == "frozenset({10, 2})", (
        "fixture assumption: Python's own order here is not the canonical one"
    )


def test_a_frozenset_never_inherits_another_ones_digest() -> None:
    """Set iteration order, "Which member of an equality class wins": no shared hash cache.

    A frozenset is hashable, so real DeepDiff lets it inherit the digest of the
    first Python-equal frozenset hashed in the run — which makes its answer
    depend on which one that was:

        DeepDiff([frozenset({1})], [frozenset({1.0})], ignore_order=True) -> {}
        DeepDiff([frozenset({1}), frozenset({1.0})], [], ignore_order=True)
        -> iterable_item_removed {"root[0]": frozenset({1})}      # one, not two

    onix keys a frozenset by its own membership, always, so the two are simply
    different items.
    """
    paired = DeepDiff([frozenset({1})], [frozenset({1.0})], ignore_order=True).to_dict()
    assert paired == {
        "values_changed": {
            "root[0]": {"old_value": frozenset({1}), "new_value": frozenset({1.0})}
        }
    }

    removed = DeepDiff([frozenset({1}), frozenset({1.0})], [], ignore_order=True).to_dict()
    assert removed == {
        "iterable_item_removed": {"root[0]": frozenset({1}), "root[1]": frozenset({1.0})}
    }

    # A set is unhashable, so DeepDiff never caches one either: both tools
    # agree here.
    sets = [{1}, {1.0}]
    assert DeepDiff(sets, [], ignore_order=True).to_dict() == RealDeepDiff(
        sets, [], ignore_order=True, verbose_level=2
    ).to_dict()


def test_a_set_versus_a_list_is_a_type_change_whatever_the_order() -> None:
    """Set iteration order, "`list(a_set) == some_list`": answered by membership in onix.

    Real DeepDiff answers it in the set's own iteration order, so whether the
    pair stays a type change depends on how the set happens to iterate:

        DeepDiff([{75, 47}], [[75, 47]], ignore_order=True)
        -> values_changed, because list({75, 47}) is [47, 75]
        DeepDiff([{75, 47}], [[47, 75]], ignore_order=True)
        -> type_changes

    onix gives the type change for both.
    """
    for sibling in ([75, 47], [47, 75]):
        report = DeepDiff([{75, 47}], [sibling], ignore_order=True).to_dict()

        assert report["type_changes"]["root[0]"]["new_type"] == "list", sibling


def test_a_sets_report_does_not_depend_on_which_member_was_hashed_first() -> None:
    """The same rule at the set level: two removals and one addition, always.

    Real DeepDiff's answer here is decided by which member of the
    ``((1.0,),)``/``((1, 1),)`` equality class its shared hash cache saw
    first, which follows the set's iteration order.
    """
    for members in ({((1.0,),), ((1,), 0)}, {((1,), 0), ((1.0,),)}):
        assert DeepDiff(members, {((1, 1),)}).to_dict() == {
            "set_item_added": ["root[((1, 1),)]"],
            "set_item_removed": ["root[((1,), 0)]", "root[((1.0,),)]"],
        }


@pytest.mark.parametrize(
    ("member", "rendered"),
    [
        (datetime.datetime(2024, 1, 1), "2024-01-01 00:00:00"),
        (
            datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc),
            "2024-01-01 00:00:00+00:00",
        ),
        (datetime.datetime(2024, 1, 1, microsecond=123456), "2024-01-01 00:00:00.123456"),
        (datetime.date(2024, 1, 1), "2024-01-01"),
        (
            (datetime.datetime(2024, 1, 1),),
            "(datetime.datetime(2024, 1, 1, 0, 0),)",
        ),
        (
            (datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc),),
            "(datetime.datetime(2024, 1, 1, 0, 0, tzinfo=datetime.timezone.utc),)",
        ),
        (
            frozenset({datetime.date(2024, 1, 1)}),
            "frozenset({datetime.date(2024, 1, 1)})",
        ),
        (
            (1, (datetime.datetime(2024, 1, 1),)),
            "(1, (datetime.datetime(2024, 1, 1, 0, 0),))",
        ),
    ],
)
def test_a_calendar_value_as_or_inside_a_set_member_matches_real_deepdiff(
    member: object, rendered: str
) -> None:
    """A datetime/date set item renders with `str()`; nested one level, with `repr()`.

    Top-level uses ``DateTime::python_str``/``Date::python_str`` (space
    separator, no ``T``) — the one item kind whose ``str()`` and ``repr()``
    genuinely differ. Nested inside a tuple or frozenset it renders the way
    every other nested item does: Python's ``repr()``.
    """
    onix = DeepDiff({member}, {"sentinel"}).to_dict()
    real = RealDeepDiff({member}, {"sentinel"}, verbose_level=2).to_dict()

    assert onix["set_item_removed"] == [f"root[{rendered}]"]
    assert onix["set_item_removed"] == list(real["set_item_removed"])


def test_a_calendar_value_is_a_real_object_in_a_reported_set() -> None:
    """A set holding a datetime and a date round-trips both as real objects."""
    diff = DeepDiff({}, {"s": {datetime.datetime(2024, 1, 1), datetime.date(2024, 1, 2)}})
    added = diff.to_dict()["dictionary_item_added"]["root['s']"]

    assert added == {datetime.datetime(2024, 1, 1), datetime.date(2024, 1, 2)}


@pytest.mark.parametrize(
    ("a", "b"),
    [
        (
            {datetime.datetime(2024, 1, 1), 1},
            {datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc), 2},
        ),
        (
            {(datetime.datetime(2024, 1, 1),), 1},
            {(datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc),), 2},
        ),
        (
            {frozenset({datetime.datetime(2024, 1, 1)}), 1},
            {frozenset({datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc)}), 2},
        ),
    ],
)
def test_a_naive_and_aware_datetime_set_member_hash_match_at_one_instant(
    a: set[object], b: set[object]
) -> None:
    """`_diff_set` hashes through `DeepHash`, which normalizes a naive value to UTC.

    Unlike plain Python `==` (which never equates a naive and an aware
    datetime), a set member's identity here follows `DeepHash._prep_datetime`
    — matching at any nesting depth. Confirmed against `deepdiff==9.1.0`.

    Each set also carries an unrelated second member (`1`/`2`) so the two
    sides are not wholly equal: a single-member pair that hash-matches makes
    the two *sets* structurally equal outright, which short-circuits before
    ever calling `_diff_set`'s per-member identity comparison and would let
    a broken identity function pass silently.
    """
    onix = DeepDiff(a, b).to_dict()
    real = RealDeepDiff(a, b, verbose_level=2).to_dict()

    assert onix == {"set_item_removed": ["root[1]"], "set_item_added": ["root[2]"]}
    assert onix == real


def test_a_date_and_a_datetime_set_member_never_hash_match() -> None:
    """A date's DeepHash digest never collides with a datetime's, even at the same midnight."""
    onix = DeepDiff({datetime.date(2024, 1, 1)}, {datetime.datetime(2024, 1, 1)}).to_dict()
    real = RealDeepDiff(
        {datetime.date(2024, 1, 1)}, {datetime.datetime(2024, 1, 1)}, verbose_level=2
    ).to_dict()

    assert onix == {
        "set_item_removed": ["root[2024-01-01]"],
        "set_item_added": ["root[2024-01-01 00:00:00]"],
    }
    assert onix == real


def test_an_unhashable_container_anywhere_inside_a_set_member_is_refused() -> None:
    """A hashable list subclass nested in a tuple member is refused at conversion.

    Accepting it produced a report `to_json()` could serialize but `to_dict()`
    could not: rebuilding the set raised `TypeError: unhashable type: 'list'`
    from inside the bindings.
    """

    class HashableList(list):
        __hash__ = object.__hash__

    for member in ((HashableList([2]),), frozenset({(HashableList([2]),)})):
        with pytest.raises(TypeError, match=r"HashableList at root\[<set member>\]"):
            diff = DeepDiff({member}, {"sentinel"})
            diff.to_dict()
            diff.to_json()


def test_a_naive_and_aware_datetime_set_member_is_two_members_in_onix_one_in_deepdiff() -> None:
    """Set iteration order, "A naive and an aware datetime": DeepDiff's hasher can report only one of two Python members.

    A real Python set never merges a naive and an aware datetime at one
    instant -- ``naive == aware`` is `False`, so ``{naive, aware}`` is a
    genuine two-member set -- but `_diff_set` groups members by `DeepHash`
    digest, and `_prep_datetime` normalizes every datetime to its UTC instant
    before hashing, so the two land in the same digest bucket; only one
    survives to be reported. onix's *matching* identity across two different
    sets is the same instant-based rule (a naive and an aware value at one
    instant match each other), but a single set's own members are still
    stored and reported individually, so a set holding both reports both.
    """
    naive = datetime.datetime(2024, 1, 1)
    aware = datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc)
    other = datetime.datetime(2024, 6, 1)

    onix = DeepDiff({naive, aware, "z"}, {other, "y"}).to_dict()
    real = RealDeepDiff({naive, aware, "z"}, {other, "y"}, verbose_level=2).to_dict()

    assert onix == {
        "set_item_removed": ["root['z']", "root[2024-01-01 00:00:00+00:00]", "root[2024-01-01 00:00:00]"],
        "set_item_added": ["root['y']", "root[2024-06-01 00:00:00]"],
    }
    # DeepDiff's own answer collapses the naive/aware pair to whichever one
    # its hasher kept -- one removal short of onix's three, shown for
    # comparison rather than asserted (order-of-collapse is process-specific).
    assert len(real["set_item_removed"]) == 2
    assert len(onix["set_item_removed"]) == 3


def test_a_tuple_set_member_matches_by_position_where_deepdiff_ignores_order_and_repetition() -> None:
    """Set iteration order, "A tuple or a frozenset set member matches order- and repetition-insensitively": Python `==`-strict in onix only.

    `DeepHash._prep_iterable` runs with `ignore_iterable_order`/
    `ignore_repetition` for *every* iterable it hashes, tuples included, not
    only the list-nested-in-an-ignore_order-list case that motivates the
    default -- so `(1, 2)` and `(2, 1)` share one digest in `_diff_set`
    despite not being Python-equal (`(1, 2) != (2, 1)`), and so do `(1, 1,
    2)` and `(1, 2, 2)` (same *distinct* members, different repetition
    counts). onix's tuple identity is positional Python `==`, matching what
    `tuple.__eq__` actually says, so it tells the two apart.
    """
    assert (1, 2) != (2, 1)

    onix_order = DeepDiff({(1, 2), "z"}, {(2, 1), "y"}).to_dict()
    real_order = RealDeepDiff({(1, 2), "z"}, {(2, 1), "y"}, verbose_level=2).to_dict()
    assert onix_order == {
        "set_item_removed": ["root['z']", "root[(1, 2)]"],
        "set_item_added": ["root['y']", "root[(2, 1)]"],
    }
    assert real_order == {
        "set_item_removed": ["root['z']"],
        "set_item_added": ["root['y']"],
    }

    onix_repeat = DeepDiff({(1, 1, 2)}, {(1, 2, 2)}).to_dict()
    real_repeat = RealDeepDiff({(1, 1, 2)}, {(1, 2, 2)}, verbose_level=2).to_dict()
    assert onix_repeat == {
        "set_item_removed": ["root[(1, 1, 2)]"],
        "set_item_added": ["root[(1, 2, 2)]"],
    }
    assert real_repeat == {}


def test_a_set_member_matches_deepdiff_by_the_per_node_cache_versus_content_decision() -> None:
    """A set member's identity follows DeepHash's per-node cache/content decision.

    `_diff_set` hashes each member through `DeepHash`, which decides at EVERY
    node whether to reuse an earlier digest or build a fresh one: a node is
    first looked up in a run-scoped cache keyed by the Python object (bare
    numbers type-wrapped, so `1` and `1.0` never share an entry, but a
    Python-equal container -- tuple or frozenset -- does; a naive datetime
    never equals an aware one), and only on a miss is its content digest built
    from the children's (already-cached) digests. So a member can miss the
    cache at its outer tuple (a naive/aware sibling blocks it) yet still hit
    the cache at an inner container, and the two outer content digests then
    coincide once the datetimes normalize to one instant. A per-*member*
    two-tier rule cannot model this; onix computes one digest per member
    through the shared cache, exactly as DeepHash does. Every case asserts
    onix's full output against real deepdiff==9.1.0 (run under TZ=UTC).
    """
    naive = datetime.datetime(2024, 1, 1)
    aware = datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc)

    cases = [
        # (a, b, expect_empty)
        # Inner container hits the cache, outer misses -> content coincides:
        ({(naive, (1,))}, {(aware, (1.0,))}, True),
        ({(naive, (True,))}, {(aware, (1,))}, True),
        ({(naive, frozenset({True}))}, {(aware, frozenset({1}))}, True),
        ({(naive, frozenset({1}))}, {(aware, frozenset({1.0}))}, True),
        ({(naive, ((1,),))}, {(aware, ((1.0,),))}, True),
        # A bare-number sibling is type-distinct with no shared cache entry:
        ({(naive, 1)}, {(aware, 1.0)}, False),
        # Whole outer tuple Python-equal -> a single cache hit:
        ({(naive, 1)}, {(naive, 1.0)}, True),
        # Outer misses (naive/aware), but the Int(1) content agrees:
        ({(naive, 1)}, {(aware, 1)}, True),
        # A bare calendar member matches by instant:
        ({naive}, {aware}, True),
        ({naive, aware}, {aware}, True),
    ]
    for a, b, expect_empty in cases:
        onix = DeepDiff(a, b).to_dict()
        real = RealDeepDiff(a, b, verbose_level=2).to_dict()

        assert onix == real, f"onix diverged from real DeepDiff for {a!r} vs {b!r}"
        assert bool(onix) == (not expect_empty), f"unexpected result for {a!r} vs {b!r}: {onix!r}"


def test_a_bool_nested_in_a_tuple_set_member_is_its_own_identity_under_the_content_path() -> None:
    """A `bool` sibling forces the content path the same way a differently-typed number does.

    Every other test that exercises the content-digest path (the cache path
    blocked by a naive/aware pair) pairs the calendar element with a plain
    `int`/`float`; this pins the `bool` element on its own, since `bool` is
    its own `ItemKey` variant rather than folding into `Number`'s `int`/
    `float` cases.
    """
    naive = datetime.datetime(2024, 1, 1)
    aware = datetime.datetime(2024, 1, 1, tzinfo=datetime.timezone.utc)

    onix_same = DeepDiff({(naive, True)}, {(aware, True)}).to_dict()
    real_same = RealDeepDiff({(naive, True)}, {(aware, True)}, verbose_level=2).to_dict()
    assert onix_same == real_same == {}

    onix_diff = DeepDiff({(naive, True)}, {(aware, False)}).to_dict()
    real_diff = RealDeepDiff({(naive, True)}, {(aware, False)}, verbose_level=2).to_dict()
    assert onix_diff == real_diff
    assert onix_diff
