"""Tests for the ViaplayProvider."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import AsyncMock, patch

import pytest

from vibecast.player import (
    DrmSystem,
    LicenseRequest,
    LicenseResponse,
    LicenseRoute,
    PlaybackState,
    PlayerState,
    StreamType,
)
from vibecast.provider import (
    LaunchCredentials,
    LoadRequest,
    MediaInfo,
    MediaMetadata,
    ProviderSession,
    ReceiverContext,
)
from vibecast.providers.viaplay._api import (
    DeviceAuthInfo,
    SessionCheckResult,
    StreamInfo,
)
from vibecast.providers.viaplay._provider import _NS_VIAPLAY, ViaplayProvider

if TYPE_CHECKING:
    from httpx import AsyncClient


def _make_session(
    session_id: str = "sess-1",
    transport_id: str = "pid-1",
    app_id: str = "6313CF39",
) -> tuple[ProviderSession, AsyncMock, AsyncMock]:
    broadcast_mock = AsyncMock()
    send_mock = AsyncMock()
    session = ProviderSession(
        session_id=session_id,
        transport_id=transport_id,
        app_id=app_id,
        http_client=cast("AsyncClient", object()),
        receiver=ReceiverContext(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="receiver-device-id",
            data_dir=Path("/tmp/vibecast-tests/providers/viaplay"),
        ),
        send_custom=send_mock,
        broadcast_custom=broadcast_mock,
    )
    return session, broadcast_mock, send_mock


class TestProperties:
    def test_provider_metadata(self) -> None:
        provider = ViaplayProvider()
        assert provider.app_ids() == frozenset({"6313CF39", "2DB7CC49"})
        assert provider.display_name() == "Viaplay"
        assert _NS_VIAPLAY in provider.namespaces()


class TestLifecycle:
    async def test_on_launch_and_stop_manage_session(self) -> None:
        provider = ViaplayProvider()
        session, _, _ = _make_session()

        await provider.on_launch(session, LaunchCredentials(credentials="token"))
        assert "sess-1" in provider._sessions  # noqa: SLF001

        await provider.on_stop(session)
        assert "sess-1" not in provider._sessions  # noqa: SLF001

    async def test_setup_info_starts_auth_flow_task(self) -> None:
        provider = ViaplayProvider()
        session, _, _ = _make_session()
        await provider.on_launch(session, LaunchCredentials(credentials="token"))

        with patch.object(
            provider, "_run_auth_flow", new_callable=AsyncMock
        ) as mock_auth:
            await provider.on_message(
                session,
                _NS_VIAPLAY,
                {
                    "type": "SETUP_INFO",
                    "contentRoot": "https://content.viaplay.se/stotta",
                    "countryCode": "se",
                    "userId": "user-1",
                    "profileId": "profile-1",
                },
            )

            state = provider._sessions["sess-1"]  # noqa: SLF001
            if state.auth_task is not None:
                await state.auth_task

        mock_auth.assert_awaited_once()


class TestResolveMedia:
    async def test_resolves_stream_and_returns_playback_media(self) -> None:
        provider = ViaplayProvider()
        session, _, _ = _make_session()
        await provider.on_launch(session, LaunchCredentials())

        state = provider._sessions[session.session_id]  # noqa: SLF001
        state.authenticated = True
        state.auth_event.set()

        stream = StreamInfo(
            url="https://cdn.example.com/manifest.mpd",
            content_type="application/dash+xml",
            stream_type=StreamType.LIVE,
            duration=3600.0,
            title="Live Match",
            drm_license_url="https://drm.example.com/license",
        )

        with patch.object(
            state.api,
            "fetch_stream",
            new_callable=AsyncMock,
            return_value=stream,
        ):
            media = await provider.resolve_media(
                session,
                LoadRequest(
                    request_id=1,
                    media=MediaInfo(
                        content_id="https://placeholder",
                        content_type="video/mp4",
                        stream_type=StreamType.BUFFERED,
                        metadata=MediaMetadata(
                            title="Fallback Title",
                            subtitle="Episode 1",
                        ),
                    ),
                    autoplay=True,
                    current_time=12.5,
                    custom_data={"playUrl": "https://content.viaplay.se/play/123"},
                ),
            )

        assert media.session_id == "sess-1"
        assert len(media.streams) == 1
        assert media.streams[0].url == "https://cdn.example.com/manifest.mpd"
        assert media.stream_type is StreamType.LIVE
        assert media.duration == 3600.0
        assert media.title == "Live Match"
        assert media.subtitle == "Episode 1"
        assert media.start_time == 12.5
        assert media.streams[0].drm is not None
        assert media.streams[0].drm.system is DrmSystem.WIDEVINE
        assert media.streams[0].drm.license_url == "https://drm.example.com/license"
        assert "Origin" in media.streams[0].drm.headers

    async def test_requires_authentication(self) -> None:
        provider = ViaplayProvider()
        session, _, _ = _make_session()
        await provider.on_launch(session, LaunchCredentials())

        async def _timeout_wait_for(
            awaitable: object, *args: object, **kwargs: object
        ) -> None:
            _ = args
            _ = kwargs
            close = getattr(awaitable, "close", None)
            if callable(close):
                _ = close()
            raise TimeoutError

        with (
            patch("asyncio.wait_for", side_effect=_timeout_wait_for),
            pytest.raises(RuntimeError, match="NOT_AUTHENTICATED"),
        ):
            _ = await provider.resolve_media(
                session,
                LoadRequest(
                    request_id=1,
                    media=MediaInfo(
                        content_id="https://placeholder",
                        stream_type=StreamType.BUFFERED,
                    ),
                    custom_data={"playUrl": "https://content.viaplay.se/play/123"},
                ),
            )


class TestPlaybackAndLicense:
    async def test_playback_update_broadcasts_receiver_state_and_posdur(self) -> None:
        provider = ViaplayProvider()
        session, broadcast, _ = _make_session()
        await provider.on_launch(session, LaunchCredentials())

        await provider.on_playback_update(
            session,
            PlaybackState(
                player_state=PlayerState.PLAYING,
                current_time=260.9,
                duration=2535.48,
            ),
        )

        assert broadcast.await_count == 2
        first_namespace, first_payload = broadcast.await_args_list[0].args
        assert first_namespace == _NS_VIAPLAY
        assert first_payload["type"] == "RECEIVER_STATE"
        assert first_payload["receiverState"]["status"] == "CASTING"

        second_namespace, second_payload = broadcast.await_args_list[1].args
        assert second_namespace == _NS_VIAPLAY
        assert second_payload["type"] == "POSDUR"
        assert second_payload["position"] == 260
        assert second_payload["duration"] == 2535

    async def test_resolve_license_uses_generic_forwarder(self) -> None:
        provider = ViaplayProvider()
        session, _, _ = _make_session()
        await provider.on_launch(session, LaunchCredentials())

        forward = AsyncMock(
            return_value=LicenseResponse(
                body=b"license-bytes",
                content_type="application/octet-stream",
                status=403,
            )
        )
        request = LicenseRequest(
            session_id=session.session_id,
            route_id="r0",
            body=b"challenge",
        )
        route = LicenseRoute(
            route_id="r0",
            system=DrmSystem.WIDEVINE,
            upstream_url="https://drm.example.com/license",
        )

        response = await provider.resolve_license(
            session,
            request,
            route,
            forward,
        )

        assert response.body == b"license-bytes"
        assert response.content_type == "application/octet-stream"
        assert response.status == 403
        forward.assert_awaited_once_with(request, route)


class TestAuthFlowEdges:
    async def test_start_device_auth_emits_authorization_required(self) -> None:
        provider = ViaplayProvider()
        session, broadcast, _ = _make_session()
        await provider.on_launch(session, LaunchCredentials())
        state = provider._sessions[session.session_id]  # noqa: SLF001
        state.user_id = "user-1"
        state.profile_id = "profile-1"

        auth_info = DeviceAuthInfo(
            user_code="ABCD",
            device_token="token",
            activate_url="https://viaplay.com/activate?userCode=ABCD",
            authorized_url="https://login.viaplay.com/authorized",
        )

        with (
            patch.object(
                state.api,
                "get_device_authorization",
                new_callable=AsyncMock,
                return_value=auth_info,
            ),
            patch.object(provider, "_poll_for_authorization", new_callable=AsyncMock),
        ):
            await provider._start_device_auth(session, state, SessionCheckResult())

        first_namespace, first_payload = broadcast.await_args_list[0].args
        assert first_namespace == _NS_VIAPLAY
        assert first_payload["type"] == "AUTHORIZATION_REQUIRED"
        assert first_payload["authorizationUrl"] == auth_info.activate_url
