"""One guard on the suite itself: no test name may be defined twice.

Python's later definition silently shadows the earlier one, so a duplicate
does not fail — it deletes a test, and pytest reports the same green count it
did before. That is exactly what a bad merge produces, and it hid a defective
test behind a fixed one here once already.
"""

import ast
from collections import Counter
from pathlib import Path

TESTS_ROOT = Path(__file__).resolve().parent


def _test_names(path: Path) -> list[str]:
    """
    List every module-level test function name in one test file.

    :param path: The test module to read.
    :return: The names, in file order, duplicates included.
    """
    return [
        node.name
        for node in ast.parse(path.read_text()).body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name.startswith("test_")
    ]


def test_no_test_name_is_defined_twice() -> None:
    """Every test in every module of this suite has a name of its own."""
    duplicates = {
        path.name: sorted(name for name, count in Counter(_test_names(path)).items() if count > 1)
        for path in sorted(TESTS_ROOT.glob("test_*.py"))
    }
    duplicates = {name: found for name, found in duplicates.items() if found}

    assert not duplicates, f"shadowed test definitions: {duplicates}"
