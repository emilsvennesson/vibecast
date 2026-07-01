"""Bundled TV4 Play app."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, override

from vibecast._models import MediaImage, PlayerState
from vibecast.app import (
    AppContext,
    AppMessageDisposition,
    LaunchCredentials,
    LoadRequest,
    MediaMetadata,
    MediaResolveFailure,
    MediaResolveFailureCode,
    MediaResolveResult,
    StatefulAppProvider,
    media_failure_from_exception,
)
from vibecast.apps.tv4play._api import (
    Tv4AuthTokens,
    Tv4PlayAPI,
    Tv4ResolvedMedia,
    merged_custom_data,
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
    StreamType,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from vibecast.apps.tv4play._models import (
        Tv4Images,
        Tv4Media,
        Tv4PlaybackItem,
        Tv4PlaybackResponse,
    )

_NS_TV4 = "urn:x-cast:avod.chromecast"
_APP_ID = "B6470434"


@dataclass(slots=True)
class _Tv4SessionState:
    """Mutable state for one TV4 app session."""

    api: Tv4PlayAPI
    broadcast: Callable[[str, dict[str, Any]], Awaitable[None]]
    tokens: Tv4AuthTokens | None = None
    current_asset_id: str | None = None
    current_metadata: Tv4Media | None = None
    current_playback: Tv4PlaybackResponse | None = None
    current_media: PlaybackMedia | None = None
    playback_state: PlaybackState = field(
        default_factory=lambda: PlaybackState(player_state=PlayerState.IDLE)
    )


class Tv4Play(StatefulAppProvider[_Tv4SessionState]):
    """TV4 Play app implementation."""

    _APP_IDS = frozenset({_APP_ID})
    _NAMESPACES = frozenset({_NS_TV4})

    @override
    async def create_session_state(
        self,
        session: AppContext,
        credentials: LaunchCredentials,
    ) -> _Tv4SessionState:
        _ = credentials
        return _Tv4SessionState(
            api=Tv4PlayAPI(client=session.http_client),
            broadcast=session.broadcast_custom,
        )

    @override
    def app_ids(self) -> frozenset[str]:
        return self._APP_IDS

    @override
    def display_name(self) -> str:
        return "TV4 Play v5"

    @override
    def app_key(self) -> str:
        return "tv4play"

    @override
    def icon_url(self) -> str | None:
        return "https://cast-receiver.a2d.tv/images/tv4play/logo.svg"

    @override
    def namespaces(self) -> frozenset[str]:
        return self._NAMESPACES

    @override
    async def on_message(
        self,
        session: AppContext,
        namespace: str,
        data: dict[str, Any],
    ) -> AppMessageDisposition:
        _ = session
        _ = data
        if namespace == _NS_TV4:
            return AppMessageDisposition.HANDLED
        return AppMessageDisposition.UNHANDLED

    @override
    async def on_sender_connected(self, session: AppContext, sender_id: str) -> None:
        _ = sender_id
        state = self.state_or_none(session)
        if state is None:
            return
        await self._broadcast_legacy_snapshot(state)

    @override
    async def resolve_media(
        self,
        session: AppContext,
        load_request: LoadRequest,
    ) -> MediaResolveResult:
        state = self.state_or_none(session)
        if state is None:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.INTERNAL_ERROR,
                detail_code="MISSING_PROVIDER_SESSION_STATE",
            )

        asset_id = _extract_asset_id(load_request)
        if not asset_id:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.INVALID_REQUEST,
                detail_code="INVALID_CONTENT_ID",
            )

        if "://" in asset_id:
            return _direct_media(session, load_request, asset_id)

        custom_data = merged_custom_data(
            load_request.custom_data,
            load_request.media.custom_data,
        )
        access_token = _optional_string(custom_data.get("accessToken"))
        refresh_token = _optional_string(custom_data.get("refreshToken"))
        profile_id = _optional_string(custom_data.get("profileId"))

        if refresh_token:
            try:
                state.tokens = await state.api.refresh_auth(
                    refresh_token=refresh_token,
                    profile_id=profile_id,
                )
                access_token = state.tokens.access_token
                custom_data["refreshToken"] = state.tokens.refresh_token
            except Exception as exc:
                return media_failure_from_exception(
                    exc,
                    detail_code="TV4_AUTH_REFRESH_FAILED",
                )
        elif state.tokens is not None:
            access_token = state.tokens.access_token

        if not access_token:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.AUTH_REQUIRED,
                detail_code="NOT_AUTHENTICATED",
            )

        try:
            resolved = await state.api.resolve_media(
                asset_id=asset_id,
                access_token=access_token,
                custom_data=custom_data,
            )
        except Exception as exc:
            message = str(exc)
            if "manifest URL" in message:
                return MediaResolveFailure(
                    code=MediaResolveFailureCode.CONTENT_UNAVAILABLE,
                    detail_code="NO_MANIFEST_URL",
                    message=message,
                )
            return media_failure_from_exception(exc, detail_code="TV4_RESOLVE_FAILED")

        playback_media = _playback_media_from_resolved(
            session=session,
            load_request=load_request,
            resolved=resolved,
            custom_data=custom_data,
            asset_id=asset_id,
        )

        state.current_asset_id = asset_id
        state.current_metadata = resolved.metadata
        state.current_playback = resolved.playback
        state.current_media = playback_media
        await self._broadcast_legacy_snapshot(state)
        return playback_media

    @override
    async def on_playback_update(
        self,
        session: AppContext,
        state: PlaybackState,
    ) -> None:
        internal = self.state_or_none(session)
        if internal is None:
            return
        internal.playback_state = state
        if internal.current_media is None:
            return
        await self._broadcast_progress(internal)

    @override
    async def resolve_license(
        self,
        session: AppContext,
        request: LicenseRequest,
        route: LicenseRoute,
        forward: Callable[[LicenseRequest, LicenseRoute], Awaitable[LicenseResponse]],
    ) -> LicenseResponse:
        if self.state_or_none(session) is None:
            return LicenseResponse(status=409, body=b"missing app session state")
        return await forward(request, route)

    async def _broadcast_legacy_snapshot(self, state: _Tv4SessionState) -> None:
        if state.current_media is None or state.current_asset_id is None:
            return
        await state.broadcast(
            _NS_TV4,
            {"type": "assetId", "value": state.current_asset_id},
        )
        await state.broadcast(
            _NS_TV4,
            {"type": "assetMetadata", "value": _asset_metadata(state)},
        )
        await state.broadcast(
            _NS_TV4,
            {"type": "playbackCapabilities", "value": _capabilities(state)},
        )
        await self._broadcast_progress(state)

    async def _broadcast_progress(self, state: _Tv4SessionState) -> None:
        media = state.current_media
        if media is None:
            return
        duration = state.playback_state.duration
        if duration is None:
            duration = media.duration or 0.0
        is_live = media.stream_type is StreamType.LIVE
        message_type = "liveProgressData" if is_live else "progressData"
        current_time = max(0.0, state.playback_state.current_time)
        payload = {
            "type": message_type,
            "currentTime": current_time,
            "position": current_time,
            "duration": max(0.0, duration),
            "isInAdBreak": False,
            "liveSeekableRange": {"start": 0, "end": max(0.0, duration)},
        }
        await state.broadcast(_NS_TV4, payload)


def _extract_asset_id(load_request: LoadRequest) -> str:
    media = load_request.media
    content_id = media.content_id.strip()
    if content_id:
        return content_id
    entity = getattr(media, "entity", None)
    if isinstance(entity, str) and entity.strip():
        return entity.strip()
    custom_data = media.custom_data or {}
    asset_id = custom_data.get("assetId")
    if isinstance(asset_id, str):
        return asset_id.strip()
    return ""


def _direct_media(
    session: AppContext,
    load_request: LoadRequest,
    content_id: str,
) -> PlaybackMedia:
    media = load_request.media
    metadata = media.metadata
    return PlaybackMedia(
        session_id=session.session_id,
        streams=(
            PlaybackStream(
                url=media.content_url or content_id,
                content_type=media.content_type or _content_type_for_url(content_id),
            ),
        ),
        stream_type=Tv4Play.normalize_stream_type(media.stream_type),
        content_id=content_id,
        title=metadata.title if metadata else None,
        subtitle=metadata.subtitle if metadata else None,
        images=tuple(metadata.images) if metadata else (),
        duration=media.duration,
        autoplay=load_request.autoplay,
        start_time=load_request.current_time,
        custom_data=merged_custom_data(load_request.custom_data, media.custom_data),
    )


def _playback_media_from_resolved(
    *,
    session: AppContext,
    load_request: LoadRequest,
    resolved: Tv4ResolvedMedia,
    custom_data: dict[str, Any],
    asset_id: str,
) -> PlaybackMedia:
    playback = resolved.playback
    item = playback.playback_item
    playback_metadata = playback.metadata
    metadata = load_request.media.metadata

    stream_type = _stream_type(playback)
    title = _title(
        resolved.metadata, playback_metadata.title if playback_metadata else None
    )
    subtitle = _subtitle(resolved.metadata, metadata)
    duration = playback_metadata.duration if playback_metadata else None
    if duration is None:
        duration = load_request.media.duration

    custom_payload = dict(custom_data)
    custom_payload["mediaType"] = (
        playback_metadata.type if playback_metadata and playback_metadata.type else ""
    )
    if item is not None:
        custom_payload["subtitles"] = [
            subtitle.model_dump(exclude_none=True) for subtitle in item.subtitles
        ]
        custom_payload["subs"] = [
            sub.model_dump(exclude_none=True) for sub in item.subs
        ]
        custom_payload["thumbnails"] = [
            thumb.model_dump(exclude_none=True) for thumb in item.thumbnails
        ]

    return PlaybackMedia(
        session_id=session.session_id,
        streams=(
            PlaybackStream(
                url=resolved.manifest_url,
                content_type=resolved.content_type,
                drm=_drm_info(item),
            ),
        ),
        stream_type=stream_type,
        content_id=asset_id,
        title=title,
        subtitle=subtitle,
        images=_images(
            resolved.metadata, playback_metadata.image if playback_metadata else None
        ),
        duration=duration,
        autoplay=load_request.autoplay,
        start_time=load_request.current_time,
        custom_data=custom_payload,
    )


def _drm_info(item: Tv4PlaybackItem | None) -> DrmInfo | None:
    if item is None or item.license is None:
        return None
    license_info = item.license
    if not license_info.castlabs_server or not license_info.castlabs_token:
        return None
    return DrmInfo(
        system=DrmSystem.WIDEVINE,
        license_url=license_info.castlabs_server,
        headers={"x-dt-auth-token": license_info.castlabs_token},
    )


def _stream_type(playback: Tv4PlaybackResponse) -> StreamType:
    metadata = playback.metadata
    if metadata is not None and metadata.is_live:
        return StreamType.LIVE
    item = playback.playback_item
    if item is not None and item.state == "live":
        return StreamType.LIVE
    return StreamType.BUFFERED


def _title(metadata: Tv4Media | None, fallback: str | None) -> str | None:
    if metadata is None:
        return fallback
    return metadata.extended_title or metadata.title or fallback


def _subtitle(metadata: Tv4Media | None, fallback: MediaMetadata | None) -> str | None:
    if metadata is not None:
        if metadata.series and metadata.series.title:
            return metadata.series.title
        if metadata.synopsis and metadata.synopsis.medium:
            return metadata.synopsis.medium
    return fallback.subtitle if fallback else None


def _images(
    metadata: Tv4Media | None, fallback_url: str | None
) -> tuple[MediaImage, ...]:
    urls: list[str] = []
    if metadata is not None:
        urls.extend(_image_urls(metadata.images))
        if metadata.series is not None:
            urls.extend(_image_urls(metadata.series.images))
    if fallback_url:
        urls.append(fallback_url)

    deduped: list[MediaImage] = []
    seen: set[str] = set()
    for url in urls:
        if not url or url in seen:
            continue
        seen.add(url)
        deduped.append(MediaImage(url=url))
    return tuple(deduped)


def _image_urls(images: Tv4Images | None) -> tuple[str, ...]:
    if images is None:
        return ()
    values = (images.main_16x9, images.poster_2x3, images.logo)
    return tuple(image.source for image in values if image is not None and image.source)


def _asset_metadata(state: _Tv4SessionState) -> dict[str, Any]:
    media = state.current_media
    playback = state.current_playback
    playback_metadata = playback.metadata if playback else None
    return {
        "id": state.current_asset_id,
        "title": media.title if media else None,
        "description": media.subtitle if media else None,
        "image": media.images[0].url if media and media.images else None,
        "type": (playback_metadata.type if playback_metadata else "") or "",
        "isLive": media.stream_type is StreamType.LIVE if media else False,
    }


def _capabilities(state: _Tv4SessionState) -> dict[str, Any]:
    playback = state.current_playback
    capabilities = playback.capabilities if playback else None
    return {
        "pause": capabilities.pause if capabilities else True,
        "seek": capabilities.seek if capabilities else True,
        "skip_ads": False,
        "stream_switch": capabilities.stream_switch if capabilities else False,
    }


def _optional_string(value: object) -> str | None:
    if isinstance(value, str) and value:
        return value
    return None


def _content_type_for_url(url: str) -> str:
    lowered = url.lower()
    if ".mpd" in lowered:
        return "application/dash+xml"
    if ".m3u8" in lowered:
        return "application/x-mpegurl"
    return "video/mp4"


__all__ = ["Tv4Play", "_NS_TV4"]
