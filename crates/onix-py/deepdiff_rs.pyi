"""Type stub for the compiled ``deepdiff_rs`` extension module.

Packaged into the wheel by maturin (which finds this file next to
``Cargo.toml`` because ``deepdiff_rs`` is a pure-extension module) alongside
an auto-generated ``py.typed`` marker — see ``crates/onix-py/tests/`` for the
test that checks both ship and that every signature here still matches the
built module's real ``inspect.signature()``.
"""

from typing import Any

class MaxDepthError(ValueError):
    """Raised when diffing (or importing an Arrow schema) would need to
    recurse past the configured ``max_depth`` — a catchable Python
    exception in place of a native stack overflow. A ``ValueError``
    subclass, so callers that only catch ``ValueError`` still catch this.
    """

MAX_DEPTH_CEILING: int
"""The largest ``max_depth`` a caller may pass to :class:`DeepDiff` or
:func:`diff_json`; a larger value raises ``ValueError`` up front.
"""

class DeepDiff:
    """A drop-in subset of ``deepdiff.DeepDiff``.

    Example::

        from deepdiff_rs import DeepDiff

        diff = DeepDiff({"a": 1}, {"a": 2})
        if diff:
            print(diff.to_json())
    """

    def __init__(
        self,
        t1: Any,
        t2: Any,
        ignore_order: bool = ...,
        max_depth: int | None = ...,
    ) -> None:
        """
        :param t1: The left value to compare. Any of ``None``, ``bool``,
            ``int``, ``float``, ``str``, ``dict`` (``str`` keys), ``list``,
            ``tuple``, ``set``, ``frozenset``, ``datetime.datetime``,
            ``datetime.date``, ``datetime.time``, or ``datetime.timedelta``,
            arbitrarily nested.
        :param t2: The right value to compare, of the same supported types.
        :param ignore_order: Mirrors ``DeepDiff(..., ignore_order=True)``.
        :param max_depth: Recursion-depth bound; defaults to 512. Raises
            ``ValueError`` up front if it exceeds :data:`MAX_DEPTH_CEILING`,
            and :class:`MaxDepthError` if diffing recurses past it.
        """

    def to_json(self) -> str:
        """The report as a ``DeepDiff``-compatible JSON string
        (``verbose_level=2`` shape)."""

    def to_dict(self) -> dict[str, Any]:
        """The report as a native ``dict``, with Python types (tuples,
        sets, datetimes) preserved rather than rendered to JSON."""

    def __bool__(self) -> bool: ...
    def __repr__(self) -> str: ...

def diff_json(
    a: str,
    b: str,
    ignore_order: bool = ...,
    max_depth: int | None = ...,
) -> str:
    """Diffs two JSON documents and returns a ``DeepDiff``-compatible JSON
    report string, entirely in Rust with no Python-object traversal.

    Example::

        from deepdiff_rs import diff_json

        print(diff_json('{"a": 1}', '{"a": 2}'))

    :param a: The left document, as a JSON string.
    :param b: The right document, as a JSON string.
    :param ignore_order: Mirrors ``DeepDiff(..., ignore_order=True)``.
    :param max_depth: Recursion-depth bound; defaults to 512. Raises
        ``ValueError`` up front if it exceeds :data:`MAX_DEPTH_CEILING`, and
        :class:`MaxDepthError` if diffing recurses past it.
    :raises ValueError: If ``a`` or ``b`` fails to parse as JSON.
    """

def diff_tables(left: Any, right: Any, *, key: list[str]) -> TableDiff:
    """Diffs two Arrow tables — schema and keyed rows.

    ``left`` and ``right`` are any object implementing the Arrow PyCapsule
    interface: a pyarrow ``Table``/``RecordBatch``, a polars ``DataFrame``,
    or a DuckDB relation.

    Example::

        import polars as pl
        from deepdiff_rs import diff_tables

        left = pl.DataFrame({"id": [1, 2], "v": [10, 20]})
        right = pl.DataFrame({"id": [2, 3], "v": [20, 30]})
        diff = diff_tables(left, right, key=["id"])
        print(diff.summary())

    :param left: The left table.
    :param right: The right table.
    :param key: The primary-key column names, required and non-empty; every
        key column must exist, with the same type, on both sides.
    :raises TypeError: If an input implements neither Arrow PyCapsule method,
        or ``key`` is a bare string rather than a list of column names.
    :raises ValueError: If a key column is missing, duplicated, or
        type-mismatched, or a column's type cannot be compared by value —
        the message names the column.
    :raises MaxDepthError: If a column's Arrow type is nested past the
        supported depth.
    """

class TableDiff:
    """The result of :func:`diff_tables`: the schema diff and the keyed row
    diff. Not constructible directly; returned by :func:`diff_tables`.
    """

    @property
    def schema(self) -> list[dict[str, Any]]:
        """The schema changes: one dict per changed column, with ``column``,
        ``change`` (``added``/``removed``/``type_changed``), ``left_type``,
        ``right_type``, ``left_nullable``, ``right_nullable``."""

    @property
    def schema_arrow(self) -> ArrowTable:
        """The schema diff as an Arrow-exportable table."""

    def summary(self) -> dict[str, int]:
        """Counts of each kind of change: ``columns_added``,
        ``columns_removed``, ``columns_type_changed``, ``rows_added``,
        ``rows_removed``, ``rows_changed``, ``duplicate_keys``,
        ``null_keys``, ``cells_changed``."""

    def to_json(self) -> str:
        """The full diff — schema, summary, and every row-level member — as
        a JSON string.

        :raises ValueError: If the row-level members would together embed
            more rows than the documented cap (see the Diffing tables
            section of the README); use the row-level accessors below, or
            :meth:`ArrowTable.to_pyarrow`/``__arrow_c_stream__``, instead.
        """

    def rows_added(self) -> ArrowTable:
        """Rows present only on the right, excluding duplicate keys."""

    def rows_removed(self) -> ArrowTable:
        """Rows present only on the left, excluding duplicate keys."""

    def cells_changed(self) -> ArrowTable:
        """Per-cell changes for rows present on both sides with differing
        non-key values: the key columns, then ``column``, ``old_value``,
        ``new_value``, and ``change``."""

    def duplicate_keys(self) -> ArrowTable:
        """Keys appearing more than once on either side: the key columns,
        then ``left_count`` and ``right_count``."""

    def __repr__(self) -> str: ...

class ArrowTable:
    """An Arrow record batch exposed through the Arrow PyCapsule interface.
    Every table-shaped result of :func:`diff_tables` is one of these. Not
    constructible directly.
    """

    def __arrow_c_stream__(self, requested_schema: object | None = ...) -> object:
        """Exports this table as an Arrow C stream capsule — the protocol
        pyarrow, polars, and pandas all use to import it. Polars needs no
        pyarrow dependency of its own to do so; pandas' own consumption of
        this protocol (``pd.api.interchange.from_dataframe``) requires
        pyarrow to be installed regardless, a pandas-side requirement, not
        one of this method's.

        :param requested_schema: An optional requested-schema capsule, as
            the Arrow PyCapsule interface defines it.
        """

    def __arrow_c_schema__(self) -> object:
        """Exports this table's schema as an Arrow C schema capsule."""

    def to_pyarrow(self) -> Any:
        """This table as a ``pyarrow.Table``.

        :raises ImportError: If pyarrow is not installed; names the
            ``deepdiff-rs[arrow]`` extra. Not needed to consume this table
            with polars — use ``__arrow_c_stream__`` (which polars calls for
            you) instead; pandas needs pyarrow either way.
        """

    def __len__(self) -> int:
        """The number of rows in this table."""

    def __repr__(self) -> str: ...
