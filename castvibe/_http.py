"""Shared receiver HTTP client and cookie persistence."""

from __future__ import annotations

from http.cookiejar import LWPCookieJar
from pathlib import Path

import httpx

from castvibe._log import get_logger

log = get_logger("http")

_DEFAULT_DATA_DIR = Path.home() / ".castvibe"
_COOKIE_FILE_NAME = "receiver_cookies.lwp"


class ReceiverHTTPClient:
    """Owns the shared HTTP client used by the receiver and providers."""

    __slots__ = ("_client", "_cookie_jar", "_closed")

    def __init__(
        self,
        *,
        data_dir: Path | None = None,
        timeout_seconds: float = 15.0,
    ) -> None:
        root = data_dir or _DEFAULT_DATA_DIR
        root.mkdir(parents=True, exist_ok=True)

        # TODO: Split cookie storage per provider instead of sharing one global jar.
        cookie_path = root / _COOKIE_FILE_NAME
        self._cookie_jar = LWPCookieJar(filename=str(cookie_path))
        _load_cookie_jar(self._cookie_jar)

        self._client = httpx.AsyncClient(
            cookies=self._cookie_jar,
            timeout=timeout_seconds,
            follow_redirects=True,
        )
        self._closed = False

    @property
    def client(self) -> httpx.AsyncClient:
        """Return the shared async HTTP client."""
        return self._client

    async def close(self) -> None:
        """Persist cookies and close the shared client."""
        if self._closed:
            return
        _save_cookie_jar(self._cookie_jar)
        await self._client.aclose()
        self._closed = True


def _load_cookie_jar(cookie_jar: LWPCookieJar) -> None:
    if cookie_jar.filename is None:
        return

    path = Path(cookie_jar.filename)
    if not path.exists():
        return

    try:
        cookie_jar.load(ignore_discard=True, ignore_expires=True)
    except Exception:
        log.debug("failed to load HTTP cookies", exc_info=True)


def _save_cookie_jar(cookie_jar: LWPCookieJar) -> None:
    if cookie_jar.filename is None:
        return

    try:
        cookie_jar.save(ignore_discard=True, ignore_expires=True)
    except Exception:
        log.debug("failed to save HTTP cookies", exc_info=True)


__all__ = ["ReceiverHTTPClient"]
