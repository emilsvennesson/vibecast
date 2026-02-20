"""Tests for the Viaplay async HTTP API client."""

from __future__ import annotations

import re
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

import pytest
from aioresponses import aioresponses

from castvibe.providers.viaplay._api import (
    DeviceAuthInfo,
    SessionCheckResult,
    ViaplayAPI,
)

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
async def api(tmp_path: Any) -> AsyncIterator[ViaplayAPI]:
    """Create a ViaplayAPI instance using a temporary data directory."""
    a = ViaplayAPI(data_dir=tmp_path)
    a.set_setup_info(
        content_root="https://content.viaplay.se/stotta",
        country_code="se",
        user_id="user-1",
        profile_id="prof-1",
    )
    yield a
    await a.close()


# ---------------------------------------------------------------------------
# Persistence tests
# ---------------------------------------------------------------------------


class TestDeviceIdPersistence:
    async def test_creates_device_id(self, tmp_path: Any) -> None:
        a = ViaplayAPI(data_dir=tmp_path)
        id_path = tmp_path / "viaplay_device_id"
        assert id_path.exists()
        stored = id_path.read_text().strip()
        assert len(stored) == 36  # UUID format
        assert a._device_id == stored  # noqa: SLF001

    async def test_reuses_existing_device_id(self, tmp_path: Any) -> None:
        id_path = tmp_path / "viaplay_device_id"
        _ = id_path.write_text("existing-id-123")
        a = ViaplayAPI(data_dir=tmp_path)
        assert a._device_id == "existing-id-123"  # noqa: SLF001


class TestDeviceKey:
    async def test_device_key_format(self, api: ViaplayAPI) -> None:
        assert api.device_key == "chromecastgoogletv4k-se"

    async def test_device_key_changes_with_country(self, tmp_path: Any) -> None:
        a = ViaplayAPI(data_dir=tmp_path)
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

    async def test_raises_without_content_root(self, tmp_path: Any) -> None:
        a = ViaplayAPI(data_dir=tmp_path)
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
