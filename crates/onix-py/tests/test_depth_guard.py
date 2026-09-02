"""Depth-guard tests: adversarially deep input must raise cleanly, never crash.

Every fixture here is built iteratively (a `for` loop wrapping a leaf in a
new single-item list, never Python-side recursion) so building the fixture
itself never hits Python's own recursion limit -- the whole point is to
prove `deepdiff_rs` itself stays safe on deep input, independent of how the
fixture is constructed.
"""

import json

import pytest

from deepdiff_rs import DeepDiff, MaxDepthError, diff_json

type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None

# onix_core::DEFAULT_MAX_DEPTH -- see crates/onix-core/src/diff/options.rs.
DEFAULT_MAX_DEPTH = 512


def _nested_list(depth: int, leaf: JsonValue) -> JsonValue:
    """
    Build a list nested `depth` levels deep around `leaf`, iteratively.

    :param depth: How many list layers to wrap `leaf` in.
    :param leaf: The innermost value.
    :return: `leaf` wrapped in `depth` single-item lists.
    """
    value = leaf

    for _ in range(depth):
        value = [value]

    return value


def test_deep_unequal_input_raises_max_depth_error_not_a_crash() -> None:
    """An adversarially deep, unequal pair raises MaxDepthError, never crashes."""
    a = _nested_list(100_000, leaf=1)
    b = _nested_list(100_000, leaf=2)

    with pytest.raises(MaxDepthError):
        DeepDiff(a, b)


def test_max_depth_error_is_a_value_error_subclass() -> None:
    """MaxDepthError is catchable by callers that only expect ValueError."""
    a = _nested_list(1000, leaf=1)
    b = _nested_list(1000, leaf=2)

    with pytest.raises(ValueError):
        DeepDiff(a, b)


def test_deep_equal_input_also_raises_at_conversion_time() -> None:
    """
    Documented MVP limitation: unlike `onix_core`'s own `diff_with_max_depth`
    (which lets two *equal* inputs of any depth diff cleanly regardless of
    `max_depth`), the bindings' Python-object-to-`Value` conversion runs
    before equality can be known and is bounded by the same `max_depth`
    budget on its own -- so an equal-but-adversarially-deep pair still
    raises here (see `crates/onix-py/src/convert.rs`'s module doc).
    """
    value = _nested_list(100_000, leaf=1)

    with pytest.raises(MaxDepthError):
        DeepDiff(value, value)


def test_max_depth_boundary_accepts_exactly_and_rejects_one_more() -> None:
    """Nesting of exactly `max_depth` succeeds; one level deeper raises."""
    max_depth = 20
    a_at_bound = _nested_list(max_depth, leaf=1)
    b_at_bound = _nested_list(max_depth, leaf=2)
    diff = DeepDiff(a_at_bound, b_at_bound, max_depth=max_depth)
    assert bool(diff) is True

    a_over_bound = _nested_list(max_depth + 1, leaf=1)
    b_over_bound = _nested_list(max_depth + 1, leaf=2)

    with pytest.raises(MaxDepthError):
        DeepDiff(a_over_bound, b_over_bound, max_depth=max_depth)


def test_default_max_depth_matches_onix_core() -> None:
    """The constructor's default max_depth is onix_core::DEFAULT_MAX_DEPTH (512)."""
    a = _nested_list(DEFAULT_MAX_DEPTH, leaf=1)
    b = _nested_list(DEFAULT_MAX_DEPTH, leaf=2)
    diff = DeepDiff(a, b)
    assert bool(diff) is True

    a_over = _nested_list(DEFAULT_MAX_DEPTH + 1, leaf=1)
    b_over = _nested_list(DEFAULT_MAX_DEPTH + 1, leaf=2)

    with pytest.raises(MaxDepthError):
        DeepDiff(a_over, b_over)


# diff_json's own depth guard (no Python-object conversion involved -- JSON
# text is parsed and diffed entirely in Rust).


def _nested_json_array(depth: int, leaf: int) -> str:
    """
    Build a JSON array literal nested `depth` levels deep around `leaf`.

    :param depth: How many array layers to wrap `leaf` in.
    :param leaf: The innermost scalar.
    :return: The JSON text, built by string concatenation (no recursion).
    """
    return ("[" * depth) + str(leaf) + ("]" * depth)


def test_diff_json_moderately_deep_input_raises_max_depth_error() -> None:
    """
    A JSON array nested past a small custom max_depth, but well under
    `serde_json`'s own 128-level parser recursion limit, parses fine and
    then raises MaxDepthError from the diff itself.
    """
    a = _nested_json_array(50, leaf=1)
    b = _nested_json_array(50, leaf=2)

    with pytest.raises(MaxDepthError):
        diff_json(a, b, max_depth=10)


def test_diff_json_past_parser_recursion_limit_raises_value_error() -> None:
    """
    A JSON array nested past `serde_json`'s own ~128-level parser recursion
    limit fails to parse at all, raising ValueError -- a different, also
    clean error path from MaxDepthError (which only fires once parsing has
    already succeeded).
    """
    a = _nested_json_array(200, leaf=1)
    b = _nested_json_array(200, leaf=2)

    with pytest.raises(ValueError):
        diff_json(a, b)


def test_diff_json_reference_parses_with_standard_json_module() -> None:
    """Sanity check: the fixture builder produces genuinely valid JSON."""
    assert json.loads(_nested_json_array(5, leaf=1)) == [[[[[1]]]]]
