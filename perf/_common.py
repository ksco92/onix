"""Shared JSON value type for the `perf/` scripts."""

from typing import Union

# A JSON-shaped value.
JsonValue = Union[dict[str, "JsonValue"], list["JsonValue"], str, int, float, bool, None]
