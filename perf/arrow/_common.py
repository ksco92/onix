"""Shared constants for the `perf/arrow/` scripts.

Plain importable module (not a standalone script): `generate_fixtures.py`,
`oracle_duckdb.py`, and their tests all import from here so the column
layout and the sidecar's field names are defined exactly once.
"""

from typing import Final

# Base schema (the "left"/`a` side): the five columns the fixture always
# starts from, before any mutation.
KEY_COLUMN: Final[str] = "id"
BASE_COLUMNS: Final[tuple[str, ...]] = ("id", "ts", "category", "amount", "payload")

# The changed ("right"/`b`) side adds this column on top of `BASE_COLUMNS`.
ADDED_COLUMN: Final[str] = "note"

# Sidecar JSON field names, shared between the generator (which writes them)
# and the oracle/tests (which read them) so a rename can't silently drift
# one side out of sync with the other.
SIDECAR_ROWS_DELETED: Final[str] = "rows_deleted"
SIDECAR_ROWS_ADDED: Final[str] = "rows_added"
SIDECAR_ROWS_MODIFIED_AMOUNT: Final[str] = "rows_modified_amount"
SIDECAR_ROWS_MODIFIED_PAYLOAD: Final[str] = "rows_modified_payload"
SIDECAR_ROWS_UNCHANGED: Final[str] = "rows_unchanged"
SIDECAR_DUPLICATE_KEYS: Final[str] = "duplicate_keys"
SIDECAR_SCHEMA_CHANGES: Final[str] = "schema_changes"
SIDECAR_SEED: Final[str] = "seed"
SIDECAR_ROWS: Final[str] = "rows"
