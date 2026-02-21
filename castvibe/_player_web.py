"""Embedded web-player assets."""

from __future__ import annotations

from functools import lru_cache
from importlib.resources import files

_WEB_PACKAGE = "castvibe._web"


@lru_cache(maxsize=1)
def player_web_page() -> str:
    """Return the default embedded player HTML page."""
    return files(_WEB_PACKAGE).joinpath("player.html").read_text(encoding="utf-8")


@lru_cache(maxsize=1)
def player_web_script() -> str:
    """Return the default embedded player JavaScript."""
    return files(_WEB_PACKAGE).joinpath("player.js").read_text(encoding="utf-8")


__all__ = ["player_web_page", "player_web_script"]
