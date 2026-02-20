"""Shared utility helpers for Cast message handling."""

from __future__ import annotations

import json
from typing import Any, cast

from castvibe._log import get_logger
from castvibe._proto.cast_channel_pb2 import CastMessage

log = get_logger("util")


def parse_json_payload(msg: CastMessage) -> dict[str, Any] | None:
    """Extract a JSON dict from a STRING-type Cast message, or *None*."""
    if msg.payload_type != CastMessage.STRING:
        return None

    try:
        parsed = json.loads(msg.payload_utf8)
    except json.JSONDecodeError:
        log.warning("invalid JSON payload", exc_info=True)
        return None

    if not isinstance(parsed, dict):
        return None
    return cast("dict[str, Any]", parsed)


def extract_request_id(
    payload: dict[str, Any],
    keys: tuple[str, ...] = ("requestId",),
) -> int:
    """Return the integer request ID from *payload*, or ``0``."""
    for key in keys:
        raw = payload.get(key)
        if isinstance(raw, int):
            return raw
    return 0
