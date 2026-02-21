"""Tests for the Viaplay async HTTP API client."""

from __future__ import annotations

import re
from typing import Any

import httpx
import pytest

from castvibe._models import StreamType
from castvibe.providers.viaplay._api import (
    DeviceAuthInfo,
    SessionCheckResult,
    ViaplayAPI,
)

# ---------------------------------------------------------------------------
# Test helpers
# ---------------------------------------------------------------------------


def _mock_api(
    *responses: tuple[re.Pattern[str] | str, dict[str, Any] | bytes, int],
    device_id: str = "receiver-device-id",
    with_setup: bool = True,
) -> ViaplayAPI:
    """Build a ``ViaplayAPI`` backed by an ``httpx.MockTransport``.

    Each *response* is ``(url_pattern, data, status_code)``.
    *data* may be a ``dict`` (returned as JSON) or ``bytes`` (returned raw).
    Unmatched requests raise :class:`AssertionError`.
    """

    def handler(request: httpx.Request) -> httpx.Response:
        url_str = str(request.url)
        for pattern, data, status in responses:
            if isinstance(pattern, str) and pattern != url_str:
                continue
            if isinstance(pattern, re.Pattern) and not pattern.match(url_str):
                continue
            if isinstance(data, bytes):
                return httpx.Response(status, content=data)
            return httpx.Response(status, json=data)
        msg = f"no mock route matched: {url_str}"
        raise AssertionError(msg)

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    api = ViaplayAPI(client=client, device_id=device_id)
    if with_setup:
        api.set_setup_info(
            content_root="https://content.viaplay.se/stotta",
            country_code="se",
            user_id="user-1",
            profile_id="prof-1",
        )
    return api


@pytest.fixture
def api() -> ViaplayAPI:
    """ViaplayAPI configured with default setup info (fails on HTTP requests)."""
    return _mock_api()


# ---------------------------------------------------------------------------
# Receiver identity tests
# ---------------------------------------------------------------------------


class TestReceiverIdentity:
    async def test_uses_receiver_device_id(self) -> None:
        a = _mock_api(with_setup=False, device_id="receiver-123")
        assert a._device_id == "receiver-123"  # noqa: SLF001


class TestDeviceKey:
    async def test_device_key_format(self, api: ViaplayAPI) -> None:
        assert api.device_key == "chromecastgoogletv4k-se"

    async def test_device_key_changes_with_country(self) -> None:
        a = _mock_api(with_setup=False, device_id="receiver-123")
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
    async def test_returns_user_on_200(self) -> None:
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
        api = _mock_api((re.compile(r"https://content\.viaplay\.se/.*"), body, 200))
        result = await api.check_session()

        assert result.user is not None
        assert result.user.user_id == "user-1"
        assert result.user.first_name == "Test"
        assert result.persistent_login_url == "https://login.viaplay.com/pl"

    async def test_raises_without_content_root(self) -> None:
        a = _mock_api(with_setup=False, device_id="receiver-123")
        with pytest.raises(RuntimeError, match="content root not set"):
            _ = await a.check_session()

    async def test_non_200_returns_result(self) -> None:
        api = _mock_api(
            (re.compile(r"https://content\.viaplay\.se/.*"), {}, 401),
        )
        result = await api.check_session()

        assert result.user is None


# ---------------------------------------------------------------------------
# persistent_login / token_login
# ---------------------------------------------------------------------------


class TestPersistentLogin:
    async def test_returns_true_on_200(self) -> None:
        api = _mock_api((re.compile(r".*"), {"ok": True}, 200))
        ok = await api.persistent_login("https://login.viaplay.com/pl{?deviceKey}")

        assert ok is True

    async def test_returns_false_on_401(self) -> None:
        api = _mock_api((re.compile(r".*"), {}, 401))
        ok = await api.persistent_login("https://login.viaplay.com/pl{?deviceKey}")

        assert ok is False


class TestTokenLogin:
    async def test_returns_true_on_200(self) -> None:
        api = _mock_api((re.compile(r".*"), b"OK", 200))
        ok = await api.token_login(
            "https://login.viaplay.com/tl{?accessToken,deviceKey}",
            "my-token",
        )

        assert ok is True

    async def test_returns_false_on_failure(self) -> None:
        api = _mock_api((re.compile(r".*"), b"Unauthorized", 401))
        ok = await api.token_login(
            "https://login.viaplay.com/tl{?accessToken}",
            "bad-token",
        )

        assert ok is False


# ---------------------------------------------------------------------------
# get_device_authorization
# ---------------------------------------------------------------------------


class TestGetDeviceAuthorization:
    async def test_returns_device_auth_info(self) -> None:
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
        api = _mock_api((re.compile(r"https://login\.viaplay\.com/.*"), auth_body, 200))
        info = await api.get_device_authorization(root_result)

        assert info.user_code == "ABCD1234"
        assert info.device_token == "dt-999"
        assert info.activate_url == "https://viaplay.com/activate"
        assert info.authorized_url == "https://login.viaplay.com/authorized"

    async def test_expands_templated_activate_url(self) -> None:
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
        api = _mock_api((re.compile(r"https://login\.viaplay\.com/.*"), auth_body, 200))
        info = await api.get_device_authorization(root_result)

        assert info.activate_url == (
            "https://login.viaplay.com/api/device/activate"
            "?deviceKey=chromecastgoogletv4k-se&userCode=ABCD1234"
        )

    async def test_raises_on_missing_user_code(self) -> None:
        root_result = SessionCheckResult(
            device_auth_url="https://login.viaplay.com/code",
        )
        api = _mock_api((re.compile(r".*"), {"deviceToken": "x"}, 200))
        with pytest.raises(RuntimeError, match="no userCode"):
            _ = await api.get_device_authorization(root_result)

    async def test_raises_on_non_200(self) -> None:
        root_result = SessionCheckResult(
            device_auth_url="https://login.viaplay.com/code",
        )
        api = _mock_api((re.compile(r".*"), {}, 500))
        with pytest.raises(RuntimeError, match="status 500"):
            _ = await api.get_device_authorization(root_result)


# ---------------------------------------------------------------------------
# poll_authorized
# ---------------------------------------------------------------------------


class TestPollAuthorized:
    async def test_returns_true_when_activated(self) -> None:
        auth_info = DeviceAuthInfo(
            user_code="CODE",
            device_token="dt",
            activate_url="",
            authorized_url="https://login.viaplay.com/authorized{?deviceToken,userCode}",
        )
        api = _mock_api((re.compile(r".*"), {}, 200))
        result = await api.poll_authorized(auth_info)

        assert result is True

    async def test_returns_false_on_403(self) -> None:
        auth_info = DeviceAuthInfo(
            user_code="CODE",
            device_token="dt",
            activate_url="",
            authorized_url="https://login.viaplay.com/authorized",
        )
        api = _mock_api((re.compile(r".*"), {}, 403))
        result = await api.poll_authorized(auth_info)

        assert result is False

    async def test_returns_false_when_no_authorized_url(self) -> None:
        auth_info = DeviceAuthInfo(
            user_code="CODE",
            device_token="dt",
            activate_url="",
            authorized_url="",
        )
        api = _mock_api()
        result = await api.poll_authorized(auth_info)
        assert result is False


# ---------------------------------------------------------------------------
# fetch_stream — all 5 resolution paths
# ---------------------------------------------------------------------------


class TestFetchStream:
    async def test_path1_embedded_media(self) -> None:
        body = {
            "_embedded": {
                "viaplay:media": {
                    "contentUrl": "https://cdn.example.com/manifest.mpd",
                    "contentType": "application/dash+xml",
                },
            },
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://content.viaplay.se/play/1234")

        assert info.url == "https://cdn.example.com/manifest.mpd"
        assert info.content_type == "application/dash+xml"

    async def test_path2_top_level_content_url(self) -> None:
        body = {
            "contentUrl": "https://cdn.example.com/video.mp4",
            "contentType": "video/mp4",
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://content.viaplay.se/play/5678")

        assert info.url == "https://cdn.example.com/video.mp4"
        assert info.content_type == "video/mp4"

    async def test_path3_encrypted_playlist_hls(self) -> None:
        body = {
            "streamingFormat": "HLS",
            "_links": {
                "viaplay:encryptedPlaylist": {
                    "href": "https://cdn.example.com/master.m3u8",
                },
            },
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://x")

        assert info.url == "https://cdn.example.com/master.m3u8"
        assert info.content_type == "application/x-mpegURL"

    async def test_path3_encrypted_playlist_dash(self) -> None:
        body = {
            "streamingFormat": "DASH",
            "_links": {
                "viaplay:encryptedPlaylist": {
                    "href": "https://cdn.example.com/manifest.mpd",
                },
            },
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://x")

        assert info.content_type == "application/dash+xml"

    async def test_path4_playlist_link(self) -> None:
        body = {
            "_links": {
                "viaplay:playlist": {
                    "href": "https://cdn.example.com/playlist.mpd",
                },
            },
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://x")

        assert info.url == "https://cdn.example.com/playlist.mpd"
        assert info.content_type == "application/dash+xml"

    async def test_path5_stream_link(self) -> None:
        body = {
            "_links": {
                "viaplay:stream": {
                    "href": "https://cdn.example.com/stream",
                },
            },
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://x")

        assert info.url == "https://cdn.example.com/stream"
        assert info.content_type == ""

    async def test_extracts_drm_license_url(self) -> None:
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
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://x")

        assert info.drm_license_url == "https://drm.example.com/license"

    async def test_extracts_title_from_product(self) -> None:
        body = {
            "product": {
                "content": {"title": "My Show", "type": "episode"},
                "streamType": "vod",
            },
            "contentUrl": "https://cdn.example.com/video.mpd",
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://x")

        assert info.title == "My Show"

    async def test_extracts_stream_type_and_duration_from_product(self) -> None:
        body = {
            "duration": 2535.6,
            "product": {
                "streamType": "VOD",
                "content": {"title": "Episode"},
            },
            "contentUrl": "https://cdn.example.com/video.mpd",
            "contentType": "application/dash+xml",
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream(
            "https://play.viaplay.com/api/stream/byguid{?deviceId}"
        )

        assert info.stream_type == StreamType.BUFFERED
        assert info.duration == pytest.approx(2535.6)

    async def test_normalizes_millisecond_duration(self) -> None:
        body = {
            "duration": 2535648,
            "product": {
                "streamType": "VOD",
                "content": {"title": "Episode"},
            },
            "contentUrl": "https://cdn.example.com/video.mpd",
            "contentType": "application/dash+xml",
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream(
            "https://play.viaplay.com/api/stream/byguid{?deviceId}"
        )

        assert info.duration == pytest.approx(2535.648)

    async def test_inferrs_live_stream_type_from_play_url(self) -> None:
        body = {
            "duration": 0,
            "contentUrl": "https://cdn.example.com/live.mpd",
            "contentType": "application/dash+xml",
        }
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream(
            "https://play-live.viaplay.com/api/stream/bymediaguid{?deviceId}&mediaGuid=X"
        )

        assert info.stream_type == StreamType.LIVE
        assert info.duration is None

    async def test_extracts_fallback_urls(self) -> None:
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
        api = _mock_api((re.compile(r".*"), body, 200))
        info = await api.fetch_stream("https://x")

        assert info.fallback_urls == (
            "https://cdn2.example.com/manifest.mpd",
            "https://cdn3.example.com/manifest.mpd",
        )

    async def test_raises_on_no_stream(self) -> None:
        api = _mock_api((re.compile(r".*"), {}, 200))
        with pytest.raises(RuntimeError, match="no stream URL"):
            _ = await api.fetch_stream("https://x")

    async def test_raises_on_non_200(self) -> None:
        api = _mock_api((re.compile(r".*"), {}, 404))
        with pytest.raises(RuntimeError, match="status 404"):
            _ = await api.fetch_stream("https://x")
