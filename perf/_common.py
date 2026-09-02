"""Shared types for the `perf/` scripts.

Plain importable module, not a standalone `uv run` script — a script's own
directory is always prepended to `sys.path`, so `generate_fixtures.py`,
`run_deepdiff.py`, and `summarize_results.py` (all in this same `perf/`
directory) can `from _common import JsonValue` with no extra packaging. The
single-file scripts under `scripts/` and `crates/onix-py/benchmarks/` each
redefine this alias inline instead: Python prepends only the running
script's own directory to `sys.path` (`uv run` inherits this), so they
cannot reach this module.
"""

# A JSON-shaped value (no `typing.Any` per the python-coding-guide's ban).
type JsonValue = dict[str, "JsonValue"] | list["JsonValue"] | str | int | float | bool | None
