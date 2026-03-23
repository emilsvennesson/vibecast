"""aiohttp bridge relaying player commands to external renderers over WS/HTTP.

Architecture: PlaybackCoordinator → PlayerBridge (this module) → Renderer (browser/Kodi).

PlayerBridge is the default ``Player`` implementation.  It runs an aiohttp server
that serves the browser-based renderer page and exposes a ``/player`` WebSocket
endpoint.  External renderers connect over that WebSocket and exchange
``PlayerCommand`` / ``PlayerReport`` messages.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol, override

from aiohttp import WSCloseCode, WSMsgType, web
from pydantic import ValidationError

from vibecast._log import get_logger
from vibecast._models import PlayerState
from vibecast._playback.headers import (
    filter_upstream_headers,
    filter_upstream_response_headers,
)
from vibecast._playback.manifest_proxy import (
    ManifestProxyRequest,
    ManifestProxyResponse,
)
from vibecast._playback.player_web import player_web_page, player_web_script
from vibecast.player import (
    DrmPayload,
    ErrorReport,
    LicenseRequest,
    LicenseResponse,
    LoadCommand,
    PauseCommand,
    PlaybackError,
    PlaybackMedia,
    PlaybackMediaPayload,
    PlaybackState,
    PlaybackStream,
    PlaybackStreamPayload,
    PlayCommand,
    Player,
    PlayerContext,
    SeekCommand,
    StateReport,
    StopCommand,
    VolumeCommand,
    player_report_adapter,
)

log = get_logger("player_bridge")


class LicenseHandler(Protocol):
    """Session-scoped DRM license proxy callback."""

    async def handle_license(self, request: LicenseRequest) -> LicenseResponse: ...


class ManifestHandler(Protocol):
    """Session-scoped manifest proxy callback."""

    async def handle_manifest(
        self,
        request: ManifestProxyRequest,
    ) -> ManifestProxyResponse: ...


@dataclass(slots=True)
class _RendererConnection:
    ws: web.WebSocketResponse
    requested_role: str
    is_primary: bool = False


@dataclass(slots=True)
class _SessionSnapshot:
    ctx: PlayerContext
    media: PlaybackMedia
    state: PlaybackState


class PlayerBridge(Player):
    """HTTP/WebSocket bridge relaying player commands to external renderers."""

    __slots__ = (
        "_app",
        "_connections",
        "_host",
        "_license_handlers",
        "_manifest_handlers",
        "_player_path",
        "_port",
        "_primary",
        "_runner",
        "_serving_port",
        "_session_snapshots",
    )

    def __init__(self, host: str = "0.0.0.0", port: int = 8010) -> None:
        self._host = host
        self._port = port
        self._player_path = "/player"
        self._app: web.Application | None = None
        self._runner: web.AppRunner | None = None
        self._connections: list[_RendererConnection] = []
        self._primary: _RendererConnection | None = None
        self._license_handlers: dict[str, LicenseHandler] = {}
        self._manifest_handlers: dict[str, ManifestHandler] = {}
        self._session_snapshots: dict[str, _SessionSnapshot] = {}
        self._serving_port: int | None = None

    async def start(self) -> None:
        """Start the aiohttp server."""
        if self._runner is not None:
            return

        app = web.Application()
        _ = app.router.add_get("/", self._handle_web_player)
        _ = app.router.add_get("/index.html", self._handle_web_player)
        _ = app.router.add_get("/player.js", self._handle_web_player_script)
        _ = app.router.add_get(self._player_path, self._handle_ws)
        _ = app.router.add_post("/license/{session_id}", self._handle_license)
        _ = app.router.add_get(
            "/manifest/{session_id}/{route_path}",
            self._handle_manifest,
        )

        runner = web.AppRunner(app)
        await runner.setup()
        site = web.TCPSite(runner, self._host, self._port)
        await site.start()

        self._serving_port = _resolve_serving_port(runner) or self._port
        self._app = app
        self._runner = runner
        log.info(
            "player bridge started (host=%s, port=%d, web=http://%s:%d/)",
            self._host,
            self._serving_port,
            self._resolved_host,
            self._serving_port,
        )

    async def stop(self) -> None:
        """Stop the aiohttp server and close open WebSocket clients."""
        runner = self._runner
        if runner is None:
            return

        for connection in list(self._connections):
            if not connection.ws.closed:
                _ = await connection.ws.close(
                    code=WSCloseCode.GOING_AWAY,
                    message=b"Server shutdown",
                )
            self._remove_connection(connection)

        self._session_snapshots.clear()
        self._license_handlers.clear()
        self._manifest_handlers.clear()
        await runner.cleanup()
        self._runner = None
        self._app = None
        self._serving_port = None
        log.info("player bridge stopped")

    @property
    def serving_port(self) -> int | None:
        """Bound TCP port (available after start)."""
        return self._serving_port

    def register_license_handler(
        self,
        session_id: str,
        handler: LicenseHandler,
    ) -> str:
        """Register a license handler and return its proxy URL."""
        self._license_handlers[session_id] = handler
        port = self._serving_port if self._serving_port is not None else self._port
        return f"http://{self._resolved_host}:{port}/license/{session_id}"

    def unregister_license_handler(self, session_id: str) -> None:
        """Unregister a previously registered session license handler."""
        _ = self._license_handlers.pop(session_id, None)

    def register_manifest_handler(
        self,
        session_id: str,
        handler: ManifestHandler,
    ) -> str:
        """Register a manifest handler and return proxy URL prefix."""
        self._manifest_handlers[session_id] = handler
        port = self._serving_port if self._serving_port is not None else self._port
        return f"http://{self._resolved_host}:{port}/manifest/{session_id}"

    def unregister_manifest_handler(self, session_id: str) -> None:
        """Unregister a previously registered session manifest handler."""
        _ = self._manifest_handlers.pop(session_id, None)

    @override
    async def on_load(self, ctx: PlayerContext, media: PlaybackMedia) -> None:
        state = PlaybackState(
            player_state=PlayerState.BUFFERING,
            current_time=media.start_time,
            duration=media.duration,
        )
        self._session_snapshots[ctx.session_id] = _SessionSnapshot(
            ctx=ctx,
            media=media,
            state=state,
        )
        command = LoadCommand(session_id=ctx.session_id, media=_media_to_payload(media))
        await self._broadcast_command(command)

    @override
    async def on_play(self, ctx: PlayerContext) -> None:
        snapshot = self._session_snapshots.get(ctx.session_id)
        if snapshot is not None:
            snapshot.state = PlaybackState(
                player_state=PlayerState.PLAYING,
                current_time=snapshot.state.current_time,
                duration=snapshot.state.duration,
            )
        await self._broadcast_command(PlayCommand(session_id=ctx.session_id))

    @override
    async def on_pause(self, ctx: PlayerContext) -> None:
        snapshot = self._session_snapshots.get(ctx.session_id)
        if snapshot is not None:
            snapshot.state = PlaybackState(
                player_state=PlayerState.PAUSED,
                current_time=snapshot.state.current_time,
                duration=snapshot.state.duration,
            )
        await self._broadcast_command(PauseCommand(session_id=ctx.session_id))

    @override
    async def on_seek(self, ctx: PlayerContext, position: float) -> None:
        snapshot = self._session_snapshots.get(ctx.session_id)
        if snapshot is not None:
            snapshot.state = PlaybackState(
                player_state=snapshot.state.player_state,
                current_time=position,
                duration=snapshot.state.duration,
                idle_reason=snapshot.state.idle_reason,
            )
        await self._broadcast_command(
            SeekCommand(session_id=ctx.session_id, position=position)
        )

    @override
    async def on_stop(self, ctx: PlayerContext) -> None:
        _ = self._session_snapshots.pop(ctx.session_id, None)
        await self._broadcast_command(StopCommand(session_id=ctx.session_id))

    @override
    async def on_volume(self, ctx: PlayerContext, level: float, muted: bool) -> None:
        await self._broadcast_command(
            VolumeCommand(session_id=ctx.session_id, level=level, muted=muted)
        )

    @property
    def _resolved_host(self) -> str:
        if self._host in {"0.0.0.0", "::"}:
            return "127.0.0.1"
        return self._host

    async def _handle_ws(self, request: web.Request) -> web.WebSocketResponse:
        ws = web.WebSocketResponse()
        _ = await ws.prepare(request)

        role = request.query.get("role", "auto")
        if role not in {"auto", "primary", "observer"}:
            role = "auto"

        connection = _RendererConnection(ws=ws, requested_role=role)
        self._connections.append(connection)
        self._assign_primary(connection)
        await self._sync_connection(connection)

        try:
            async for message in ws:
                if message.type != WSMsgType.TEXT:
                    continue
                if not isinstance(message.data, str):
                    continue
                await self._handle_report(connection, message.data)
        finally:
            self._remove_connection(connection)

        return ws

    async def _handle_web_player(self, request: web.Request) -> web.Response:
        _ = request
        return web.Response(text=player_web_page(), content_type="text/html")

    async def _handle_web_player_script(self, request: web.Request) -> web.Response:
        _ = request
        return web.Response(
            text=player_web_script(),
            content_type="application/javascript",
        )

    async def _handle_report(
        self, connection: _RendererConnection, payload: str
    ) -> None:
        try:
            report = player_report_adapter.validate_json(payload)
        except ValidationError:
            log.warning("invalid player report payload", exc_info=True)
            return

        if not connection.is_primary:
            return

        match report:
            case StateReport():
                snapshot = self._session_snapshots.get(report.session_id)
                if snapshot is None:
                    return
                state = PlaybackState(
                    player_state=report.player_state,
                    current_time=report.current_time,
                    duration=report.duration,
                    idle_reason=report.idle_reason,
                )
                snapshot.state = state
                await snapshot.ctx.report_state(state)
            case ErrorReport():
                snapshot = self._session_snapshots.get(report.session_id)
                if snapshot is None:
                    return
                await snapshot.ctx.report_error(
                    PlaybackError(code=report.code, message=report.message)
                )

    def _assign_primary(self, connection: _RendererConnection) -> None:
        if connection.requested_role == "observer":
            return

        if self._primary is None:
            self._primary = connection
            connection.is_primary = True
            return

        if connection.requested_role == "primary":
            self._primary.is_primary = False
            self._primary = connection
            connection.is_primary = True

    def _remove_connection(self, connection: _RendererConnection) -> None:
        if connection in self._connections:
            self._connections.remove(connection)

        if connection is self._primary:
            self._primary = None
            connection.is_primary = False
            self._promote_primary()

    def _promote_primary(self) -> None:
        for candidate in self._connections:
            if candidate.requested_role == "observer":
                continue
            candidate.is_primary = True
            self._primary = candidate
            return

    async def _sync_connection(self, connection: _RendererConnection) -> None:
        for snapshot in self._session_snapshots.values():
            if snapshot.state.player_state is PlayerState.IDLE:
                continue

            commands: list[LoadCommand | SeekCommand | PlayCommand | PauseCommand] = [
                LoadCommand(
                    session_id=snapshot.ctx.session_id,
                    media=_media_to_payload(snapshot.media),
                )
            ]

            if snapshot.state.current_time > 0:
                commands.append(
                    SeekCommand(
                        session_id=snapshot.ctx.session_id,
                        position=snapshot.state.current_time,
                    )
                )

            if snapshot.state.player_state in {
                PlayerState.PLAYING,
                PlayerState.BUFFERING,
            }:
                commands.append(PlayCommand(session_id=snapshot.ctx.session_id))
            elif snapshot.state.player_state is PlayerState.PAUSED:
                commands.append(PauseCommand(session_id=snapshot.ctx.session_id))

            for command in commands:
                await self._send_command(connection, command)

    async def _broadcast_command(
        self,
        command: LoadCommand
        | PlayCommand
        | PauseCommand
        | SeekCommand
        | StopCommand
        | VolumeCommand,
    ) -> None:
        for connection in list(self._connections):
            await self._send_command(connection, command)

    async def _send_command(
        self,
        connection: _RendererConnection,
        command: LoadCommand
        | PlayCommand
        | PauseCommand
        | SeekCommand
        | StopCommand
        | VolumeCommand,
    ) -> None:
        if connection.ws.closed:
            self._remove_connection(connection)
            return

        try:
            await connection.ws.send_str(command.model_dump_json(exclude_none=True))
        except (ConnectionResetError, BrokenPipeError, OSError, RuntimeError):
            log.warning("failed to send player command", exc_info=True)
            self._remove_connection(connection)

    async def _handle_license(self, request: web.Request) -> web.Response:
        session_id = request.match_info["session_id"]
        handler = self._license_handlers.get(session_id)
        if handler is None:
            return web.Response(status=404)

        route_id = request.query.get("route")
        body = await request.read()
        content_type = request.headers.get("Content-Type", "")
        license_request = LicenseRequest(
            session_id=session_id,
            body=body,
            content_type=content_type,
            route_id=route_id,
            headers=_filter_request_headers(request),
        )

        try:
            response = await handler.handle_license(license_request)
        except Exception:
            log.warning("license request failed for %s", session_id, exc_info=True)
            return web.Response(status=500)

        return web.Response(
            body=response.body,
            status=response.status,
            headers={"Content-Type": response.content_type},
        )

    async def _handle_manifest(self, request: web.Request) -> web.Response:
        session_id = request.match_info["session_id"]
        handler = self._manifest_handlers.get(session_id)
        if handler is None:
            return web.Response(status=404)

        route_path = request.match_info["route_path"]
        route_id, _, _ = route_path.partition(".")
        if not route_id:
            return web.Response(status=400)

        manifest_request = ManifestProxyRequest(
            session_id=session_id,
            route_id=route_id,
            method=request.method,
            headers=_filter_request_headers(request),
        )

        try:
            response = await handler.handle_manifest(manifest_request)
        except Exception:
            log.warning("manifest request failed for %s", session_id, exc_info=True)
            return web.Response(status=500)

        headers = filter_upstream_response_headers(response.headers)
        headers["Content-Type"] = response.content_type

        if request.method == "HEAD":
            return web.Response(status=response.status, headers=headers)

        return web.Response(
            body=response.body,
            status=response.status,
            headers=headers,
        )


def _media_to_payload(media: PlaybackMedia) -> PlaybackMediaPayload:
    return PlaybackMediaPayload(
        streams=[_stream_to_payload(stream) for stream in media.streams],
        stream_type=media.stream_type,
        title=media.title,
        subtitle=media.subtitle,
        images=list(media.images),
        duration=media.duration,
        autoplay=media.autoplay,
        start_time=media.start_time,
        custom_data=dict(media.custom_data),
    )


def _stream_to_payload(stream: PlaybackStream) -> PlaybackStreamPayload:
    drm = stream.drm
    payload_drm = (
        None
        if drm is None
        else DrmPayload(
            system=drm.system,
            license_url=drm.license_url,
            headers=dict(drm.headers),
        )
    )
    return PlaybackStreamPayload(
        url=stream.url,
        content_type=stream.content_type,
        drm=payload_drm,
    )


def _filter_request_headers(request: web.Request) -> dict[str, str]:
    return filter_upstream_headers(request.headers)


def _resolve_serving_port(runner: web.AppRunner) -> int | None:
    addresses = runner.addresses
    if not addresses:
        return None

    host, port, *_ = addresses[0]
    _ = host
    return int(port)


__all__ = ["LicenseHandler", "ManifestHandler", "PlayerBridge"]
