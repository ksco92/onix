"""Shared test-only helpers for this suite, imported by the test modules that need them."""

import pytest


def require_deepdiff() -> None:
    """Skip the importing module if real ``deepdiff`` (Python >= 3.10) is unavailable."""
    pytest.importorskip("deepdiff", reason="deepdiff requires Python >= 3.10")


def _normalize_types(value: object) -> object:
    """
    Replace any Python type object in a report with its name.

    Real DeepDiff's `to_dict()` reports a `type_changes` entry's `old_type`/
    `new_type` as the type objects themselves, where `deepdiff_rs` reports the
    names its `to_json()` uses (`"tuple"`, `"list"`, ...). That one difference
    is a documented gap of this MVP, so it is normalized away here rather than
    swamping every other comparison a test exists to make.

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
