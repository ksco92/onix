# /// script
# requires-python = "==3.13.*"
# dependencies = []
# ///
"""Generate onix's deterministic benchmark fixture matrix.

Writes one directory per fixture under ``perf/fixtures/`` (gitignored), each
holding ``a.json``/``b.json`` plus a top-level ``manifest.json`` recording
the seed and every fixture's name and byte sizes.

Every fixture derives its own `random.Random` from `BASE_SEED` and is written
with fixed `json.dump` settings, so two runs are byte-identical: verify with
two generator runs, `shasum -a 256` over `perf/fixtures`, and an empty `diff`.
"""

import copy
import json
import random
import shutil
from collections.abc import Callable
from pathlib import Path
from typing import Final

from _common import JsonValue

##############################################
##############################################
##############################################
##############################################
# Configuration

FIXTURES_ROOT: Final[Path] = Path(__file__).resolve().parent / "fixtures"

# Recorded base seed. Every fixture below derives its own sub-seed by adding
# a fixed, named offset to this, so fixtures never share (or interfere with)
# an RNG stream regardless of generation order.
BASE_SEED: Final[int] = 20260831

# Controlled mutation rate applied between a fixture's `a` and `b` values,
# per this harness's fixture-matrix design (~5% changed, ~2% added, ~2%
# removed). Changed/added values are always drawn from a range disjoint from
# the original value range (see `_changed_int`/`_added_int` below), so a
# mutation is always a genuine, guaranteed difference rather than a
# coincidental no-op.
VALUE_CHANGE_RATE: Final[float] = 0.05
ADD_RATE: Final[float] = 0.02
REMOVE_RATE: Final[float] = 0.02

# Scale constants: bump these to grow the matrix later without touching the
# generation logic below.
FLAT_DICT_SIZES: Final[dict[str, int]] = {"10k": 10_000, "100k": 100_000, "1m": 1_000_000}
FLAT_LIST_SIZE: Final[int] = 100_000
NESTED_DEPTH: Final[int] = 6
NESTED_BRANCH: Final[int] = 10
# 120: both tools' real depth ceiling is narrower than originally
# anticipated — see RESULTS.md's "Finding: onix's practical depth ceiling is
# lower than expected" section for the full empirical probe and rationale.
DEEP_NESTING_DEPTH: Final[int] = 120
IGNORE_ORDER_LIST_SIZE: Final[int] = 10_000

# 50_000: capped well below the originally suggested ~50-200MB so the full
# deterministic harness (every fixture run multiple times) stays practical
# to run in one sitting — see RESULTS.md's "Deferred work" section.
API_PAYLOAD_RECORD_COUNT: Final[int] = 50_000

# Disjoint value ranges so a "changed" or "added" scalar can never coincide
# with an original value (see the module docstring).
_ORIGINAL_INT_RANGE: Final[tuple[int, int]] = (0, 1_000_000)
_CHANGED_INT_RANGE: Final[tuple[int, int]] = (10_000_000, 20_000_000)
_ADDED_INT_RANGE: Final[tuple[int, int]] = (20_000_000, 30_000_000)


##############################################
##############################################
##############################################
##############################################
# Generic mutation helpers (reused by every dict/list-shaped fixture)


def mutate_dict(
    base: dict[str, JsonValue],
    rng: random.Random,
    change_value: Callable[[random.Random], JsonValue],
    add_value: Callable[[random.Random], JsonValue],
) -> dict[str, JsonValue]:
    """Build a mutated copy of `base`: changed, removed, and added keys are disjoint, at the module's rates."""
    keys = list(base.keys())
    shuffled = keys[:]
    rng.shuffle(shuffled)

    change_n = int(len(keys) * VALUE_CHANGE_RATE)
    remove_n = int(len(keys) * REMOVE_RATE)
    add_n = int(len(keys) * ADD_RATE)
    change_keys = set(shuffled[:change_n])
    remove_keys = set(shuffled[change_n : change_n + remove_n])

    mutated: dict[str, JsonValue] = {}

    for key, value in base.items():
        if key in remove_keys:
            continue

        if key in change_keys:
            mutated[key] = change_value(rng)
        else:
            mutated[key] = value

    for i in range(add_n):
        mutated[f"added_{i:07d}"] = add_value(rng)

    return mutated


def mutate_list(
    base: list[JsonValue],
    rng: random.Random,
    change_item: Callable[[JsonValue, random.Random], JsonValue],
    new_item: Callable[[int, random.Random], JsonValue],
) -> list[JsonValue]:
    """Build a mutated copy of `base`: items change in place; removals/additions are tail-only (no index-shift avalanche)."""
    n = len(base)
    change_n = int(n * VALUE_CHANGE_RATE)
    remove_n = int(n * REMOVE_RATE)
    add_n = int(n * ADD_RATE)

    mutated = list(base)

    for index in rng.sample(range(n), change_n):
        mutated[index] = change_item(mutated[index], rng)

    if remove_n:
        mutated = mutated[:-remove_n]

    for offset in range(add_n):
        mutated.append(new_item(n + offset, rng))

    return mutated


def _changed_int(rng: random.Random) -> int:
    """Draw a "changed" integer, disjoint from `_ORIGINAL_INT_RANGE`."""
    return rng.randint(*_CHANGED_INT_RANGE)


def _added_int(rng: random.Random) -> int:
    """Draw an "added" integer, disjoint from the other two ranges."""
    return rng.randint(*_ADDED_INT_RANGE)


##############################################
##############################################
##############################################
##############################################
# Fixture builders


def build_flat_dict(size: int, seed: int) -> tuple[JsonValue, JsonValue]:
    """Build a flat dict pair of `size` keys at the module's mutation rate."""
    rng = random.Random(seed)
    a: dict[str, JsonValue] = {f"key_{i:07d}": rng.randint(*_ORIGINAL_INT_RANGE) for i in range(size)}
    b = mutate_dict(a, rng, _changed_int, _added_int)

    return a, b


def build_flat_list(size: int, seed: int) -> tuple[JsonValue, JsonValue]:
    """Build a flat list pair of `size` items at the module's mutation rate."""
    rng = random.Random(seed)
    a: list[JsonValue] = [rng.randint(*_ORIGINAL_INT_RANGE) for _ in range(size)]
    b = mutate_list(a, rng, lambda _value, r: _changed_int(r), lambda _idx, r: _added_int(r))

    return a, b


def _build_tree(rng: random.Random, depth: int, branch: int) -> JsonValue:
    """Recursively build a uniform tree: `branch` children per level down to `depth`, with a scalar leaf at depth 0."""
    if depth == 0:
        return rng.randint(*_ORIGINAL_INT_RANGE)

    return {f"b{i}": _build_tree(rng, depth - 1, branch) for i in range(branch)}


def _decode_path(index: int, length: int, branch: int) -> list[int]:
    """Decode `index` into `length` base-`branch` digits, most significant first."""
    digits: list[int] = []

    for _ in range(length):
        digits.append(index % branch)
        index //= branch

    digits.reverse()

    return digits


def _descend(tree: JsonValue, digits: list[int]) -> JsonValue:
    """Walk `tree` down `digits`, one `f"b{d}"` dict hop per digit."""
    node = tree

    for d in digits:
        assert isinstance(node, dict)
        node = node[f"b{d}"]

    return node


def build_nested_uniform(depth: int, branch: int, seed: int) -> tuple[JsonValue, JsonValue]:
    """Build a uniform-tree pair at the module's mutation rate, applied to leaves; adds/removes fold into whole leaf-groups."""
    rng = random.Random(seed)
    a = _build_tree(rng, depth, branch)
    b = copy.deepcopy(a)

    total_leaves = branch**depth
    total_groups = branch ** (depth - 1)

    change_n = int(total_leaves * VALUE_CHANGE_RATE)

    for leaf_index in rng.sample(range(total_leaves), change_n):
        leaf_digits = _decode_path(leaf_index, depth, branch)
        group = _descend(b, leaf_digits[:-1])
        assert isinstance(group, dict)
        group[f"b{leaf_digits[-1]}"] = _changed_int(rng)

    group_n = int(total_leaves * ADD_RATE)
    shuffled_groups = rng.sample(range(total_groups), 2 * group_n)
    add_groups, remove_groups = shuffled_groups[:group_n], shuffled_groups[group_n:]

    for group_index in add_groups:
        group_digits = _decode_path(group_index, depth - 1, branch)
        group = _descend(b, group_digits)
        assert isinstance(group, dict)
        group["extra"] = _added_int(rng)

    for group_index in remove_groups:
        group_digits = _decode_path(group_index, depth - 1, branch)
        group = _descend(b, group_digits)
        assert isinstance(group, dict)
        del group[f"b{rng.randrange(branch)}"]

    return a, b


def build_deep_nesting(depth: int, seed: int) -> tuple[JsonValue, JsonValue]:
    """Build a single-chain nested dict pair `depth` levels deep, with one changed leaf value."""
    rng = random.Random(seed)
    original_leaf = rng.randint(*_ORIGINAL_INT_RANGE)
    changed_leaf = _changed_int(rng)

    a: JsonValue = {"leaf": original_leaf}
    b: JsonValue = {"leaf": changed_leaf}

    for _ in range(depth):
        a = {"nested": a}
        b = {"nested": b}

    return a, b


def _make_record(index: int, rng: random.Random) -> dict[str, JsonValue]:
    """Build one heterogeneous "API payload" record: a realistic mix of scalars, a nested object, and nested lists."""
    tag_count = rng.randint(0, 5)
    history_count = rng.randint(0, 3)

    return {
        "id": index,
        "uuid": f"{index:08x}-{rng.getrandbits(32):08x}-{rng.getrandbits(32):08x}",
        "name": f"user_{index:07d}",
        "email": f"user_{index:07d}@example.test",
        "active": rng.random() < 0.8,
        "score": round(rng.uniform(0, 100), 4),
        # Wrapped only to keep this fixture byte-identical to RESULTS.md's published shape — see summarize_results.py's render_correctness_section.
        "tags": [{"tag": f"tag_{rng.randint(0, 999)}"} for _ in range(tag_count)],
        "address": {
            "street": f"{rng.randint(1, 9999)} Main St",
            "city": rng.choice(["Springfield", "Shelbyville", "Ogdenville", "Capital City"]),
            "state": rng.choice(["CA", "NY", "TX", "WA", "CO"]),
            "zip": f"{rng.randint(10000, 99999)}",
        },
        "created_at": f"2024-{rng.randint(1, 12):02d}-{rng.randint(1, 28):02d}T00:00:00Z",
        "metadata": {
            "source": rng.choice(["web", "mobile", "api", "batch"]),
            "priority": rng.randint(0, 5),
            "flags": [{"value": rng.random() < 0.5} for _ in range(3)],
        },
        "history": [
            {
                "event": rng.choice(["created", "updated", "viewed", "archived"]),
                "at": f"2024-{rng.randint(1, 12):02d}-{rng.randint(1, 28):02d}T00:00:00Z",
                "value": round(rng.uniform(0, 1), 4),
            }
            for _ in range(history_count)
        ],
    }


def _mutate_record(record: dict[str, JsonValue], rng: random.Random) -> dict[str, JsonValue]:
    """Mutate one API-payload record: a new score, flipped `active`, and one appended tag."""
    tags = record["tags"]
    assert isinstance(tags, list)
    mutated = dict(record)
    mutated["score"] = round(rng.uniform(0, 100), 4)
    mutated["active"] = not record["active"]
    mutated["tags"] = [*tags, {"tag": f"tag_extra_{rng.randint(0, 999)}"}]

    return mutated


def build_api_payloads(record_count: int, seed: int) -> tuple[JsonValue, JsonValue]:
    """Build the "API payload" fixture: `record_count` heterogeneous records, mutated at record granularity."""
    rng = random.Random(seed)
    a: list[JsonValue] = [_make_record(i, rng) for i in range(record_count)]

    def change_item(value: JsonValue, r: random.Random) -> JsonValue:
        """Mutate one existing record in place, for `mutate_list`'s "changed" case."""
        assert isinstance(value, dict)
        return _mutate_record(value, r)

    def new_item(index: int, r: random.Random) -> JsonValue:
        """Build one brand-new record, for `mutate_list`'s "added" case."""
        return _make_record(index, r)

    b = mutate_list(a, rng, change_item, new_item)

    return a, b


def build_ignore_order_list(size: int, seed: int) -> tuple[JsonValue, JsonValue]:
    """Build the `ignore_order_10k` fixture: `b` is a shuffled, mutated copy of `a`."""
    rng = random.Random(seed)
    a: list[JsonValue] = [rng.randint(*_ORIGINAL_INT_RANGE) for _ in range(size)]
    b = list(a)
    rng.shuffle(b)

    change_n = int(size * VALUE_CHANGE_RATE)

    for index in rng.sample(range(size), change_n):
        b[index] = _changed_int(rng)

    return a, b


##############################################
##############################################
##############################################
##############################################
# Writing + manifest


def write_json(path: Path, value: JsonValue) -> int:
    """Write `value` as compact JSON (no pretty-printing; fixtures aren't committed)."""
    text = json.dumps(value, separators=(",", ":"), sort_keys=False, ensure_ascii=True)
    path.write_text(text, encoding="utf-8")

    return len(text.encode("utf-8"))


def write_fixture(name: str, pair: tuple[JsonValue, JsonValue]) -> dict[str, JsonValue]:
    """Write one fixture's `a.json`/`b.json` and return its manifest entry."""
    fixture_dir = FIXTURES_ROOT / name
    fixture_dir.mkdir(parents=True, exist_ok=True)
    a_bytes = write_json(fixture_dir / "a.json", pair[0])
    b_bytes = write_json(fixture_dir / "b.json", pair[1])

    return {"name": name, "a_bytes": a_bytes, "b_bytes": b_bytes}


def main() -> None:
    """Regenerate every fixture directory and the manifest under `perf/fixtures/`."""
    if FIXTURES_ROOT.exists():
        shutil.rmtree(FIXTURES_ROOT)

    FIXTURES_ROOT.mkdir(parents=True)

    manifest: list[dict[str, JsonValue]] = []

    for label, size in FLAT_DICT_SIZES.items():
        manifest.append(
            write_fixture(f"flat_dict_{label}", build_flat_dict(size, BASE_SEED + size)),
        )

    manifest.append(
        write_fixture(
            "flat_list_100k",
            build_flat_list(FLAT_LIST_SIZE, BASE_SEED + 1_100_000),
        ),
    )
    manifest.append(
        write_fixture(
            "nested_uniform_d6_b10",
            build_nested_uniform(NESTED_DEPTH, NESTED_BRANCH, BASE_SEED + 1_200_000),
        ),
    )
    manifest.append(
        write_fixture(
            "api_payloads",
            build_api_payloads(API_PAYLOAD_RECORD_COUNT, BASE_SEED + 1_300_000),
        ),
    )
    manifest.append(
        write_fixture(
            f"deep_narrow_d{DEEP_NESTING_DEPTH}",
            build_deep_nesting(DEEP_NESTING_DEPTH, BASE_SEED + 1_400_000),
        ),
    )
    manifest.append(
        write_fixture(
            "startup_trivial",
            ({}, {}),
        ),
    )
    manifest.append(
        write_fixture(
            "ignore_order_10k",
            build_ignore_order_list(IGNORE_ORDER_LIST_SIZE, BASE_SEED + 1_500_000),
        ),
    )

    # identical_1m: flat_dict_1m's `a.json` copied to both sides of a new
    # fixture (byte-identical files, not merely equal values) — the no-diff
    # fast path this harness's design calls for.
    flat_dict_1m_a = FIXTURES_ROOT / "flat_dict_1m" / "a.json"
    identical_dir = FIXTURES_ROOT / "identical_1m"
    identical_dir.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(flat_dict_1m_a, identical_dir / "a.json")
    shutil.copyfile(flat_dict_1m_a, identical_dir / "b.json")
    manifest.append(
        {
            "name": "identical_1m",
            "a_bytes": flat_dict_1m_a.stat().st_size,
            "b_bytes": flat_dict_1m_a.stat().st_size,
        },
    )

    manifest_path = FIXTURES_ROOT / "manifest.json"
    manifest_document = {"base_seed": BASE_SEED, "fixtures": manifest}
    manifest_path.write_text(
        json.dumps(manifest_document, indent=2, sort_keys=False, ensure_ascii=True) + "\n",
        encoding="utf-8",
    )

    def entry_size(entry: dict[str, JsonValue]) -> int:
        """Sum one manifest entry's `a_bytes` and `b_bytes`."""
        a_bytes, b_bytes = entry["a_bytes"], entry["b_bytes"]
        assert isinstance(a_bytes, int)
        assert isinstance(b_bytes, int)

        return a_bytes + b_bytes

    total_bytes = sum(entry_size(entry) for entry in manifest)
    print(f"Wrote {len(manifest)} fixtures ({total_bytes / 1_000_000:.1f} MB total) to {FIXTURES_ROOT}")


if __name__ == "__main__":
    main()
