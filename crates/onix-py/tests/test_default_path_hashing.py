"""Dict keys crafted to collide under the engine's fixed-seed hasher must cost what ordinary keys cost.

With default options a dict's keys reach no table hashed by content: the generator below solves
FxHash's last word so every key lands on one hash, which turns a hashed table quadratic.
"""

import sys
import time
from typing import Final

from deepdiff_rs import DeepDiff

FX_SEED: Final[int] = 0x517CC1B727220A95
MASK: Final[int] = (1 << 64) - 1
KEY_COUNT: Final[int] = 20_000
# FxHash writes a byte slice's length before its bytes.
KEY_BYTES: Final[int] = 16


def _fx_add(state: int, word: int) -> int:
    """Fold one 64-bit word into an FxHash state."""
    rotated = ((state << 5) | (state >> 59)) & MASK
    return ((rotated ^ word) * FX_SEED) & MASK


def _colliding_keys(count: int) -> list[str]:
    """
    Build `count` distinct 16-byte keys that share one FxHash as byte slices.

    :param count: Number of keys.
    :return: The keys, each an 8-byte ASCII word then a word solved to zero the final state.
    """
    keys: list[str] = []
    counter = 0

    while len(keys) < count:
        first = bytes(ord("a") + ((counter >> (4 * nibble)) & 0xF) for nibble in range(8))
        counter += 1
        state = 0
        for word in (KEY_BYTES, int.from_bytes(first, sys.byteorder)):
            state = _fx_add(state, word)
        second = (((state << 5) | (state >> 59)) & MASK).to_bytes(8, sys.byteorder)
        try:
            keys.append((first + second).decode("utf-8"))
        except UnicodeDecodeError:
            continue

    return keys


def _distinct_keys(count: int) -> list[str]:
    """Build `count` ordinary 15-byte keys."""
    return [f"key{i:012d}" for i in range(count)]


def _dicts(keys: list[str]) -> tuple[dict[str, int], dict[str, int]]:
    """`{k: 1}` against `{k: 2}`."""
    return {key: 1 for key in keys}, {key: 2 for key in keys}


def _fastest_diff(keys: list[str]) -> float:
    """Time the fastest of three default-options diffs of `_dicts(keys)`, in seconds."""
    a, b = _dicts(keys)
    samples = []

    for _ in range(3):
        start = time.perf_counter()
        diff = DeepDiff(a, b)
        samples.append(time.perf_counter() - start)
        assert diff

    return min(samples)


def test_colliding_dict_keys_cost_what_distinct_keys_cost() -> None:
    """Crafted-collision dict keys diff within a small factor of ordinary keys."""
    colliding = _fastest_diff(_colliding_keys(KEY_COUNT))
    distinct = _fastest_diff(_distinct_keys(KEY_COUNT))

    assert colliding < 4 * distinct, f"colliding {colliding:.3f}s vs distinct {distinct:.3f}s"
