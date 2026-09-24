"""Shared test-only helpers for this suite, imported by the test modules that need them."""

import pytest


def require_deepdiff() -> None:
    """Skip the importing module if real ``deepdiff`` (Python >= 3.10) is unavailable."""
    pytest.importorskip("deepdiff", reason="deepdiff requires Python >= 3.10")


def _normalize_types(value: object) -> object:
    """Replace any Python type object in a report with its name: real DeepDiff
    reports `type_changes`' `old_type`/`new_type` as the type objects
    themselves, where `deepdiff_rs` reports their names."""
    if isinstance(value, dict):
        return {key: _normalize_types(item) for key, item in value.items()}

    if isinstance(value, list):
        return [_normalize_types(item) for item in value]

    if isinstance(value, tuple):
        return tuple(_normalize_types(item) for item in value)

    if isinstance(value, type):
        return value.__name__

    return value
