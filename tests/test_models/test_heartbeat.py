"""Tests for heartbeat namespace models."""

from castvibe._models import Ping, Pong, heartbeat_message_adapter


class TestPingPong:
    """Ping and Pong round-trip correctly."""

    def test_ping_round_trip(self) -> None:
        ping = Ping()
        data = ping.model_dump(exclude_none=True)
        assert data == {"type": "PING"}
        restored = Ping.model_validate(data)
        assert restored.type == "PING"

    def test_pong_round_trip(self) -> None:
        pong = Pong()
        data = pong.model_dump(exclude_none=True)
        assert data == {"type": "PONG"}
        restored = Pong.model_validate(data)
        assert restored.type == "PONG"


class TestHeartbeatDiscriminator:
    """heartbeat_message_adapter dispatches on the type field."""

    def test_ping_dispatch(self) -> None:
        msg = heartbeat_message_adapter.validate_python({"type": "PING"})
        assert isinstance(msg, Ping)

    def test_pong_dispatch(self) -> None:
        msg = heartbeat_message_adapter.validate_python({"type": "PONG"})
        assert isinstance(msg, Pong)
