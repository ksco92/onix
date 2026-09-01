"""Shared types for onix's M6 `perf/` scripts.

Plain importable module, not a standalone `uv run` script — a script's own
directory is always prepended to `sys.path`, so `generate_fixtures.py`,
`run_deepdiff.py`, and `summarize_results.py` can `from _common import
JsonValue` with no extra packaging. `scripts/gen_goldens.py` keeps its own
copy of this alias (predates this module; out of scope for M6).
"""

# A JSON-shaped value (no `typing.Any` per the python-coding-guide's ban).
type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None
