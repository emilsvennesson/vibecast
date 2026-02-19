"""Tests verifying protobuf compilation and imports."""

from castvibe._proto.cast_channel_pb2 import (
    AuthChallenge,
    AuthResponse,
    CastMessage,
    DeviceAuthMessage,
)


def test_cast_message_import() -> None:
    """CastMessage can be imported and instantiated."""
    msg = CastMessage()
    assert msg is not None


def test_cast_message_fields() -> None:
    """CastMessage has the expected protobuf fields."""
    msg = CastMessage()
    msg.protocol_version = CastMessage.CASTV2_1_0
    msg.source_id = "sender-0"
    msg.destination_id = "receiver-0"
    msg.namespace = "urn:x-cast:com.google.cast.tp.heartbeat"
    msg.payload_type = CastMessage.STRING
    msg.payload_utf8 = '{"type": "PING"}'

    assert msg.source_id == "sender-0"
    assert msg.payload_utf8 == '{"type": "PING"}'


def test_device_auth_message_import() -> None:
    """DeviceAuthMessage and sub-messages can be imported and composed."""
    challenge = AuthChallenge()
    response = AuthResponse()
    auth_msg = DeviceAuthMessage(challenge=challenge, response=response)
    assert auth_msg.HasField("challenge")
    assert auth_msg.HasField("response")


def test_cast_message_serialization() -> None:
    """CastMessage can be serialized and deserialized via protobuf."""
    original = CastMessage()
    original.protocol_version = CastMessage.CASTV2_1_0
    original.source_id = "sender-0"
    original.destination_id = "receiver-0"
    original.namespace = "urn:x-cast:com.google.cast.tp.heartbeat"
    original.payload_type = CastMessage.STRING
    original.payload_utf8 = '{"type": "PONG"}'

    data = original.SerializeToString()
    restored = CastMessage()
    _ = restored.ParseFromString(data)

    assert restored.source_id == "sender-0"
    assert restored.payload_utf8 == '{"type": "PONG"}'
