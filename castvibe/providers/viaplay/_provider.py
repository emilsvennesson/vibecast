"""Bundled Viaplay provider.

Implements the Viaplay Cast receiver protocol including authentication
(persistent login, token login, device-code flow) and stream resolution.
"""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, override

if TYPE_CHECKING:
    from pathlib import Path

import castvibe._namespace as ns
from castvibe._models import (
    IdleReason,
    LoadFailedResponse,
    LoadRequest,
    MediaGetStatusRequest,
    MediaInfo,
    MediaRequest,
    MediaSetVolumeRequest,
    MediaStatus,
    MediaStatusResponse,
    MediaStopRequest,
    PauseRequest,
    PlayerState,
    PlayRequest,
    SeekRequest,
    Volume,
)
from castvibe.provider import (
    DefaultMediaEventHandler,
    LaunchCredentials,
    MediaEventHandler,
    MediaLoadInfo,
    Provider,
    ProviderSession,
)
from castvibe.providers.viaplay._api import ViaplayAPI
from castvibe.providers.viaplay._models import (
    AuthorizationRequiredMessage,
    ReceiverStateMessage,
    SessionOkMessage,
    UserProfile,
    ViaplayReceiverState,
)

log = logging.getLogger("castvibe.viaplay")

# Viaplay custom namespace (provider-specific, not part of platform protocol).
_NS_VIAPLAY = "urn:x-cast:tv.viaplay.chromecast"

# Bitmask for supported media commands (PAUSE | SEEK | STREAM_VOLUME |
# STREAM_MUTE | SKIP_FORWARD | SKIP_BACKWARD and more).
_SUPPORTED_MEDIA_COMMANDS = 274447


# ---------------------------------------------------------------------------
# Per-session state
# ---------------------------------------------------------------------------


@dataclass
class _ViaplayState:
    """Mutable per-session state for a single Viaplay app session."""

    api: ViaplayAPI
    credentials: LaunchCredentials
    broadcast: Any  # stored broadcast_custom closure
    authenticated: bool = False
    auth_pending: bool = False

    # User / setup info
    user_id: str = ""
    profile_id: str = ""
    user_display_name: str = ""
    country_code: str = "se"
    receiver_name: str = ""
    receiver_language_code: str = "en"

    # Media state
    media_session_id: int = 1
    player_state: PlayerState = PlayerState.IDLE
    current_media: MediaInfo | None = None
    current_time: float = 0.0
    stream_url: str = ""
    stream_content_type: str = ""

    # Background tasks
    auth_task: asyncio.Task[None] | None = field(default=None, repr=False)
    poll_task: asyncio.Task[None] | None = field(default=None, repr=False)


# ---------------------------------------------------------------------------
# ViaplayProvider
# ---------------------------------------------------------------------------


class ViaplayProvider(Provider):
    """Full Viaplay Cast receiver provider.

    Args:
        media_handler: Optional handler for media events.  When a stream is
            resolved the handler's :meth:`~MediaEventHandler.on_load` method
            is called with a :class:`~castvibe.provider.MediaLoadInfo`.
        data_dir: Directory for cookie / device-id persistence.
    """

    _APP_IDS = frozenset({"6313CF39", "2DB7CC49"})
    _NAMESPACES = frozenset({_NS_VIAPLAY})

    def __init__(
        self,
        *,
        media_handler: MediaEventHandler | None = None,
        data_dir: Path | None = None,
    ) -> None:
        self._media_handler: MediaEventHandler = (
            media_handler or DefaultMediaEventHandler()
        )
        self._data_dir = data_dir
        self._sessions: dict[str, _ViaplayState] = {}

    # -- Provider ABC --------------------------------------------------------

    @override
    def app_ids(self) -> frozenset[str]:
        return self._APP_IDS

    @override
    def display_name(self) -> str:
        return "Viaplay"

    @override
    def namespaces(self) -> frozenset[str]:
        return self._NAMESPACES

    @override
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        api = ViaplayAPI(data_dir=self._data_dir)
        state = _ViaplayState(
            api=api,
            credentials=credentials,
            broadcast=session.broadcast_custom,
        )
        self._sessions[session.session_id] = state
        log.info("viaplay session %s launched", session.session_id)

    @override
    async def on_sender_connected(
        self,
        session: ProviderSession,
        sender_id: str,
    ) -> None:
        state = self._sessions.get(session.session_id)
        if state is None:
            return

        # Broadcast empty MEDIA_STATUS (matches capture behavior)
        empty_status = MediaStatusResponse(request_id=0, status=[])
        await session.broadcast_custom(
            ns.MEDIA, empty_status.model_dump(exclude_none=True)
        )

        # Broadcast RECEIVER_STATE with status=IDLE
        await self._broadcast_receiver_state(state, "IDLE")

    @override
    async def on_message(
        self,
        session: ProviderSession,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        state = self._sessions.get(session.session_id)
        if state is None:
            return

        msg_type = data.get("type", "")

        if msg_type == "SETUP_INFO":
            self._handle_setup_info(session, state, data)
        elif msg_type == "AUTHORIZATION_DONE":
            state.auth_task = asyncio.create_task(
                self._complete_device_auth(session, state)
            )
        else:
            log.debug("unhandled viaplay message type: %s", msg_type)

    @override
    async def on_media_message(
        self,
        session: ProviderSession,
        message: MediaRequest,
    ) -> None:
        state = self._sessions.get(session.session_id)
        if state is None:
            return

        match message:
            case LoadRequest():
                state.auth_task = asyncio.create_task(
                    self._handle_load(session, state, message)
                )
            case PlayRequest():
                state.player_state = PlayerState.PLAYING
                await self._send_media_status(session, state, message.request_id)
                await self._media_handler.on_play(session.session_id)
            case PauseRequest():
                state.player_state = PlayerState.PAUSED
                await self._send_media_status(session, state, message.request_id)
                await self._media_handler.on_pause(session.session_id)
            case SeekRequest():
                state.current_time = message.current_time
                await self._send_media_status(session, state, message.request_id)
                await self._media_handler.on_seek(
                    session.session_id, message.current_time
                )
            case MediaGetStatusRequest():
                await self._send_media_status(session, state, message.request_id)
            case MediaStopRequest():
                state.player_state = PlayerState.IDLE
                state.current_media = None
                state.stream_url = ""
                empty = MediaStatusResponse(request_id=message.request_id, status=[])
                await session.broadcast_custom(
                    ns.MEDIA, empty.model_dump(exclude_none=True)
                )
                await self._media_handler.on_stop(session.session_id)
            case MediaSetVolumeRequest():
                await self._media_handler.on_volume(
                    session.session_id,
                    message.volume.level,
                    message.volume.muted,
                )
                await self._send_media_status(session, state, message.request_id)
            case _:
                # QueueLoadRequest or any unknown — send empty status
                empty = MediaStatusResponse(request_id=message.request_id, status=[])
                await session.send_custom(ns.MEDIA, empty.model_dump(exclude_none=True))

    @override
    async def on_stop(self, session: ProviderSession) -> None:
        state = self._sessions.pop(session.session_id, None)
        if state is None:
            return
        # Cancel background tasks
        for task in (state.auth_task, state.poll_task):  # noqa: SLF001
            if task is not None and not task.done():
                _ = task.cancel()
        await state.api.close()
        log.info("viaplay session %s stopped", session.session_id)

    @override
    async def update_playback(
        self,
        session_id: str,
        player_state: PlayerState,
        current_time: float = 0.0,
        idle_reason: IdleReason | None = None,
    ) -> None:
        """Push playback state from an external player (e.g. Kodi)."""
        state = self._sessions.get(session_id)
        if state is None:
            return
        state.player_state = player_state
        state.current_time = current_time

        status = self._build_media_status(state, idle_reason=idle_reason)
        response = MediaStatusResponse(request_id=0, status=[status])
        await state.broadcast(ns.MEDIA, response.model_dump(exclude_none=True))

    # -- SETUP_INFO handler --------------------------------------------------

    def _handle_setup_info(
        self,
        session: ProviderSession,
        state: _ViaplayState,
        data: dict[str, Any],
    ) -> None:
        """Process ``SETUP_INFO`` and spawn the auth flow."""
        state.user_id = data.get("userId", "")
        state.profile_id = data.get("profileId", "")
        state.country_code = data.get("countryCode", "se")
        state.receiver_name = data.get("receiverName", "")
        state.receiver_language_code = data.get("receiverLanguageCode", "en")

        content_root = data.get("contentRoot", "")
        state.api.set_setup_info(
            content_root=content_root,
            country_code=state.country_code,
            user_id=state.user_id,
            profile_id=state.profile_id,
        )

        state.auth_task = asyncio.create_task(self._run_auth_flow(session, state))

    # -- Auth flow -----------------------------------------------------------

    async def _run_auth_flow(
        self,
        session: ProviderSession,
        state: _ViaplayState,
    ) -> None:
        """Three-step authentication cascade."""
        try:
            result = await state.api.check_session()

            # 1) Check if already authenticated
            if result.user and result.user.user_id == state.user_id:
                state.authenticated = True
                state.user_display_name = (
                    f"{result.user.first_name} {result.user.last_name}".strip()
                )
                await self._send_session_ok(session, state)
                return

            # 2) Try persistent login
            pl_url = result.links.get("viaplay:persistentLogin")
            if pl_url:
                ok = await state.api.persistent_login(pl_url)
                if ok:
                    recheck = await state.api.check_session()
                    if recheck.user and recheck.user.user_id == state.user_id:
                        state.authenticated = True
                        state.user_display_name = f"{recheck.user.first_name} {recheck.user.last_name}".strip()
                        await self._send_session_ok(session, state)
                        return
                    # Login returned 200 but session check failed — proceed anyway
                    state.authenticated = True
                    await self._send_session_ok(session, state)
                    return

            # 3) Try token login
            token = state.credentials.credentials or ""
            tl_url = result.links.get("viaplay:tokenLogin")
            if token and tl_url:
                ok = await state.api.token_login(tl_url, token)
                if ok:
                    recheck = await state.api.check_session()
                    if recheck.user and recheck.user.user_id == state.user_id:
                        state.authenticated = True
                        state.user_display_name = f"{recheck.user.first_name} {recheck.user.last_name}".strip()
                        await self._send_session_ok(session, state)
                        return
                    state.authenticated = True
                    await self._send_session_ok(session, state)
                    return

            # 4) Device code fallback
            await self._start_device_auth(session, state, result)

        except Exception:
            log.exception("auth flow failed")

    async def _start_device_auth(
        self,
        session: ProviderSession,
        state: _ViaplayState,
        root_result: Any,
    ) -> None:
        """Initiate device-code authorization flow."""
        auth_info = await state.api.get_device_authorization(root_result)
        state.auth_pending = True

        activate_url = auth_info.activate_url
        if activate_url and auth_info.user_code:
            sep = "&" if "?" in activate_url else "?"
            activate_url = f"{activate_url}{sep}userCode={auth_info.user_code}"

        # Build receiver state for AUTHORIZATION_REQUIRED
        rs = self._build_viaplay_receiver_state(
            state,
            status="AUTHORIZATION_REQUIRED",
            authorization_url=activate_url,
            user_code=auth_info.user_code,
        )

        auth_msg = AuthorizationRequiredMessage(receiver_state=rs)
        await session.broadcast_custom(
            _NS_VIAPLAY, auth_msg.model_dump(exclude_none=True)
        )

        state_msg = ReceiverStateMessage(receiver_state=rs)
        await session.broadcast_custom(
            _NS_VIAPLAY, state_msg.model_dump(exclude_none=True)
        )

        # Start polling
        state.poll_task = asyncio.create_task(
            self._poll_for_authorization(session, state, auth_info)
        )

    async def _poll_for_authorization(
        self,
        session: ProviderSession,
        state: _ViaplayState,
        auth_info: Any,
    ) -> None:
        """Poll every 3s for up to 5 minutes."""
        timeout = 300  # 5 minutes
        interval = 3
        elapsed = 0.0

        while elapsed < timeout and not state.authenticated:
            await asyncio.sleep(interval)
            elapsed += interval
            try:
                activated = await state.api.poll_authorized(
                    auth_info,
                    auth_info.device_token,
                    auth_info.user_code,
                )
                if activated:
                    await self._complete_device_auth(session, state)
                    return
            except Exception:
                log.debug("poll authorized error", exc_info=True)

        if not state.authenticated:
            log.warning("device auth timed out after %ds", timeout)
            state.auth_pending = False

    async def _complete_device_auth(
        self,
        session: ProviderSession,
        state: _ViaplayState,
    ) -> None:
        """Finalize device-code authentication."""
        if state.authenticated:
            return

        try:
            result = await state.api.check_session()
            state.authenticated = True
            state.auth_pending = False
            if result.user:
                state.user_display_name = (
                    f"{result.user.first_name} {result.user.last_name}".strip()
                )
            await self._send_session_ok(session, state)
        except Exception:
            log.exception("complete device auth failed")

    # -- LOAD handler --------------------------------------------------------

    async def _handle_load(
        self,
        session: ProviderSession,
        state: _ViaplayState,
        load_req: LoadRequest,
    ) -> None:
        """Resolve stream URL and notify external player."""
        # Wait for auth (up to 30s)
        for _ in range(30):
            if state.authenticated:
                break
            await asyncio.sleep(1)

        if not state.authenticated:
            fail = LoadFailedResponse(
                request_id=load_req.request_id,
                reason="NOT_AUTHENTICATED",
            )
            await session.send_custom(ns.MEDIA, fail.model_dump(exclude_none=True))
            return

        # Extract play URL from customData
        custom = load_req.custom_data or {}
        play_url = custom.get("playUrl") or custom.get("contentUrl") or ""
        if not play_url:
            fail = LoadFailedResponse(
                request_id=load_req.request_id,
                reason="NO_PLAY_URL",
            )
            await session.send_custom(ns.MEDIA, fail.model_dump(exclude_none=True))
            return

        # Resolve stream
        try:
            stream_info = await state.api.fetch_stream(play_url)
        except Exception:
            log.exception("stream resolution failed for %s", play_url)
            fail = LoadFailedResponse(
                request_id=load_req.request_id,
                reason="STREAM_FETCH_FAILED",
            )
            await session.send_custom(ns.MEDIA, fail.model_dump(exclude_none=True))
            return

        # Update state
        state.stream_url = stream_info.url
        state.stream_content_type = stream_info.content_type
        state.player_state = PlayerState.BUFFERING
        state.current_time = load_req.current_time
        state.current_media = load_req.media

        # Extract metadata
        title: str | None = None
        if load_req.media.metadata:
            title = load_req.media.metadata.title

        stream_type = load_req.media.stream_type

        # Send MEDIA_STATUS (BUFFERING)
        await self._send_media_status(session, state, load_req.request_id)

        # Broadcast RECEIVER_STATE (PLAYING)
        await self._broadcast_receiver_state(state, "PLAYING")

        # Notify external player
        info = MediaLoadInfo(
            session_id=session.session_id,
            stream_url=stream_info.url,
            content_type=stream_info.content_type,
            stream_type=stream_type,
            title=title,
            duration=load_req.media.duration,
            autoplay=load_req.autoplay,
            start_time=load_req.current_time,
            custom_data=custom,
        )
        await self._media_handler.on_load(info)

    # -- Message helpers -----------------------------------------------------

    async def _send_session_ok(
        self,
        session: ProviderSession,
        state: _ViaplayState,
    ) -> None:
        rs = self._build_viaplay_receiver_state(state, status="IDLE")
        msg = SessionOkMessage(
            user_id=state.user_id,
            profile_id=state.profile_id,
            user_display_name=state.user_display_name or None,
            receiver_state=rs,
        )
        await session.broadcast_custom(_NS_VIAPLAY, msg.model_dump(exclude_none=True))

    async def _broadcast_receiver_state(
        self,
        state: _ViaplayState,
        status: str,
    ) -> None:
        rs = self._build_viaplay_receiver_state(state, status=status)
        msg = ReceiverStateMessage(receiver_state=rs)
        await state.broadcast(_NS_VIAPLAY, msg.model_dump(exclude_none=True))

    async def _send_media_status(
        self,
        session: ProviderSession,
        state: _ViaplayState,
        request_id: int,
    ) -> None:
        status = self._build_media_status(state)
        response = MediaStatusResponse(request_id=request_id, status=[status])
        await session.broadcast_custom(ns.MEDIA, response.model_dump(exclude_none=True))

    # -- State builders ------------------------------------------------------

    def _build_viaplay_receiver_state(
        self,
        state: _ViaplayState,
        *,
        status: str = "IDLE",
        authorization_url: str | None = None,
        user_code: str | None = None,
    ) -> ViaplayReceiverState:
        return ViaplayReceiverState(
            status=status,
            is_scrubbable=status == "PLAYING",
            pne_in_progress=False,
            user_id=state.user_id or None,
            user_profile=UserProfile(id=state.profile_id or None)
            if state.profile_id
            else None,
            user_display_name=state.user_display_name or None,
            country_code=state.country_code,
            receiver_name=state.receiver_name,
            receiver_language_code=state.receiver_language_code,
            authorization_url=authorization_url,
            user_code=user_code,
        )

    def _build_media_status(
        self,
        state: _ViaplayState,
        *,
        idle_reason: IdleReason | None = None,
    ) -> MediaStatus:
        media: MediaInfo | None = None
        if state.current_media and state.stream_url:
            media = MediaInfo(
                content_id=state.stream_url,
                content_type=state.stream_content_type,
                stream_type=state.current_media.stream_type,
                metadata=state.current_media.metadata,
                duration=state.current_media.duration,
            )
        return MediaStatus(
            media_session_id=state.media_session_id,
            media=media,
            player_state=state.player_state,
            current_time=state.current_time,
            supported_media_commands=_SUPPORTED_MEDIA_COMMANDS,
            volume=Volume(level=1.0, muted=False),
            idle_reason=idle_reason,
        )


__all__ = ["ViaplayProvider"]
