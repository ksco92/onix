"""Differential fuzz test: onix's Python bindings vs real DeepDiff on live objects.

Runs through the actual `deepdiff_rs.DeepDiff` class (not the fast JSON-string
path), so this exercises the Python-object-to-`Value` conversion layer itself,
not just the diff engine underneath it.

Two batches, each of `SEED_COUNT` seeded cases run twice (ordered and
`ignore_order=True`): one over the JSON-shaped types, and one that also emits
tuples, both as containers in their own right and as elements of lists, dicts
and other tuples. Both batches compare `to_json()` (canonically, i.e. parsed —
neither tool promises a key order) *and* `to_dict()` by `==`, which is the
comparison that can see a tuple where the JSON one cannot.
"""

import json
import random
from typing import Final

from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff as OnixDeepDiff

type JsonValue = (
    dict[str, "JsonValue"]
    | list["JsonValue"]
    | tuple["JsonValue", ...]
    | str
    | int
    | float
    | bool
    | None
)

DICT_KEYS: Final[list[str]] = ["a", "b", "c", "d", "e"]
SCALARS: Final[list[JsonValue]] = [
    None, True, False, 0, 1, -1, 2, 3, 0.0, 1.5, -2.25, "x", "y", "z", "",
]

# Comfortably over the >=500-case target; each case also runs twice (once
# ordered, once ignore_order=True), so this is 2x SEED_COUNT diffs per batch.
SEED_COUNT: Final[int] = 300

# The tuple batch draws from a disjoint seed range, so the two batches are
# independent corpora rather than the same shapes with tuples sprinkled in.
TUPLE_SEED_BASE: Final[int] = 1_000_000

# Two edits that only the tuple batch applies, both aimed at shapes this slice
# turns on and that kind-preserving mutation alone can never produce: flipping
# a sequence between list and tuple while keeping its items, and re-typing a
# number inside a tuple within Python's `1 == 1.0 == True` family (which is
# what makes DeepHash's cache hand two tuples the same digest). Kept
# low-probability so a case still usually differs in more ordinary ways too.
KIND_FLIP_PROBABILITY: Final[float] = 0.15
RETYPE_PROBABILITY: Final[float] = 0.25


def _gen_scalar(rng: random.Random, scalars: list[JsonValue] | None = None) -> JsonValue:
    """
    Pick a random scalar.

    :param rng: Seeded RNG.
    :param scalars: Alphabet to draw from; defaults to the module `SCALARS`.
    :return: A random scalar value.
    """
    return rng.choice(SCALARS if scalars is None else scalars)


def _gen_value(
    rng: random.Random,
    depth: int,
    scalars: list[JsonValue] | None = None,
    tuples: bool = False,
) -> JsonValue:
    """
    Generate a random JSON-shaped value, nesting up to `depth` levels.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget.
    :param scalars: Scalar alphabet to draw leaves from; defaults to `SCALARS`.
    :param tuples: Whether half of the generated sequences are tuples rather
        than lists. The RNG is only consulted for this when it is set, so the
        `False` corpus is exactly the one that existed before tuples were
        supported.
    :return: A random value built from the MVP-supported types only.
    """
    if depth <= 0:
        return _gen_scalar(rng, scalars)

    kind = rng.random()

    # The tuple corpus leans harder on sequences (fewer bare scalars), so that
    # tuples actually show up in most cases rather than a minority of them.
    if kind < (0.3 if tuples else 0.5):
        return _gen_scalar(rng, scalars)

    if kind < 0.75:
        length = rng.randint(0, 4)
        items = [_gen_value(rng, depth - 1, scalars, tuples) for _ in range(length)]

        return tuple(items) if tuples and rng.random() < 0.5 else items

    keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))
    return {key: _gen_value(rng, depth - 1, scalars, tuples) for key in keys}


def _mutate(rng: random.Random, value: JsonValue, tuples: bool = False) -> JsonValue:
    """
    Build a related-but-different copy of `value` (shuffle + selective mutation).

    A sequence keeps its own kind: a mutated tuple is still a tuple, so a
    case's two sides differ in contents rather than in container type (which
    would make every case a single type change).

    :param rng: Seeded RNG.
    :param value: The value to derive a mutated copy from.
    :param tuples: Whether replacement values may themselves be tuples.
    :return: A structurally related, partially mutated copy.
    """
    if isinstance(value, (list, tuple)):
        mutated = list(value)
        rng.shuffle(mutated)

        for index in range(len(mutated)):
            if rng.random() < 0.3:
                mutated[index] = _gen_value(rng, 2, tuples=tuples)

        return tuple(mutated) if isinstance(value, tuple) else mutated

    if isinstance(value, dict):
        mutated = dict(value)

        for key in list(mutated):
            if rng.random() < 0.3:
                mutated[key] = _gen_value(rng, 2, tuples=tuples)

        if rng.random() < 0.3:
            mutated[rng.choice(DICT_KEYS)] = _gen_value(rng, 2, tuples=tuples)

        return mutated

    return _gen_value(rng, 2, tuples=tuples)


def _generate_case(seed: int, tuples: bool = False) -> tuple[JsonValue, JsonValue]:
    """
    Generate one seeded `(a, b)` pair.

    :param seed: The seed driving this case's RNG.
    :param tuples: Whether the pair may contain tuples.
    :return: A related-but-different `(a, b)` pair.
    """
    rng = random.Random(seed)
    a = _gen_value(rng, 3, tuples=tuples)
    b = _mutate(rng, a, tuples=tuples)

    if tuples:
        b = _tuple_edge_mutations(rng, b)

    return a, b


def _retype_number(rng: random.Random, value: JsonValue) -> JsonValue:
    """
    Re-type a number within Python's numeric equality family, keeping its value.

    :param rng: Seeded RNG.
    :param value: The number to re-type.
    :return: An equal value of a different type (`1` -> `1.0` or `True`), or
        `value` unchanged when no equal re-typing exists.
    """
    if isinstance(value, bool):
        return int(value) if rng.random() < 0.5 else float(value)

    if isinstance(value, int):
        if value in (0, 1) and rng.random() < 0.5:
            return bool(value)

        return float(value)

    if isinstance(value, float) and value.is_integer() and abs(value) < 2**53:
        return int(value) if rng.random() < 0.5 else bool(value) if value in (0.0, 1.0) else int(value)

    return value


def _tuple_edge_mutations(rng: random.Random, value: JsonValue, in_tuple: bool = False) -> JsonValue:
    """
    Apply the two tuple-specific edits recursively (see KIND_FLIP_PROBABILITY).

    :param rng: Seeded RNG.
    :param value: The value to edit.
    :param in_tuple: Whether `value` sits inside a tuple, which is where a
        numeric re-type is worth making.
    :return: The edited value.
    """
    if isinstance(value, (list, tuple)):
        as_tuple = isinstance(value, tuple)
        items = [_tuple_edge_mutations(rng, item, in_tuple=as_tuple) for item in value]

        if rng.random() < KIND_FLIP_PROBABILITY:
            as_tuple = not as_tuple

        return tuple(items) if as_tuple else items

    if isinstance(value, dict):
        return {key: _tuple_edge_mutations(rng, item) for key, item in value.items()}

    if in_tuple and isinstance(value, (bool, int, float)) and rng.random() < RETYPE_PROBABILITY:
        return _retype_number(rng, value)

    return value


def _normalize_types(value: JsonValue | type) -> JsonValue:
    """
    Replace any Python type object in a report with its name.

    Real DeepDiff's `to_dict()` reports a `type_changes` entry's `old_type`/
    `new_type` as the type objects themselves, where `deepdiff_rs` reports the
    names its `to_json()` uses (`"tuple"`, `"list"`, ...). That one difference
    is a documented gap of this MVP, so it is normalized away here rather than
    swamping every other comparison this test exists to make.

    :param value: A report, or any part of one.
    :return: The same value with type objects replaced by their names.
    """
    if isinstance(value, dict):
        return {key: _normalize_types(item) for key, item in value.items()}

    if isinstance(value, list):
        return [_normalize_types(item) for item in value]

    if isinstance(value, tuple):
        return tuple(_normalize_types(item) for item in value)

    if isinstance(value, type):
        return value.__name__

    return value


def _diverges(a: JsonValue, b: JsonValue, ignore_order: bool) -> tuple[JsonValue, JsonValue] | None:
    """
    Diff `a`/`b` with both engines and return both reports if they disagree.

    Both renderings are compared: the JSON one (canonically, by parsing) and
    the dict one, which is the only one that can see a tuple.

    :param a: The first value.
    :param b: The second value.
    :param ignore_order: Whether to diff with `ignore_order=True`.
    :return: `(expected, actual)` if they diverge, else `None`.
    """
    real = RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2)
    onix = OnixDeepDiff(a, b, ignore_order=ignore_order)

    expected_json = json.loads(real.to_json())
    actual_json = json.loads(onix.to_json())

    if actual_json != expected_json:
        return expected_json, actual_json

    expected_dict = _normalize_types(real.to_dict())
    actual_dict = _normalize_types(onix.to_dict())

    if actual_dict != expected_dict:
        return expected_dict, actual_dict

    return None


def _run_batch(seeds: range, tuples: bool) -> list[tuple[int, bool, JsonValue, JsonValue, JsonValue, JsonValue]]:
    """
    Run one batch of seeded cases through both engines, ordered and ignore_order.

    :param seeds: The seeds to generate cases from.
    :param tuples: Whether the generated cases may contain tuples.
    :return: One entry per diverging (seed, ignore_order) combination.
    """
    mismatches = []

    for seed in seeds:
        a, b = _generate_case(seed, tuples=tuples)

        for ignore_order in (False, True):
            divergence = _diverges(a, b, ignore_order)

            if divergence is not None:
                expected, actual = divergence
                mismatches.append((seed, ignore_order, a, b, expected, actual))

    return mismatches


def test_differential_fuzz_matches_real_deepdiff_ordered_and_ignore_order() -> None:
    """Runs SEED_COUNT seeded cases through both engines, both ordered and ignore_order=True."""
    mismatches = _run_batch(range(SEED_COUNT), tuples=False)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} fuzz cases diverged from real DeepDiff "
        f"(showing up to 3): {mismatches[:3]}"
    )


def test_differential_fuzz_with_tuples_matches_real_deepdiff() -> None:
    """Runs a second SEED_COUNT-case batch whose values also contain tuples."""
    seeds = range(TUPLE_SEED_BASE, TUPLE_SEED_BASE + SEED_COUNT)
    mismatches = _run_batch(seeds, tuples=True)

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} tuple fuzz cases diverged from real DeepDiff "
        f"(showing up to 3): {mismatches[:3]}"
    )
