"""Bundled SVT Play provider."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, override
from urllib.parse import urlsplit

from vibecast.player import PlaybackMedia, PlaybackStream
from vibecast.provider import (
    LaunchCredentials,
    LoadRequest,
    MediaMetadata,
    MediaResolveFailure,
    MediaResolveFailureCode,
    MediaResolveResult,
    ProviderSession,
    StatefulProvider,
    media_failure_from_exception,
)
from vibecast.providers.svtplay._api import SvtPlayAPI

if TYPE_CHECKING:
    from vibecast.player import MediaImage


@dataclass(slots=True)
class _SessionState:
    """Mutable provider state for one app session."""

    api: SvtPlayAPI


class SvtPlayProvider(StatefulProvider[_SessionState]):
    """SVT Play provider implementation."""

    _APP_IDS = frozenset({"95370A1C"})

    @override
    async def create_session_state(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> _SessionState:
        _ = credentials
        return _SessionState(
            api=SvtPlayAPI(
                client=session.http_client,
            )
        )

    @override
    def app_ids(self) -> frozenset[str]:
        return self._APP_IDS

    @override
    def display_name(self) -> str:
        return "SVT Play"

    @override
    def provider_key(self) -> str:
        return "svtplay"

    @override
    def icon_url(self) -> str | None:
        return "https://lh3.googleusercontent.com/K3wumlt002dZrHoe4uKKdW-zMRLXdiPdgT1SRP90dnmMvLqsR-zaA3v-360EEIWLL5-SzJVt65XfqlgENw"

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

        media = load_request.media
        svt_id = _extract_svt_id(media.content_id)
        if not svt_id:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.INVALID_REQUEST,
                detail_code="INVALID_CONTENT_ID",
            )

        try:
            resolved = await state.api.resolve_media(svt_id, media.custom_data)
        except RuntimeError as exc:
            mapped = _resolve_failure_from_runtime_error(str(exc))
            if mapped is not None:
                return mapped
            return media_failure_from_exception(
                exc,
                detail_code="SVT_RESOLVE_RUNTIME_ERROR",
            )
        except Exception as exc:
            return media_failure_from_exception(
                exc,
                detail_code="SVT_RESOLVE_EXCEPTION",
            )

        metadata = media.metadata

        stream_type = self.normalize_stream_type(media.stream_type)

        streams = tuple(
            PlaybackStream(
                url=stream.url,
                content_type=stream.content_type,
                drm=stream.drm,
            )
            for stream in resolved.streams
        )

        if not streams:
            return MediaResolveFailure(
                code=MediaResolveFailureCode.CONTENT_UNAVAILABLE,
                detail_code="NO_RESOLVED_STREAMS",
            )

        return PlaybackMedia(
            session_id=session.session_id,
            streams=streams,
            stream_type=stream_type,
            title=resolved.title or _metadata_title(metadata),
            subtitle=resolved.subtitle or _metadata_subtitle(metadata),
            images=_metadata_images(metadata),
            duration=resolved.duration
            if resolved.duration is not None
            else media.duration,
            autoplay=load_request.autoplay,
            start_time=load_request.current_time,
            custom_data=resolved.custom_data,
        )


def _extract_svt_id(content_id: str) -> str:
    stripped = content_id.strip()
    if "://" not in stripped:
        return stripped

    path = urlsplit(stripped).path.rstrip("/")
    if "/video/" in path:
        return path.split("/video/", 1)[1]

    tail = path.rsplit("/", 1)
    if len(tail) == 2:
        return tail[1]
    return stripped


def _metadata_title(metadata: MediaMetadata | None) -> str | None:
    if metadata is None:
        return None
    return metadata.title


def _metadata_subtitle(metadata: MediaMetadata | None) -> str | None:
    if metadata is None:
        return None
    return metadata.subtitle


def _metadata_images(metadata: MediaMetadata | None) -> tuple[MediaImage, ...]:
    if metadata is None:
        return ()
    return tuple(metadata.images)


def _resolve_failure_from_runtime_error(
    message: str,
) -> MediaResolveFailure | None:
    normalized = message.strip().upper()
    if normalized == "NO_DASH_REFERENCE":
        return MediaResolveFailure(
            code=MediaResolveFailureCode.CONTENT_UNAVAILABLE,
            detail_code="NO_DASH_REFERENCE",
            message=message,
        )
    if normalized == "NO_RESOLVED_STREAMS":
        return MediaResolveFailure(
            code=MediaResolveFailureCode.CONTENT_UNAVAILABLE,
            detail_code="NO_RESOLVED_STREAMS",
            message=message,
        )

    return None


__all__ = ["SvtPlayProvider"]
