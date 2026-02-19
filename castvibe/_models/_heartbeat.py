"""Heartbeat namespace messages (PING / PONG)."""

from typing import Annotated, Literal

from pydantic import Discriminator, TypeAdapter

from castvibe._models._base import CastModel


class Ping(CastModel):
    """Keepalive ping sent by the sender."""

    type: Literal["PING"] = "PING"


class Pong(CastModel):
    """Keepalive pong sent by the receiver."""

    type: Literal["PONG"] = "PONG"


HeartbeatMessage = Annotated[Ping | Pong, Discriminator("type")]
"""Discriminated union of all heartbeat messages."""

heartbeat_message_adapter: TypeAdapter[HeartbeatMessage] = TypeAdapter(
    HeartbeatMessage,
)
