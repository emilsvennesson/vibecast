"""Per-session playback coordinator mediating sender, provider, and player."""

from __future__ import annotations

from dataclasses import replace
from typing import TYPE_CHECKING, Any

import vibecast._namespace as ns
from vibecast._log import get_logger
from vibecast._models import (
    IdleReason,
    LoadFailedResponse,
    LoadRequest,
    MediaGetStatusRequest,
    MediaInfo,
    MediaMetadata,
    MediaRequest,
    MediaSetVolumeRequest,
    MediaStatus,
    MediaStatusResponse,
    MediaStopRequest,
    PauseRequest,
    PlayerState,
    PlayRequest,
    QueueGetItemIdsRequest,
    QueueItemIdsResponse,
    SeekRequest,
    Volume,
)
from vibecast.player import (
    LicenseRequest,
    LicenseResponse,
    LicenseRoute,
    PlaybackError,
    PlaybackMedia,
    PlaybackState,
    PlaybackStream,
    Player,
    PlayerContext,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from vibecast._connection import Connection
    from vibecast._player_server import PlayerServer
    from vibecast.provider import Provider, ProviderSession

log = get_logger("coordinator")

# Cast SDK ``Command`` enum bitmask for ``MediaStatus.supportedMediaCommands``.
# LOAD, PLAY, and STOP are always implicitly supported.
#
#   PAUSE            1
#   SEEK             2
#   STREAM_VOLUME    4
#   STREAM_MUTE      8
#   SKIP_FORWARD    16
#   SKIP_BACKWARD   32
#   QUEUE_NEXT      64
#   QUEUE_PREV     128
#   QUEUE_SHUFFLE  256
#   SKIP_AD        512
#   EDIT_TRACKS   4096
#   PLAYBACK_RATE 8192
#   LIKE         16384
#   DISLIKE      32768
#   FOLLOW       65536
#   UNFOLLOW    131072
#   STREAM_TRANSFER 262144
_SUPPORTED_MEDIA_COMMANDS = 1 | 2 | 4 | 8  # PAUSE | SEEK | STREAM_VOLUME | STREAM_MUTE
_HOP_BY_HOP_REQUEST_HEADERS = {
    "connection",
    "content-length",
    "host",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
}


class PlaybackCoordinator:
    """Session-scoped mediator for generic Cast media handling."""

    __slots__ = (
        "_broadcast_fn",
        "_current_media",
        "_current_time",
        "_idle_reason",
        "_license_routes",
        "_media_session_id",
        "_playback_media",
        "_player",
        "_player_context",
        "_player_server",
        "_player_state",
        "_provider",
        "_provider_session",
        "_send_fn",
        "_volume",
        "session_id",
        "transport_id",
    )

    def __init__(
        self,
        *,
        session_id: str,
        transport_id: str,
        provider: Provider,
        provider_session: ProviderSession,
        player: Player,
        player_server: PlayerServer | None,
        broadcast_fn: Callable[[str, dict[str, Any]], Awaitable[None]],
        send_fn: Callable[
            [Connection, str, str, dict[str, Any]],
            Awaitable[None],
        ],
        initial_volume: Volume,
    ) -> None:
        self.session_id = session_id
        self.transport_id = transport_id
        self._provider = provider
        self._provider_session = provider_session
        self._player = player
        self._player_server = player_server
        self._broadcast_fn = broadcast_fn
        self._send_fn = send_fn
        self._media_session_id = 1
        self._license_routes: dict[str, LicenseRoute] = {}
        self._player_state = PlayerState.IDLE
        self._current_time = 0.0
        self._current_media: MediaInfo | None = None
        self._playback_media: PlaybackMedia | None = None
        self._idle_reason: IdleReason | None = None
        self._volume = initial_volume.model_copy(deep=True)
        self._player_context = PlayerContext(
            session_id=session_id,
            report_state=self.on_state_report,
            report_error=self.on_error_report,
        )

    async def handle_media_message(
        self,
        connection: Connection,
        sender_id: str,
        message: MediaRequest,
    ) -> None:
        """Handle media namespace requests for this app session."""
        match message:
            case LoadRequest():
                await self._handle_load(connection, sender_id, message)
            case PlayRequest():
                self._player_state = PlayerState.PLAYING
                self._idle_reason = None
                await self._broadcast_media_status(message.request_id)
                try:
                    await self._player.on_play(self._player_context)
                except Exception:
                    log.warning(
                        "player on_play failed for session %s",
                        self.session_id,
                        exc_info=True,
                    )
                await self._notify_provider()
            case PauseRequest():
                self._player_state = PlayerState.PAUSED
                self._idle_reason = None
                await self._broadcast_media_status(message.request_id)
                try:
                    await self._player.on_pause(self._player_context)
                except Exception:
                    log.warning(
                        "player on_pause failed for session %s",
                        self.session_id,
                        exc_info=True,
                    )
                await self._notify_provider()
            case SeekRequest():
                self._current_time = message.current_time
                self._idle_reason = None
                await self._broadcast_media_status(message.request_id)
                try:
                    await self._player.on_seek(
                        self._player_context, message.current_time
                    )
                except Exception:
                    log.warning(
                        "player on_seek failed for session %s",
                        self.session_id,
                        exc_info=True,
                    )
                await self._notify_provider()
            case MediaStopRequest():
                self._set_idle_state(idle_reason=IdleReason.CANCELLED)
                await self._broadcast_empty_media_status(message.request_id)
                try:
                    await self._player.on_stop(self._player_context)
                except Exception:
                    log.warning(
                        "player on_stop failed for session %s",
                        self.session_id,
                        exc_info=True,
                    )
                self._clear_media()
                self._unregister_license_handler()
                await self._notify_provider()
            case MediaGetStatusRequest():
                await self._send_media_status_to_sender(
                    connection,
                    sender_id,
                    message.request_id,
                )
            case MediaSetVolumeRequest():
                fields_set = message.volume.model_fields_set
                if "level" in fields_set:
                    self._volume.level = message.volume.level
                if "muted" in fields_set:
                    self._volume.muted = message.volume.muted
                if "control_type" in fields_set:
                    self._volume.control_type = message.volume.control_type
                if "step_interval" in fields_set:
                    self._volume.step_interval = message.volume.step_interval

                await self._broadcast_media_status(message.request_id)
                try:
                    await self._player.on_volume(
                        self._player_context,
                        self._volume.level,
                        self._volume.muted,
                    )
                except Exception:
                    log.warning(
                        "player callback failed for volume session %s",
                        self.session_id,
                        exc_info=True,
                    )
                await self._notify_provider()
            case QueueGetItemIdsRequest():
                item_ids = (
                    [self._media_session_id]
                    if self._current_media is not None
                    and self._player_state is not PlayerState.IDLE
                    else []
                )
                response = QueueItemIdsResponse(
                    request_id=message.request_id,
                    item_ids=item_ids,
                    sequence_number=0,
                )
                await self._send_fn(
                    connection,
                    sender_id,
                    ns.MEDIA,
                    response.model_dump(exclude_none=True),
                )
            case _:
                response = MediaStatusResponse(request_id=message.request_id, status=[])
                await self._send_fn(
                    connection,
                    sender_id,
                    ns.MEDIA,
                    response.model_dump(exclude_none=True),
                )

    async def on_state_report(self, state: PlaybackState) -> None:
        """Apply an incoming player state report from the primary player."""
        self._player_state = state.player_state
        self._current_time = state.current_time
        self._idle_reason = state.idle_reason

        if state.duration is not None and self._current_media is not None:
            self._current_media = self._current_media.model_copy(
                update={"duration": state.duration}
            )
        if state.duration is not None and self._playback_media is not None:
            self._playback_media = replace(
                self._playback_media, duration=state.duration
            )

        if state.player_state is PlayerState.IDLE:
            self._unregister_license_handler()

        await self._broadcast_media_status(request_id=0)
        await self._notify_provider()

    async def on_error_report(self, error: PlaybackError) -> None:
        """Handle a player error report and translate it to IDLE/ERROR."""
        log.warning(
            "player error session=%s code=%s message=%s",
            self.session_id,
            error.code,
            error.message,
        )

        await self.on_state_report(
            PlaybackState(
                player_state=PlayerState.IDLE,
                current_time=self._current_time,
                duration=self._current_media.duration
                if self._current_media is not None
                else None,
                idle_reason=IdleReason.ERROR,
            )
        )

    async def handle_license(self, request: LicenseRequest) -> LicenseResponse:
        """Resolve one proxied DRM license request through provider/forwarder."""
        route_id = request.route_id
        if route_id is None:
            return LicenseResponse(status=400, body=b"missing license route")

        route = self._license_routes.get(route_id)
        if route is None:
            return LicenseResponse(status=404, body=b"unknown license route")

        return await self._provider.resolve_license(
            self._provider_session,
            request,
            route,
            self._forward_license_request,
        )

    async def send_current_status(
        self,
        connection: Connection,
        sender_id: str,
    ) -> None:
        """Send current MEDIA_STATUS to a reconnecting sender."""
        await self._send_media_status_to_sender(connection, sender_id, request_id=0)

    async def close(self) -> None:
        """Release coordinator resources on app-session teardown."""
        if self._playback_media is not None:
            try:
                await self._player.on_stop(self._player_context)
            except Exception:
                log.warning(
                    "player on_stop failed for session %s",
                    self.session_id,
                    exc_info=True,
                )
        self._clear_media()
        self._unregister_license_handler()

    async def _handle_load(
        self,
        connection: Connection,
        sender_id: str,
        request: LoadRequest,
    ) -> None:
        self._media_session_id += 1

        try:
            media = await self._provider.resolve_media(self._provider_session, request)
        except Exception:
            log.warning(
                "provider failed to resolve media for session %s",
                self.session_id,
                exc_info=True,
            )
            failed = LoadFailedResponse(
                request_id=request.request_id, reason="LOAD_FAILED"
            )
            await self._send_fn(
                connection,
                sender_id,
                ns.MEDIA,
                failed.model_dump(exclude_none=True),
            )
            return

        if media.session_id != self.session_id:
            media = replace(media, session_id=self.session_id)

        if not media.streams:
            log.warning("provider returned no streams for session %s", self.session_id)
            failed = LoadFailedResponse(
                request_id=request.request_id,
                reason="LOAD_FAILED",
            )
            await self._send_fn(
                connection,
                sender_id,
                ns.MEDIA,
                failed.model_dump(exclude_none=True),
            )
            return

        media = self._with_license_proxy(media)

        self._playback_media = media
        self._current_media = _build_media_info(media)
        self._current_time = media.start_time
        self._player_state = PlayerState.BUFFERING
        self._idle_reason = None

        await self._broadcast_media_status(request.request_id)

        try:
            await self._player.on_load(self._player_context, media)
        except Exception:
            log.warning(
                "player on_load failed for session %s", self.session_id, exc_info=True
            )
            self._set_idle_state(idle_reason=IdleReason.ERROR)
            self._clear_media()
            self._unregister_license_handler()

            failed = LoadFailedResponse(
                request_id=request.request_id,
                reason="PLAYER_LOAD_FAILED",
            )
            await self._send_fn(
                connection,
                sender_id,
                ns.MEDIA,
                failed.model_dump(exclude_none=True),
            )
            await self._broadcast_empty_media_status(request.request_id)
            await self._notify_provider()
            return

        await self._notify_provider()

    async def _send_media_status_to_sender(
        self,
        connection: Connection,
        sender_id: str,
        request_id: int,
    ) -> None:
        response = self._build_media_status_response(request_id)
        await self._send_fn(
            connection,
            sender_id,
            ns.MEDIA,
            response.model_dump(exclude_none=True),
        )

    async def _broadcast_media_status(self, request_id: int) -> None:
        response = self._build_media_status_response(request_id)
        await self._broadcast_fn(ns.MEDIA, response.model_dump(exclude_none=True))

    async def _broadcast_empty_media_status(self, request_id: int) -> None:
        response = MediaStatusResponse(request_id=request_id, status=[])
        await self._broadcast_fn(ns.MEDIA, response.model_dump(exclude_none=True))

    def _build_media_status_response(self, request_id: int) -> MediaStatusResponse:
        status = self._build_media_status()
        return MediaStatusResponse(
            request_id=request_id,
            status=[] if status is None else [status],
        )

    def _build_media_status(self) -> MediaStatus | None:
        if self._current_media is None:
            return None

        return MediaStatus(
            media_session_id=self._media_session_id,
            media=self._current_media,
            player_state=self._player_state,
            current_time=self._current_time,
            supported_media_commands=_SUPPORTED_MEDIA_COMMANDS,
            volume=self._volume.model_copy(deep=True),
            idle_reason=self._idle_reason,
        )

    async def _notify_provider(self) -> None:
        try:
            await self._provider.on_playback_update(
                self._provider_session,
                PlaybackState(
                    player_state=self._player_state,
                    current_time=self._current_time,
                    duration=self._current_media.duration
                    if self._current_media is not None
                    else None,
                    idle_reason=self._idle_reason,
                ),
            )
        except Exception:
            log.warning(
                "provider playback update failed for session %s",
                self.session_id,
                exc_info=True,
            )

    def _set_idle_state(self, *, idle_reason: IdleReason | None) -> None:
        self._player_state = PlayerState.IDLE
        self._current_time = 0.0
        self._idle_reason = idle_reason

    def _clear_media(self) -> None:
        self._current_media = None
        self._playback_media = None

    def _with_license_proxy(self, media: PlaybackMedia) -> PlaybackMedia:
        self._unregister_license_handler()
        if self._player_server is None:
            return media

        if not any(stream.drm is not None for stream in media.streams):
            return media

        proxy_url = self._player_server.register_license_handler(self.session_id, self)
        rewritten_streams: list[PlaybackStream] = []
        for index, stream in enumerate(media.streams):
            drm = stream.drm
            if drm is None:
                rewritten_streams.append(stream)
                continue

            route_id = f"r{index}"
            self._license_routes[route_id] = LicenseRoute(
                route_id=route_id,
                system=drm.system,
                upstream_url=drm.license_url,
                headers=dict(drm.headers),
            )

            proxied_url = f"{proxy_url}?route={route_id}"
            rewritten_streams.append(
                replace(
                    stream,
                    drm=replace(drm, license_url=proxied_url, headers={}),
                )
            )

        return replace(media, streams=tuple(rewritten_streams))

    def _unregister_license_handler(self) -> None:
        self._license_routes.clear()
        if self._player_server is None:
            return
        self._player_server.unregister_license_handler(self.session_id)

    async def _forward_license_request(
        self,
        request: LicenseRequest,
        route: LicenseRoute,
    ) -> LicenseResponse:
        headers = dict(route.headers)
        for key, value in request.headers.items():
            if key.lower() in _HOP_BY_HOP_REQUEST_HEADERS:
                continue
            if key not in headers:
                headers[key] = value

        if request.content_type:
            headers["Content-Type"] = request.content_type

        response = await self._provider_session.http_client.post(
            route.upstream_url,
            content=request.body,
            headers=headers,
        )
        return LicenseResponse(
            body=response.content,
            content_type=response.headers.get(
                "content-type", "application/octet-stream"
            ),
            status=response.status_code,
        )


def _build_media_info(media: PlaybackMedia) -> MediaInfo:
    if not media.streams:
        msg = "playback media contains no streams"
        raise RuntimeError(msg)
    primary_stream = media.streams[0]

    metadata = None
    if media.title or media.subtitle or media.images:
        metadata = MediaMetadata(
            title=media.title,
            subtitle=media.subtitle,
            images=list(media.images),
        )

    custom_data = media.custom_data or None
    return MediaInfo(
        content_id=primary_stream.url,
        content_type=primary_stream.content_type,
        stream_type=media.stream_type,
        metadata=metadata,
        duration=media.duration,
        custom_data=custom_data,
    )


__all__ = ["PlaybackCoordinator"]
