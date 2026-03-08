"""Shared HTTP helpers for provider API clients."""

from __future__ import annotations


def cast_default_headers(origin: str, referer: str) -> dict[str, str]:
    """Build default Cast-like browser headers for provider HTTP calls."""
    return {
        "Accept": "*/*",
        "Accept-Language": "en-US",
        "Origin": origin,
        "Referer": referer,
    }


__all__ = ["cast_default_headers"]
