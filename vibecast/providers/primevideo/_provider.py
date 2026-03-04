"""Bundled Amazon Prime Video provider."""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, cast, override
from urllib.parse import parse_qsl, urlsplit

from vibecast.player import (
    DrmInfo,
    DrmSystem,
    LicenseRequest,
    LicenseResponse,
    LicenseRoute,
    PlaybackMedia,
    PlaybackStream,
)
from vibecast.provider import (
    LaunchCredentials,
    LoadRequest,
    MediaResolveFailure,
    MediaResolveFailureCode,
    MediaResolveResult,
    ProviderMessageDisposition,
    ProviderSession,
    StatefulProvider,
    media_failure_from_exception,
)
from vibecast.providers.primevideo._api import PrimeVideoAPI
from vibecast.providers.primevideo._models import (
    AmIRegisteredError,
    AmIRegisteredMessage,
    AmIRegisteredResponseMessage,
    ApplySettingsMessage,
    ApplySettingsResponseMessage,
    PlaybackUrlSetPayload,
    PreloadMessage,
    PreloadResponseMessage,
    RegisterMessage,
    RegisterResponseMessage,
    prime_message_adapter,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

log = logging.getLogger("vibecast.primevideo")

_NS_PRIME = "urn:x-cast:com.amazon.primevideo.cast"
_DEFAULT_MARKETPLACE_ID = "A3K6Y4MI8GDYMT"
_DEFAULT_LOCALE = "en_US"
_DEFAULT_AUTH_BASE_URL = "https://api.amazon.co.uk"
_DEFAULT_HDCP_LEVEL = "1.4"
_DEFAULT_MAX_VIDEO_RESOLUTION = "1080p"
_DEFAULT_SUPPORTED_CODECS = ("H265", "H264")
_DEFAULT_DYNAMIC_RANGE_FORMATS = ("None",)
_DEFAULT_SUPPORTED_FRAME_RATES = ("Standard", "High")
_DEFAULT_SUPPORTED_SUBTITLE_FORMATS = ("TTMLv2", "DFXP")


@dataclass(slots=True)
class _TitlePlaybackState:
    playback_envelope: str
    correlation_id: str | None = None
    session_handoff_token: str | None = None


@dataclass(slots=True)
class _PrimeSessionState:
    api: PrimeVideoAPI
    marketplace_id: str = _DEFAULT_MARKETPLACE_ID
    locale: str = _DEFAULT_LOCALE
    actor_id: str | None = None
    device_id: str | None = None
    actor_access_token: str | None = None
    account_refresh_token: str | None = None
    title_state: dict[str, _TitlePlaybackState] = field(default_factory=dict)
    current_title_id: str | None = None


class PrimeVideoProvider(StatefulProvider[_PrimeSessionState]):
    """Amazon Prime Video provider implementation."""

    _APP_IDS = frozenset({"17608BC8"})
    _NAMESPACES = frozenset({_NS_PRIME})

    def __init__(self) -> None:
        super().__init__()
        self._default_marketplace_id = _DEFAULT_MARKETPLACE_ID
        self._default_locale = _DEFAULT_LOCALE
        self._auth_base_url = _DEFAULT_AUTH_BASE_URL
        self._hdcp_level = _DEFAULT_HDCP_LEVEL
        self._max_video_resolution = _DEFAULT_MAX_VIDEO_RESOLUTION
        self._supported_codecs = _DEFAULT_SUPPORTED_CODECS
        self._dynamic_range_formats = _DEFAULT_DYNAMIC_RANGE_FORMATS
        self._supported_frame_rates = _DEFAULT_SUPPORTED_FRAME_RATES
        self._supported_subtitle_formats = _DEFAULT_SUPPORTED_SUBTITLE_FORMATS

    @override
    def app_ids(self) -> frozenset[str]:
        return self._APP_IDS

    @override
    def display_name(self) -> str:
        return "Prime Video"

    @override
    def icon_url(self) -> str | None:
        return "https://lh3.googleusercontent.com/QYGuZRR5YakSPcLFA65pr9BSwrvCpOjcsWiRaMN58t8374iv1HxlRs1mNQm3o0MEq5jmwMtEarN2CLI"

    @override
    def namespaces(self) -> frozenset[str]:
        return self._NAMESPACES

    @override
    def provider_key(self) -> str:
        return "primevideo"

    @override
    def configure(self, config: dict[str, Any]) -> None:
        self._default_marketplace_id = _config_string(
            config,
            key="marketplace_id",
            default=self._default_marketplace_id,
            path="providers.primevideo",
        )
        self._default_locale = _config_string(
            config,
            key="locale",
            default=self._default_locale,
            path="providers.primevideo",
        )
        self._auth_base_url = _config_string(
            config,
            key="auth_base_url",
            default=self._auth_base_url,
            path="providers.primevideo",
        )
        self._hdcp_level = _config_string(
            config,
            key="hdcp_level",
            default=self._hdcp_level,
            path="providers.primevideo",
        )
        self._max_video_resolution = _config_string(
            config,
            key="max_video_resolution",
            default=self._max_video_resolution,
            path="providers.primevideo",
        )
        self._supported_codecs = tuple(
            _config_string_list(
                config,
                key="supported_codecs",
                default=self._supported_codecs,
                path="providers.primevideo",
            )
        )
        self._dynamic_range_formats = tuple(
            _config_string_list(
                config,
                key="dynamic_range_formats",
                default=self._dynamic_range_formats,
                path="providers.primevideo",
            )
        )
        self._supported_frame_rates = tuple(
            _config_string_list(
                config,
                key="supported_frame_rates",
                default=self._supported_frame_rates,
                path="providers.primevideo",
            )
        )
        self._supported_subtitle_formats = tuple(
            _config_string_list(
                config,
                key="supported_subtitle_formats",
                default=self._supported_subtitle_formats,
                path="providers.primevideo",
            )
        )

    @override
    async def create_session_state(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> _PrimeSessionState:
        state = _PrimeSessionState(
            api=PrimeVideoAPI(
                client=session.http_client,
                auth_base_url=self._auth_base_url,
                display_width=session.receiver.display_width,
                display_height=session.receiver.display_height,
                hdcp_level=self._hdcp_level,
                max_video_resolution=self._max_video_resolution,
                supported_codecs=self._supported_codecs,
                dynamic_range_formats=self._dynamic_range_formats,
                supported_frame_rates=self._supported_frame_rates,
                supported_subtitle_formats=self._supported_subtitle_formats,
            ),
            marketplace_id=self._default_marketplace_id,
            locale=self._default_locale,
        )
        state.device_id = session.receiver.device_id
        if credentials.credentials:
            state.actor_access_token = credentials.credentials
        return state

    @override
    async def on_message(
        self,
        session: ProviderSession,
        namespace: str,
        data: dict[str, Any],
    ) -> ProviderMessageDisposition:
        _ = namespace
        state = self.state_or_none(session)
        if state is None:
            return ProviderMessageDisposition.UNHANDLED

        message = self.parse_message(prime_message_adapter, data)
        if message is None:
            return ProviderMessageDisposition.UNHANDLED

        match message:
            case AmIRegisteredMessage():
                await self._handle_am_i_registered(session, state, message)
            case RegisterMessage():
                await self._handle_register(session, state, message)
            case ApplySettingsMessage():
                self._handle_apply_settings(state, message)
                await session.send_custom(
                    _NS_PRIME,
                    ApplySettingsResponseMessage().model_dump(exclude_none=True),
                )
            case PreloadMessage():
                self._handle_preload(state, message)
                await session.send_custom(
                    _NS_PRIME,
                    PreloadResponseMessage().model_dump(exclude_none=True),
                )

        return ProviderMessageDisposition.HANDLED

    @override
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> MediaResolveResult:
        state = self.state_or_none(session)
        if state is None:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.INTERNAL_ERROR,
                detail_code="MISSING_PROVIDER_SESSION_STATE",
            )

        token = state.actor_access_token
        if not token:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.AUTH_REQUIRED,
                detail_code="NOT_AUTHENTICATED",
            )

        title_id = load_request.media.content_id.strip()
        if not title_id:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.INVALID_REQUEST,
                detail_code="INVALID_CONTENT_ID",
            )

        route_data = self._preload_for_title(load_request, state, title_id)
        if not route_data.playback_envelope:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.MISSING_CONTEXT,
                detail_code="NO_PLAYBACK_ENVELOPE",
            )

        device_id = self._device_id(load_request.custom_data, state, session)
        marketplace_id = self._marketplace_id(load_request.custom_data, state)

        if route_data.correlation_id:
            try:
                refreshed = await state.api.refresh_playback_envelope(
                    token=token,
                    device_id=device_id,
                    marketplace_id=marketplace_id,
                    title_id=title_id,
                    correlation_id=route_data.correlation_id,
                )
                route_data.playback_envelope = refreshed.playback_envelope
                route_data.correlation_id = refreshed.correlation_id
            except Exception:
                log.debug("prime envelope refresh failed", exc_info=True)

        try:
            resources = await state.api.get_vod_playback_resources(
                token=token,
                device_id=device_id,
                marketplace_id=marketplace_id,
                title_id=title_id,
                playback_envelope=route_data.playback_envelope,
                locale=state.locale,
            )
            default_url_set_id, url_sets = state.api.extract_playback_url_sets(
                resources
            )
        except Exception as exc:
            return media_failure_from_exception(exc)

        ordered_sets = _ordered_url_sets(
            url_sets, default_url_set_id=default_url_set_id
        )
        if not ordered_sets:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.CONTENT_UNAVAILABLE,
                detail_code="NO_STREAM_URL",
            )

        license_url = state.api.widevine_license_url(
            device_id=device_id,
            marketplace_id=marketplace_id,
            title_id=title_id,
            locale=state.locale,
        )
        drm = DrmInfo(system=DrmSystem.WIDEVINE, license_url=license_url)

        streams = tuple(
            PlaybackStream(
                url=state.api.with_device_type_query(url_set.url),
                content_type="application/dash+xml",
                drm=drm,
            )
            for url_set in ordered_sets
        )

        route_data.session_handoff_token = (
            resources.sessionization.session_handoff_token
            if resources.sessionization
            else None
        )
        state.title_state[title_id] = route_data
        state.current_title_id = title_id
        state.device_id = device_id
        state.marketplace_id = marketplace_id

        metadata = load_request.media.metadata
        return PlaybackMedia(
            session_id=session.session_id,
            streams=streams,
            stream_type=self.normalize_stream_type(load_request.media.stream_type),
            content_id=title_id,
            title=metadata.title if metadata else None,
            subtitle=metadata.subtitle if metadata else None,
            images=tuple(metadata.images) if metadata else (),
            duration=load_request.media.duration
            if load_request.media.duration is not None
            and load_request.media.duration > 0
            else None,
            autoplay=load_request.autoplay,
            start_time=load_request.current_time,
            custom_data={
                **(load_request.media.custom_data or {}),
                **(load_request.custom_data or {}),
            },
        )

    @override
    async def resolve_license(
        self,
        session: ProviderSession,
        request: LicenseRequest,
        route: LicenseRoute,
        forward: Callable[[LicenseRequest, LicenseRoute], Awaitable[LicenseResponse]],
    ) -> LicenseResponse:
        _ = forward
        state = self.state_or_none(session)
        if state is None:
            return LicenseResponse(status=409, body=b"missing provider session state")

        token = state.actor_access_token
        if not token:
            return LicenseResponse(status=403, body=b"not authenticated")

        title_id = state.current_title_id or _title_id_from_url(route.upstream_url)
        if not title_id:
            return LicenseResponse(status=400, body=b"missing title id")

        title_state = state.title_state.get(title_id)
        if title_state is None or not title_state.playback_envelope:
            return LicenseResponse(status=409, body=b"missing playback envelope")

        if state.device_id is None:
            return LicenseResponse(status=500, body=b"missing device id")

        try:
            body = await state.api.get_widevine_license(
                token=token,
                device_id=state.device_id,
                marketplace_id=state.marketplace_id,
                title_id=title_id,
                playback_envelope=title_state.playback_envelope,
                session_handoff_token=title_state.session_handoff_token,
                challenge=request.body,
                locale=state.locale,
            )
        except Exception:
            log.exception("prime license request failed")
            return LicenseResponse(status=502, body=b"license request failed")

        return LicenseResponse(body=body)

    async def _handle_register(
        self,
        session: ProviderSession,
        state: _PrimeSessionState,
        message: RegisterMessage,
    ) -> None:
        if message.marketplace_id:
            state.marketplace_id = message.marketplace_id
        if message.device_id:
            state.device_id = message.device_id
        if message.actor_id:
            state.actor_id = message.actor_id

        if message.pre_authorized_link_code and state.actor_id:
            try:
                registered = await state.api.register_device(
                    link_code=message.pre_authorized_link_code,
                    device_id=message.device_id or session.receiver.device_id,
                )
                exchanged = await state.api.exchange_actor_token(
                    actor_id=state.actor_id,
                    account_refresh_token=registered.account_refresh_token,
                )
                state.account_refresh_token = exchanged.account_refresh_token
                state.actor_access_token = exchanged.actor_access_token
            except Exception:
                log.exception("prime register flow failed")

        await session.send_custom(
            _NS_PRIME,
            RegisterResponseMessage().model_dump(exclude_none=True),
        )

    async def _handle_am_i_registered(
        self,
        session: ProviderSession,
        state: _PrimeSessionState,
        message: AmIRegisteredMessage,
    ) -> None:
        if message.device_id:
            state.device_id = message.device_id

        response = AmIRegisteredResponseMessage()
        if not state.actor_access_token:
            response.error = AmIRegisteredError(
                code="NotRegistered",
                internal_name="NotRegistered",
                message=(
                    f"deviceId {message.device_id or state.device_id or session.receiver.device_id} "
                    "is not registered"
                ),
            )

        await session.send_custom(
            _NS_PRIME,
            response.model_dump(exclude_none=True),
        )

    @staticmethod
    def _handle_apply_settings(
        state: _PrimeSessionState,
        message: ApplySettingsMessage,
    ) -> None:
        locale = _extract_locale(message.settings)
        if locale:
            state.locale = locale
        if message.device_id:
            state.device_id = message.device_id

    @staticmethod
    def _handle_preload(state: _PrimeSessionState, message: PreloadMessage) -> None:
        if message.content_id is None:
            return
        envelope = message.playback_envelope
        if envelope is None:
            return
        state.title_state[message.content_id] = _TitlePlaybackState(
            playback_envelope=envelope.envelope,
            correlation_id=envelope.correlation_id,
        )

    @staticmethod
    def _preload_for_title(
        load_request: LoadRequest,
        state: _PrimeSessionState,
        title_id: str,
    ) -> _TitlePlaybackState:
        existing = state.title_state.get(title_id)
        if existing is not None:
            return existing

        payload = load_request.custom_data or {}
        playback_envelope = payload.get("playbackEnvelope")
        if isinstance(playback_envelope, dict):
            envelope_payload = cast("dict[str, Any]", playback_envelope)
            envelope = envelope_payload.get("envelope")
            correlation_id = envelope_payload.get("correlationId")
            if isinstance(envelope, str) and envelope:
                return _TitlePlaybackState(
                    playback_envelope=envelope,
                    correlation_id=correlation_id
                    if isinstance(correlation_id, str)
                    else None,
                )

        return _TitlePlaybackState(playback_envelope="")

    @staticmethod
    def _device_id(
        custom_data: dict[str, Any] | None,
        state: _PrimeSessionState,
        session: ProviderSession,
    ) -> str:
        if custom_data:
            raw = custom_data.get("deviceId")
            if isinstance(raw, str) and raw:
                return raw
        if state.device_id:
            return state.device_id
        return session.receiver.device_id

    @staticmethod
    def _marketplace_id(
        custom_data: dict[str, Any] | None,
        state: _PrimeSessionState,
    ) -> str:
        if custom_data:
            raw = custom_data.get("marketplaceId")
            if isinstance(raw, str) and raw:
                return raw
        return state.marketplace_id


def _config_string(
    config: dict[str, Any],
    *,
    key: str,
    default: str,
    path: str,
) -> str:
    value = config.get(key, default)
    if isinstance(value, str):
        return value
    msg = f"{path}.{key} must be a string"
    raise TypeError(msg)


def _config_string_list(
    config: dict[str, Any],
    *,
    key: str,
    default: tuple[str, ...],
    path: str,
) -> list[str]:
    value = config.get(key)
    if value is None:
        return list(default)
    if not isinstance(value, list | tuple):
        msg = f"{path}.{key} must be an array of strings"
        raise TypeError(msg)

    parsed: list[str] = []
    items = cast("list[object] | tuple[object, ...]", value)
    for index, item in enumerate(items):
        if not isinstance(item, str):
            msg = f"{path}.{key}[{index}] must be a string"
            raise TypeError(msg)
        parsed.append(item)
    return parsed


def _extract_locale(settings: dict[str, Any] | None) -> str | None:
    if settings is None:
        return None
    raw_locale = settings.get("locale")
    if not isinstance(raw_locale, str) or not raw_locale:
        return None
    return raw_locale.replace("-", "_")


def _ordered_url_sets(
    url_sets: tuple[PlaybackUrlSetPayload, ...],
    *,
    default_url_set_id: str | None,
) -> tuple[PlaybackUrlSetPayload, ...]:
    deduped: list[PlaybackUrlSetPayload] = []
    seen: set[str] = set()
    for url_set in url_sets:
        if url_set.url in seen:
            continue
        seen.add(url_set.url)
        deduped.append(url_set)

    if default_url_set_id is None:
        return tuple(deduped)

    for index, url_set in enumerate(deduped):
        if url_set.url_set_id != default_url_set_id:
            continue
        return (url_set, *deduped[:index], *deduped[index + 1 :])
    return tuple(deduped)


def _title_id_from_url(url: str) -> str | None:
    query = dict(parse_qsl(urlsplit(url).query, keep_blank_values=True))
    title_id = query.get("titleId")
    if not title_id:
        return None
    return title_id


__all__ = ["PrimeVideoProvider", "_NS_PRIME"]
