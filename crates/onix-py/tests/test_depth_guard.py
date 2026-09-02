"""Depth-guard tests: adversarially deep input must raise cleanly, never crash.

Every fixture here is built iteratively (a `for` loop wrapping a leaf in a
new single-item list, never Python-side recursion) so building the fixture
itself never hits Python's own recursion limit -- the whole point is to
prove `deepdiff_rs` itself stays safe on deep input, independent of how the
fixture is constructed.
"""

import json
import subprocess
import sys
import textwrap

import pytest

from deepdiff_rs import MAX_DEPTH_CEILING, DeepDiff, MaxDepthError, diff_json

type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None

# onix_core::DEFAULT_MAX_DEPTH -- see crates/onix-core/src/diff/options.rs.
DEFAULT_MAX_DEPTH = 512


def _run_isolated(body: str) -> subprocess.CompletedProcess[str]:
    """
    Run `body` in a fresh Python subprocess and return the completed process.

    Used for the cases whose *whole point* is that they no longer crash the
    interpreter: before the sized-worker fix they aborted the process with a
    native SIGSEGV, which in-process would take pytest itself down (an
    unhelpful "dead test run" rather than a failed test). Isolating them means
    a regression surfaces as a non-zero return code on this one subprocess --
    an ordinary failed assertion -- not a dead suite.

    :param body: Python source to run; it should assert its own expectations
        and exit 0 on success.
    :return: The completed subprocess (inspect ``returncode``/``stderr``).
    """
    src = "from deepdiff_rs import DeepDiff, MaxDepthError, diff_json\n" + textwrap.dedent(body)
    return subprocess.run(
        [sys.executable, "-c", src],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )


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


def test_input_deeper_than_max_depth_raises_at_conversion_time() -> None:
    """Input nested past a raised (but in-ceiling) max_depth raises MaxDepthError at conversion."""
    max_depth = 15_000
    a = _nested_list(max_depth + 5_000, leaf=1)
    b = _nested_list(max_depth + 5_000, leaf=2)

    with pytest.raises(MaxDepthError):
        DeepDiff(a, b, max_depth=max_depth)


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


# The sized-worker cases: genuinely-unequal input nested BELOW max_depth (so
# conversion succeeds and the diff itself runs), which is the exact shape that
# used to overflow the native stack and SIGSEGV the interpreter. Each runs in
# its own subprocess so a regression is a failed assertion, not a dead suite.


def test_deep_unequal_lists_below_max_depth_return_correct_diff() -> None:
    """10,000-deep unequal lists with a higher max_depth diff correctly (no crash)."""
    result = _run_isolated(
        """
        import json
        depth = 10_000
        a = 1
        b = 2
        for _ in range(depth):
            a = [a]
            b = [b]
        report = json.loads(DeepDiff(a, b, max_depth=12_000).to_json())
        changed = report["values_changed"]
        assert len(changed) == 1, changed
        (path, delta), = changed.items()
        assert path.count("[") == depth, path
        assert delta == {"new_value": 2, "old_value": 1}, delta
        print("OK")
        """,
    )
    assert result.returncode == 0, result.stderr
    assert "OK" in result.stdout


def test_near_ceiling_lists_and_dicts_return_correct_diff() -> None:
    """Unequal lists AND dicts nested near the ceiling diff correctly (no crash)."""
    result = _run_isolated(
        f"""
        import json
        depth = {MAX_DEPTH_CEILING} - 1_000
        max_depth = {MAX_DEPTH_CEILING}

        list_a, list_b = 1, 2
        dict_a, dict_b = 1, 2
        for _ in range(depth):
            list_a, list_b = [list_a], [list_b]
            dict_a, dict_b = {{"k": dict_a}}, {{"k": dict_b}}

        for a, b, opener in ((list_a, list_b, "["), (dict_a, dict_b, "[")):
            report = json.loads(DeepDiff(a, b, max_depth=max_depth).to_json())
            (path, delta), = report["values_changed"].items()
            assert path.count(opener) == depth, (opener, path.count(opener))
            assert delta == {{"new_value": 2, "old_value": 1}}, delta
        print("OK")
        """,
    )
    assert result.returncode == 0, result.stderr
    assert "OK" in result.stdout


def test_deep_report_lifecycle_to_dict_then_del_does_not_crash() -> None:
    """A deep diff report survives to_dict() and del (its deep Value drops safely)."""
    result = _run_isolated(
        f"""
        big = 1
        for _ in range({MAX_DEPTH_CEILING} - 5_000):
            big = {{"k": big}}
        diff = DeepDiff({{}}, {{"x": big}}, max_depth={MAX_DEPTH_CEILING})
        parsed = diff.to_dict()
        assert "dictionary_item_added" in parsed, parsed.keys()
        del parsed
        del diff
        print("OK")
        """,
    )
    assert result.returncode == 0, result.stderr
    assert "OK" in result.stdout


def test_max_depth_above_ceiling_raises_value_error_naming_the_ceiling() -> None:
    """A max_depth above the ceiling is refused up front with a catchable ValueError."""
    with pytest.raises(ValueError, match=str(MAX_DEPTH_CEILING)) as excinfo:
        DeepDiff([1], [2], max_depth=MAX_DEPTH_CEILING + 1)

    # Not a MaxDepthError: this is the up-front ceiling refusal, a different
    # (shallower) path than a nesting that exceeds an in-range max_depth.
    assert not isinstance(excinfo.value, MaxDepthError)


def test_max_depth_exactly_at_ceiling_is_accepted() -> None:
    """max_depth == the ceiling is allowed (the boundary is inclusive)."""
    diff = DeepDiff([1], [2], max_depth=MAX_DEPTH_CEILING)
    assert bool(diff) is True


def test_diff_json_max_depth_above_ceiling_raises_value_error() -> None:
    """diff_json enforces the same ceiling as the DeepDiff class."""
    with pytest.raises(ValueError, match=str(MAX_DEPTH_CEILING)):
        diff_json("[1]", "[2]", max_depth=MAX_DEPTH_CEILING + 1)


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
