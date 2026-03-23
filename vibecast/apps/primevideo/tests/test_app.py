"""Tests for the Amazon Prime Video app."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import AsyncMock, patch

from vibecast.app import (
    AppContext,
    LaunchCredentials,
    LoadRequest,
    MediaInfo,
    MediaMetadata,
    MediaResolveFailure,
    ReceiverContext,
)
from vibecast.apps.primevideo._api import PrimeCatalogMetadata, PrimeVideoAPI
from vibecast.apps.primevideo._app import (
    _NS_PRIME,
    PrimeVideo,
    _TitlePlaybackState,
)
from vibecast.apps.primevideo._models import (
    LivePlaybackResourcesResponse,
    VodPlaybackResourcesResponse,
)
from vibecast.player import DrmSystem, LicenseRequest, LicenseRoute, StreamType

if TYPE_CHECKING:
    from httpx import AsyncClient


def _make_session() -> AppContext:
    return AppContext(
        session_id="sess-1",
        transport_id="pid-1",
        app_id="17608BC8",
        http_client=cast("AsyncClient", object()),
        receiver=ReceiverContext(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="receiver-device-id",
            data_dir=Path("/tmp/vibecast-tests/apps/primevideo"),
        ),
        send_custom=AsyncMock(),
        broadcast_custom=AsyncMock(),
    )


def test_app_metadata() -> None:
    app = PrimeVideo()
    assert app.app_ids() == frozenset({"17608BC8"})
    assert app.display_name() == "Prime Video"
    assert app.namespaces() == frozenset({_NS_PRIME})


async def test_resolve_media_uses_preload_stream_data() -> None:
    app = PrimeVideo()
    session = _make_session()
    await app.on_launch(session, LaunchCredentials(credentials="actor-token"))

    _ = await app.on_message(
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
        media = await app.resolve_media(
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
    assert "/playback/drm-vod/GetWidevineLicense" in media.streams[0].drm.license_url
    assert "titleId=amzn1.dv.gti.example" in media.streams[0].drm.license_url
    assert media.title == "Episode 1"
    assert media.subtitle == "Pilot"


async def test_resolve_media_live_uses_live_playback_resources() -> None:
    app = PrimeVideo()
    session = _make_session()
    await app.on_launch(session, LaunchCredentials(credentials="actor-token"))

    resources = LivePlaybackResourcesResponse.model_validate(
        {
            "sessionization": {"sessionHandoffToken": "live-handoff-1"},
            "livePlaybackUrls": {
                "result": {
                    "defaultUrlSetId": "live-main",
                    "urlSets": [
                        {
                            "urlSetId": "live-alt",
                            "urls": {
                                "manifest": {
                                    "url": "https://cdn.example.com/live-alt.mpd"
                                }
                            },
                        },
                        {
                            "urlSetId": "live-main",
                            "urls": {
                                "manifest": {
                                    "url": "https://cdn.example.com/live-main.mpd"
                                }
                            },
                        },
                    ],
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
            "get_live_playback_resources",
            new=AsyncMock(return_value=resources),
        ),
    ):
        media = await app.resolve_media(
            session,
            LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="amzn1.dv.gti.live-example",
                    content_type="video/mp4",
                    stream_type=StreamType.LIVE,
                    metadata=MediaMetadata(title="Live Match", subtitle="Prime Video"),
                    duration=0.0,
                ),
                current_time=64092211200,
                custom_data={
                    "deviceId": "cast-device-live",
                    "playbackEnvelope": {
                        "envelope": "live-envelope-v1",
                        "correlationId": "live-corr-1",
                    },
                },
            ),
        )

    assert not isinstance(media, MediaResolveFailure)
    assert media.stream_type is StreamType.LIVE
    assert media.start_time == 64092211200
    assert len(media.streams) == 2
    assert media.streams[0].url.startswith("https://cdn.example.com/live-main.mpd")
    assert media.streams[0].drm is not None
    assert "/playback/drm/GetWidevineLicense" in media.streams[0].drm.license_url


async def test_resolve_media_uses_catalog_metadata_when_title_missing() -> None:
    app = PrimeVideo()
    session = _make_session()
    await app.on_launch(session, LaunchCredentials(credentials="actor-token"))

    resources = LivePlaybackResourcesResponse.model_validate(
        {
            "sessionization": {"sessionHandoffToken": "live-handoff-1"},
            "livePlaybackUrls": {
                "result": {
                    "defaultUrlSetId": "live-main",
                    "urlSets": [
                        {
                            "urlSetId": "live-main",
                            "urls": {
                                "manifest": {
                                    "url": "https://cdn.example.com/live-main.mpd"
                                }
                            },
                        }
                    ],
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
            "get_live_playback_resources",
            new=AsyncMock(return_value=resources),
        ),
        patch.object(
            PrimeVideoAPI,
            "get_catalog_metadata",
            new=AsyncMock(
                return_value=PrimeCatalogMetadata(
                    title="Newcastle v Manchester United",
                    subtitle="Premier League",
                )
            ),
        ),
    ):
        media = await app.resolve_media(
            session,
            LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="amzn1.dv.gti.live-example",
                    content_type="video/mp4",
                    stream_type=StreamType.LIVE,
                    metadata=MediaMetadata(title="", subtitle=""),
                    duration=0.0,
                ),
                current_time=0,
                custom_data={
                    "deviceId": "cast-device-live",
                    "playbackEnvelope": {
                        "envelope": "live-envelope-v1",
                        "correlationId": "live-corr-1",
                    },
                },
            ),
        )

    assert not isinstance(media, MediaResolveFailure)
    assert media.title == "Newcastle v Manchester United"
    assert media.subtitle == "Premier League"


async def test_resolve_license_uses_prime_api() -> None:
    app = PrimeVideo()
    session = _make_session()
    await app.on_launch(session, LaunchCredentials(credentials="actor-token"))

    state = app._sessions[session.session_id]  # noqa: SLF001
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
        response = await app.resolve_license(
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
    await_args = mock_license.await_args
    assert await_args is not None
    assert await_args.kwargs["is_live"] is False


async def test_resolve_license_live_uses_live_license_mode() -> None:
    app = PrimeVideo()
    session = _make_session()
    await app.on_launch(session, LaunchCredentials(credentials="actor-token"))

    state = app._sessions[session.session_id]  # noqa: SLF001
    state.device_id = "cast-device-1"
    state.marketplace_id = "A3K6Y4MI8GDYMT"
    state.current_title_id = "amzn1.dv.gti.live-example"
    state.title_state[state.current_title_id] = _TitlePlaybackState(
        playback_envelope="envelope-live-v1",
        session_handoff_token="handoff-live-1",
        is_live=True,
    )

    with patch.object(
        PrimeVideoAPI,
        "get_widevine_license",
        new=AsyncMock(return_value=b"live-license-bytes"),
    ) as mock_license:
        response = await app.resolve_license(
            session,
            LicenseRequest(session_id=session.session_id, route_id="r0", body=b"abc"),
            LicenseRoute(
                route_id="r0",
                system=DrmSystem.WIDEVINE,
                upstream_url="https://example.com/license?titleId=amzn1.dv.gti.live-example",
            ),
            AsyncMock(),
        )

    assert response.status == 200
    assert response.body == b"live-license-bytes"
    mock_license.assert_awaited_once()
    await_args = mock_license.await_args
    assert await_args is not None
    assert await_args.kwargs["is_live"] is True


async def test_am_i_registered_returns_not_registered_without_token() -> None:
    app = PrimeVideo()
    session = _make_session()
    await app.on_launch(session, LaunchCredentials())

    _ = await app.on_message(
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
    app = PrimeVideo()
    session = _make_session()
    await app.on_launch(session, LaunchCredentials(credentials="actor-token"))

    _ = await app.on_message(
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
