"""Async HTTP helper for resolving SVT Play media manifests."""

from __future__ import annotations

import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, cast
from urllib.parse import urlencode

from vibecast.player import DrmInfo, DrmSystem
from vibecast.providers.svtplay._models import (
    SvtResolveResponse,
    SvtVideoReference,
    SvtVideoResponse,
)

if TYPE_CHECKING:
    from httpx import AsyncClient

_ORIGIN = "https://www.svtstatic.se"
_REFERER = "https://www.svtstatic.se/"

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
_DASH_MIME_TYPE = "application/dash+xml"
_CLEARKEY_SCHEME_URI = "urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e"
_WIDEVINE_SCHEME_URI = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"

_DASHIF_LAURL_RE = re.compile(
    r"<dashif:Laurl[^>]*>([^<]+)</dashif:Laurl>", re.IGNORECASE
)
_MS_LAURL_TEXT_RE = re.compile(r"<ms:laurl[^>]*>([^<]+)</ms:laurl>", re.IGNORECASE)
_MS_LAURL_ATTR_RE = re.compile(
    r'<ms:laurl[^>]*(?:licenseUrl|href)="([^"]+)"[^>]*?/?>', re.IGNORECASE
)


@dataclass(slots=True, frozen=True)
class SvtResolvedStream:
    """Single resolved stream candidate for playback."""

    url: str
    content_type: str
    drm: DrmInfo | None = None


@dataclass(slots=True, frozen=True)
class SvtResolvedMedia:
    """Resolved playback payload for a single SVT content item."""

    streams: tuple[SvtResolvedStream, ...]
    title: str | None
    subtitle: str | None
    duration: float | None
    custom_data: dict[str, Any]


class SvtPlayAPI:
    """Minimal SVT Play API client used by :class:`SvtPlayProvider`."""

    def __init__(
        self,
        *,
        client: AsyncClient,
    ) -> None:
        self._client = client

    def _default_headers(self) -> dict[str, str]:
        return {
            "Accept": "*/*",
            "Accept-Language": "en-US",
            "Origin": _ORIGIN,
            "Referer": _REFERER,
        }

    async def _get_json(self, url: str) -> dict[str, Any]:
        response = await self._client.get(url, headers=self._default_headers())
        _ = response.raise_for_status()
        payload = response.json()
        if not isinstance(payload, dict):
            msg = "unexpected JSON payload"
            raise TypeError(msg)
        return cast("dict[str, Any]", payload)

    async def _get_text(self, url: str) -> str:
        response = await self._client.get(url, headers=self._default_headers())
        _ = response.raise_for_status()
        return response.text

    async def fetch_video(self, svt_id: str) -> SvtVideoResponse:
        """Fetch media metadata and references for one SVT content ID."""
        payload = await self._get_json(f"https://video.svt.se/video/{svt_id}")
        return SvtVideoResponse.model_validate(payload)

    async def resolve_media(
        self,
        svt_id: str,
        media_custom_data: Mapping[str, Any] | None,
    ) -> SvtResolvedMedia:
        """Resolve a Cast ``LOAD`` request into ordered stream candidates."""
        video = await self.fetch_video(svt_id)

        default_variant = video.variants.get("default")
        reference_pool = (
            default_variant.video_references
            if default_variant is not None
            else video.video_references
        )

        primary_ref = _pick_dash_reference(reference_pool)
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
        ditto_manifest_url = f"{_DITTO_MANIFEST_ENDPOINT}?{urlencode(params)}"

        streams: list[SvtResolvedStream] = []
        seen_urls: set[str] = set()

        async def _add_stream(url: str) -> None:
            if not url or url in seen_urls:
                return
            seen_urls.add(url)
            streams.append(
                SvtResolvedStream(
                    url=url,
                    content_type=_DASH_MIME_TYPE,
                    drm=await self._detect_manifest_drm(url),
                )
            )

        await _add_stream(ditto_manifest_url)
        await _add_stream(primary_manifest)

        for fallback_ref in _pick_dash_fallback_references(reference_pool):
            fallback_manifest = await self._resolve_reference(fallback_ref)
            await _add_stream(fallback_manifest)

        if not streams:
            msg = "NO_RESOLVED_STREAMS"
            raise RuntimeError(msg)

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
            streams=tuple(streams),
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

    async def _detect_manifest_drm(self, manifest_url: str) -> DrmInfo | None:
        try:
            manifest = await self._get_text(manifest_url)
        except Exception:
            return None

        lowered = manifest.lower()
        license_url = _extract_license_url(manifest)

        if _CLEARKEY_SCHEME_URI in lowered and license_url is not None:
            return DrmInfo(system=DrmSystem.CLEARKEY, license_url=license_url)
        if _WIDEVINE_SCHEME_URI in lowered and license_url is not None:
            return DrmInfo(system=DrmSystem.WIDEVINE, license_url=license_url)
        return None


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


def _pick_dash_fallback_references(
    references: Sequence[SvtVideoReference],
) -> tuple[SvtVideoReference, ...]:
    fallbacks: list[SvtVideoReference] = []
    for format_name in ("dash-hbbtv-avc", "dash-avc", "dash"):
        reference = _pick_reference_by_format(references, format_name)
        if reference is None or reference in fallbacks:
            continue
        fallbacks.append(reference)
    return tuple(fallbacks)


def _pick_reference_by_format(
    references: Sequence[SvtVideoReference],
    format_name: str,
) -> SvtVideoReference | None:
    for reference in references:
        if reference.format == format_name:
            return reference
    return None


def _extract_license_url(manifest: str) -> str | None:
    dashif_match = _DASHIF_LAURL_RE.search(manifest)
    if dashif_match is not None:
        value = dashif_match.group(1).strip()
        if value:
            return value

    ms_attr_match = _MS_LAURL_ATTR_RE.search(manifest)
    if ms_attr_match is not None:
        value = ms_attr_match.group(1).strip()
        if value:
            return value

    ms_text_match = _MS_LAURL_TEXT_RE.search(manifest)
    if ms_text_match is not None:
        value = ms_text_match.group(1).strip()
        if value:
            return value

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


__all__ = ["SvtPlayAPI", "SvtResolvedMedia", "SvtResolvedStream"]
