"""Async HTTP client for TV4 Play playback resolution."""

from __future__ import annotations

import html
import re
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any
from urllib.parse import urlencode, urljoin

from vibecast._log import get_logger
from vibecast.app import AppHttpStatusError
from vibecast.apps.common.http import cast_default_headers
from vibecast.apps.tv4play._models import (
    Tv4AuthTokenResponse,
    Tv4GraphqlResponse,
    Tv4Media,
    Tv4PlaybackResponse,
)

if TYPE_CHECKING:
    from collections.abc import Mapping

    from httpx import AsyncClient, Response

_ORIGIN = "https://cast-receiver.a2d.tv"
_REFERER = "https://cast-receiver.a2d.tv/"
_CLIENT_NAME = "nordic-chromecast"
_CLIENT_VERSION = "1.24.0"
_AUTH_URL = "https://auth.tv4.a2d.tv/v2/auth/token"
_GRAPHQL_URL = "https://nordic-gateway.tv4.a2d.tv/graphql"
_PLAYBACK_BASE_URL = "https://playback2.a2d.tv"
_DASH_CONTENT_TYPE = "application/dash+xml"
log = get_logger("tv4play.api")
_YOSPACE_BUILDER_RE = re.compile(
    r"https?://[^\s\"'<>]+/csm/builder/[^\s\"'<>]+?\.mpd(?:\?[^\s\"'<>]+)?"
)

_VIDEO_QUERY = """
      query Video($id: ID!) {
        media(id: $id) {
          ... on Channel { __typename, title, channelType: type, isDrmProtected, images { main16x9 { source }, poster2x3 { source }, logo { source } } }
          ... on Clip { __typename, title, isDrmProtected, images { main16x9 { source } } }
          ... on Movie { __typename, title, parentalRating { sweden { ageRecommendation, suitableForChildren }, finland { ageRestriction, reason, containsProductPlacement } }, isDrmProtected, images { main16x9 { source }, poster2x3 { source } }, synopsis { medium } }
          ... on SportEvent { __typename, title, isDrmProtected, images { main16x9 { source }, poster2x3 { source } }, synopsis { medium }, liveEventEnd { isoString } }
          ... on Episode { __typename, title, parentalRating { sweden { ageRecommendation, suitableForChildren }, finland { ageRestriction, reason, containsProductPlacement } }, extendedTitle, endScreenLoadThreshold, isDrmProtected, images { main16x9 { source } }, synopsis { medium }, liveEventEnd { isoString }, series { id, title, images { logo { source }, poster2x3 { source } } } }
        }
      }
"""


@dataclass(slots=True, frozen=True)
class Tv4AuthTokens:
    """Refreshed TV4 auth token pair."""

    access_token: str
    refresh_token: str
    expires_in: int | None = None


@dataclass(slots=True, frozen=True)
class Tv4ResolvedMedia:
    """TV4 API resolution result."""

    asset_id: str
    manifest_url: str
    content_type: str
    playback: Tv4PlaybackResponse
    metadata: Tv4Media | None


class Tv4PlayAPI:
    """Minimal TV4 Play API client used by :class:`Tv4Play`."""

    def __init__(self, *, client: AsyncClient) -> None:
        self._client = client

    def _base_headers(self) -> dict[str, str]:
        headers = cast_default_headers(_ORIGIN, _REFERER)
        headers["Client-Name"] = _CLIENT_NAME
        return headers

    async def refresh_auth(
        self,
        *,
        refresh_token: str,
        profile_id: str | None,
    ) -> Tv4AuthTokens:
        """Refresh sender-provided TV4 credentials."""
        payload: dict[str, Any] = {
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }
        if profile_id:
            payload["profile_id"] = profile_id

        response = await self._client.post(
            _AUTH_URL,
            json=payload,
            headers={**self._base_headers(), "Content-Type": "application/json"},
        )
        self._raise_for_status(response, detail_code="TV4_AUTH_REFRESH_FAILED")
        token_response = Tv4AuthTokenResponse.model_validate(response.json())
        if not token_response.access_token:
            msg = "TV4 auth response did not include access_token"
            raise RuntimeError(msg)
        return Tv4AuthTokens(
            access_token=token_response.access_token,
            refresh_token=token_response.refresh_token or refresh_token,
            expires_in=token_response.expires_in,
        )

    async def fetch_metadata(
        self,
        *,
        asset_id: str,
        access_token: str | None,
    ) -> Tv4Media | None:
        """Fetch GraphQL metadata for one TV4 asset."""
        headers = {
            **self._base_headers(),
            "Client-Version": _CLIENT_VERSION,
            "Content-Type": "application/json",
        }
        if access_token:
            headers["Authorization"] = f"Bearer {access_token}"
        response = await self._client.post(
            _GRAPHQL_URL,
            json={"query": _VIDEO_QUERY, "variables": {"id": asset_id}},
            headers=headers,
        )
        self._raise_for_status(response, detail_code="TV4_GRAPHQL_FAILED")
        payload = Tv4GraphqlResponse.model_validate(response.json())
        return payload.data.media if payload.data is not None else None

    async def fetch_playback(
        self,
        *,
        asset_id: str,
        access_token: str | None,
        custom_data: Mapping[str, Any],
    ) -> Tv4PlaybackResponse:
        """Resolve one TV4 asset to playback metadata and manifest URL."""
        query: dict[str, str] = {
            "preview": "false",
            "capabilities": "live-drm-adstitch-2,yospace3",
            "service": "tv4play",
            "drm": "widevine",
            "device": "chromecast",
            "protocol": "dash",
            "browser": "GoogleChrome",
        }
        _copy_query_param(query, custom_data, "gdpr", "gdpr_consent")
        _copy_query_param(query, custom_data, "ifa", "ifa")
        _copy_query_param(query, custom_data, "ifaType", "ifa_type")
        _copy_query_param(query, custom_data, "orientation", "orientation")

        headers = self._base_headers()
        if access_token:
            headers["x-jwt"] = f"Bearer {access_token}"

        url = f"{_PLAYBACK_BASE_URL}/play/{asset_id}?{urlencode(query)}"
        response = await self._client.get(url, headers=headers)
        self._raise_for_status(response, detail_code="TV4_PLAYBACK_FAILED")
        return Tv4PlaybackResponse.model_validate(response.json())

    async def resolve_media(
        self,
        *,
        asset_id: str,
        access_token: str | None,
        custom_data: Mapping[str, Any],
    ) -> Tv4ResolvedMedia:
        """Fetch metadata/playback and select the playable manifest."""
        try:
            metadata = await self.fetch_metadata(
                asset_id=asset_id,
                access_token=access_token,
            )
        except Exception:
            log.debug("TV4 metadata lookup failed for %s", asset_id, exc_info=True)
            metadata = None
        playback = await self.fetch_playback(
            asset_id=asset_id,
            access_token=access_token,
            custom_data=custom_data,
        )
        item = playback.playback_item
        manifest_url = ""
        if item is not None:
            if item.access_url_type == "yospace" and item.access_url:
                manifest_url = await self._resolve_yospace_manifest(item.access_url)
            elif item.manifest_url:
                manifest_url = item.manifest_url
        if not manifest_url:
            msg = "TV4 playback response did not include a manifest URL"
            raise RuntimeError(msg)
        return Tv4ResolvedMedia(
            asset_id=asset_id,
            manifest_url=manifest_url,
            content_type=_content_type_for_manifest(manifest_url),
            playback=playback,
            metadata=metadata,
        )

    async def _resolve_yospace_manifest(self, access_url: str) -> str:
        response = await self._client.get(access_url, headers=self._base_headers())
        self._raise_for_status(response, detail_code="TV4_YOSPACE_ACCESS_FAILED")

        builder_url = _extract_yospace_builder_url(response.text, str(response.url))
        if builder_url is None:
            msg = "TV4 Yospace access response did not include a builder manifest URL"
            raise RuntimeError(msg)
        return builder_url

    @staticmethod
    def _raise_for_status(response: Response, *, detail_code: str) -> None:
        if response.status_code < 400:
            return
        raise AppHttpStatusError(
            response.status_code,
            f"TV4 request failed with {response.status_code}",
            detail_code=detail_code,
        )


def _copy_query_param(
    query: dict[str, str],
    custom_data: Mapping[str, Any],
    source_key: str,
    target_key: str,
) -> None:
    raw = custom_data.get(source_key)
    if raw is None:
        return
    if isinstance(raw, str | int | float | bool):
        query[target_key] = str(raw)


def _content_type_for_manifest(url: str) -> str:
    lowered = url.lower()
    if ".m3u8" in lowered:
        return "application/x-mpegurl"
    return _DASH_CONTENT_TYPE


def _extract_yospace_builder_url(body: str, base_url: str) -> str | None:
    unescaped = html.unescape(body)
    match = _YOSPACE_BUILDER_RE.search(unescaped)
    if match is not None:
        return match.group(0)

    relative_match = re.search(
        r"(?P<url>/csm/builder/[^\s\"'<>]+?\.mpd(?:\?[^\s\"'<>]+)?)",
        unescaped,
    )
    if relative_match is None:
        return None
    return urljoin(base_url, relative_match.group("url"))


def merged_custom_data(*values: Mapping[str, Any] | None) -> dict[str, Any]:
    """Merge sender customData layers while preserving later overrides."""
    merged: dict[str, Any] = {}
    for value in values:
        if value is not None:
            merged.update(dict(value))
    return merged


__all__ = ["Tv4AuthTokens", "Tv4PlayAPI", "Tv4ResolvedMedia", "merged_custom_data"]
