"""Connection namespace messages (CONNECT / CLOSE)."""

from typing import Annotated, Any, Literal

from pydantic import Discriminator, TypeAdapter

from castvibe._models._base import CastModel


class SenderInfo(CastModel):
    """Metadata about the connecting sender (embedded in CONNECT)."""

    sdk_type: int | None = None
    version: str | None = None
    browser_version: str | None = None
    platform: int | None = None
    connection_type: int | None = None
    model: str | None = None
    system_version: str | None = None


class ConnectRequest(CastModel):
    """Virtual connection request from a sender.

    The ``conn_type`` field is present in some sender implementations
    (observed as a numeric value in go-cast).  We accept it but don't
    require it.
    """

    type: Literal["CONNECT"] = "CONNECT"
    origin: dict[str, Any] = {}
    user_agent: str | None = None
    sender_info: SenderInfo | None = None
    conn_type: int | None = None


class CloseRequest(CastModel):
    """Virtual connection close from a sender."""

    type: Literal["CLOSE"] = "CLOSE"
    reason_code: int | None = None


ConnectionMessage = Annotated[
    ConnectRequest | CloseRequest,
    Discriminator("type"),
]
"""Discriminated union of all connection namespace messages."""

connection_message_adapter: TypeAdapter[ConnectionMessage] = TypeAdapter(
    ConnectionMessage,
)
