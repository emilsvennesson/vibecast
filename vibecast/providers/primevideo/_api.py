"""Async HTTP client for Amazon Prime provider APIs."""

from __future__ import annotations

import base64
import json
import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, cast
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit
from uuid import uuid4

from vibecast.providers.primevideo._models import (
    AuthRegisterResponse,
    AuthTokenResponse,
    PlaybackUrlSetPayload,
    RefreshedEnvelopeResponse,
    VodPlaybackResourcesResponse,
    WidevineLicenseResponse,
)

if TYPE_CHECKING:
    from collections.abc import Mapping

    from httpx import AsyncClient

log = logging.getLogger("vibecast.primevideo.api")

_ORIGIN = "https://cloudfront.xp-assets.aiv-cdn.net"
_REFERER = "https://cloudfront.xp-assets.aiv-cdn.net/"
_PLAYBACK_BASE_URL = "https://aby4wfamebrp.api.amazonvideo.com"
_PLAYBACK_ZAZ_BASE_URL = "https://aby4wfamebrp.zaz.api.amazonvideo.com"

_API_DEVICE_TYPE_ID = "A2Y2Z7THWOTN8I"
_API_FIRMWARE_VERSION = "1"
_API_VERSION = "1"


@dataclass(slots=True, frozen=True)
class PrimeAuthTokens:
    """Prime auth token tuple derived from auth APIs."""

    account_refresh_token: str
    actor_access_token: str
    actor_refresh_token: str | None


@dataclass(slots=True, frozen=True)
class PrimeEnvelopeData:
    """Refreshed playback envelope fields for one title."""

    playback_envelope: str
    correlation_id: str | None = None


class PrimeVideoAPI:
    """Prime API helper for auth, stream resolution, and DRM licensing."""

    __slots__ = (
        "_auth_base_url",
        "_client",
        "_display_height",
        "_display_width",
        "_dynamic_range_formats",
        "_hdcp_level",
        "_max_video_resolution",
        "_supported_codecs",
        "_supported_frame_rates",
        "_supported_subtitle_formats",
    )

    def __init__(
        self,
        *,
        client: AsyncClient,
        auth_base_url: str,
        display_width: int,
        display_height: int,
        hdcp_level: str,
        max_video_resolution: str,
        supported_codecs: tuple[str, ...],
        dynamic_range_formats: tuple[str, ...],
        supported_frame_rates: tuple[str, ...],
        supported_subtitle_formats: tuple[str, ...],
    ) -> None:
        self._client = client
        self._auth_base_url = auth_base_url
        self._display_width = display_width
        self._display_height = display_height
        self._hdcp_level = hdcp_level
        self._max_video_resolution = max_video_resolution
        self._supported_codecs = supported_codecs
        self._dynamic_range_formats = dynamic_range_formats
        self._supported_frame_rates = supported_frame_rates
        self._supported_subtitle_formats = supported_subtitle_formats

    @property
    def api_device_type_id(self) -> str:
        """Return Prime's Chromecast device type ID used in API calls."""
        return _API_DEVICE_TYPE_ID

    def widevine_license_url(
        self,
        *,
        device_id: str,
        marketplace_id: str,
        title_id: str,
        locale: str,
    ) -> str:
        """Build Prime Widevine license endpoint URL for one title."""
        query = {
            "deviceID": device_id,
            "deviceTypeID": _API_DEVICE_TYPE_ID,
            "gascEnabled": True,
            "marketplaceID": marketplace_id,
            "uxLocale": locale,
            "firmware": _API_FIRMWARE_VERSION,
            "titleId": title_id,
            "nerid": self._new_nerid(),
        }
        return self._build_url(
            _PLAYBACK_ZAZ_BASE_URL,
            "/playback/drm-vod/GetWidevineLicense",
            query,
        )

    async def register_device(
        self,
        *,
        link_code: str,
        device_id: str,
    ) -> PrimeAuthTokens:
        """Run Prime's register + actor-token flow using link code."""
        register_url = f"{self._auth_base_url}/auth/register"
        register_payload = {
            "registration_data": {
                "device_serial": device_id,
                "os_version": "Android",
                "app_name": "Prime Video",
                "app_version": "1.0",
                "device_model": "Generic GCast",
                "device_name": f"GCast:{_API_DEVICE_TYPE_ID}.{device_id}",
                "device_type": _API_DEVICE_TYPE_ID,
                "domain": "Device",
                "software_version": "1.0",
            },
            "auth_data": {"code": link_code},
            "requested_token_type": ["bearer"],
            "scopes": ["aiv:full"],
        }
        register_response = await self._post_json(
            register_url,
            register_payload,
            content_type="application/json",
        )
        register = AuthRegisterResponse.model_validate(register_response)
        bearer = (
            register.response.success.tokens.bearer
            if register.response
            and register.response.success
            and register.response.success.tokens
            else None
        )
        if bearer is None or not bearer.refresh_token:
            msg = "prime register returned no refresh token"
            raise RuntimeError(msg)

        return PrimeAuthTokens(
            account_refresh_token=bearer.refresh_token,
            actor_access_token="",
            actor_refresh_token=None,
        )

    async def exchange_actor_token(
        self,
        *,
        actor_id: str,
        account_refresh_token: str,
    ) -> PrimeAuthTokens:
        """Exchange account refresh token for actor access token."""
        token_url = f"{self._auth_base_url}/auth/token"
        token_payload = {
            "actor_id": actor_id,
            "app_name": "Prime Video",
            "requested_token_type": "actor_access_token",
            "source_token_type": "refresh_token",
            "source_device_tokens": [
                {
                    "account_refresh_token": {"token": account_refresh_token},
                    "device_type": _API_DEVICE_TYPE_ID,
                }
            ],
        }
        token_response = await self._post_json(
            token_url,
            token_payload,
            content_type="application/json",
        )
        tokens = AuthTokenResponse.model_validate(token_response)
        if not tokens.device_tokens:
            msg = "prime token exchange returned no device tokens"
            raise RuntimeError(msg)

        device_token = tokens.device_tokens[0]
        actor_access = device_token.actor_access_token
        if actor_access is None or not actor_access.token:
            msg = "prime token exchange returned no actor access token"
            raise RuntimeError(msg)

        actor_refresh = device_token.actor_refresh_token
        return PrimeAuthTokens(
            account_refresh_token=account_refresh_token,
            actor_access_token=actor_access.token,
            actor_refresh_token=actor_refresh.token if actor_refresh else None,
        )

    async def refresh_playback_envelope(
        self,
        *,
        token: str,
        device_id: str,
        marketplace_id: str,
        title_id: str,
        correlation_id: str,
    ) -> PrimeEnvelopeData:
        """Refresh playback envelope for a title using correlation ID."""
        query = {
            "deviceID": device_id,
            "deviceTypeID": _API_DEVICE_TYPE_ID,
            "gascEnabled": True,
            "marketplaceID": marketplace_id,
            "firmware": _API_FIRMWARE_VERSION,
            "version": _API_VERSION,
            "nerid": self._new_nerid(),
        }
        url = self._build_url(
            _PLAYBACK_BASE_URL,
            "/playback/tags/getRefreshedPlaybackEnvelope",
            query,
        )
        payload = {
            "deviceId": device_id,
            "deviceTypeId": _API_DEVICE_TYPE_ID,
            "identifiers": {title_id: correlation_id},
            "geoToken": None,
            "identityContext": None,
        }
        response = await self._post_json(
            url,
            payload,
            token=token,
            content_type="text/plain",
        )
        refreshed = RefreshedEnvelopeResponse.model_validate(response)
        item = refreshed.response.get(title_id)
        experience = item.playback_experience if item else None
        if experience is None or not experience.playback_envelope:
            msg = "prime envelope refresh returned no playback envelope"
            raise RuntimeError(msg)
        return PrimeEnvelopeData(
            playback_envelope=experience.playback_envelope,
            correlation_id=experience.correlation_id,
        )

    async def get_vod_playback_resources(
        self,
        *,
        token: str,
        device_id: str,
        marketplace_id: str,
        title_id: str,
        playback_envelope: str,
        locale: str,
    ) -> VodPlaybackResourcesResponse:
        """Resolve Prime title ID to playback URL sets + sessionization data."""
        query = {
            "deviceID": device_id,
            "deviceTypeID": _API_DEVICE_TYPE_ID,
            "gascEnabled": True,
            "marketplaceID": marketplace_id,
            "uxLocale": locale,
            "firmware": _API_FIRMWARE_VERSION,
            "titleId": title_id,
            "nerid": self._new_nerid(),
        }
        url = self._build_url(
            _PLAYBACK_ZAZ_BASE_URL,
            "/playback/prs/GetVodPlaybackResources",
            query,
        )
        payload = _build_vod_playback_request(
            title_id=title_id,
            playback_envelope=playback_envelope,
            display_width=self._display_width,
            display_height=self._display_height,
            hdcp_level=self._hdcp_level,
            max_video_resolution=self._max_video_resolution,
            supported_codecs=self._supported_codecs,
            dynamic_range_formats=self._dynamic_range_formats,
            supported_frame_rates=self._supported_frame_rates,
            supported_subtitle_formats=self._supported_subtitle_formats,
        )
        response = await self._post_json(
            url,
            payload,
            token=token,
            content_type="text/plain",
        )
        return VodPlaybackResourcesResponse.model_validate(response)

    async def get_widevine_license(
        self,
        *,
        token: str,
        device_id: str,
        marketplace_id: str,
        title_id: str,
        playback_envelope: str,
        session_handoff_token: str | None,
        challenge: bytes,
        locale: str,
    ) -> bytes:
        """Resolve one Widevine challenge to a license blob."""
        url = self.widevine_license_url(
            device_id=device_id,
            marketplace_id=marketplace_id,
            title_id=title_id,
            locale=locale,
        )
        payload: dict[str, Any] = {
            "includeHdcpTestKey": True,
            "playbackEnvelope": playback_envelope,
            "licenseChallenge": base64.b64encode(challenge).decode("ascii"),
        }
        if session_handoff_token:
            payload["sessionHandoffToken"] = session_handoff_token

        response = await self._post_json(
            url,
            payload,
            token=token,
            content_type="text/plain",
        )
        parsed = WidevineLicenseResponse.model_validate(response)
        license_b64 = parsed.widevine_license.license if parsed.widevine_license else ""
        if not license_b64:
            msg = "prime license response missing widevine license"
            raise RuntimeError(msg)
        return _decode_b64(license_b64)

    @staticmethod
    def extract_playback_url_sets(
        resources: VodPlaybackResourcesResponse,
    ) -> tuple[str | None, tuple[PlaybackUrlSetPayload, ...]]:
        """Extract default URL-set ID and URL sets from playback response."""
        playback_urls = (
            resources.vod_playback_urls.result.playback_urls
            if resources.vod_playback_urls and resources.vod_playback_urls.result
            else None
        )
        if playback_urls is None:
            msg = "prime playback response missing playback URLs"
            raise RuntimeError(msg)
        return playback_urls.default_url_set_id, tuple(playback_urls.url_sets)

    def with_device_type_query(self, url: str) -> str:
        """Ensure ``amznDtid`` query parameter is present on playback URLs."""
        parts = urlsplit(url)
        query = dict(parse_qsl(parts.query, keep_blank_values=True))
        _ = query.setdefault("amznDtid", _API_DEVICE_TYPE_ID)
        return urlunsplit(
            (parts.scheme, parts.netloc, parts.path, urlencode(query), parts.fragment)
        )

    async def _post_json(
        self,
        url: str,
        payload: Mapping[str, Any],
        *,
        token: str | None = None,
        content_type: str,
    ) -> dict[str, Any]:
        headers = self._headers(token=token, content_type=content_type)
        if content_type == "application/json":
            response = await self._client.post(url, json=payload, headers=headers)
        else:
            body = json.dumps(payload, separators=(",", ":"))
            response = await self._client.post(url, content=body, headers=headers)

        if response.status_code >= 400:
            preview = response.text[:300].replace("\n", " ")
            msg = f"prime api request failed ({response.status_code}): {preview}"
            raise RuntimeError(msg)

        data = response.json()
        if not isinstance(data, dict):
            msg = "prime api returned non-object JSON"
            raise TypeError(msg)
        return cast("dict[str, Any]", data)

    def _headers(
        self,
        *,
        token: str | None,
        content_type: str,
    ) -> dict[str, str]:
        headers = {
            "Accept": "*/*",
            "Accept-Language": "en-US",
            "Origin": _ORIGIN,
            "Referer": _REFERER,
            "Content-Type": content_type,
        }
        if token:
            headers["Authorization"] = f"Bearer {token}"
        return headers

    @staticmethod
    def _build_url(
        base: str,
        path: str,
        query: Mapping[str, str | int | bool],
    ) -> str:
        encoded = urlencode(
            {key: _normalize_query_value(value) for key, value in query.items()}
        )
        return f"{base}{path}?{encoded}"

    @staticmethod
    def _new_nerid() -> str:
        return f"vibecast{uuid4().hex[:16]}"


def _build_vod_playback_request(
    *,
    title_id: str,
    playback_envelope: str,
    display_width: int,
    display_height: int,
    hdcp_level: str,
    max_video_resolution: str,
    supported_codecs: tuple[str, ...],
    dynamic_range_formats: tuple[str, ...],
    supported_frame_rates: tuple[str, ...],
    supported_subtitle_formats: tuple[str, ...],
) -> dict[str, Any]:
    return {
        "globalParameters": {
            "deviceCapabilityFamily": "WebPlayer",
            "playbackEnvelope": playback_envelope,
            "capabilityDiscriminators": {
                "operatingSystem": {"name": "Android", "version": "11.0"},
                "deviceModel": {"name": "SHIELD Android TV", "version": "UNKNOWN"},
                "middleware": {"name": "Chrome", "version": "92.0.4515.0"},
                "nativeApplication": {
                    "name": "CAF Receiver SDK",
                    "version": "3.0.0137",
                },
                "firmware": {"name": "UNKNOWN", "version": "1.56.500000"},
                "hfrControlMode": "Legacy",
                "displayResolution": {
                    "height": display_height,
                    "width": display_width,
                },
            },
        },
        "auditPingsRequest": {},
        "widevineServiceCertificateRequest": {},
        "playbackDataRequest": {},
        "timedTextUrlsRequest": {
            "supportedTimedTextFormats": list(supported_subtitle_formats)
        },
        "trickplayUrlsRequest": {},
        "transitionTimecodesRequest": {},
        "vodPlaybackUrlsRequest": {
            "device": {
                "hdcpLevel": hdcp_level,
                "maxVideoResolution": max_video_resolution,
                "supportedStreamingTechnologies": ["DASH"],
                "streamingTechnologies": {
                    "DASH": {
                        "bitrateAdaptations": ["CBR", "CVBR"],
                        "codecs": list(supported_codecs),
                        "drmKeyScheme": "DualKey",
                        "drmType": "Widevine",
                        "dynamicRangeFormats": list(dynamic_range_formats),
                        "edgeDeliveryAuthorizationSchemes": [
                            "PVExchangeV1",
                            "Transparent",
                        ],
                        "fragmentRepresentations": ["ByteOffsetRange", "SeparateFile"],
                        "frameRates": list(supported_frame_rates),
                        "stitchType": "MultiPeriod",
                        "segmentInfoType": "Base",
                        "timedTextRepresentations": [
                            "NotInManifestNorStream",
                            "SeparateStreamInManifest",
                        ],
                        "trickplayRepresentations": ["NotInManifestNorStream"],
                        "variableAspectRatio": "unsupported",
                    }
                },
                "displayWidth": display_width,
                "displayHeight": display_height,
            },
            "ads": {
                "sitePageUrl": (
                    "https://cloudfront.xp-assets.aiv-cdn.net/"
                    "packages/ATVGCastReceiver-1.0/prod/index.html"
                ),
                "gdpr": {"enabled": False, "consentMap": {}},
                "mainContentResumeOffsetHintMillis": 0,
            },
            "playbackCustomizations": {},
            "playbackSettingsRequest": {
                "deviceModel": "SHIELD Android TV",
                "firmware": "1.56.500000",
                "playerType": "xp",
                "responseFormatVersion": "1.0.0",
                "titleId": title_id,
            },
        },
        "vodXrayMetadataRequest": {
            "xrayDeviceClass": "normal",
            "xrayPlaybackMode": "playback",
            "xrayToken": "XRAY_WEB_2023_V2",
        },
    }


def _normalize_query_value(value: str | int | bool) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def _decode_b64(value: str) -> bytes:
    padded = value + ("=" * (-len(value) % 4))
    return base64.b64decode(padded)


__all__ = [
    "PrimeAuthTokens",
    "PrimeEnvelopeData",
    "PrimeVideoAPI",
]
