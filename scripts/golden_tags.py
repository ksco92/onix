"""The tagged JSON encoding the golden corpus uses for Python values JSON cannot express.

A golden case's ``a.json``/``b.json`` are plain JSON files, but the values they stand
for are Python objects, and several of the types DeepDiff diffs (``tuple``, ``set``,
``frozenset``, ``datetime``, ``date``) have no JSON literal. This module defines the one
encoding that closes that gap, shared by every reader of the corpus:

- A JSON object with **exactly one** key, and that key one of :data:`RESERVED_TAGS`, is a
  tagged value and decodes to the corresponding Python object.
- **Any other** JSON object is plain data and decodes to a ``dict``, recursively.

So ``{"$tuple": [1, 2]}`` is the tuple ``(1, 2)``, while ``{"$tuple": [1], "x": 2}`` and
``{"other": 1}`` are ordinary dicts. The cost of the encoding is that a dict whose only
key is literally one of the reserved names cannot be written as a golden fixture;
:func:`encode_tags` refuses such a value rather than writing a file that would decode
back into something else.

This is corpus tooling only. onix's own parse paths (``onix_core::Value``'s
``Deserialize``, ``deepdiff_rs.diff_json``, the CLI) never interpret these names — a
tagged object is an ordinary dict to all of them, which the test suites pin down.

The Rust reader (``crates/onix-core/tests/golden.rs``) implements the identical rule
against the same fixtures.
"""

from typing import Final

TUPLE_TAG: Final[str] = "$tuple"

# Every tag name the encoding reserves. Only TUPLE_TAG is implemented; the rest are
# claimed here so a fixture can never use one as an ordinary dict key in the meantime,
# and so all three readers agree on the full set from the start.
RESERVED_TAGS: Final[frozenset[str]] = frozenset(
    {TUPLE_TAG, "$set", "$frozenset", "$datetime", "$date"}
)

# A JSON-shaped value, plus the Python types the tags decode to. Named instead of
# `typing.Any` per the python-coding-guide's ban on `Any`.
type TaggedValue = (
    dict[str, "TaggedValue"]
    | list["TaggedValue"]
    | tuple["TaggedValue", ...]
    | str
    | int
    | float
    | bool
    | None
)


def _sole_tag(value: dict[str, TaggedValue]) -> str | None:
    """
    Return the reserved tag `value` is an encoding of, or ``None`` if it is plain data.

    :param value: A decoded JSON object.
    :return: The single reserved key, or ``None``.
    """
    if len(value) != 1:
        return None

    key = next(iter(value))

    return key if key in RESERVED_TAGS else None


def encode_tags(value: TaggedValue) -> TaggedValue:
    """
    Encode a Python value into its JSON-writable tagged form.

    :param value: The value to encode; tuples become tagged objects, everything else is
        rebuilt unchanged.
    :raises ValueError: If a plain dict would encode to something a decoder would read
        back as a tagged value (its only key is a reserved name).
    :return: A value containing only JSON-expressible types.
    """
    if isinstance(value, tuple):
        return {TUPLE_TAG: [encode_tags(item) for item in value]}

    if isinstance(value, list):
        return [encode_tags(item) for item in value]

    if isinstance(value, dict):
        if _sole_tag(value) is not None:
            raise ValueError(
                f"cannot encode a dict whose only key is the reserved tag {next(iter(value))!r}: "
                "it would decode back as a tagged value, not as a dict"
            )

        return {key: encode_tags(item) for key, item in value.items()}

    return value


def decode_tags(value: TaggedValue) -> TaggedValue:
    """
    Decode a parsed JSON value, turning tagged objects into their Python counterparts.

    :param value: A value parsed from a golden fixture file.
    :raises NotImplementedError: If the value carries a reserved tag no decoder supports
        yet (the corpus must not use one before its slice lands).
    :return: The Python value the fixture stands for.
    """
    if isinstance(value, list):
        return [decode_tags(item) for item in value]

    if isinstance(value, dict):
        tag = _sole_tag(value)

        if tag == TUPLE_TAG:
            return tuple(decode_tags(item) for item in value[tag])

        if tag is not None:
            raise NotImplementedError(f"the {tag!r} tag is reserved but not decodable yet")

        return {key: decode_tags(item) for key, item in value.items()}

    return value
