from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any


PLAYBACK_MESSAGE_TYPES = frozenset({"load", "play", "pause", "seek", "stop", "volume"})
SETTINGS_SNAPSHOT = "settingsSnapshot"
SETTINGS_UPDATE_RESULT = "settingsUpdateResult"


class ProtocolError(ValueError):
    pass


@dataclass(slots=True, frozen=True)
class ServerMessage:
    message_type: str
    payload: dict[str, Any]


def decode_server_message(message: str) -> ServerMessage:
    """Decode the current server envelope in one migration-friendly place."""
    try:
        payload = json.loads(message)
    except (TypeError, json.JSONDecodeError) as exc:
        raise ProtocolError("message is not valid JSON") from exc

    if not isinstance(payload, dict):
        raise ProtocolError("message must be an object")

    message_type = payload.get("type")
    if not isinstance(message_type, str) or not message_type:
        raise ProtocolError("message type must be a non-empty string")

    return ServerMessage(message_type=message_type, payload=payload)


def registration_message(
    player_id: str,
    name: str,
    capabilities: dict[str, Any],
) -> dict[str, Any]:
    return {
        "type": "register",
        "player": {
            "playerId": player_id,
            "name": name,
            "capabilities": capabilities,
        },
    }


def settings_update_message(
    request_id: str,
    app_key: str,
    expected_revision: int,
    changes: dict[str, Any],
) -> dict[str, Any]:
    if not request_id or not app_key:
        raise ProtocolError("request and app identifiers must not be empty")
    if (
        isinstance(expected_revision, bool)
        or not isinstance(expected_revision, int)
        or expected_revision < 0
    ):
        raise ProtocolError("expected revision must be a non-negative integer")
    if not changes or any(not isinstance(key, str) or not key for key in changes):
        raise ProtocolError("changes must contain non-empty setting keys")

    return {
        "type": "settingsUpdate",
        "requestId": request_id,
        "appKey": app_key,
        "expectedRevision": expected_revision,
        "changes": dict(changes),
    }
