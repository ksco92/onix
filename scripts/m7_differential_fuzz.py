# /// script
# requires-python = "==3.13.*"
# dependencies = ["deepdiff==9.1.0"]
# ///
"""M7 differential fuzzer: onix's `--ignore-order` CLI vs real DeepDiff.

Generates random JSON list pairs (scalars, nested dicts/lists), runs both
onix's CLI and real `DeepDiff(ignore_order=True)`, and diffs canonical JSON
output. This is a development-time verification tool — not part of `make
check`, since it shells out to a debug build and to real `deepdiff`.

`object_diff` applies DeepDiff's `threshold_to_diff_deeper=0.33` dict
collapse unconditionally (root and nested, ordered and under
ignore_order), so `is_known_threshold_divergence` below is expected to
report zero hits — it stays in place as a classifier rather than being
removed, so a future regression here would surface as a named bucket
instead of an unexplained mismatch.

Usage::

    uv run scripts/m7_differential_fuzz.py [seed] [count] [--bias-nested-low-overlap-dicts]

The optional third argument switches to a generator biased toward nested
single-dict-in-a-list elements with low key overlap between `a`/`b` --
this shape exercises `ignore_order`'s own pairing decisions when a
candidate pair's distance depends on a nested dict-vs-dict comparison; the
plain generator above essentially never happens to hit this shape on its
own.
"""

import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Final

type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None

REPO_ROOT = Path(__file__).resolve().parent.parent
ONIX_BIN = REPO_ROOT / "target" / "debug" / "onix"

DICT_KEYS: Final[list[str]] = ["a", "b", "c", "d"]
SCALARS: Final[list[JsonValue]] = [
    None, True, False, 0, 1, 2, 3, -3, 0.0, 1.0, 2.0, 3.8, "x", "y", "z",
]


def gen_scalar(rng: random.Random) -> JsonValue:
    """Pick a random scalar."""
    return rng.choice(SCALARS)


def gen_value(rng: random.Random, depth: int) -> JsonValue:
    """
    Generate a random JSON value, nesting up to `depth` levels.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget.
    :return: A random JSON value.
    """
    if depth <= 0:
        return gen_scalar(rng)
    kind = rng.random()
    if kind < 0.55:
        return gen_scalar(rng)
    if kind < 0.8:
        length = rng.randint(0, 4)
        return [gen_value(rng, depth - 1) for _ in range(length)]
    keys = rng.sample(DICT_KEYS, rng.randint(0, len(DICT_KEYS)))
    return {k: gen_value(rng, depth - 1) for k in keys}


def gen_list(rng: random.Random, depth: int) -> list[JsonValue]:
    """Generate a random top-level list (the ignore_order target)."""
    length = rng.randint(0, 8)
    return [gen_value(rng, depth) for _ in range(length)]


def mutate(rng: random.Random, a: list[JsonValue]) -> list[JsonValue]:
    """Shuffle a copy of `a` and mutate a random subset of positions."""
    b = list(a)
    rng.shuffle(b)
    if b:
        change_n = rng.randint(0, len(b))
        for index in rng.sample(range(len(b)), change_n):
            b[index] = gen_value(rng, 2)
    return b


DICT_KEY_POOL: Final[list[str]] = [*DICT_KEYS, "e", "f", "g", "h"]


def gen_list_with_nested_low_overlap_dicts(rng: random.Random, depth: int) -> list[JsonValue]:
    """
    Like `gen_list`, but with an elevated chance that an element is a
    single-item list wrapping a multi-key dict -- the exact shape
    (`count_array_diff_leaves`'s trial sub-diff recursing into a nested
    dict-vs-dict pair) whose distance the disclosed
    `threshold_to_diff_deeper` reported-shape gap used to corrupt when
    computed via the real, non-threshold-aware `object_diff` -- see
    `crate::ignore_order::THRESHOLD_TO_DIFF_DEEPER`'s doc.

    :param rng: Seeded RNG.
    :param depth: Remaining nesting budget for the non-biased elements.
    :return: A random top-level list biased toward the trigger shape.
    """
    length = rng.randint(2, 8)
    result: list[JsonValue] = []
    for _ in range(length):
        if rng.random() < 0.4:
            keys = rng.sample(DICT_KEY_POOL, rng.randint(2, len(DICT_KEY_POOL)))
            result.append([{k: gen_value(rng, 1) for k in keys}])
        else:
            result.append(gen_value(rng, depth))
    return result


def mutate_toward_low_overlap(rng: random.Random, a: list[JsonValue]) -> list[JsonValue]:
    """
    Like `mutate`, but a nested single-dict-in-a-list element (see
    `gen_list_with_nested_low_overlap_dicts`) has an elevated chance of
    being replaced by a near-disjoint-keyed sibling rather than an
    unrelated random value -- keeping the low-key-overlap dict-vs-dict
    shape *paired up* across `a`/`b` (not just present on one side), which
    is what makes it a genuine pairing candidate rather than a guaranteed
    add/remove.

    :param rng: Seeded RNG.
    :param a: The list to mutate a shuffled copy of.
    :return: A shuffled, selectively mutated copy of `a`.
    """
    b = list(a)
    rng.shuffle(b)
    for index, item in enumerate(b):
        if (
            isinstance(item, list)
            and len(item) == 1
            and isinstance(item[0], dict)
            and item[0]
            and rng.random() < 0.6
        ):
            old_keys = set(item[0].keys())
            disjoint_pool = [k for k in DICT_KEY_POOL if k not in old_keys] or DICT_KEY_POOL
            new_keys = rng.sample(disjoint_pool, rng.randint(0, min(2, len(disjoint_pool))))
            b[index] = [{k: gen_value(rng, 1) for k in new_keys}]
        elif rng.random() < 0.3:
            b[index] = gen_value(rng, 2)
    return b


def run_onix(scratch: Path, a: JsonValue, b: JsonValue) -> JsonValue:
    """Run the onix CLI with --ignore-order and return its parsed report."""
    a_path = scratch / "a.json"
    b_path = scratch / "b.json"
    a_path.write_text(json.dumps(a))
    b_path.write_text(json.dumps(b))
    result = subprocess.run(
        [str(ONIX_BIN), "diff", str(a_path), str(b_path), "--ignore-order"],
        capture_output=True, text=True, check=True,
    )
    return json.loads(result.stdout)


def run_deepdiff(a: JsonValue, b: JsonValue) -> JsonValue:
    """Run real DeepDiff(ignore_order=True) and return its parsed report."""
    from deepdiff import DeepDiff
    return json.loads(DeepDiff(a, b, ignore_order=True, verbose_level=2).to_json())


def is_known_threshold_divergence(expected: JsonValue, actual: JsonValue) -> bool:
    """
    Heuristic: does this mismatch look like the threshold_to_diff_deeper=0.33
    dict-collapse in the reported diff shape? Kept as a named bucket so a
    future regression here shows up distinctly rather than as an
    unexplained mismatch; expected to report zero hits.

    :param expected: Real DeepDiff's report.
    :param actual: onix's report.
    :return: True if `expected` has a values_changed whose old/new values are
        both dicts and `actual` has no such entry at that path.
    """
    expected_vc = expected.get("values_changed", {}) if isinstance(expected, dict) else {}
    actual_vc = actual.get("values_changed", {}) if isinstance(actual, dict) else {}
    for path, entry in expected_vc.items():
        if isinstance(entry, dict) and isinstance(entry.get("old_value"), dict) and isinstance(entry.get("new_value"), dict):
            if path not in actual_vc:
                return True
    return False


def _strip_new_path(value: JsonValue) -> JsonValue:
    """
    Recursively strip every `new_path` key from a report, for classifying
    the "new_path composition" divergence class in isolation from
    everything else.

    :param value: A JSON value (typically a diff report or sub-object).
    :return: A deep copy of `value` with every `new_path` key removed.
    """
    if isinstance(value, dict):
        return {k: _strip_new_path(v) for k, v in value.items() if k != "new_path"}
    if isinstance(value, list):
        return [_strip_new_path(v) for v in value]
    return value


def is_new_path_only_divergence(expected: JsonValue, actual: JsonValue) -> bool:
    """
    Is this mismatch confined entirely to `new_path` string values —
    i.e. does the report match byte-for-byte once every `new_path` key is
    stripped from both sides?

    :param expected: Real DeepDiff's report.
    :param actual: onix's report.
    :return: True if only `new_path` fields differ.
    """
    return _strip_new_path(expected) == _strip_new_path(actual)


def main() -> None:
    """Run the fuzzer and print a summary."""
    if not ONIX_BIN.exists():
        print(f"error: {ONIX_BIN} not found; run `cargo build --workspace` first", file=sys.stderr)
        raise SystemExit(1)

    seed = int(sys.argv[1], 0) if len(sys.argv) > 1 else 0xDEC0DE
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 300
    bias_nested_dicts = len(sys.argv) > 3 and sys.argv[3] == "--bias-nested-low-overlap-dicts"
    rng = random.Random(seed)

    mismatches: list[tuple[int, JsonValue, JsonValue, JsonValue, JsonValue]] = []
    known_divergences = 0
    new_path_only_divergences = 0

    with tempfile.TemporaryDirectory(prefix="onix-m7-fuzz-") as tmp:
        scratch = Path(tmp)
        for i in range(count):
            if bias_nested_dicts:
                a = gen_list_with_nested_low_overlap_dicts(rng, depth=2)
                b = mutate_toward_low_overlap(rng, a)
            else:
                a = gen_list(rng, depth=2)
                b = mutate(rng, a)

            expected = run_deepdiff(a, b)
            actual = run_onix(scratch, a, b)

            if actual != expected:
                if is_known_threshold_divergence(expected, actual):
                    known_divergences += 1
                elif is_new_path_only_divergence(expected, actual):
                    new_path_only_divergences += 1
                else:
                    mismatches.append((i, a, b, expected, actual))

    print(f"Ran {count} cases (seed={seed}).")
    print(f"Known pre-existing threshold_to_diff_deeper divergences: {known_divergences}")
    print(f"new_path-composition-only divergences: {new_path_only_divergences}")
    print(f"Unexplained mismatches: {len(mismatches)}")
    for i, a, b, expected, actual in mismatches[:5]:
        print(f"--- mismatch #{i} ---")
        print("a:", json.dumps(a))
        print("b:", json.dumps(b))
        print("expected:", json.dumps(expected, sort_keys=True))
        print("actual:  ", json.dumps(actual, sort_keys=True))


if __name__ == "__main__":
    main()
