"""Tests for the Amazon Prime provider."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import AsyncMock, patch

from vibecast.player import DrmSystem, LicenseRequest, LicenseRoute, StreamType
from vibecast.provider import (
    LaunchCredentials,
    LoadRequest,
    MediaInfo,
    MediaMetadata,
    MediaResolveFailure,
    ProviderSession,
    ReceiverContext,
)
from vibecast.providers.primevideo._api import PrimeVideoAPI
from vibecast.providers.primevideo._models import VodPlaybackResourcesResponse
from vibecast.providers.primevideo._provider import (
    _NS_PRIME,
    PrimeVideoProvider,
    _TitlePlaybackState,
)

if TYPE_CHECKING:
    from httpx import AsyncClient


def _make_session() -> ProviderSession:
    return ProviderSession(
        session_id="sess-1",
        transport_id="pid-1",
        app_id="17608BC8",
        http_client=cast("AsyncClient", object()),
        receiver=ReceiverContext(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="receiver-device-id",
            data_dir=Path("/tmp/vibecast-tests/providers/primevideo"),
        ),
        send_custom=AsyncMock(),
        broadcast_custom=AsyncMock(),
    )


def test_provider_metadata() -> None:
    provider = PrimeVideoProvider()
    assert provider.app_ids() == frozenset({"17608BC8"})
    assert provider.display_name() == "Prime Video"
    assert provider.namespaces() == frozenset({_NS_PRIME})


async def test_resolve_media_uses_preload_stream_data() -> None:
    provider = PrimeVideoProvider()
    session = _make_session()
    await provider.on_launch(session, LaunchCredentials(credentials="actor-token"))

    _ = await provider.on_message(
        session,
        _NS_PRIME,
        {
            "type": "Preload",
            "contentId": "amzn1.dv.gti.example",
            "playbackEnvelope": {
                "envelope": "envelope-v1",
                "correlationId": "corr-1",
            },
        },
    )

    resources = VodPlaybackResourcesResponse.model_validate(
        {
            "sessionization": {"sessionHandoffToken": "handoff-1"},
            "vodPlaybackUrls": {
                "result": {
                    "playbackUrls": {
                        "defaultUrlSetId": "dash-main",
                        "urlSets": [
                            {
                                "urlSetId": "dash-main",
                                "url": "https://cdn.example.com/main.mpd",
                            },
                            {
                                "urlSetId": "dash-alt",
                                "url": "https://cdn.example.com/alt.mpd",
                            },
                        ],
                    }
                }
            },
        }
    )

    with (
        patch.object(
            PrimeVideoAPI,
            "refresh_playback_envelope",
            new=AsyncMock(side_effect=RuntimeError("skip refresh")),
        ),
        patch.object(
            PrimeVideoAPI,
            "get_vod_playback_resources",
            new=AsyncMock(return_value=resources),
        ),
    ):
        media = await provider.resolve_media(
            session,
            LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="amzn1.dv.gti.example",
                    content_type="video/mp4",
                    stream_type=StreamType.BUFFERED,
                    metadata=MediaMetadata(title="Episode 1", subtitle="Pilot"),
                    duration=120.0,
                ),
                custom_data={"deviceId": "cast-device-1"},
            ),
        )

    assert not isinstance(media, MediaResolveFailure)
    assert len(media.streams) == 2
    assert media.streams[0].url.startswith("https://cdn.example.com/main.mpd")
    assert media.streams[0].drm is not None
    assert "titleId=amzn1.dv.gti.example" in media.streams[0].drm.license_url
    assert media.title == "Episode 1"
    assert media.subtitle == "Pilot"


async def test_resolve_license_uses_prime_api() -> None:
    provider = PrimeVideoProvider()
    session = _make_session()
    await provider.on_launch(session, LaunchCredentials(credentials="actor-token"))

    state = provider._sessions[session.session_id]  # noqa: SLF001
    state.device_id = "cast-device-1"
    state.marketplace_id = "A3K6Y4MI8GDYMT"
    state.current_title_id = "amzn1.dv.gti.example"
    state.title_state[state.current_title_id] = _TitlePlaybackState(
        playback_envelope="envelope-v1",
        session_handoff_token="handoff-1",
    )

    with patch.object(
        PrimeVideoAPI,
        "get_widevine_license",
        new=AsyncMock(return_value=b"license-bytes"),
    ) as mock_license:
        response = await provider.resolve_license(
            session,
            LicenseRequest(session_id=session.session_id, route_id="r0", body=b"abc"),
            LicenseRoute(
                route_id="r0",
                system=DrmSystem.WIDEVINE,
                upstream_url="https://example.com/license?titleId=amzn1.dv.gti.example",
            ),
            AsyncMock(),
        )

    assert response.status == 200
    assert response.body == b"license-bytes"
    mock_license.assert_awaited_once()


async def test_am_i_registered_returns_not_registered_without_token() -> None:
    provider = PrimeVideoProvider()
    session = _make_session()
    await provider.on_launch(session, LaunchCredentials())

    _ = await provider.on_message(
        session,
        _NS_PRIME,
        {
            "type": "AmIRegistered",
            "messageProtocolVersion": 1,
            "deviceId": "cast-device-1",
        },
    )

    send_custom = cast("AsyncMock", session._send_custom)  # noqa: SLF001
    send_custom.assert_awaited_once_with(
        _NS_PRIME,
        {
            "type": "AmIRegisteredResponse",
            "error": {
                "code": "NotRegistered",
                "internalName": "NotRegistered",
                "message": "deviceId cast-device-1 is not registered",
                "isFatal": False,
            },
        },
    )


async def test_am_i_registered_returns_success_with_token() -> None:
    provider = PrimeVideoProvider()
    session = _make_session()
    await provider.on_launch(session, LaunchCredentials(credentials="actor-token"))

    _ = await provider.on_message(
        session,
        _NS_PRIME,
        {
            "type": "AmIRegistered",
            "messageProtocolVersion": 1,
            "deviceId": "cast-device-1",
        },
    )

    send_custom = cast("AsyncMock", session._send_custom)  # noqa: SLF001
    send_custom.assert_awaited_once_with(
        _NS_PRIME,
        {
            "type": "AmIRegisteredResponse",
        },
    )
