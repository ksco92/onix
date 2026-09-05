"""Runs ``mypy --strict`` over a script that exercises every stub-declared
callable, so the stub is proven usable — not just present — by an actual
type checker rather than only by the signature-comparison test.
"""

import subprocess
import sys
import textwrap
from pathlib import Path

SCRIPT = textwrap.dedent(
    """
    from typing import Any

    import pyarrow as pa

    from deepdiff_rs import DeepDiff, MaxDepthError, MAX_DEPTH_CEILING, diff_json, diff_tables

    diff: DeepDiff = DeepDiff({"a": 1}, {"a": 2}, ignore_order=True, max_depth=64)
    is_different: bool = bool(diff)
    report_json: str = diff.to_json()
    report_dict: dict[str, Any] = diff.to_dict()

    ceiling: int = MAX_DEPTH_CEILING
    try:
        diff_json('{"a": 1}', '{"a": 2}', ignore_order=False, max_depth=32)
    except MaxDepthError as error:
        raise ValueError(str(error)) from error

    left = pa.table({"id": pa.array([1, 2], pa.int64())})
    right = pa.table({"id": pa.array([2, 3], pa.int64())})
    table_diff = diff_tables(left, right, key=["id"])

    schema_rows: list[dict[str, Any]] = table_diff.schema
    schema_summary: dict[str, int] = table_diff.summary()
    schema_json: str = table_diff.to_json()

    for member in (
        table_diff.schema_arrow,
        table_diff.rows_added(),
        table_diff.rows_removed(),
        table_diff.cells_changed(),
        table_diff.duplicate_keys(),
    ):
        row_count: int = len(member)
        capsule = member.__arrow_c_stream__()
        schema_capsule = member.__arrow_c_schema__()
        as_pyarrow = member.to_pyarrow()
    """
)


def test_stub_typechecks_under_mypy_strict(tmp_path: Path) -> None:
    script = tmp_path / "stub_usage.py"
    script.write_text(SCRIPT)

    result = subprocess.run(
        # pyarrow ships no py.typed marker of its own; ignoring that one
        # import-untyped error leaves every check on deepdiff_rs's own stub
        # (the thing under test here) at full strictness.
        [sys.executable, "-m", "mypy", "--strict", "--disable-error-code=import-untyped", str(script)],
        cwd=Path(__file__).resolve().parent.parent,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stdout + result.stderr
