"""Tests for the TV4 Play app."""

from __future__ import annotations

from pathlib import Path
from typing import Any, cast
from unittest.mock import AsyncMock

import httpx

from vibecast.app import (
    AppContext,
    LaunchCredentials,
    LoadRequest,
    MediaInfo,
    MediaResolveFailure,
    PlaybackProxy,
    ReceiverContext,
)
from vibecast.apps.tv4play._app import _NS_TV4, Tv4Play
from vibecast.player import PlaybackState, PlayerState, StreamType


def _make_session(
    client: httpx.AsyncClient,
    *,
    broadcast_custom: AsyncMock | None = None,
) -> AppContext:
    return AppContext(
        session_id="sess-1",
        transport_id="pid-1",
        app_id="B6470434",
        http_client=client,
        receiver=ReceiverContext(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="receiver-device-id",
            data_dir=Path("/tmp/vibecast-tests/apps/tv4play"),
        ),
        send_custom=AsyncMock(),
        broadcast_custom=broadcast_custom or AsyncMock(),
    )


def test_app_metadata() -> None:
    app = Tv4Play()
    assert app.app_ids() == frozenset({"B6470434"})
    assert app.display_name() == "TV4 Play v5"
    assert app.app_key() == "tv4play"
    assert app.namespaces() == frozenset({_NS_TV4})
    assert not app.playback_proxy_policy().enables(PlaybackProxy.MANIFEST)


async def test_resolve_media_refreshes_auth_and_resolves_vod_yospace() -> None:
    app = Tv4Play()
    requests: list[httpx.Request] = []

    def _handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        url = str(request.url)
        if url == "https://auth.tv4.a2d.tv/v2/auth/token":
            return httpx.Response(
                200,
                json={
                    "access_token": "access-1",
                    "refresh_token": "refresh-2",
                    "expires_in": 10800000,
                },
                request=request,
            )
        if url == "https://nordic-gateway.tv4.a2d.tv/graphql":
            return httpx.Response(
                200,
                json={
                    "data": {
                        "media": {
                            "__typename": "Episode",
                            "title": "Avsnitt 1",
                            "extendedTitle": "Coldwater - Avsnitt 1, Sasong 1",
                            "isDrmProtected": True,
                            "images": {"main16x9": {"source": "https://img/main.jpg"}},
                            "series": {"title": "Coldwater"},
                        }
                    }
                },
                request=request,
            )
        if url.startswith("https://playback2.a2d.tv/play/asset-vod?"):
            assert request.headers["x-jwt"] == "Bearer access-1"
            return httpx.Response(
                200,
                json=_playback_payload(
                    asset_id="asset-vod",
                    is_live=False,
                    state="vod",
                    manifest_url="https://vod.streaming.a2d.tv/original.mpd",
                    access_url="https://yospace.example/access",
                    access_url_type="yospace",
                ),
                request=request,
            )
        if url == "https://yospace.example/access":
            return httpx.Response(
                200,
                text=(
                    '<Response><MPD href="/csm/builder/proxy.1,proxy.2.mpd'
                    '?yo.p.si=abc&amp;ss.sig=sig" /></Response>'
                ),
                request=request,
            )
        return httpx.Response(404, json={"error": "unexpected"}, request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
        broadcast_custom = AsyncMock()
        session = _make_session(client, broadcast_custom=broadcast_custom)
        await app.on_launch(session, LaunchCredentials())

        media = await app.resolve_media(
            session,
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="asset-vod", stream_type=StreamType.NONE),
                custom_data={
                    "refreshToken": "refresh-1",
                    "profileId": "default",
                    "gdpr": "consent-1",
                },
            ),
        )

    assert not isinstance(media, MediaResolveFailure)
    assert media.stream_type is StreamType.BUFFERED
    assert media.streams[0].url == (
        "https://yospace.example/csm/builder/proxy.1,proxy.2.mpd?yo.p.si=abc&ss.sig=sig"
    )
    assert media.streams[0].content_type == "application/dash+xml"
    assert media.streams[0].drm is not None
    assert media.streams[0].drm.license_url == "https://lic.example/wv"
    assert media.streams[0].drm.headers == {"x-dt-auth-token": "drm-token-1"}
    assert media.title == "Coldwater - Avsnitt 1, Sasong 1"
    assert media.subtitle == "Coldwater"
    assert media.images[0].url == "https://img/main.jpg"
    assert media.duration == 3019
    assert media.custom_data["refreshToken"] == "refresh-2"
    assert media.custom_data["mediaType"] == "episode"

    auth_request = requests[0]
    assert auth_request.headers["client-name"] == "nordic-chromecast"
    assert auth_request.headers["origin"] == "https://cast-receiver.a2d.tv"
    assert (
        auth_request.read()
        == b'{"grant_type":"refresh_token","refresh_token":"refresh-1","profile_id":"default"}'
    )

    message_types = [call.args[1]["type"] for call in broadcast_custom.await_args_list]
    assert message_types == [
        "assetId",
        "assetMetadata",
        "playbackCapabilities",
        "progressData",
    ]


async def test_resolve_media_live_uses_manifest_url() -> None:
    app = Tv4Play()

    def _handler(request: httpx.Request) -> httpx.Response:
        url = str(request.url)
        if url == "https://nordic-gateway.tv4.a2d.tv/graphql":
            return httpx.Response(
                200,
                json={
                    "data": {
                        "media": {
                            "__typename": "Channel",
                            "title": "TV4",
                            "channelType": "STANDARD",
                            "isDrmProtected": True,
                            "images": {"logo": {"source": "https://img/logo.svg"}},
                        }
                    }
                },
                request=request,
            )
        if url.startswith("https://playback2.a2d.tv/play/live-asset?"):
            return httpx.Response(
                200,
                json=_playback_payload(
                    asset_id="live-asset",
                    is_live=True,
                    state="live",
                    manifest_url="https://live.streaming.a2d.tv/content/channels/tv4/dash.mpd",
                ),
                request=request,
            )
        return httpx.Response(404, json={"error": "unexpected"}, request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
        session = _make_session(client)
        await app.on_launch(session, LaunchCredentials())
        state = app.require_state(session)
        state.tokens = cast("Any", type("Tokens", (), {"access_token": "access-1"})())

        media = await app.resolve_media(
            session,
            LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="live-asset", stream_type=StreamType.BUFFERED
                ),
            ),
        )

    assert not isinstance(media, MediaResolveFailure)
    assert media.stream_type is StreamType.LIVE
    assert (
        media.streams[0].url
        == "https://live.streaming.a2d.tv/content/channels/tv4/dash.mpd"
    )
    assert media.content_id == "live-asset"
    assert media.title == "TV4"
    assert media.duration == 0


async def test_resolve_media_uses_custom_data_asset_id_for_content_id() -> None:
    app = Tv4Play()

    def _handler(request: httpx.Request) -> httpx.Response:
        url = str(request.url)
        if url == "https://nordic-gateway.tv4.a2d.tv/graphql":
            return httpx.Response(
                200,
                json={
                    "data": {
                        "media": {
                            "__typename": "Channel",
                            "title": "TV4",
                            "channelType": "STANDARD",
                            "isDrmProtected": True,
                            "images": {"logo": {"source": "https://img/logo.svg"}},
                        }
                    }
                },
                request=request,
            )
        if url.startswith("https://playback2.a2d.tv/play/custom-asset?"):
            return httpx.Response(
                200,
                json=_playback_payload(
                    asset_id="custom-asset",
                    is_live=True,
                    state="live",
                    manifest_url="https://live.streaming.a2d.tv/content/channels/tv4/dash.mpd",
                ),
                request=request,
            )
        return httpx.Response(404, json={"error": "unexpected"}, request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
        session = _make_session(client)
        await app.on_launch(session, LaunchCredentials())
        state = app.require_state(session)
        state.tokens = cast("Any", type("Tokens", (), {"access_token": "access-1"})())

        media = await app.resolve_media(
            session,
            LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="",
                    stream_type=StreamType.BUFFERED,
                    custom_data={"assetId": "custom-asset"},
                ),
            ),
        )

    assert not isinstance(media, MediaResolveFailure)
    assert media.content_id == "custom-asset"


async def test_graphql_422_does_not_block_playback_resolution() -> None:
    app = Tv4Play()

    def _handler(request: httpx.Request) -> httpx.Response:
        url = str(request.url)
        if url == "https://nordic-gateway.tv4.a2d.tv/graphql":
            return httpx.Response(
                422,
                json={"errors": [{"message": "unprocessable"}]},
                request=request,
            )
        if url.startswith("https://playback2.a2d.tv/play/game-asset?"):
            return httpx.Response(
                200,
                json=_playback_payload(
                    asset_id="game-asset",
                    is_live=True,
                    state="live",
                    manifest_url="https://live.streaming.a2d.tv/asset/game.isml/widevine.mpd",
                ),
                request=request,
            )
        return httpx.Response(404, json={"error": "unexpected"}, request=request)

    async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
        session = _make_session(client)
        await app.on_launch(session, LaunchCredentials())
        state = app.require_state(session)
        state.tokens = cast("Any", type("Tokens", (), {"access_token": "access-1"})())

        media = await app.resolve_media(
            session,
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="game-asset", stream_type=StreamType.LIVE),
            ),
        )

    assert not isinstance(media, MediaResolveFailure)
    assert media.stream_type is StreamType.LIVE
    assert (
        media.streams[0].url
        == "https://live.streaming.a2d.tv/asset/game.isml/widevine.mpd"
    )
    assert media.title == "Playback title"


async def test_missing_auth_returns_auth_required() -> None:
    app = Tv4Play()
    async with httpx.AsyncClient(
        transport=httpx.MockTransport(_unused_handler)
    ) as client:
        session = _make_session(client)
        await app.on_launch(session, LaunchCredentials())
        result = await app.resolve_media(
            session,
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="asset-vod"),
            ),
        )
    assert isinstance(result, MediaResolveFailure)
    assert result.detail_code == "NOT_AUTHENTICATED"


async def test_playback_update_broadcasts_progress_without_ad_breaks() -> None:
    app = Tv4Play()
    broadcast_custom = AsyncMock()

    async with httpx.AsyncClient(
        transport=httpx.MockTransport(_unused_handler)
    ) as client:
        session = _make_session(client, broadcast_custom=broadcast_custom)
        await app.on_launch(session, LaunchCredentials())
        state = app.require_state(session)
        state.current_asset_id = "asset-1"
        state.current_media = cast(
            "Any",
            type(
                "Media",
                (),
                {
                    "title": "Title",
                    "subtitle": "Subtitle",
                    "images": [],
                    "duration": 120.0,
                    "stream_type": StreamType.BUFFERED,
                },
            )(),
        )

        await app.on_playback_update(
            session,
            PlaybackState(
                player_state=PlayerState.PLAYING,
                current_time=42.0,
                duration=120.0,
            ),
        )

    args = broadcast_custom.await_args
    assert args is not None
    payload = args.args[1]
    assert payload == {
        "type": "progressData",
        "currentTime": 42.0,
        "position": 42.0,
        "duration": 120.0,
        "isInAdBreak": False,
        "liveSeekableRange": {"start": 0, "end": 120.0},
    }


def _playback_payload(
    *,
    asset_id: str,
    is_live: bool,
    state: str,
    manifest_url: str,
    access_url: str | None = None,
    access_url_type: str | None = None,
) -> dict[str, Any]:
    playback_item: dict[str, Any] = {
        "type": "dash",
        "state": state,
        "manifestUrl": manifest_url,
        "license": {
            "castlabsAssetId": "castlabs-1",
            "castlabsServer": "https://lic.example/wv",
            "castlabsToken": "drm-token-1",
            "type": "widevine",
        },
        "subtitles": [{"type": "vtt", "language": "sv", "url": "https://subs/sv.vtt"}],
        "subs": [],
        "thumbnails": [],
    }
    if access_url is not None:
        playback_item["accessUrl"] = access_url
    if access_url_type is not None:
        playback_item["accessUrlType"] = access_url_type
    return {
        "id": asset_id,
        "metadata": {
            "title": "Playback title",
            "type": "channel" if is_live else "episode",
            "duration": 0 if is_live else 3019,
            "isLive": is_live,
            "isDrmProtected": True,
            "image": "https://img/fallback.jpg",
            "videoId": asset_id,
        },
        "playbackItem": playback_item,
        "capabilities": {
            "pause": True,
            "seek": True,
            "stream_switch": False,
        },
    }


def _unused_handler(request: httpx.Request) -> httpx.Response:
    _ = request
    msg = "unexpected HTTP request"
    raise AssertionError(msg)


__all__: list[str] = []
