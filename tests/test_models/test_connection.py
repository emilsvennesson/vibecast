"""Tests for connection namespace models."""

from typing import Any

from castvibe._models import (
    CloseRequest,
    ConnectRequest,
    SenderInfo,
    connection_message_adapter,
)


class TestConnectRequest:
    """ConnectRequest parsing and serialization."""

    def test_basic_connect(self) -> None:
        raw: dict[str, Any] = {"type": "CONNECT", "origin": {}}
        msg = ConnectRequest.model_validate(raw)
        assert msg.type == "CONNECT"
        assert msg.origin == {}

    def test_connect_with_sender_info(self) -> None:
        raw = {
            "type": "CONNECT",
            "origin": {},
            "userAgent": "iOS/17.0",
            "senderInfo": {
                "sdkType": 2,
                "version": "1.56.0",
                "browserVersion": "70",
                "platform": 4,
                "connectionType": 1,
                "model": "iPhone15,4",
                "systemVersion": "17.0",
            },
        }
        msg = ConnectRequest.model_validate(raw)
        assert msg.user_agent == "iOS/17.0"
        assert isinstance(msg.sender_info, SenderInfo)
        assert msg.sender_info.sdk_type == 2
        assert msg.sender_info.model == "iPhone15,4"
        assert msg.sender_info.platform == 4

    def test_connect_with_conn_type(self) -> None:
        """go-cast sends connType as a numeric value."""
        raw: dict[str, Any] = {"type": "CONNECT", "origin": {}, "connType": 0}
        msg = ConnectRequest.model_validate(raw)
        assert msg.conn_type == 0

    def test_lenient_parsing_extra_fields(self) -> None:
        """Unknown fields from senders are preserved, not rejected."""
        raw: dict[str, Any] = {
            "type": "CONNECT",
            "origin": {},
            "userAgent": "test",
            "someFutureField": "value",
            "anotherOne": 42,
        }
        msg = ConnectRequest.model_validate(raw)
        assert msg.user_agent == "test"
        data = msg.model_dump()
        assert data["someFutureField"] == "value"
        assert data["anotherOne"] == 42

    def test_round_trip(self) -> None:
        original = ConnectRequest(
            origin={},
            user_agent="Chrome/120",
            sender_info=SenderInfo(sdk_type=2, version="1.0"),
        )
        data = original.model_dump(exclude_none=True)
        restored = ConnectRequest.model_validate(data)
        assert restored.user_agent == "Chrome/120"
        assert restored.sender_info is not None
        assert restored.sender_info.sdk_type == 2


class TestCloseRequest:
    """CloseRequest parsing."""

    def test_basic_close(self) -> None:
        raw = {"type": "CLOSE"}
        msg = CloseRequest.model_validate(raw)
        assert msg.type == "CLOSE"

    def test_close_with_reason_code(self) -> None:
        raw = {"type": "CLOSE", "reasonCode": 5}
        msg = CloseRequest.model_validate(raw)
        assert msg.reason_code == 5


class TestConnectionDiscriminator:
    """connection_message_adapter dispatches on type."""

    def test_connect_dispatch(self) -> None:
        msg = connection_message_adapter.validate_python(
            {"type": "CONNECT", "origin": {}}
        )
        assert isinstance(msg, ConnectRequest)

    def test_close_dispatch(self) -> None:
        msg = connection_message_adapter.validate_python({"type": "CLOSE"})
        assert isinstance(msg, CloseRequest)
