"""Per-session playback coordinator mediating sender, provider, and player."""

from __future__ import annotations

from dataclasses import dataclass, replace
from typing import TYPE_CHECKING, Any

import vibecast._namespace as ns
from vibecast._log import get_logger
from vibecast._manifest_proxy import (
    ManifestKind,
    ManifestProxyRequest,
    ManifestProxyResponse,
    default_manifest_content_type,
    infer_manifest_kind,
    manifest_route_suffix,
    normalize_manifest_bytes,
)
from vibecast._models import (
    ExtendedStatus,
    IdleReason,
    LoadFailedResponse,
    LoadRequest,
    MediaCategory,
    MediaCommand,
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
    RepeatMode,
    SeekRequest,
    StreamType,
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
from vibecast.provider import MediaResolveFailure, MediaResolveFailureCode

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Mapping

    from vibecast._connection import Connection
    from vibecast._player_server import PlayerServer
    from vibecast.provider import Provider, ProviderSession

log = get_logger("coordinator")

# Supported commands when IDLE (no PAUSE — nothing to pause).
_IDLE_COMMANDS = MediaCommand.SEEK | MediaCommand.STREAM_VOLUME

# Supported commands during active playback.
_ACTIVE_COMMANDS = (
    MediaCommand.PAUSE
    | MediaCommand.SEEK
    | MediaCommand.STREAM_VOLUME
    | MediaCommand.STREAM_MUTE
)

_LOADING_PLAYER_STATE = "LOADING"

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

_HOP_BY_HOP_RESPONSE_HEADERS = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
}

_MANIFEST_PROXY_BLOCKED_RESPONSE_HEADERS = {
    *_HOP_BY_HOP_RESPONSE_HEADERS,
    "content-encoding",
    "content-length",
    "content-type",
    "set-cookie",
}


@dataclass(slots=True, frozen=True)
class _ManifestRoute:
    route_id: str
    kind: ManifestKind
    upstream_url: str
    content_type: str


class PlaybackCoordinator:
    """Session-scoped mediator for generic Cast media handling."""

    __slots__ = (
        "_broadcast_fn",
        "_current_media",
        "_current_time",
        "_idle_reason",
        "_license_routes",
        "_manifest_routes",
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
        self._manifest_routes: dict[str, _ManifestRoute] = {}
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
                await self._broadcast_media_status(message.request_id)
                try:
                    await self._player.on_stop(self._player_context)
                except Exception:
                    log.warning(
                        "player on_stop failed for session %s",
                        self.session_id,
                        exc_info=True,
                    )
                self._clear_media()
                self._unregister_manifest_handler()
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
            self._unregister_manifest_handler()
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

        try:
            return await self._provider.resolve_license(
                self._provider_session,
                request,
                route,
                self._forward_license_request,
            )
        except Exception:
            log.warning(
                "provider failed to resolve license for session %s",
                self.session_id,
                exc_info=True,
            )
            return LicenseResponse(
                status=502, body=b"provider license resolution failed"
            )

    async def handle_manifest(
        self,
        request: ManifestProxyRequest,
    ) -> ManifestProxyResponse:
        """Resolve one proxied manifest request with normalization transforms."""
        route = self._manifest_routes.get(request.route_id)
        if route is None:
            return ManifestProxyResponse(
                status=404,
                body=b"unknown manifest route",
                content_type="text/plain",
            )

        headers = _filter_upstream_headers(request.headers)
        try:
            response = await self._provider_session.http_client.request(
                request.method,
                route.upstream_url,
                headers=headers,
            )
        except Exception:
            log.warning(
                "upstream manifest request failed for session %s route=%s",
                self.session_id,
                route.route_id,
                exc_info=True,
            )
            return ManifestProxyResponse(
                status=502,
                body=b"manifest request failed",
                content_type="text/plain",
            )

        content_type = (
            response.headers.get("content-type")
            or route.content_type
            or default_manifest_content_type(route.kind)
        )
        response_headers = _filter_upstream_response_headers(response.headers)

        if request.method.upper() == "HEAD":
            return ManifestProxyResponse(
                status=response.status_code,
                body=b"",
                content_type=content_type,
                headers=response_headers,
            )

        body = response.content
        if response.status_code < 400:
            try:
                body, content_type = normalize_manifest_bytes(
                    body,
                    upstream_url=route.upstream_url,
                    content_type=content_type,
                    provider_key=self._provider.provider_key(),
                )
            except Exception:
                log.warning(
                    "manifest normalization failed for session %s route=%s",
                    self.session_id,
                    route.route_id,
                    exc_info=True,
                )

        return ManifestProxyResponse(
            status=response.status_code,
            body=body,
            content_type=content_type,
            headers=response_headers,
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
        self._unregister_manifest_handler()
        self._unregister_license_handler()

    # -- load ----------------------------------------------------------------

    async def _handle_load(
        self,
        connection: Connection,
        sender_id: str,
        request: LoadRequest,
    ) -> None:
        self._media_session_id += 1

        # Phase 1: broadcast IDLE + LOADING with the original LOAD request
        # media info so senders get immediate feedback.
        loading_media = _build_loading_media_info(request)
        self._current_media = loading_media
        self._player_state = PlayerState.IDLE
        self._idle_reason = None
        self._current_time = 0.0
        await self._broadcast_loading_status(request.request_id, loading_media)

        # Phase 2: resolve media through the provider.
        try:
            resolved = await self._provider.resolve_media(
                self._provider_session, request
            )
        except Exception:
            await self._fail_load(
                connection,
                sender_id,
                request_id=request.request_id,
                failure=MediaResolveFailure(
                    code=MediaResolveFailureCode.INTERNAL_ERROR,
                    detail_code="PROVIDER_EXCEPTION",
                    message="provider threw during resolve_media",
                ),
            )
            log.warning(
                "provider failed to resolve media for session %s",
                self.session_id,
                exc_info=True,
            )
            return

        if isinstance(resolved, MediaResolveFailure):
            await self._fail_load(
                connection,
                sender_id,
                request_id=request.request_id,
                failure=resolved,
            )
            return

        media = resolved

        if media.session_id != self.session_id:
            media = replace(media, session_id=self.session_id)

        if not media.streams:
            await self._fail_load(
                connection,
                sender_id,
                request_id=request.request_id,
                failure=MediaResolveFailure(
                    code=MediaResolveFailureCode.INTERNAL_ERROR,
                    detail_code="INVALID_PROVIDER_MEDIA",
                    message="provider returned no playable streams",
                ),
            )
            return

        media = self._with_manifest_proxy(media)
        media = self._with_license_proxy(media)

        # Phase 3: broadcast IDLE + LOADING with fully resolved media info.
        self._playback_media = media
        self._current_media = _build_media_info(media)
        self._current_time = media.start_time
        await self._broadcast_loading_status(request.request_id, self._current_media)

        # Phase 4: hand off to player and transition to BUFFERING.
        self._player_state = PlayerState.BUFFERING
        self._idle_reason = None
        await self._broadcast_media_status(request.request_id)

        try:
            await self._player.on_load(self._player_context, media)
        except Exception:
            await self._fail_load(
                connection,
                sender_id,
                request_id=request.request_id,
                failure=MediaResolveFailure(
                    code=MediaResolveFailureCode.PLAYER_FAILURE,
                    detail_code="PLAYER_ON_LOAD_FAILED",
                    message="player.on_load raised",
                    retryable=True,
                ),
            )
            log.warning(
                "player on_load failed for session %s", self.session_id, exc_info=True
            )
            return

        await self._notify_provider()

    async def _fail_load(
        self,
        connection: Connection,
        sender_id: str,
        *,
        request_id: int,
        failure: MediaResolveFailure,
    ) -> None:
        if failure.message is not None:
            log.warning(
                "load failure session=%s reason=%s detail=%s retryable=%s message=%s",
                self.session_id,
                failure.code.value,
                failure.detail_code,
                failure.retryable,
                failure.message,
            )
        else:
            log.warning(
                "load failure session=%s reason=%s detail=%s retryable=%s",
                self.session_id,
                failure.code.value,
                failure.detail_code,
                failure.retryable,
            )

        self._set_idle_state(idle_reason=IdleReason.ERROR)
        self._clear_media()
        self._unregister_manifest_handler()
        self._unregister_license_handler()

        failed = LoadFailedResponse(
            request_id=request_id,
            reason=failure.code.value,
        )
        await self._send_fn(
            connection,
            sender_id,
            ns.MEDIA,
            failed.model_dump(exclude_none=True),
        )
        await self._broadcast_media_status(request_id)
        await self._notify_provider()

    async def _broadcast_loading_status(
        self,
        request_id: int,
        media: MediaInfo,
    ) -> None:
        """Broadcast an IDLE + LOADING extended status during media resolution."""
        status = MediaStatus(
            media_session_id=self._media_session_id,
            playback_rate=1.0,
            player_state=PlayerState.IDLE,
            current_time=0.0,
            supported_media_commands=_IDLE_COMMANDS,
            volume=self._volume.model_copy(deep=True),
            media=media,
            current_item_id=1,
            repeat_mode=RepeatMode.REPEAT_OFF,
            extended_status=ExtendedStatus(
                player_state=_LOADING_PLAYER_STATE,
                media=media,
                media_session_id=self._media_session_id,
            ),
        )
        response = MediaStatusResponse(request_id=request_id, status=[status])
        await self._broadcast_fn(ns.MEDIA, response.model_dump(exclude_none=True))

    # -- status helpers ------------------------------------------------------

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

    def _build_media_status_response(self, request_id: int) -> MediaStatusResponse:
        status = self._build_media_status()
        return MediaStatusResponse(
            request_id=request_id,
            status=[] if status is None else [status],
        )

    def _build_media_status(self) -> MediaStatus | None:
        if self._current_media is None and self._idle_reason is None:
            return None

        is_idle = self._player_state is PlayerState.IDLE
        is_active = self._player_state in {
            PlayerState.PLAYING,
            PlayerState.PAUSED,
            PlayerState.BUFFERING,
        }

        commands = _ACTIVE_COMMANDS if is_active else _IDLE_COMMANDS
        playback_rate = 1.0 if self._player_state is PlayerState.PLAYING else 0.0

        return MediaStatus(
            media_session_id=self._media_session_id,
            media=self._current_media if not is_idle else None,
            player_state=self._player_state,
            current_time=self._current_time,
            supported_media_commands=commands,
            volume=self._volume.model_copy(deep=True),
            idle_reason=self._idle_reason,
            playback_rate=playback_rate,
            current_item_id=1,
            repeat_mode=RepeatMode.REPEAT_OFF if is_active else None,
        )

    # -- provider notification -----------------------------------------------

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

    # -- state management ----------------------------------------------------

    def _set_idle_state(self, *, idle_reason: IdleReason | None) -> None:
        self._player_state = PlayerState.IDLE
        self._current_time = 0.0
        self._idle_reason = idle_reason

    def _clear_media(self) -> None:
        self._current_media = None
        self._playback_media = None

    # -- manifest proxy ------------------------------------------------------

    def _with_manifest_proxy(self, media: PlaybackMedia) -> PlaybackMedia:
        self._unregister_manifest_handler()
        if self._player_server is None:
            return media

        rewritten_streams: list[PlaybackStream] = []
        proxy_url: str | None = None

        for index, stream in enumerate(media.streams):
            kind = infer_manifest_kind(stream.content_type, stream.url)
            if kind is ManifestKind.UNKNOWN:
                rewritten_streams.append(stream)
                continue

            if proxy_url is None:
                proxy_url = self._player_server.register_manifest_handler(
                    self.session_id,
                    self,
                )

            route_id = f"m{index}"
            self._manifest_routes[route_id] = _ManifestRoute(
                route_id=route_id,
                kind=kind,
                upstream_url=stream.url,
                content_type=stream.content_type,
            )

            proxied_url = f"{proxy_url}/{route_id}{manifest_route_suffix(kind)}"
            rewritten_streams.append(replace(stream, url=proxied_url))

        return replace(media, streams=tuple(rewritten_streams))

    def _unregister_manifest_handler(self) -> None:
        self._manifest_routes.clear()
        if self._player_server is None:
            return
        self._player_server.unregister_manifest_handler(self.session_id)

    # -- license proxy -------------------------------------------------------

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
        normalized_header_names = {key.lower() for key in headers}
        for key, value in request.headers.items():
            lowered = key.lower()
            if lowered in _HOP_BY_HOP_REQUEST_HEADERS:
                continue
            if lowered not in normalized_header_names:
                headers[key] = value
                normalized_header_names.add(lowered)

        if request.content_type:
            headers["Content-Type"] = request.content_type

        response = await self._provider_session.http_client.post(
            route.upstream_url,
            content=request.body,
            headers=headers,
        )
        if response.status_code >= 400:
            raw_preview = response.content[:200]
            preview = raw_preview.decode("utf-8", errors="replace").replace("\n", " ")
            body_prefix = request.body[:16].hex()
            log.warning(
                "upstream license rejected: status=%d route=%s body_bytes=%d body_prefix=%s response=%r",
                response.status_code,
                route.route_id,
                len(request.body),
                body_prefix,
                preview,
            )

        return LicenseResponse(
            body=response.content,
            content_type=response.headers.get(
                "content-type", "application/octet-stream"
            ),
            status=response.status_code,
        )


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _build_loading_media_info(request: LoadRequest) -> MediaInfo:
    """Build a minimal ``MediaInfo`` from the original LOAD request.

    Used for the initial LOADING extended-status broadcast before the
    provider has resolved the actual stream.
    """
    return MediaInfo(
        content_id=request.media.content_id,
        content_type=request.media.content_type or "video/*",
        stream_type=StreamType.NONE,
        metadata=request.media.metadata,
        duration=0.0,
        media_category=MediaCategory.VIDEO,
    )


def _build_media_info(media: PlaybackMedia) -> MediaInfo:
    """Build a fully resolved ``MediaInfo`` from provider-resolved media."""
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

    content_id = media.content_id or primary_stream.url
    is_live = media.stream_type is StreamType.LIVE
    custom_data = media.custom_data or None

    return MediaInfo(
        content_id=content_id,
        content_url=primary_stream.url,
        content_type=primary_stream.content_type,
        stream_type=media.stream_type,
        metadata=metadata,
        duration=media.duration,
        custom_data=custom_data,
        media_category=MediaCategory.VIDEO,
        is_live_media=is_live if is_live else None,
    )


def _filter_upstream_headers(headers: Mapping[str, str]) -> dict[str, str]:
    filtered: dict[str, str] = {}
    for key, value in headers.items():
        if key.lower() in _HOP_BY_HOP_REQUEST_HEADERS:
            continue
        filtered[key] = value
    return filtered


def _filter_upstream_response_headers(headers: Mapping[str, str]) -> dict[str, str]:
    blocked = set(_MANIFEST_PROXY_BLOCKED_RESPONSE_HEADERS)
    blocked.update(_connection_header_tokens(headers))

    filtered: dict[str, str] = {}
    for key, value in headers.items():
        if key.lower() in blocked:
            continue
        filtered[key] = value
    return filtered


def _connection_header_tokens(headers: Mapping[str, str]) -> set[str]:
    tokens: set[str] = set()
    for key, value in headers.items():
        if key.lower() != "connection":
            continue
        for token in value.split(","):
            normalized = token.strip().lower()
            if normalized:
                tokens.add(normalized)
    return tokens


__all__ = ["PlaybackCoordinator"]
