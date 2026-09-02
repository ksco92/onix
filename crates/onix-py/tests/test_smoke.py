"""Smoke test: the compiled extension module imports and the drop-in class works."""

from deepdiff_rs import DeepDiff


def test_import_and_basic_diff() -> None:
    """The extension module imports and a trivial diff round-trips."""
    diff = DeepDiff({"a": 1}, {"a": 2})
    assert diff.to_dict() == {"values_changed": {"root['a']": {"new_value": 2, "old_value": 1}}}
