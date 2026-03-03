"""Bundled SVT Play provider."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, override
from urllib.parse import urlsplit

from vibecast._models import LoadRequest, StreamType
from vibecast.player import PlaybackMedia, PlaybackStream
from vibecast.provider import LaunchCredentials, Provider, ProviderSession
from vibecast.providers.svtplay._api import SvtPlayAPI

if TYPE_CHECKING:
    from vibecast._models import MediaImage, MediaMetadata


@dataclass(slots=True)
class _SessionState:
    """Mutable provider state for one app session."""

    api: SvtPlayAPI


class SvtPlayProvider(Provider):
    """SVT Play provider implementation."""

    _APP_IDS = frozenset({"95370A1C"})
    _NAMESPACES = frozenset[str]()

    def __init__(self) -> None:
        self._sessions: dict[str, _SessionState] = {}

    @override
    def app_ids(self) -> frozenset[str]:
        return self._APP_IDS

    @override
    def display_name(self) -> str:
        return "SVT Play"

    @override
    def icon_url(self) -> str | None:
        return "https://lh3.googleusercontent.com/K3wumlt002dZrHoe4uKKdW-zMRLXdiPdgT1SRP90dnmMvLqsR-zaA3v-360EEIWLL5-SzJVt65XfqlgENw"

    @override
    def namespaces(self) -> frozenset[str]:
        return self._NAMESPACES

    @override
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        _ = credentials
        self._sessions[session.session_id] = _SessionState(
            api=SvtPlayAPI(client=session.http_client)
        )

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
        state = self._sessions.get(session.session_id)
        if state is None:
            msg = "unknown session"
            raise RuntimeError(msg)

        media = load_request.media
        svt_id = _extract_svt_id(media.content_id)
        if not svt_id:
            msg = "INVALID_CONTENT_ID"
            raise RuntimeError(msg)

        resolved = await state.api.resolve_media(svt_id, media.custom_data)
        metadata = media.metadata

        stream_type = media.stream_type
        if stream_type is StreamType.NONE:
            stream_type = StreamType.BUFFERED

        streams = tuple(
            PlaybackStream(
                url=stream.url,
                content_type=stream.content_type,
                drm=stream.drm,
            )
            for stream in resolved.streams
        )

        if not streams:
            msg = "NO_RESOLVED_STREAMS"
            raise RuntimeError(msg)

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

    @override
    async def on_stop(self, session: ProviderSession) -> None:
        _ = self._sessions.pop(session.session_id, None)


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


__all__ = ["SvtPlayProvider"]
