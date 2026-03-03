"""Bundled Viaplay provider."""

from __future__ import annotations

import asyncio
import contextlib
import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, override

from vibecast.player import (
    DrmInfo,
    DrmSystem,
    LicenseRequest,
    LicenseResponse,
    LicenseRoute,
    PlaybackMedia,
    PlaybackState,
    PlaybackStream,
    PlayerState,
    StreamType,
)
from vibecast.provider import LaunchCredentials, LoadRequest, Provider, ProviderSession
from vibecast.providers.viaplay._api import (
    DeviceAuthInfo,
    SessionCheckResult,
    ViaplayAPI,
)
from vibecast.providers.viaplay._models import (
    AudioTrackState,
    AuthorizationDone,
    AuthorizationRequiredMessage,
    GotoIdle,
    PosDurMessage,
    ReceiverStateMessage,
    SessionOkMessage,
    SetupInfo,
    SubtitleState,
    UserProfile,
    ViaplayReceiverState,
    viaplay_request_adapter,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

log = logging.getLogger("vibecast.viaplay")

_NS_VIAPLAY = "urn:x-cast:tv.viaplay.chromecast"


@dataclass
class _ViaplayState:
    """Mutable per-session state for a single Viaplay app session."""

    api: ViaplayAPI
    credentials: LaunchCredentials
    broadcast: Callable[[str, dict[str, Any]], Awaitable[None]]
    authenticated: bool = False
    auth_pending: bool = False
    auth_event: asyncio.Event = field(default_factory=asyncio.Event)

    user_id: str = ""
    profile_id: str = ""
    user_display_name: str = ""
    country_code: str = ""
    receiver_name: str = ""
    receiver_language_code: str = ""

    current_product_url: str | None = None
    loading_product_url: str | None = None
    subtitle_active_language_code: str | None = None
    subtitle_enabled: bool | dict[str, Any] | None = True
    audio_active_track: str | None = None
    stream_type: StreamType = StreamType.BUFFERED
    playback_state: PlaybackState = field(
        default_factory=lambda: PlaybackState(player_state=PlayerState.IDLE)
    )

    auth_task: asyncio.Task[None] | None = field(default=None, repr=False)
    poll_task: asyncio.Task[None] | None = field(default=None, repr=False)


class ViaplayProvider(Provider):
    """Viaplay provider implementation."""

    _APP_IDS = frozenset({"6313CF39", "2DB7CC49"})
    _NAMESPACES = frozenset({_NS_VIAPLAY})

    def __init__(self) -> None:
        self._sessions: dict[str, _ViaplayState] = {}

    @override
    def app_ids(self) -> frozenset[str]:
        return self._APP_IDS

    @override
    def display_name(self) -> str:
        return "Viaplay"

    @override
    def icon_url(self) -> str | None:
        return "https://lh3.googleusercontent.com/qXqoFPVkEZBwm7f1Yo8_7Xjv8wVeqbBeI-HfbD_KHjt0aOJf5dP_kbyQKMB1stIc0HIywc__C_Qq2CKjsg"

    @override
    def namespaces(self) -> frozenset[str]:
        return self._NAMESPACES

    @override
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        api = ViaplayAPI(
            client=session.http_client,
            device_id=session.receiver.device_id,
            user_agent=session.receiver.user_agent,
        )
        self._sessions[session.session_id] = _ViaplayState(
            api=api,
            credentials=credentials,
            broadcast=session.broadcast_custom,
        )
        log.info("viaplay session %s launched", session.session_id)

    @override
    async def on_sender_connected(
        self,
        session: ProviderSession,
        sender_id: str,
    ) -> None:
        _ = sender_id
        state = self._sessions.get(session.session_id)
        if state is None:
            return
        await self._broadcast_receiver_state(
            state,
            self._receiver_status_from_player_state(state.playback_state.player_state),
        )

    @override
    async def on_message(
        self,
        session: ProviderSession,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        _ = namespace
        state = self._sessions.get(session.session_id)
        if state is None:
            return

        try:
            msg = viaplay_request_adapter.validate_python(data)
        except Exception:
            log.debug("unhandled viaplay message type: %s", data.get("type", ""))
            return

        match msg:
            case SetupInfo():
                await self._handle_setup_info(session, state, msg)
            case AuthorizationDone():
                if not msg.success:
                    log.debug("ignoring AUTHORIZATION_DONE with success=false")
                    return
                await _cancel_task(state.auth_task)
                state.auth_task = asyncio.create_task(
                    self._complete_device_auth(session, state)
                )
            case GotoIdle():
                state.playback_state = PlaybackState(player_state=PlayerState.IDLE)
                await self._broadcast_receiver_state(state, "IDLE")

    @override
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> PlaybackMedia:
        state = self._sessions.get(session.session_id)
        if state is None:
            msg = "unknown session"
            raise RuntimeError(msg)

        with contextlib.suppress(TimeoutError):
            _ = await asyncio.wait_for(state.auth_event.wait(), timeout=30)

        if not state.authenticated:
            msg = "NOT_AUTHENTICATED"
            raise RuntimeError(msg)

        custom_data = load_request.custom_data or {}
        templated_product_url = custom_data.get(
            "templatedproducturl"
        ) or custom_data.get("templatedProductUrl")
        if isinstance(templated_product_url, str) and templated_product_url:
            state.current_product_url = templated_product_url
        else:
            product_url = custom_data.get("producturl") or custom_data.get("productUrl")
            state.current_product_url = (
                product_url if isinstance(product_url, str) and product_url else None
            )
        state.loading_product_url = None

        subtitle_lang = custom_data.get("subtitleLanguageCode")
        state.subtitle_active_language_code = (
            subtitle_lang if isinstance(subtitle_lang, str) and subtitle_lang else None
        )

        audio_track = custom_data.get("audioTrackLanguageCode")
        state.audio_active_track = (
            audio_track if isinstance(audio_track, str) and audio_track else None
        )

        subtitle_enabled = custom_data.get("subtitleActive")
        if isinstance(subtitle_enabled, dict | bool):
            state.subtitle_enabled = subtitle_enabled

        play_url = custom_data.get("playUrl") or custom_data.get("contentUrl") or ""
        if not isinstance(play_url, str) or not play_url:
            msg = "NO_PLAY_URL"
            raise RuntimeError(msg)

        stream_info = await state.api.fetch_stream(play_url)

        resolved_stream_type = stream_info.stream_type or load_request.media.stream_type
        if resolved_stream_type is StreamType.NONE:
            resolved_stream_type = StreamType.BUFFERED

        resolved_duration = stream_info.duration
        if resolved_duration is None and load_request.media.duration:
            resolved_duration = load_request.media.duration

        metadata = load_request.media.metadata
        title = stream_info.title or (metadata.title if metadata else None)
        subtitle = metadata.subtitle if metadata else None
        images = tuple(metadata.images) if metadata else ()

        drm: DrmInfo | None = None
        if stream_info.drm_license_url:
            drm = DrmInfo(
                system=DrmSystem.WIDEVINE,
                license_url=stream_info.drm_license_url,
                headers=state.api.request_headers(),
            )

        streams: list[PlaybackStream] = []
        seen_urls: set[str] = set()

        def _add_stream(url: str) -> None:
            if not url or url in seen_urls:
                return
            seen_urls.add(url)
            streams.append(
                PlaybackStream(
                    url=url,
                    content_type=stream_info.content_type,
                    drm=drm,
                )
            )

        _add_stream(stream_info.url)
        for fallback_url in stream_info.fallback_urls:
            _add_stream(fallback_url)

        if not streams:
            msg = "NO_STREAM_URL"
            raise RuntimeError(msg)

        state.stream_type = resolved_stream_type

        return PlaybackMedia(
            session_id=session.session_id,
            streams=tuple(streams),
            stream_type=resolved_stream_type,
            content_id=load_request.media.content_id,
            title=title,
            subtitle=subtitle,
            images=images,
            duration=resolved_duration,
            autoplay=load_request.autoplay,
            start_time=load_request.current_time,
            custom_data=custom_data,
        )

    @override
    async def on_playback_update(
        self,
        session: ProviderSession,
        state: PlaybackState,
    ) -> None:
        internal = self._sessions.get(session.session_id)
        if internal is None:
            return
        internal.playback_state = state

        receiver_status = self._receiver_status_from_player_state(state.player_state)
        await self._broadcast_receiver_state(internal, receiver_status)
        if receiver_status == "CASTING":
            await self._broadcast_posdur(internal, state)

    @override
    async def resolve_license(
        self,
        session: ProviderSession,
        request: LicenseRequest,
        route: LicenseRoute,
        forward: Callable[[LicenseRequest, LicenseRoute], Awaitable[LicenseResponse]],
    ) -> LicenseResponse:
        if session.session_id not in self._sessions:
            return LicenseResponse(status=500, body=b"unknown session")
        return await forward(request, route)

    @override
    async def on_stop(self, session: ProviderSession) -> None:
        state = self._sessions.pop(session.session_id, None)
        if state is None:
            return
        for task in (state.auth_task, state.poll_task):
            await _cancel_task(task)
        log.info("viaplay session %s stopped", session.session_id)

    async def _handle_setup_info(
        self,
        session: ProviderSession,
        state: _ViaplayState,
        msg: SetupInfo,
    ) -> None:
        state.user_id = msg.user_id
        state.profile_id = msg.profile_id
        state.country_code = msg.country_code
        if msg.receiver_name:
            state.receiver_name = msg.receiver_name
        state.receiver_language_code = msg.receiver_language_code

        state.api.set_setup_info(
            content_root=msg.content_root,
            country_code=state.country_code,
            user_id=state.user_id,
            profile_id=state.profile_id,
        )

        await _cancel_task(state.auth_task)
        await _cancel_task(state.poll_task)
        state.authenticated = False
        state.auth_pending = False
        state.auth_event.clear()
        state.user_display_name = ""
        state.poll_task = None
        state.auth_task = asyncio.create_task(self._run_auth_flow(session, state))

    async def _run_auth_flow(
        self,
        session: ProviderSession,
        state: _ViaplayState,
    ) -> None:
        try:
            result = await state.api.check_session()

            if result.user and result.user.user_id == state.user_id:
                self._mark_authenticated(
                    state,
                    result.user.first_name,
                    result.user.last_name,
                )
                await self._send_session_ok(session, state)
                return

            persistent_login_url = result.persistent_login_url
            if persistent_login_url and await state.api.persistent_login(
                persistent_login_url
            ):
                recheck = await state.api.check_session()
                if recheck.user and recheck.user.user_id == state.user_id:
                    self._mark_authenticated(
                        state,
                        recheck.user.first_name,
                        recheck.user.last_name,
                    )
                    await self._send_session_ok(session, state)
                    return

            token = state.credentials.credentials or ""
            token_login_url = result.token_login_url
            if (
                token
                and token_login_url
                and await state.api.token_login(token_login_url, token)
            ):
                recheck = await state.api.check_session()
                if recheck.user and recheck.user.user_id == state.user_id:
                    self._mark_authenticated(
                        state,
                        recheck.user.first_name,
                        recheck.user.last_name,
                    )
                    await self._send_session_ok(session, state)
                    return

            await self._start_device_auth(session, state, result)
        except Exception:
            log.exception("auth flow failed")
            state.auth_event.set()

    @staticmethod
    def _mark_authenticated(
        state: _ViaplayState,
        first_name: str = "",
        last_name: str = "",
    ) -> None:
        state.authenticated = True
        if first_name or last_name:
            state.user_display_name = f"{first_name} {last_name}".strip()
        state.auth_event.set()

    async def _start_device_auth(
        self,
        session: ProviderSession,
        state: _ViaplayState,
        root_result: SessionCheckResult,
    ) -> None:
        auth_info = await state.api.get_device_authorization(root_result)
        state.auth_pending = True

        receiver_state = self._build_viaplay_receiver_state(
            state,
            status="AUTHORIZATION_REQUIRED",
            authorization_url=auth_info.activate_url,
            user_code=auth_info.user_code,
        )
        message = AuthorizationRequiredMessage(
            authorization_url=auth_info.activate_url or None,
            receiver_state=receiver_state,
        )
        await session.broadcast_custom(
            _NS_VIAPLAY, message.model_dump(exclude_none=True)
        )
        await session.broadcast_custom(
            _NS_VIAPLAY,
            ReceiverStateMessage(receiver_state=receiver_state).model_dump(
                exclude_none=True
            ),
        )

        await _cancel_task(state.poll_task)
        state.poll_task = asyncio.create_task(
            self._poll_for_authorization(session, state, auth_info)
        )

    async def _poll_for_authorization(
        self,
        session: ProviderSession,
        state: _ViaplayState,
        auth_info: DeviceAuthInfo,
    ) -> None:
        timeout = 300
        interval = 3
        elapsed = 0.0

        while elapsed < timeout and not state.authenticated:
            await asyncio.sleep(interval)
            elapsed += interval
            try:
                activated = await state.api.poll_authorized(auth_info)
                if activated:
                    await self._complete_device_auth(session, state)
                    if state.authenticated:
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
        if state.authenticated:
            return

        try:
            result = await state.api.check_session()
            if result.user is None or result.user.user_id != state.user_id:
                return

            self._mark_authenticated(
                state,
                result.user.first_name,
                result.user.last_name,
            )
            state.auth_pending = False
            await self._send_session_ok(session, state)
        except Exception:
            log.exception("complete device auth failed")

    async def _send_session_ok(
        self,
        session: ProviderSession,
        state: _ViaplayState,
    ) -> None:
        receiver_state = self._build_viaplay_receiver_state(state, status="IDLE")
        message = SessionOkMessage(
            user_id=state.user_id,
            profile_id=state.profile_id,
            user_display_name=state.user_display_name or None,
            receiver_state=receiver_state,
        )
        await session.broadcast_custom(
            _NS_VIAPLAY, message.model_dump(exclude_none=True)
        )

    async def _broadcast_receiver_state(
        self,
        state: _ViaplayState,
        status: str,
    ) -> None:
        receiver_state = self._build_viaplay_receiver_state(state, status=status)
        message = ReceiverStateMessage(receiver_state=receiver_state)
        await state.broadcast(_NS_VIAPLAY, message.model_dump(exclude_none=True))

    async def _broadcast_posdur(
        self,
        state: _ViaplayState,
        playback_state: PlaybackState,
    ) -> None:
        is_live = state.stream_type is StreamType.LIVE
        duration = playback_state.duration

        # LIVE streams send POSDUR even when duration is 0 (the seekable
        # window grows over time).  VOD streams only send POSDUR once
        # the duration is known.
        if not is_live and (duration is None or duration <= 0):
            return

        receiver_state = self._build_viaplay_receiver_state(state, status="CASTING")
        message = PosDurMessage(
            position=max(0, int(playback_state.current_time)),
            duration=max(0, int(duration)) if duration is not None else 0,
            receiver_state=receiver_state,
        )
        await state.broadcast(_NS_VIAPLAY, message.model_dump(exclude_none=True))

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
            is_scrubbable=True,
            pne_in_progress=False,
            user_id=state.user_id or None,
            user_profile=UserProfile(id=state.profile_id or None)
            if state.profile_id
            else None,
            user_display_name=state.user_display_name or None,
            country_code=state.country_code,
            receiver_name=state.receiver_name,
            receiver_language_code=state.receiver_language_code,
            current_product_url=state.current_product_url,
            loading_product_url=state.loading_product_url,
            authorization_url=authorization_url,
            user_code=user_code,
            subtitles=SubtitleState(
                active_language_code=state.subtitle_active_language_code,
                available_language_codes=[],
                enabled=state.subtitle_enabled,
            ),
            audio_tracks=AudioTrackState(
                active_audio_track=state.audio_active_track,
                available_audio_tracks=[],
            ),
        )

    @staticmethod
    def _receiver_status_from_player_state(player_state: PlayerState) -> str:
        if player_state in {
            PlayerState.PLAYING,
            PlayerState.PAUSED,
            PlayerState.BUFFERING,
        }:
            return "CASTING"
        return "IDLE"


async def _cancel_task(task: asyncio.Task[None] | None) -> None:
    if task is None or task.done():
        return
    _ = task.cancel()
    with contextlib.suppress(asyncio.CancelledError, Exception):
        await task


__all__ = ["ViaplayProvider", "_NS_VIAPLAY"]
