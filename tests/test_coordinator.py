"""Tests for PlaybackCoordinator."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any, cast, override

import httpx

from vibecast import _namespace as ns
from vibecast._coordinator import PlaybackCoordinator, _filter_upstream_response_headers
from vibecast._manifest_proxy import ManifestProxyRequest
from vibecast._models import (
    LoadRequest,
    MediaInfo,
    MediaSetVolumeRequest,
    MediaStopRequest,
    PauseRequest,
    PlayerState,
    PlayRequest,
    QueueGetItemIdsRequest,
    SeekRequest,
    StreamType,
    Volume,
)
from vibecast.player import (
    DrmInfo,
    DrmSystem,
    LicenseRequest,
    LicenseResponse,
    LicenseRoute,
    PlaybackMedia,
    PlaybackState,
    PlaybackStream,
    Player,
    PlayerContext,
)
from vibecast.provider import (
    LaunchCredentials,
    MediaResolveFailure,
    MediaResolveFailureCode,
    MediaResolveResult,
    Provider,
    ProviderMessageDisposition,
    ProviderSession,
    ReceiverContext,
)

if TYPE_CHECKING:
    from httpx import AsyncClient


class FakeProvider(Provider):
    def __init__(
        self,
        media: MediaResolveResult,
        *,
        use_forwarder: bool = False,
        raise_on_resolve: bool = False,
        raise_on_license: bool = False,
    ) -> None:
        self._media = media
        self._use_forwarder = use_forwarder
        self._raise_on_resolve = raise_on_resolve
        self._raise_on_license = raise_on_license
        self.playback_updates: list[PlaybackState] = []
        self.license_requests: list[LicenseRequest] = []
        self.license_routes: list[LicenseRoute] = []

    @override
    def app_ids(self) -> frozenset[str]:
        return frozenset({"APP"})

    @override
    def display_name(self) -> str:
        return "Provider"

    @override
    def provider_key(self) -> str:
        return "test_provider"

    @override
    def namespaces(self) -> frozenset[str]:
        return frozenset({"urn:x-cast:test"})

    @override
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        _ = session
        _ = credentials

    @override
    async def on_message(
        self,
        session: ProviderSession,
        namespace: str,
        data: dict[str, Any],
    ) -> ProviderMessageDisposition:
        _ = session
        _ = namespace
        _ = data
        return ProviderMessageDisposition.HANDLED

    @override
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> MediaResolveResult:
        _ = session
        _ = load_request
        if self._raise_on_resolve:
            msg = "resolve exploded"
            raise RuntimeError(msg)
        return self._media

    @override
    async def on_playback_update(
        self,
        session: ProviderSession,
        state: PlaybackState,
    ) -> None:
        _ = session
        self.playback_updates.append(state)

    @override
    async def resolve_license(
        self,
        session: ProviderSession,
        request: LicenseRequest,
        route: LicenseRoute,
        forward: Any,
    ) -> LicenseResponse:
        _ = session
        if self._raise_on_license:
            msg = "license exploded"
            raise RuntimeError(msg)
        self.license_requests.append(request)
        self.license_routes.append(route)
        if self._use_forwarder:
            return await forward(request, route)
        return LicenseResponse(body=b"license-response")


class FakePlayer(Player):
    def __init__(self) -> None:
        self.load_calls: list[PlaybackMedia] = []
        self.play_calls = 0
        self.pause_calls = 0
        self.seek_calls: list[float] = []
        self.stop_calls = 0
        self.volume_calls: list[tuple[float, bool]] = []

    @override
    async def on_load(self, ctx: PlayerContext, media: PlaybackMedia) -> None:
        _ = ctx
        self.load_calls.append(media)

    @override
    async def on_play(self, ctx: PlayerContext) -> None:
        _ = ctx
        self.play_calls += 1

    @override
    async def on_pause(self, ctx: PlayerContext) -> None:
        _ = ctx
        self.pause_calls += 1

    @override
    async def on_seek(self, ctx: PlayerContext, position: float) -> None:
        _ = ctx
        self.seek_calls.append(position)

    @override
    async def on_stop(self, ctx: PlayerContext) -> None:
        _ = ctx
        self.stop_calls += 1

    @override
    async def on_volume(self, ctx: PlayerContext, level: float, muted: bool) -> None:
        _ = ctx
        self.volume_calls.append((level, muted))


class FakePlayerServer:
    def __init__(self) -> None:
        self.register_calls: list[str] = []
        self.unregister_calls: list[str] = []
        self.manifest_register_calls: list[str] = []
        self.manifest_unregister_calls: list[str] = []

    def register_license_handler(self, session_id: str, handler: object) -> str:
        _ = handler
        self.register_calls.append(session_id)
        return f"http://127.0.0.1:8010/license/{session_id}"

    def unregister_license_handler(self, session_id: str) -> None:
        self.unregister_calls.append(session_id)

    def register_manifest_handler(self, session_id: str, handler: object) -> str:
        _ = handler
        self.manifest_register_calls.append(session_id)
        return f"http://127.0.0.1:8010/manifest/{session_id}"

    def unregister_manifest_handler(self, session_id: str) -> None:
        self.manifest_unregister_calls.append(session_id)


def _provider_session(
    session_id: str = "session-1",
    *,
    http_client: AsyncClient | None = None,
) -> ProviderSession:
    async def _send_custom(namespace: str, data: dict[str, Any]) -> None:
        _ = namespace
        _ = data

    async def _broadcast_custom(namespace: str, data: dict[str, Any]) -> None:
        _ = namespace
        _ = data

    return ProviderSession(
        session_id=session_id,
        transport_id="pid-1",
        app_id="APP",
        http_client=(
            http_client if http_client is not None else cast("AsyncClient", object())
        ),
        receiver=ReceiverContext(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="device-1",
            data_dir=Path("/tmp/vibecast-tests/providers/fake"),
        ),
        send_custom=_send_custom,
        broadcast_custom=_broadcast_custom,
    )


class TestCoordinator:
    def test_filter_upstream_response_headers_drops_hop_by_hop_values(self) -> None:
        filtered = _filter_upstream_response_headers(
            {
                "Connection": "keep-alive, X-Remove-Me",
                "Content-Encoding": "gzip",
                "Content-Length": "123",
                "Content-Type": "application/dash+xml",
                "Keep-Alive": "timeout=5",
                "Proxy-Authenticate": 'Basic realm="manifest"',
                "Proxy-Authorization": "Basic abc",
                "Set-Cookie": "sid=123",
                "TE": "trailers",
                "Trailer": "Expires",
                "Transfer-Encoding": "chunked",
                "Upgrade": "h2c",
                "X-Remove-Me": "1",
                "X-Preserved": "ok",
            }
        )

        assert filtered == {"X-Preserved": "ok"}

    async def test_load_registers_license_proxy_and_notifies_player(self) -> None:
        media = PlaybackMedia(
            session_id="session-1",
            streams=(
                PlaybackStream(
                    url="https://cdn.example.com/manifest.mpd",
                    content_type="application/dash+xml",
                    drm=DrmInfo(
                        system=DrmSystem.WIDEVINE,
                        license_url="https://drm.example.com",
                    ),
                ),
            ),
            stream_type=StreamType.BUFFERED,
            start_time=5.0,
        )
        provider = FakeProvider(media)
        player = FakePlayer()
        player_server = FakePlayerServer()

        sent: list[tuple[str, str, dict[str, Any]]] = []
        broadcast: list[tuple[str, dict[str, Any]]] = []

        async def _send_fn(
            connection: object,
            sender_id: str,
            namespace: str,
            data: dict[str, Any],
        ) -> None:
            _ = connection
            sent.append((sender_id, namespace, data))

        async def _broadcast_fn(namespace: str, data: dict[str, Any]) -> None:
            broadcast.append((namespace, data))

        coordinator = PlaybackCoordinator(
            session_id="session-1",
            transport_id="pid-1",
            provider=provider,
            provider_session=_provider_session(),
            player=player,
            player_server=cast("Any", player_server),
            broadcast_fn=_broadcast_fn,
            send_fn=_send_fn,
            initial_volume=Volume(level=1.0, muted=False),
        )

        await coordinator.handle_media_message(
            cast("Any", object()),
            "sender-1",
            LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="https://placeholder",
                    stream_type=StreamType.BUFFERED,
                ),
                current_time=5.0,
                custom_data={"playUrl": "https://content.viaplay.se/play/123"},
            ),
        )

        assert player_server.register_calls == ["session-1"]
        assert player_server.manifest_register_calls == ["session-1"]
        assert len(player.load_calls) == 1
        assert (
            player.load_calls[0].streams[0].url
            == "http://127.0.0.1:8010/manifest/session-1/m0.mpd"
        )
        assert player.load_calls[0].streams[0].drm is not None
        assert (
            player.load_calls[0].streams[0].drm.license_url
            == "http://127.0.0.1:8010/license/session-1?route=r0"
        )

        # Load produces 3 broadcasts: LOADING (minimal), LOADING (resolved),
        # then BUFFERING once the player is handed the media.
        assert len(broadcast) >= 3

        # First broadcast: IDLE + LOADING extended status (pre-resolution).
        ns0, p0 = broadcast[0]
        assert ns0 == ns.MEDIA
        assert p0["status"][0]["playerState"] == "IDLE"
        assert p0["status"][0]["extendedStatus"]["playerState"] == "LOADING"

        # Second broadcast: IDLE + LOADING with resolved media info.
        ns1, p1 = broadcast[1]
        assert ns1 == ns.MEDIA
        assert p1["status"][0]["playerState"] == "IDLE"
        assert p1["status"][0]["extendedStatus"]["playerState"] == "LOADING"
        assert "contentUrl" in p1["status"][0]["media"]

        # Third broadcast: BUFFERING with start_time applied.
        ns2, p2 = broadcast[2]
        assert ns2 == ns.MEDIA
        assert p2["status"][0]["playerState"] == "BUFFERING"
        assert p2["status"][0]["currentTime"] == 5.0

        assert provider.playback_updates[-1].player_state is PlayerState.BUFFERING

        _ = sent

    async def test_play_pause_seek_stop_and_volume(self) -> None:
        provider = FakeProvider(
            PlaybackMedia(
                session_id="session-1",
                streams=(
                    PlaybackStream(
                        url="https://cdn.example.com/manifest.mpd",
                        content_type="application/dash+xml",
                    ),
                ),
                stream_type=StreamType.BUFFERED,
            )
        )
        player = FakePlayer()
        player_server = FakePlayerServer()
        sent: list[tuple[str, str, dict[str, Any]]] = []
        broadcast: list[tuple[str, dict[str, Any]]] = []

        async def _send_fn(
            connection: object,
            sender_id: str,
            namespace: str,
            data: dict[str, Any],
        ) -> None:
            _ = connection
            sent.append((sender_id, namespace, data))

        async def _broadcast_fn(namespace: str, data: dict[str, Any]) -> None:
            broadcast.append((namespace, data))

        coordinator = PlaybackCoordinator(
            session_id="session-1",
            transport_id="pid-1",
            provider=provider,
            provider_session=_provider_session(),
            player=player,
            player_server=cast("Any", player_server),
            broadcast_fn=_broadcast_fn,
            send_fn=_send_fn,
            initial_volume=Volume(level=1.0, muted=False),
        )

        connection = cast("Any", object())
        await coordinator.handle_media_message(
            connection,
            "sender-1",
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
                custom_data={"playUrl": "https://content.viaplay.se/play/123"},
            ),
        )
        await coordinator.handle_media_message(
            connection,
            "sender-1",
            PlayRequest(request_id=2, media_session_id=1),
        )
        await coordinator.handle_media_message(
            connection,
            "sender-1",
            PauseRequest(request_id=3, media_session_id=1),
        )
        await coordinator.handle_media_message(
            connection,
            "sender-1",
            SeekRequest(request_id=4, media_session_id=1, current_time=44.0),
        )
        await coordinator.handle_media_message(
            connection,
            "sender-1",
            MediaSetVolumeRequest(
                request_id=5,
                media_session_id=1,
                volume=Volume(level=0.4, muted=True),
            ),
        )
        await coordinator.handle_media_message(
            connection,
            "sender-1",
            MediaStopRequest(request_id=6, media_session_id=1),
        )

        assert player.play_calls == 1
        assert player.pause_calls == 1
        assert player.seek_calls == [44.0]
        assert player.volume_calls == [(0.4, True)]
        assert player.stop_calls == 1
        assert player_server.unregister_calls[-1] == "session-1"
        assert player_server.manifest_unregister_calls[-1] == "session-1"
        # Media stop now sends a proper IDLE status with idleReason, not an
        # empty array.  The media field is omitted on IDLE.
        stop_status = broadcast[-1][1]["status"]
        assert len(stop_status) == 1
        assert stop_status[0]["playerState"] == "IDLE"
        assert stop_status[0]["idleReason"] == "CANCELLED"
        assert "media" not in stop_status[0]

        await coordinator.handle_media_message(
            connection,
            "sender-1",
            QueueGetItemIdsRequest(request_id=7, media_session_id=1),
        )
        assert sent[-1][2]["type"] == "QUEUE_ITEM_IDS"
        assert sent[-1][2]["itemIds"] == []

    async def test_state_report_updates_status_and_provider(self) -> None:
        provider = FakeProvider(
            PlaybackMedia(
                session_id="session-1",
                streams=(
                    PlaybackStream(
                        url="https://cdn.example.com/manifest.mpd",
                        content_type="application/dash+xml",
                    ),
                ),
                stream_type=StreamType.BUFFERED,
            )
        )
        player = FakePlayer()
        broadcast: list[tuple[str, dict[str, Any]]] = []

        async def _send_fn(
            connection: object,
            sender_id: str,
            namespace: str,
            data: dict[str, Any],
        ) -> None:
            _ = connection
            _ = sender_id
            _ = namespace
            _ = data

        async def _broadcast_fn(namespace: str, data: dict[str, Any]) -> None:
            broadcast.append((namespace, data))

        coordinator = PlaybackCoordinator(
            session_id="session-1",
            transport_id="pid-1",
            provider=provider,
            provider_session=_provider_session(),
            player=player,
            player_server=None,
            broadcast_fn=_broadcast_fn,
            send_fn=_send_fn,
            initial_volume=Volume(level=1.0, muted=False),
        )

        await coordinator.handle_media_message(
            cast("Any", object()),
            "sender-1",
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
                custom_data={"playUrl": "https://content.viaplay.se/play/123"},
            ),
        )

        await coordinator.on_state_report(
            PlaybackState(
                player_state=PlayerState.PLAYING,
                current_time=20.0,
                duration=100.0,
            )
        )

        namespace, payload = broadcast[-1]
        assert namespace == ns.MEDIA
        assert payload["requestId"] == 0
        assert payload["status"][0]["playerState"] == "PLAYING"
        assert payload["status"][0]["currentTime"] == 20.0
        assert provider.playback_updates[-1].player_state is PlayerState.PLAYING

    async def test_send_current_status_and_license_delegation(self) -> None:
        provider = FakeProvider(
            PlaybackMedia(
                session_id="session-1",
                streams=(
                    PlaybackStream(
                        url="https://cdn.example.com/manifest.mpd",
                        content_type="application/dash+xml",
                        drm=DrmInfo(
                            system=DrmSystem.WIDEVINE,
                            license_url="https://drm.example.com",
                        ),
                    ),
                ),
                stream_type=StreamType.BUFFERED,
            )
        )
        player = FakePlayer()
        player_server = FakePlayerServer()
        sent: list[tuple[str, str, dict[str, Any]]] = []

        async def _send_fn(
            connection: object,
            sender_id: str,
            namespace: str,
            data: dict[str, Any],
        ) -> None:
            _ = connection
            sent.append((sender_id, namespace, data))

        async def _broadcast_fn(namespace: str, data: dict[str, Any]) -> None:
            _ = namespace
            _ = data

        coordinator = PlaybackCoordinator(
            session_id="session-1",
            transport_id="pid-1",
            provider=provider,
            provider_session=_provider_session(),
            player=player,
            player_server=cast("Any", player_server),
            broadcast_fn=_broadcast_fn,
            send_fn=_send_fn,
            initial_volume=Volume(level=1.0, muted=False),
        )

        await coordinator.handle_media_message(
            cast("Any", object()),
            "sender-1",
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
                custom_data={"playUrl": "https://content.viaplay.se/play/123"},
            ),
        )

        await coordinator.send_current_status(cast("Any", object()), "sender-2")
        assert sent[-1][0] == "sender-2"
        assert sent[-1][1] == ns.MEDIA
        assert sent[-1][2]["type"] == "MEDIA_STATUS"

        response = await coordinator.handle_license(
            LicenseRequest(session_id="session-1", route_id="r0", body=b"challenge")
        )
        assert response.body == b"license-response"
        assert provider.license_requests[-1].body == b"challenge"
        assert provider.license_routes[-1].upstream_url == "https://drm.example.com"

    async def test_license_forwarder_posts_to_upstream(self) -> None:
        media = PlaybackMedia(
            session_id="session-1",
            streams=(
                PlaybackStream(
                    url="https://cdn.example.com/manifest.mpd",
                    content_type="application/dash+xml",
                    drm=DrmInfo(
                        system=DrmSystem.WIDEVINE,
                        license_url="https://drm.example.com/license",
                        headers={"X-Provider": "viaplay"},
                    ),
                ),
            ),
            stream_type=StreamType.BUFFERED,
        )
        provider = FakeProvider(media, use_forwarder=True)

        captured_request: dict[str, Any] = {}

        async def _handler(request: httpx.Request) -> httpx.Response:
            captured_request["url"] = str(request.url)
            captured_request["content_type"] = request.headers.get("Content-Type")
            captured_request["provider_header"] = request.headers.get("X-Provider")
            captured_request["body"] = request.content
            return httpx.Response(
                status_code=201,
                content=b"upstream-license",
                headers={"Content-Type": "application/octet-stream"},
                request=request,
            )

        async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
            coordinator = PlaybackCoordinator(
                session_id="session-1",
                transport_id="pid-1",
                provider=provider,
                provider_session=_provider_session(http_client=client),
                player=FakePlayer(),
                player_server=cast("Any", FakePlayerServer()),
                broadcast_fn=lambda _namespace, _data: _noop_async(),
                send_fn=lambda _c, _s, _n, _d: _noop_async(),
                initial_volume=Volume(level=1.0, muted=False),
            )

            await coordinator.handle_media_message(
                cast("Any", object()),
                "sender-1",
                LoadRequest(
                    request_id=1,
                    media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
                    custom_data={"playUrl": "https://content.viaplay.se/play/123"},
                ),
            )

            response = await coordinator.handle_license(
                LicenseRequest(
                    session_id="session-1",
                    route_id="r0",
                    body=b"challenge",
                    content_type="application/octet-stream",
                )
            )

        assert captured_request["url"] == "https://drm.example.com/license"
        assert captured_request["content_type"] == "application/octet-stream"
        assert captured_request["provider_header"] == "viaplay"
        assert captured_request["body"] == b"challenge"
        assert response.status == 201
        assert response.body == b"upstream-license"

    async def test_manifest_proxy_normalizes_dash_pattern(self) -> None:
        media = PlaybackMedia(
            session_id="session-1",
            streams=(
                PlaybackStream(
                    url="https://cdn.example.com/live/manifest.mpd",
                    content_type="application/dash+xml",
                ),
            ),
            stream_type=StreamType.LIVE,
        )
        provider = FakeProvider(media)

        raw_manifest = """<?xml version=\"1.0\"?>
<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"dynamic\">
  <Period>
    <AdaptationSet mimeType=\"audio/mp4\">
      <Representation id=\"a1\" codecs=\"mp4a.40.2\">
        <SegmentTemplate media=\"a_$Number$.m4s\" initialization=\"a_init.mp4\" timescale=\"32000\">
          <SegmentTimeline>
            <Pattern t=\"0\" r=\"1\">
              <S d=\"64512\"/>
              <S d=\"63488\"/>
            </Pattern>
          </SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"""

        async def _handler(request: httpx.Request) -> httpx.Response:
            if str(request.url) == "https://cdn.example.com/live/manifest.mpd":
                return httpx.Response(
                    status_code=200,
                    content=raw_manifest.encode("utf-8"),
                    headers={"Content-Type": "application/dash+xml"},
                    request=request,
                )
            return httpx.Response(status_code=404, request=request)

        async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
            coordinator = PlaybackCoordinator(
                session_id="session-1",
                transport_id="pid-1",
                provider=provider,
                provider_session=_provider_session(http_client=client),
                player=FakePlayer(),
                player_server=cast("Any", FakePlayerServer()),
                broadcast_fn=lambda _namespace, _data: _noop_async(),
                send_fn=lambda _c, _s, _n, _d: _noop_async(),
                initial_volume=Volume(level=1.0, muted=False),
            )

            await coordinator.handle_media_message(
                cast("Any", object()),
                "sender-1",
                LoadRequest(
                    request_id=1,
                    media=MediaInfo(content_id="x", stream_type=StreamType.LIVE),
                ),
            )

            response = await coordinator.handle_manifest(
                ManifestProxyRequest(
                    session_id="session-1",
                    route_id="m0",
                    method="GET",
                )
            )

        normalized = response.body.decode("utf-8")
        assert response.status == 200
        assert response.content_type == "application/dash+xml"
        assert "<Pattern" not in normalized
        assert normalized.count('<S d="64512"') == 2
        assert "<BaseURL>https://cdn.example.com/live/</BaseURL>" in normalized

    async def test_provider_load_failure_reason_is_passthrough(self) -> None:
        provider = FakeProvider(
            MediaResolveFailure(code=MediaResolveFailureCode.AUTH_REQUIRED)
        )
        player = FakePlayer()
        player_server = FakePlayerServer()
        sent: list[tuple[str, str, dict[str, Any]]] = []
        broadcast: list[tuple[str, dict[str, Any]]] = []

        async def _send_fn(
            connection: object,
            sender_id: str,
            namespace: str,
            data: dict[str, Any],
        ) -> None:
            _ = connection
            sent.append((sender_id, namespace, data))

        async def _broadcast_fn(namespace: str, data: dict[str, Any]) -> None:
            broadcast.append((namespace, data))

        coordinator = PlaybackCoordinator(
            session_id="session-1",
            transport_id="pid-1",
            provider=provider,
            provider_session=_provider_session(),
            player=player,
            player_server=cast("Any", player_server),
            broadcast_fn=_broadcast_fn,
            send_fn=_send_fn,
            initial_volume=Volume(level=1.0, muted=False),
        )

        await coordinator.handle_media_message(
            cast("Any", object()),
            "sender-1",
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
            ),
        )

        assert player.load_calls == []
        assert sent[-1][1] == ns.MEDIA
        assert sent[-1][2]["type"] == "LOAD_FAILED"
        assert sent[-1][2]["reason"] == MediaResolveFailureCode.AUTH_REQUIRED
        assert player_server.unregister_calls[-1] == "session-1"
        assert broadcast[-1][1]["status"][0]["playerState"] == "IDLE"
        assert broadcast[-1][1]["status"][0]["idleReason"] == "ERROR"

    async def test_provider_load_exception_maps_to_internal_reason(self) -> None:
        provider = FakeProvider(
            MediaResolveFailure(code=MediaResolveFailureCode.CONTENT_UNAVAILABLE),
            raise_on_resolve=True,
        )
        player_server = FakePlayerServer()
        sent: list[tuple[str, str, dict[str, Any]]] = []

        async def _send_fn(
            connection: object,
            sender_id: str,
            namespace: str,
            data: dict[str, Any],
        ) -> None:
            _ = connection
            sent.append((sender_id, namespace, data))

        coordinator = PlaybackCoordinator(
            session_id="session-1",
            transport_id="pid-1",
            provider=provider,
            provider_session=_provider_session(),
            player=FakePlayer(),
            player_server=cast("Any", player_server),
            broadcast_fn=lambda _namespace, _data: _noop_async(),
            send_fn=_send_fn,
            initial_volume=Volume(level=1.0, muted=False),
        )

        await coordinator.handle_media_message(
            cast("Any", object()),
            "sender-1",
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
            ),
        )

        assert sent[-1][2]["type"] == "LOAD_FAILED"
        assert sent[-1][2]["reason"] == MediaResolveFailureCode.INTERNAL_ERROR
        assert player_server.unregister_calls[-1] == "session-1"

    async def test_failed_second_load_clears_stale_license_routes(self) -> None:
        first_media = PlaybackMedia(
            session_id="session-1",
            streams=(
                PlaybackStream(
                    url="https://cdn.example.com/manifest.mpd",
                    content_type="application/dash+xml",
                    drm=DrmInfo(
                        system=DrmSystem.WIDEVINE,
                        license_url="https://drm.example.com/license",
                    ),
                ),
            ),
            stream_type=StreamType.BUFFERED,
        )
        provider = FakeProvider(first_media)
        player_server = FakePlayerServer()

        coordinator = PlaybackCoordinator(
            session_id="session-1",
            transport_id="pid-1",
            provider=provider,
            provider_session=_provider_session(),
            player=FakePlayer(),
            player_server=cast("Any", player_server),
            broadcast_fn=lambda _namespace, _data: _noop_async(),
            send_fn=lambda _c, _s, _n, _d: _noop_async(),
            initial_volume=Volume(level=1.0, muted=False),
        )

        await coordinator.handle_media_message(
            cast("Any", object()),
            "sender-1",
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
            ),
        )
        assert player_server.register_calls == ["session-1"]

        provider._media = MediaResolveFailure(  # noqa: SLF001
            code=MediaResolveFailureCode.CONTENT_UNAVAILABLE
        )
        await coordinator.handle_media_message(
            cast("Any", object()),
            "sender-1",
            LoadRequest(
                request_id=2,
                media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
            ),
        )

        response = await coordinator.handle_license(
            LicenseRequest(session_id="session-1", route_id="r0", body=b"challenge")
        )
        assert response.status == 404
        assert response.body == b"unknown license route"

    async def test_license_provider_exception_returns_502(self) -> None:
        media = PlaybackMedia(
            session_id="session-1",
            streams=(
                PlaybackStream(
                    url="https://cdn.example.com/manifest.mpd",
                    content_type="application/dash+xml",
                    drm=DrmInfo(
                        system=DrmSystem.WIDEVINE,
                        license_url="https://drm.example.com/license",
                    ),
                ),
            ),
            stream_type=StreamType.BUFFERED,
        )
        provider = FakeProvider(media, raise_on_license=True)

        coordinator = PlaybackCoordinator(
            session_id="session-1",
            transport_id="pid-1",
            provider=provider,
            provider_session=_provider_session(),
            player=FakePlayer(),
            player_server=cast("Any", FakePlayerServer()),
            broadcast_fn=lambda _namespace, _data: _noop_async(),
            send_fn=lambda _c, _s, _n, _d: _noop_async(),
            initial_volume=Volume(level=1.0, muted=False),
        )

        await coordinator.handle_media_message(
            cast("Any", object()),
            "sender-1",
            LoadRequest(
                request_id=1,
                media=MediaInfo(content_id="x", stream_type=StreamType.BUFFERED),
            ),
        )

        response = await coordinator.handle_license(
            LicenseRequest(session_id="session-1", route_id="r0", body=b"challenge")
        )

        assert response.status == 502
        assert response.body == b"provider license resolution failed"


async def _noop_async() -> None:
    return None
