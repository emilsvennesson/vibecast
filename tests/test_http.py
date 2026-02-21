"""Tests for the shared receiver HTTP client and cookie persistence."""

from __future__ import annotations

from http.cookiejar import Cookie
from typing import TYPE_CHECKING, Any

from castvibe._http import ReceiverHTTPClient

if TYPE_CHECKING:
    from pathlib import Path


def _make_cookie(name: str, value: str, domain: str = ".example.com") -> Cookie:
    """Build a minimal ``Cookie`` for testing persistence."""
    return Cookie(
        version=0,
        name=name,
        value=value,
        port=None,
        port_specified=False,
        domain=domain,
        domain_specified=True,
        domain_initial_dot=domain.startswith("."),
        path="/",
        path_specified=True,
        secure=False,
        expires=None,
        discard=True,
        comment=None,
        comment_url=None,
        rest={},
    )


class TestDataDirectory:
    def test_creates_data_directory(self, tmp_path: Path) -> None:
        data_dir = tmp_path / "nested" / "data"
        assert not data_dir.exists()

        _ = ReceiverHTTPClient(data_dir=data_dir)

        assert data_dir.is_dir()


class TestCookiePersistence:
    async def test_round_trip(self, tmp_path: Path) -> None:
        """Cookies saved on close are loaded by a new instance."""
        http = ReceiverHTTPClient(data_dir=tmp_path)
        http._cookie_jar.set_cookie(_make_cookie("session", "abc123"))  # noqa: SLF001
        await http.close()

        cookie_file = tmp_path / "receiver_cookies.lwp"
        assert cookie_file.exists()

        http2 = ReceiverHTTPClient(data_dir=tmp_path)
        names = [c.name for c in http2._cookie_jar]  # noqa: SLF001
        assert "session" in names
        await http2.close()

    async def test_close_is_idempotent(self, tmp_path: Path) -> None:
        http = ReceiverHTTPClient(data_dir=tmp_path)
        await http.close()
        await http.close()  # should not raise

    async def test_corrupt_cookie_file(self, tmp_path: Path) -> None:
        """A corrupted cookie file should not prevent construction."""
        cookie_path = tmp_path / "receiver_cookies.lwp"
        _ = cookie_path.write_text("<<<not valid lwp cookies>>>")

        http = ReceiverHTTPClient(data_dir=tmp_path)
        assert not http.client.is_closed
        await http.close()


class TestClientLifecycle:
    def test_client_not_closed_after_init(self, tmp_path: Path) -> None:
        http = ReceiverHTTPClient(data_dir=tmp_path)
        assert not http.client.is_closed

    async def test_client_closed_after_close(self, tmp_path: Any) -> None:
        http = ReceiverHTTPClient(data_dir=tmp_path)
        await http.close()
        assert http.client.is_closed
