from typing import Any

__version__: str

class MaxDepthError(ValueError):
    """Raised when diffing two values would need to recurse past the configured ``max_depth``."""

MAX_DEPTH_CEILING: int

class DeepDiff:
    """A drop-in subset of ``deepdiff.DeepDiff``, diffing ``t1``/``t2`` at ``verbose_level=2``."""

    def __init__(
        self,
        t1: Any,
        t2: Any,
        ignore_order: bool = ...,
        max_depth: int | None = ...,
    ) -> None: ...
    def to_json(self) -> str:
        """The report as a ``DeepDiff``-compatible JSON string at ``verbose_level=2``; a deep report renders on the sized worker thread rather than inline."""

    def to_dict(self) -> dict[str, Any]:
        """The report as a native Python dict, Python types (tuples, sets, datetimes) intact."""

    def __bool__(self) -> bool: ...
    def __repr__(self) -> str: ...

def diff_json(
    a: str,
    b: str,
    ignore_order: bool = ...,
    max_depth: int | None = ...,
) -> str:
    """Diffs two JSON documents and returns a ``DeepDiff``-compatible JSON report string (``verbose_level=2`` shape)."""

def diff_tables(left: Any, right: Any, *, key: list[str], threads: int | None = None) -> TableDiff:
    """Diffs two Arrow tables."""

class TableDiff:
    """The result of ``diff_tables``: the schema diff and the row-level members (``rows_added``, ``rows_removed``, ``cells_changed``, ``duplicate_keys``)."""

    @property
    def schema(self) -> list[dict[str, Any]]:
        """The schema changes as a list of dicts, one per changed column, each with ``column``, ``change`` (``added``/``removed``/``type_changed``), ``left_type``, ``right_type``, ``left_nullable``, ``right_nullable``."""

    @property
    def schema_arrow(self) -> ArrowTable:
        """The schema diff as an Arrow-exportable table: it implements ``__arrow_c_stream__`` and offers ``ArrowTable.to_pyarrow``."""

    def summary(self) -> dict[str, int]:
        """Counts of each kind of change: the schema counts (``columns_added``, ``columns_removed``, ``columns_type_changed``), the row counts (``rows_added``, ``rows_removed``, ``rows_changed``, ``duplicate_keys``, ``null_keys``), and ``cells_changed`` (the total number of changed cells)."""

    def to_json(self) -> str:
        """The full diff as a JSON string: the schema diff, the summary, and ``rows_added``, ``rows_removed``, ``cells_changed``, and ``duplicate_keys`` (each an array of one JSON object per row, keyed by column name, with a null cell as JSON ``null``)."""

    def rows_added(self) -> ArrowTable:
        """Rows present only on the right (added), in the right table's schema and excluding duplicate keys."""

    def rows_removed(self) -> ArrowTable:
        """Rows present only on the left (removed), in the left table's schema and excluding duplicate keys."""

    def cells_changed(self) -> ArrowTable:
        """Per-cell changes for rows present on both sides with differing non-key values: the key columns, then ``column``, ``old_value``, ``new_value`` (canonical string renderings, null for a null cell), and ``change`` (``value_changed``, ``type_changed``, ``became_null``, or ``became_non_null``)."""

    def duplicate_keys(self) -> ArrowTable:
        """Keys appearing more than once on either side: the key columns, then ``left_count`` and ``right_count``."""

    def __repr__(self) -> str: ...

class ArrowTable:
    """An Arrow record batch exposed to Python through the Arrow ``PyCapsule`` interface."""

    def __arrow_c_stream__(self, requested_schema: object | None = ...) -> object:
        """Exports this table as an Arrow C stream (one record batch) in a ``PyCapsule``, the standard zero-copy hand-off pyarrow, polars, and pandas all understand."""

    def __arrow_c_schema__(self) -> object:
        """Exports the schema of this table as an Arrow C schema in a ``PyCapsule``."""

    def to_pyarrow(self) -> Any:
        """This table as a ``pyarrow.Table``."""

    def __len__(self) -> int: ...
    def __repr__(self) -> str: ...
