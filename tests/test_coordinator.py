"""Tests for PlaybackCoordinator."""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING, Any, cast, override

import httpx

from castvibe import _namespace as ns
from castvibe._coordinator import PlaybackCoordinator
from castvibe._models import (
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
from castvibe.player import (
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
from castvibe.provider import (
    LaunchCredentials,
    Provider,
    ProviderSession,
    ReceiverContext,
)

if TYPE_CHECKING:
    from httpx import AsyncClient


class FakeProvider(Provider):
    def __init__(self, media: PlaybackMedia, *, use_forwarder: bool = False) -> None:
        self._media = media
        self._use_forwarder = use_forwarder
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
    ) -> None:
        _ = session
        _ = namespace
        _ = data

    @override
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> PlaybackMedia:
        _ = session
        _ = load_request
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

    def register_license_handler(self, session_id: str, handler: object) -> str:
        _ = handler
        self.register_calls.append(session_id)
        return f"http://127.0.0.1:8010/license/{session_id}"

    def unregister_license_handler(self, session_id: str) -> None:
        self.unregister_calls.append(session_id)


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
            data_dir=Path("/tmp/castvibe-tests/providers/fake"),
        ),
        send_custom=_send_custom,
        broadcast_custom=_broadcast_custom,
    )


class TestCoordinator:
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
        assert len(player.load_calls) == 1
        assert player.load_calls[0].streams[0].drm is not None
        assert (
            player.load_calls[0].streams[0].drm.license_url
            == "http://127.0.0.1:8010/license/session-1?route=r0"
        )

        namespace, payload = broadcast[0]
        assert namespace == ns.MEDIA
        assert payload["type"] == "MEDIA_STATUS"
        assert payload["status"][0]["playerState"] == "BUFFERING"
        assert payload["status"][0]["currentTime"] == 5.0

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
        assert broadcast[-1][1]["status"] == []

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


async def _noop_async() -> None:
    return None
