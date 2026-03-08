"""Shared HTTP header filtering for playback proxy flows."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Mapping

HOP_BY_HOP_REQUEST_HEADERS = frozenset(
    {
        "connection",
        "content-length",
        "host",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    }
)

HOP_BY_HOP_RESPONSE_HEADERS = frozenset(
    {
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    }
)

MANIFEST_PROXY_BLOCKED_RESPONSE_HEADERS = frozenset(
    {
        *HOP_BY_HOP_RESPONSE_HEADERS,
        "content-encoding",
        "content-length",
        "content-type",
        "set-cookie",
    }
)


def filter_upstream_headers(headers: Mapping[str, str]) -> dict[str, str]:
    """Drop hop-by-hop request headers before forwarding upstream."""
    blocked = set(HOP_BY_HOP_REQUEST_HEADERS)
    blocked.update(connection_header_tokens(headers))

    filtered: dict[str, str] = {}
    for key, value in headers.items():
        if key.lower() in blocked:
            continue
        filtered[key] = value
    return filtered


def filter_upstream_response_headers(headers: Mapping[str, str]) -> dict[str, str]:
    """Drop blocked and hop-by-hop response headers."""
    blocked = set(MANIFEST_PROXY_BLOCKED_RESPONSE_HEADERS)
    blocked.update(connection_header_tokens(headers))

    filtered: dict[str, str] = {}
    for key, value in headers.items():
        if key.lower() in blocked:
            continue
        filtered[key] = value
    return filtered


def connection_header_tokens(headers: Mapping[str, str]) -> set[str]:
    """Extract token list from ``Connection`` headers."""
    tokens: set[str] = set()
    for key, value in headers.items():
        if key.lower() != "connection":
            continue
        for token in value.split(","):
            normalized = token.strip().lower()
            if normalized:
                tokens.add(normalized)
    return tokens


__all__ = [
    "HOP_BY_HOP_REQUEST_HEADERS",
    "HOP_BY_HOP_RESPONSE_HEADERS",
    "MANIFEST_PROXY_BLOCKED_RESPONSE_HEADERS",
    "connection_header_tokens",
    "filter_upstream_headers",
    "filter_upstream_response_headers",
]
