"""Base model for all Cast protocol JSON messages."""

from pydantic import BaseModel, ConfigDict
from pydantic.alias_generators import to_camel


class CastModel(BaseModel):
    """Base for all Cast protocol JSON message models.

    Features:
    - ``alias_generator=to_camel``: Python ``snake_case`` fields are
      serialized/deserialized as ``camelCase`` on the wire.
    - ``populate_by_name=True``: models can be constructed with either
      the Python name or the camelCase alias.
    - ``extra="allow"``: unknown fields from senders are preserved rather
      than rejected, keeping the receiver lenient.
    - ``serialize_by_alias=True``: ``model_dump()`` produces camelCase
      keys by default, matching the Cast wire format.
    """

    model_config = ConfigDict(
        extra="allow",
        populate_by_name=True,
        alias_generator=to_camel,
        serialize_by_alias=True,
    )
