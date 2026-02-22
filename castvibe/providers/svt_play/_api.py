"""Async HTTP helper for resolving SVT Play media manifests."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, cast
from urllib.parse import urlencode

from castvibe.providers.svt_play._models import (
    SvtResolveResponse,
    SvtVideoReference,
    SvtVideoResponse,
)

if TYPE_CHECKING:
    from httpx import AsyncClient

_USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 11.0; Build/RQ1A.210105.003) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/92.0.4515.0 "
    "Safari/537.36 CrKey/1.56.500000 DeviceType/AndroidTV"
)
_ORIGIN = "https://www.svtstatic.se"
_REFERER = "https://www.svtstatic.se/"
_CAST_DEVICE_CAPABILITIES = (
    '{"display_supported":true,'
    '"hi_res_audio_supported":false,'
    '"remote_control_input_supported":true,'
    '"touch_input_supported":false}'
)

_DITTO_MANIFEST_ENDPOINT = "https://api.svt.se/ditto/api/v3/manifest"
_PLATFORM = "chromecast;cc-androidtv"
_AUDIO_CODECS = "mp4a.40.2"
_VIDEO_CODECS = (
    "hvc1.2.4.L123.90,"
    "hvc1.1.6.L123.90,"
    "avc1.64002a,"
    "avc1.640029,"
    "avc1.640020,"
    "avc1.64001f,"
    "avc1.4d401f,"
    "avc1.42c01f,"
    "avc1.42c015"
)
_BUILD_PARAM = "-6334"


@dataclass(slots=True, frozen=True)
class SvtResolvedMedia:
    """Resolved playback payload for a single SVT content item."""

    manifest_url: str
    title: str | None
    subtitle: str | None
    duration: float | None
    custom_data: dict[str, Any]


class SvtPlayAPI:
    """Minimal SVT Play API client used by :class:`SvtPlayProvider`."""

    def __init__(self, *, client: AsyncClient) -> None:
        self._client = client

    @staticmethod
    def _default_headers() -> dict[str, str]:
        return {
            "User-Agent": _USER_AGENT,
            "Accept": "*/*",
            "Accept-Language": "en-US",
            "Origin": _ORIGIN,
            "Referer": _REFERER,
            "CAST-DEVICE-CAPABILITIES": _CAST_DEVICE_CAPABILITIES,
        }

    async def _get_json(self, url: str) -> dict[str, Any]:
        response = await self._client.get(url, headers=self._default_headers())
        _ = response.raise_for_status()
        payload = response.json()
        if not isinstance(payload, dict):
            msg = "unexpected JSON payload"
            raise TypeError(msg)
        return cast("dict[str, Any]", payload)

    async def fetch_video(self, svt_id: str) -> SvtVideoResponse:
        """Fetch media metadata and references for one SVT content ID."""
        payload = await self._get_json(f"https://video.svt.se/video/{svt_id}")
        return SvtVideoResponse.model_validate(payload)

    async def resolve_media(
        self,
        svt_id: str,
        media_custom_data: Mapping[str, Any] | None,
    ) -> SvtResolvedMedia:
        """Resolve a Cast ``LOAD`` request into a final ditto manifest URL."""
        video = await self.fetch_video(svt_id)

        default_variant = video.variants.get("default")
        primary_ref = _pick_dash_reference(
            default_variant.video_references
            if default_variant is not None
            else video.video_references
        )
        if primary_ref is None:
            primary_ref = _pick_dash_reference(video.video_references)
        if primary_ref is None:
            msg = "NO_DASH_REFERENCE"
            raise RuntimeError(msg)

        primary_manifest = await self._resolve_reference(primary_ref)

        sign_manifest: str | None = None
        sign_variant = video.variants.get("signInterpreted")
        if sign_variant is not None:
            sign_ref = _pick_dash_reference(sign_variant.video_references)
            if sign_ref is not None:
                sign_manifest = await self._resolve_reference(sign_ref)

        audio_manifest: str | None = None
        audio_variant = video.variants.get("audioDescribed")
        if audio_variant is not None:
            audio_ref = _pick_dash_reference(audio_variant.video_references)
            if audio_ref is not None:
                audio_manifest = await self._resolve_reference(audio_ref)

        params: list[tuple[str, str]] = [
            ("manifestUrl", primary_manifest),
            ("platform", _PLATFORM),
            ("includeAudioCodecs", _AUDIO_CODECS),
            ("includeVideoCodecs", _VIDEO_CODECS),
        ]
        if sign_manifest is not None:
            params.append(("manifestUrlSignLanguage", sign_manifest))
        if audio_manifest is not None:
            params.append(("manifestUrlAudioDescription", audio_manifest))

        preferred_video_track = _preferred_video_track(media_custom_data)
        if preferred_video_track is not None and (
            sign_manifest is not None or audio_manifest is not None
        ):
            params.append(("preferredVideoTrack", preferred_video_track))

        params.append(("b", _BUILD_PARAM))
        manifest_url = f"{_DITTO_MANIFEST_ENDPOINT}?{urlencode(params)}"

        custom_data: dict[str, Any] = dict(media_custom_data or {})
        custom_data.setdefault(
            "response",
            {
                "svtId": video.svt_id,
                "programTitle": video.program_title,
                "episodeTitle": video.episode_title,
                "contentDuration": video.content_duration,
            },
        )

        return SvtResolvedMedia(
            manifest_url=manifest_url,
            title=video.program_title,
            subtitle=video.episode_title,
            duration=video.content_duration,
            custom_data=custom_data,
        )

    async def _resolve_reference(self, reference: SvtVideoReference) -> str:
        if reference.resolve is None:
            return reference.url

        payload = await self._get_json(reference.resolve)
        resolved = SvtResolveResponse.model_validate(payload)
        return resolved.location


def _pick_dash_reference(
    references: Sequence[SvtVideoReference],
) -> SvtVideoReference | None:
    for reference in references:
        if reference.format == "dash-full":
            return reference
    for reference in references:
        if reference.format is not None and reference.format.startswith("dash"):
            return reference
    for reference in references:
        if reference.url.endswith(".mpd"):
            return reference
    return None


def _preferred_video_track(media_custom_data: Mapping[str, Any] | None) -> str | None:
    if media_custom_data is None:
        return None

    custom_data = dict(media_custom_data)
    raw_preference = custom_data.get("videoTrackPreference")
    if not isinstance(raw_preference, Mapping):
        return None

    preference = cast("Mapping[str, Any]", raw_preference)
    raw_kind = preference.get("kind")
    if not isinstance(raw_kind, str):
        return None

    kind = raw_kind.strip().lower()
    if kind == "original":
        return "original"
    return None


__all__ = ["SvtPlayAPI", "SvtResolvedMedia"]
