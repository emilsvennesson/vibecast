"""Tests for the Viaplay async HTTP API client."""

from __future__ import annotations

import re
from typing import TYPE_CHECKING, Any, cast
from unittest.mock import patch

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

import httpx
import pytest

from castvibe._models import StreamType
from castvibe.providers.viaplay._api import (
    DeviceAuthInfo,
    SessionCheckResult,
    ViaplayAPI,
)

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


class _NoopClient:
    async def get(self, url: str, *args: Any, **kwargs: Any) -> httpx.Response:
        _ = args
        _ = kwargs
        msg = f"unexpected HTTP request: {url}"
        raise AssertionError(msg)


class aioresponses:  # noqa: N801
    """Tiny compatibility shim for existing aioresponses-style tests."""

    def __init__(self) -> None:
        self._routes: list[
            tuple[re.Pattern[str] | str, dict[str, Any] | None, bytes | None, int]
        ] = []
        self._patcher: Any = None

    def get(
        self,
        url: re.Pattern[str] | str,
        *,
        payload: dict[str, Any] | None = None,
        body: bytes | None = None,
        status: int = 200,
    ) -> None:
        self._routes.append((url, payload, body, status))

    def __enter__(self) -> aioresponses:
        routes = self._routes

        async def _mock_get(
            _client: httpx.AsyncClient,
            url: str,
            *args: Any,
            **kwargs: Any,
        ) -> httpx.Response:
            _ = args
            _ = kwargs
            url_str = str(url)
            for pattern, payload, body, status in routes:
                if _matches_url(pattern, url_str):
                    request = httpx.Request("GET", url_str)
                    if payload is not None:
                        return httpx.Response(status, json=payload, request=request)
                    if body is not None:
                        return httpx.Response(status, content=body, request=request)
                    return httpx.Response(status, json={}, request=request)
            msg = f"unexpected mocked GET request: {url_str}"
            raise AssertionError(msg)

        self._patcher = patch("httpx.AsyncClient.get", new=_mock_get)
        self._patcher.start()
        return self

    def __exit__(self, *args: object) -> None:
        _ = args
        if self._patcher is not None:
            self._patcher.stop()


def _matches_url(pattern: re.Pattern[str] | str, url: str) -> bool:
    if isinstance(pattern, str):
        return pattern == url
    return pattern.match(url) is not None


@pytest.fixture
async def api() -> AsyncIterator[ViaplayAPI]:
    """Create a ViaplayAPI instance using a real httpx client."""
    client = httpx.AsyncClient()
    a = ViaplayAPI(client=client, device_id="receiver-device-id")
    a.set_setup_info(
        content_root="https://content.viaplay.se/stotta",
        country_code="se",
        user_id="user-1",
        profile_id="prof-1",
    )
    yield a
    await client.aclose()


# ---------------------------------------------------------------------------
# Receiver identity tests
# ---------------------------------------------------------------------------


class TestReceiverIdentity:
    async def test_uses_receiver_device_id(self) -> None:
        a = ViaplayAPI(client=cast("Any", _NoopClient()), device_id="receiver-123")
        assert a._device_id == "receiver-123"  # noqa: SLF001


class TestDeviceKey:
    async def test_device_key_format(self, api: ViaplayAPI) -> None:
        assert api.device_key == "chromecastgoogletv4k-se"

    async def test_device_key_changes_with_country(self) -> None:
        a = ViaplayAPI(client=cast("Any", _NoopClient()), device_id="receiver-123")
        a.set_setup_info("https://x", "no", "u", "p")
        assert a.device_key == "chromecastgoogletv4k-no"


# ---------------------------------------------------------------------------
# URI template expansion
# ---------------------------------------------------------------------------


class TestTemplateExpansion:
    async def test_expand_includes_device_vars(self, api: ViaplayAPI) -> None:
        result = api._expand("https://example.com/{deviceKey}/{deviceType}")  # noqa: SLF001
        assert "chromecastgoogletv4k-se" in result
        assert "chromecast" in result

    async def test_expand_with_extra_vars(self, api: ViaplayAPI) -> None:
        result = api._expand(  # noqa: SLF001
            "https://example.com{?accessToken}",
            {"accessToken": "tok123"},
        )
        assert "accessToken=tok123" in result

    async def test_expand_includes_user_agent_var(self, api: ViaplayAPI) -> None:
        result = api._expand(  # noqa: SLF001
            "https://example.com{?userAgent}",
        )
        assert "userAgent=" in result


# ---------------------------------------------------------------------------
# check_session
# ---------------------------------------------------------------------------


class TestCheckSession:
    async def test_returns_user_on_200(self, api: ViaplayAPI) -> None:
        body = {
            "user": {
                "userId": "user-1",
                "firstName": "Test",
                "lastName": "User",
            },
            "_links": {
                "viaplay:persistentLogin": {"href": "https://login.viaplay.com/pl"},
                "viaplay:tokenLogin": {"href": "https://login.viaplay.com/tl"},
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r"https://content\.viaplay\.se/.*"), payload=body)
            result = await api.check_session()

        assert result.user is not None
        assert result.user.user_id == "user-1"
        assert result.user.first_name == "Test"
        assert result.persistent_login_url == "https://login.viaplay.com/pl"

    async def test_raises_without_content_root(self) -> None:
        a = ViaplayAPI(client=cast("Any", _NoopClient()), device_id="receiver-123")
        with pytest.raises(RuntimeError, match="content root not set"):
            _ = await a.check_session()

    async def test_non_200_returns_result(self, api: ViaplayAPI) -> None:
        with aioresponses() as m:
            m.get(
                re.compile(r"https://content\.viaplay\.se/.*"), payload={}, status=401
            )
            result = await api.check_session()

        assert result.user is None


# ---------------------------------------------------------------------------
# persistent_login / token_login
# ---------------------------------------------------------------------------


class TestPersistentLogin:
    async def test_returns_true_on_200(self, api: ViaplayAPI) -> None:
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload={"ok": True})
            ok = await api.persistent_login("https://login.viaplay.com/pl{?deviceKey}")

        assert ok is True

    async def test_returns_false_on_401(self, api: ViaplayAPI) -> None:
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload={}, status=401)
            ok = await api.persistent_login("https://login.viaplay.com/pl{?deviceKey}")

        assert ok is False


class TestTokenLogin:
    async def test_returns_true_on_200(self, api: ViaplayAPI) -> None:
        with aioresponses() as m:
            m.get(re.compile(r".*"), body=b"OK")
            ok = await api.token_login(
                "https://login.viaplay.com/tl{?accessToken,deviceKey}",
                "my-token",
            )

        assert ok is True

    async def test_returns_false_on_failure(self, api: ViaplayAPI) -> None:
        with aioresponses() as m:
            m.get(re.compile(r".*"), body=b"Unauthorized", status=401)
            ok = await api.token_login(
                "https://login.viaplay.com/tl{?accessToken}",
                "bad-token",
            )

        assert ok is False


# ---------------------------------------------------------------------------
# get_device_authorization
# ---------------------------------------------------------------------------


class TestGetDeviceAuthorization:
    async def test_returns_device_auth_info(self, api: ViaplayAPI) -> None:
        root_result = SessionCheckResult(
            device_auth_url="https://login.viaplay.com/api/device/code{?deviceKey,deviceId}",
        )
        auth_body = {
            "userCode": "ABCD1234",
            "deviceToken": "dt-999",
            "_links": {
                "viaplay:activate": {"href": "https://viaplay.com/activate"},
                "viaplay:authorized": {"href": "https://login.viaplay.com/authorized"},
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r"https://login\.viaplay\.com/.*"), payload=auth_body)
            info = await api.get_device_authorization(root_result)

        assert info.user_code == "ABCD1234"
        assert info.device_token == "dt-999"
        assert info.activate_url == "https://viaplay.com/activate"
        assert info.authorized_url == "https://login.viaplay.com/authorized"

    async def test_expands_templated_activate_url(self, api: ViaplayAPI) -> None:
        root_result = SessionCheckResult(
            device_auth_url="https://login.viaplay.com/api/device/code{?deviceKey,deviceId}",
        )
        auth_body = {
            "userCode": "ABCD1234",
            "deviceToken": "dt-999",
            "verificationUrl": "https://viaplay.com/activate",
            "_links": {
                "viaplay:activate": {
                    "href": "https://login.viaplay.com/api/device/activate{?deviceKey,userCode}"
                },
                "viaplay:authorized": {
                    "href": "https://login.viaplay.com/api/device/authorized{?deviceId,deviceToken,userCode}"
                },
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r"https://login\.viaplay\.com/.*"), payload=auth_body)
            info = await api.get_device_authorization(root_result)

        assert info.activate_url == (
            "https://login.viaplay.com/api/device/activate"
            "?deviceKey=chromecastgoogletv4k-se&userCode=ABCD1234"
        )

    async def test_raises_on_missing_user_code(self, api: ViaplayAPI) -> None:
        root_result = SessionCheckResult(
            device_auth_url="https://login.viaplay.com/code",
        )
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload={"deviceToken": "x"})
            with pytest.raises(RuntimeError, match="no userCode"):
                _ = await api.get_device_authorization(root_result)

    async def test_raises_on_non_200(self, api: ViaplayAPI) -> None:
        root_result = SessionCheckResult(
            device_auth_url="https://login.viaplay.com/code",
        )
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload={}, status=500)
            with pytest.raises(RuntimeError, match="status 500"):
                _ = await api.get_device_authorization(root_result)


# ---------------------------------------------------------------------------
# poll_authorized
# ---------------------------------------------------------------------------


class TestPollAuthorized:
    async def test_returns_true_when_activated(self, api: ViaplayAPI) -> None:
        auth_info = DeviceAuthInfo(
            user_code="CODE",
            device_token="dt",
            activate_url="",
            authorized_url="https://login.viaplay.com/authorized{?deviceToken,userCode}",
        )
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload={})
            result = await api.poll_authorized(auth_info)

        assert result is True

    async def test_returns_false_on_403(self, api: ViaplayAPI) -> None:
        auth_info = DeviceAuthInfo(
            user_code="CODE",
            device_token="dt",
            activate_url="",
            authorized_url="https://login.viaplay.com/authorized",
        )
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload={}, status=403)
            result = await api.poll_authorized(auth_info)

        assert result is False

    async def test_returns_false_when_no_authorized_url(self, api: ViaplayAPI) -> None:
        auth_info = DeviceAuthInfo(
            user_code="CODE",
            device_token="dt",
            activate_url="",
            authorized_url="",
        )
        result = await api.poll_authorized(auth_info)
        assert result is False


# ---------------------------------------------------------------------------
# fetch_stream — all 5 resolution paths
# ---------------------------------------------------------------------------


class TestFetchStream:
    async def test_path1_embedded_media(self, api: ViaplayAPI) -> None:
        body = {
            "_embedded": {
                "viaplay:media": {
                    "contentUrl": "https://cdn.example.com/manifest.mpd",
                    "contentType": "application/dash+xml",
                },
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://content.viaplay.se/play/1234")

        assert info.url == "https://cdn.example.com/manifest.mpd"
        assert info.content_type == "application/dash+xml"

    async def test_path2_top_level_content_url(self, api: ViaplayAPI) -> None:
        body = {
            "contentUrl": "https://cdn.example.com/video.mp4",
            "contentType": "video/mp4",
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://content.viaplay.se/play/5678")

        assert info.url == "https://cdn.example.com/video.mp4"
        assert info.content_type == "video/mp4"

    async def test_path3_encrypted_playlist_hls(self, api: ViaplayAPI) -> None:
        body = {
            "streamingFormat": "HLS",
            "_links": {
                "viaplay:encryptedPlaylist": {
                    "href": "https://cdn.example.com/master.m3u8",
                },
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://x")

        assert info.url == "https://cdn.example.com/master.m3u8"
        assert info.content_type == "application/x-mpegURL"

    async def test_path3_encrypted_playlist_dash(self, api: ViaplayAPI) -> None:
        body = {
            "streamingFormat": "DASH",
            "_links": {
                "viaplay:encryptedPlaylist": {
                    "href": "https://cdn.example.com/manifest.mpd",
                },
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://x")

        assert info.content_type == "application/dash+xml"

    async def test_path4_playlist_link(self, api: ViaplayAPI) -> None:
        body = {
            "_links": {
                "viaplay:playlist": {
                    "href": "https://cdn.example.com/playlist.mpd",
                },
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://x")

        assert info.url == "https://cdn.example.com/playlist.mpd"
        assert info.content_type == "application/dash+xml"

    async def test_path5_stream_link(self, api: ViaplayAPI) -> None:
        body = {
            "_links": {
                "viaplay:stream": {
                    "href": "https://cdn.example.com/stream",
                },
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://x")

        assert info.url == "https://cdn.example.com/stream"
        assert info.content_type == ""

    async def test_extracts_drm_license_url(self, api: ViaplayAPI) -> None:
        body = {
            "_links": {
                "viaplay:encryptedPlaylist": {
                    "href": "https://cdn.example.com/manifest.mpd",
                    "streamingFormat": "Dash",
                },
                "viaplay:widevineLicense": {
                    "href": "https://drm.example.com/license",
                    "releasePid": "abc123",
                },
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://x")

        assert info.drm_license_url == "https://drm.example.com/license"

    async def test_extracts_title_from_product(self, api: ViaplayAPI) -> None:
        body = {
            "product": {
                "content": {"title": "My Show", "type": "episode"},
                "streamType": "vod",
            },
            "contentUrl": "https://cdn.example.com/video.mpd",
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://x")

        assert info.title == "My Show"

    async def test_extracts_stream_type_and_duration_from_product(
        self, api: ViaplayAPI
    ) -> None:
        body = {
            "duration": 2535.6,
            "product": {
                "streamType": "VOD",
                "content": {"title": "Episode"},
            },
            "contentUrl": "https://cdn.example.com/video.mpd",
            "contentType": "application/dash+xml",
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream(
                "https://play.viaplay.com/api/stream/byguid{?deviceId}"
            )

        assert info.stream_type == StreamType.BUFFERED
        assert info.duration == pytest.approx(2535.6)

    async def test_normalizes_millisecond_duration(self, api: ViaplayAPI) -> None:
        body = {
            "duration": 2535648,
            "product": {
                "streamType": "VOD",
                "content": {"title": "Episode"},
            },
            "contentUrl": "https://cdn.example.com/video.mpd",
            "contentType": "application/dash+xml",
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream(
                "https://play.viaplay.com/api/stream/byguid{?deviceId}"
            )

        assert info.duration == pytest.approx(2535.648)

    async def test_inferrs_live_stream_type_from_play_url(
        self, api: ViaplayAPI
    ) -> None:
        body = {
            "duration": 0,
            "contentUrl": "https://cdn.example.com/live.mpd",
            "contentType": "application/dash+xml",
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream(
                "https://play-live.viaplay.com/api/stream/bymediaguid{?deviceId}&mediaGuid=X"
            )

        assert info.stream_type == StreamType.LIVE
        assert info.duration is None

    async def test_extracts_fallback_urls(self, api: ViaplayAPI) -> None:
        body = {
            "_links": {
                "viaplay:encryptedPlaylist": {
                    "href": "https://cdn1.example.com/manifest.mpd",
                },
                "viaplay:fallbackMedia": [
                    {
                        "href": "https://cdn2.example.com/manifest.mpd",
                        "streamingFormat": "Dash",
                    },
                    {
                        "href": "https://cdn3.example.com/manifest.mpd",
                        "streamingFormat": "Dash",
                    },
                ],
            },
        }
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload=body)
            info = await api.fetch_stream("https://x")

        assert info.fallback_urls == (
            "https://cdn2.example.com/manifest.mpd",
            "https://cdn3.example.com/manifest.mpd",
        )

    async def test_raises_on_no_stream(self, api: ViaplayAPI) -> None:
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload={})
            with pytest.raises(RuntimeError, match="no stream URL"):
                _ = await api.fetch_stream("https://x")

    async def test_raises_on_non_200(self, api: ViaplayAPI) -> None:
        with aioresponses() as m:
            m.get(re.compile(r".*"), payload={}, status=404)
            with pytest.raises(RuntimeError, match="status 404"):
                _ = await api.fetch_stream("https://x")
