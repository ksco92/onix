"""Differential fuzz test: onix's Python bindings vs real DeepDiff on live objects.

Runs through the actual `deepdiff_rs.DeepDiff` class (not the fast JSON-string
path), so this exercises the Python-object-to-`Value` conversion layer itself,
not just the diff engine underneath it.
"""

import json
import random
from typing import Final

from deepdiff import DeepDiff as RealDeepDiff

from deepdiff_rs import DeepDiff as OnixDeepDiff

type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None

DICT_KEYS: Final[list[str]] = ["a", "b", "c", "d", "e"]
SCALARS: Final[list[JsonValue]] = [
    None, True, False, 0, 1, -1, 2, 3, 0.0, 1.5, -2.25, "x", "y", "z", "",
]

# Comfortably over the >=500-case target; each case also runs twice (once
# ordered, once ignore_order=True), so this is 2x SEED_COUNT diffs total.
SEED_COUNT: Final[int] = 300


def _gen_scalar(rng: random.Random) -> JsonValue:
    """
    Pick a random scalar.

    :param rng: Seeded RNG.
    :return: A random scalar value.
    """
    return rng.choice(SCALARS)


def _gen_value(rng: random.Random, depth: int) -> JsonValue:
    """
    Generate a random JSON-shaped value, nesting up to `depth` levels.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget.
    :return: A random value built from the MVP-supported types only.
    """
    if depth <= 0:
        return _gen_scalar(rng)

    kind = rng.random()

    if kind < 0.5:
        return _gen_scalar(rng)

    if kind < 0.75:
        length = rng.randint(0, 4)
        return [_gen_value(rng, depth - 1) for _ in range(length)]

    keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))
    return {key: _gen_value(rng, depth - 1) for key in keys}


def _mutate(rng: random.Random, value: JsonValue) -> JsonValue:
    """
    Build a related-but-different copy of `value` (shuffle + selective mutation).

    :param rng: Seeded RNG.
    :param value: The value to derive a mutated copy from.
    :return: A structurally related, partially mutated copy.
    """
    if isinstance(value, list):
        mutated = list(value)
        rng.shuffle(mutated)

        for index in range(len(mutated)):
            if rng.random() < 0.3:
                mutated[index] = _gen_value(rng, 2)

        return mutated

    if isinstance(value, dict):
        mutated = dict(value)

        for key in list(mutated):
            if rng.random() < 0.3:
                mutated[key] = _gen_value(rng, 2)

        if rng.random() < 0.3:
            mutated[rng.choice(DICT_KEYS)] = _gen_value(rng, 2)

        return mutated

    return _gen_value(rng, 2)


def _generate_case(seed: int) -> tuple[JsonValue, JsonValue]:
    """
    Generate one seeded `(a, b)` pair.

    :param seed: The seed driving this case's RNG.
    :return: A related-but-different `(a, b)` pair.
    """
    rng = random.Random(seed)
    a = _gen_value(rng, 3)
    b = _mutate(rng, a)
    return a, b


def _diverges(a: JsonValue, b: JsonValue, ignore_order: bool) -> tuple[JsonValue, JsonValue] | None:
    """
    Diff `a`/`b` with both engines and return both reports if they disagree.

    :param a: The first value.
    :param b: The second value.
    :param ignore_order: Whether to diff with `ignore_order=True`.
    :return: `(expected, actual)` if they diverge, else `None`.
    """
    expected = json.loads(RealDeepDiff(a, b, ignore_order=ignore_order, verbose_level=2).to_json())
    actual = json.loads(OnixDeepDiff(a, b, ignore_order=ignore_order).to_json())

    if actual != expected:
        return expected, actual

    return None


def test_differential_fuzz_matches_real_deepdiff_ordered_and_ignore_order() -> None:
    """Runs SEED_COUNT seeded cases through both engines, both ordered and ignore_order=True."""
    mismatches = []

    for seed in range(SEED_COUNT):
        a, b = _generate_case(seed)

        for ignore_order in (False, True):
            divergence = _diverges(a, b, ignore_order)

            if divergence is not None:
                expected, actual = divergence
                mismatches.append((seed, ignore_order, a, b, expected, actual))

    assert not mismatches, (
        f"{len(mismatches)} of {SEED_COUNT * 2} fuzz cases diverged from real DeepDiff "
        f"(showing up to 3): {mismatches[:3]}"
    )
