"""Async HTTP client for the Viaplay API.

Handles authentication flows (persistent login, token login, device-code
authorization) and stream resolution. Uses the receiver-managed
:class:`httpx.AsyncClient` for request and cookie handling.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from uritemplate import expand as uri_expand

from vibecast._config import CastConfig, cast_device_capabilities_header
from vibecast._models import StreamType
from vibecast.providers.viaplay._models import (
    ViaplayAuthorizedPollResponse,
    ViaplayDeviceAuthResponse,
    ViaplaySessionResponse,
    ViaplayStreamResponse,
)

if TYPE_CHECKING:
    from httpx import AsyncClient

log = logging.getLogger("vibecast.viaplay.api")
_DEFAULT_CAST_CONFIG = CastConfig()
_DEFAULT_CAST_CAPABILITIES = cast_device_capabilities_header(
    _DEFAULT_CAST_CONFIG.device_capabilities
)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_ORIGIN = "https://viaplay-chromecast.viaplay.com"
_REFERER = "https://viaplay-chromecast.viaplay.com/"

_DEVICE_CODE_FALLBACK = "https://login.viaplay.com/api/device/code{?deviceKey,deviceId}"


# ---------------------------------------------------------------------------
# Response data classes
# ---------------------------------------------------------------------------


@dataclass(slots=True)
class ViaplayUser:
    """Authenticated user information."""

    user_id: str = ""
    first_name: str = ""
    last_name: str = ""


@dataclass(slots=True)
class SessionCheckResult:
    """Result of a content-root session check."""

    user: ViaplayUser | None = None
    persistent_login_url: str | None = None
    token_login_url: str | None = None
    device_auth_url: str | None = None


@dataclass(slots=True)
class DeviceAuthInfo:
    """Device-code authorization data."""

    user_code: str
    device_token: str
    activate_url: str
    authorized_url: str


@dataclass(slots=True)
class StreamInfo:
    """Resolved stream information."""

    url: str
    content_type: str
    stream_type: StreamType | None = None
    duration: float | None = None
    title: str | None = None
    drm_license_url: str | None = None
    fallback_urls: tuple[str, ...] = ()


# ---------------------------------------------------------------------------
# ViaplayAPI
# ---------------------------------------------------------------------------


class ViaplayAPI:
    """Async HTTP client for the Viaplay API."""

    def __init__(
        self,
        *,
        client: AsyncClient,
        device_id: str,
        user_agent: str = _DEFAULT_CAST_CONFIG.user_agent,
        cast_capabilities: str = _DEFAULT_CAST_CAPABILITIES,
    ) -> None:
        self._client = client
        self._device_id = device_id
        self._user_agent = user_agent
        self._cast_capabilities = cast_capabilities

        # Populated by set_setup_info()
        self._content_root = ""
        self._country_code = ""
        self._user_id = ""
        self._profile_id = ""

    # -- setup ---------------------------------------------------------------

    def set_setup_info(
        self,
        content_root: str,
        country_code: str,
        user_id: str,
        profile_id: str,
    ) -> None:
        """Store values received from the sender's ``SETUP_INFO`` message."""
        self._content_root = content_root
        self._country_code = country_code
        self._user_id = user_id
        self._profile_id = profile_id

    @property
    def device_key(self) -> str:
        return f"chromecastgoogletv4k-{self._country_code}"

    # -- URI template expansion ----------------------------------------------

    def _template_vars(self, extra: dict[str, str] | None = None) -> dict[str, Any]:
        variables: dict[str, Any] = {
            "deviceId": self._device_id,
            "deviceKey": self.device_key,
            "deviceType": "chromecast",
            "deviceName": "chromecast-receiver-v3",
            "userAgent": self._user_agent,
            "profileId": self._profile_id,
            "cse": "true",
        }
        if extra:
            variables.update(extra)
        return variables

    def _expand(self, template: str, extra: dict[str, str] | None = None) -> str:
        return uri_expand(template, var_dict=self._template_vars(extra))

    def _default_headers(self) -> dict[str, str]:
        return {
            "User-Agent": self._user_agent,
            "Accept": "*/*",
            "Accept-Language": "en-US",
            "Origin": _ORIGIN,
            "Referer": _REFERER,
            "CAST-DEVICE-CAPABILITIES": self._cast_capabilities,
        }

    def request_headers(self) -> dict[str, str]:
        """Return default Viaplay headers mimicking a real Chromecast."""
        return self._default_headers()

    # -- HTTP helpers --------------------------------------------------------

    async def _get(
        self,
        url: str,
        *,
        expand: bool = True,
        extra_vars: dict[str, str] | None = None,
    ) -> tuple[dict[str, Any], int]:
        """GET *url*, optionally expanding URI templates.  Returns (json, status)."""
        if expand:
            url = self._expand(url, extra_vars)
        response = await self._client.get(url, headers=self._default_headers())
        body = response.json()
        return body, response.status_code

    async def _get_raw(
        self,
        url: str,
        *,
        expand: bool = True,
        extra_vars: dict[str, str] | None = None,
    ) -> tuple[bytes, int]:
        """GET *url* returning raw bytes."""
        if expand:
            url = self._expand(url, extra_vars)
        response = await self._client.get(url, headers=self._default_headers())
        return response.content, response.status_code

    # -- authentication methods ----------------------------------------------

    async def check_session(self) -> SessionCheckResult:
        """Check current session by fetching the content root.

        Returns a :class:`SessionCheckResult` with user info and HAL links.
        """
        if not self._content_root:
            msg = "content root not set; call set_setup_info first"
            raise RuntimeError(msg)

        url = f"{self._content_root}/{{deviceKey}}"
        if self._profile_id:
            url += "?profileId={profileId}"

        body, status = await self._get(url)
        resp = ViaplaySessionResponse.model_validate(body)

        user: ViaplayUser | None = None
        if resp.user:
            user = ViaplayUser(
                user_id=resp.user.user_id,
                first_name=resp.user.first_name,
                last_name=resp.user.last_name,
            )

        links = resp.links
        result = SessionCheckResult(
            user=user,
            persistent_login_url=links.persistent_login.href
            if links and links.persistent_login
            else None,
            token_login_url=links.token_login.href
            if links and links.token_login
            else None,
            device_auth_url=links.device_authorization.href
            if links and links.device_authorization
            else None,
        )

        if status != 200:
            log.debug("session check returned status %d", status)
        elif result.user and result.user.user_id == self._user_id:
            log.info("session valid for user %s", self._user_id)
        else:
            log.debug("session check: no matching user")

        return result

    async def persistent_login(self, url: str) -> bool:
        """Attempt persistent login at *url*.  Returns True on success."""
        _, status = await self._get(url)
        if status == 200:
            log.info("persistent login succeeded")
            return True
        log.debug("persistent login returned %d", status)
        return False

    async def token_login(self, url_template: str, access_token: str) -> bool:
        """Attempt token login.  Returns True on success."""
        url = self._expand(url_template, {"accessToken": access_token})
        _, status = await self._get_raw(url, expand=False)
        if status == 200:
            log.info("token login succeeded")
            return True
        log.debug("token login returned %d", status)
        return False

    async def get_device_authorization(
        self,
        root_result: SessionCheckResult | None = None,
    ) -> DeviceAuthInfo:
        """Request a device authorization code.

        Raises :class:`RuntimeError` if the API does not return a user code.
        """
        # Find the deviceAuthorization link
        auth_url: str | None = None
        if root_result:
            auth_url = root_result.device_auth_url

        if not auth_url:
            # Re-fetch root to get links
            root_result = await self.check_session()
            auth_url = root_result.device_auth_url

        if not auth_url:
            auth_url = _DEVICE_CODE_FALLBACK

        body, status = await self._get(auth_url)
        if status != 200:
            msg = f"device authorization request failed with status {status}"
            raise RuntimeError(msg)

        resp = ViaplayDeviceAuthResponse.model_validate(body)
        if not resp.user_code:
            msg = "no userCode in device authorization response"
            raise RuntimeError(msg)

        links = resp.links
        activate_url = ""
        if links and links.activate:
            activate_url = self._expand(
                links.activate.href,
                {"userCode": resp.user_code},
            )
        elif resp.verification_url:
            activate_url = resp.verification_url

        authorized_url = links.authorized.href if links and links.authorized else ""

        log.info("device auth: code=%s", resp.user_code)
        return DeviceAuthInfo(
            user_code=resp.user_code,
            device_token=resp.device_token,
            activate_url=activate_url,
            authorized_url=authorized_url,
        )

    async def poll_authorized(self, auth_info: DeviceAuthInfo) -> bool:
        """Poll the authorized endpoint.  Returns True when the code is activated."""
        if not auth_info.authorized_url:
            return False

        extra = {
            "deviceToken": auth_info.device_token,
            "userCode": auth_info.user_code,
        }
        body, status = await self._get(auth_info.authorized_url, extra_vars=extra)

        if status == 200:
            # Try persistent login from the response
            resp = ViaplayAuthorizedPollResponse.model_validate(body)
            if resp.links and resp.links.persistent_login:
                _ = await self.persistent_login(resp.links.persistent_login.href)
            return True

        if status == 403:
            return False  # not yet activated

        log.debug("poll authorized returned %d", status)
        return False

    # -- stream resolution ---------------------------------------------------

    async def fetch_stream(self, play_url: str) -> StreamInfo:
        """Resolve *play_url* to a streaming manifest.

        Tries multiple HAL paths in order, matching the resolution strategy
        observed in real captures.

        Raises :class:`RuntimeError` if no stream URL can be found.
        """
        resolved_url = self._expand(play_url)
        body, status = await self._get(resolved_url, expand=False)
        if status != 200:
            log.warning(
                "stream fetch returned %d for %s: %s",
                status,
                resolved_url,
                body,
            )
            msg = f"stream fetch failed with status {status}"
            raise RuntimeError(msg)

        resp = ViaplayStreamResponse.model_validate(body)
        stream_type = self._resolve_stream_type(resp, play_url)
        duration = self._normalize_duration(resp.duration)
        title = resp.product.content.title if resp.product else None
        drm_url = self._extract_drm_url(resp)
        fallbacks = self._extract_fallbacks(resp)

        def _info(url: str, content_type: str) -> StreamInfo:
            return StreamInfo(
                url=url,
                content_type=content_type,
                stream_type=stream_type,
                duration=duration,
                title=title,
                drm_license_url=drm_url,
                fallback_urls=fallbacks,
            )

        # Path 1: _embedded.viaplay:media.contentUrl
        if resp.embedded and resp.embedded.media and resp.embedded.media.content_url:
            ct = resp.embedded.media.content_type or "application/dash+xml"
            return _info(resp.embedded.media.content_url, ct)

        # Path 2: top-level contentUrl
        if resp.content_url:
            return _info(resp.content_url, resp.content_type or "application/dash+xml")

        # Path 3: _links.viaplay:encryptedPlaylist
        if resp.links and resp.links.encrypted_playlist:
            ep = resp.links.encrypted_playlist
            # streamingFormat may be on the link or at the top level.
            fmt = ep.streaming_format or resp.streaming_format or ""
            ct = "application/x-mpegURL" if fmt == "HLS" else "application/dash+xml"
            return _info(ep.href, ct)

        # Path 4: _links.viaplay:playlist
        if resp.links and resp.links.playlist:
            return _info(resp.links.playlist.href, "application/dash+xml")

        # Path 5: _links.viaplay:stream
        if resp.links and resp.links.stream:
            return _info(resp.links.stream.href, "")

        msg = "no stream URL found in API response"
        raise RuntimeError(msg)

    @staticmethod
    def _extract_drm_url(resp: ViaplayStreamResponse) -> str | None:
        """Return the best DRM license URL from a stream response."""
        if resp.links is None:
            return None
        if resp.links.widevine_license:
            return resp.links.widevine_license.href
        if resp.links.license_link:
            return resp.links.license_link.href
        return None

    @staticmethod
    def _resolve_stream_type(
        resp: ViaplayStreamResponse,
        play_url: str,
    ) -> StreamType | None:
        """Infer Cast stream type from API response and play URL."""
        if resp.product and resp.product.stream_type:
            raw = resp.product.stream_type.upper()
            if raw == "LIVE":
                return StreamType.LIVE
            if raw in {"VOD", "BUFFERED"}:
                return StreamType.BUFFERED

        lowered = play_url.lower()
        if "bymediaguid" in lowered or "play-live." in lowered:
            return StreamType.LIVE
        if "byguid" in lowered:
            return StreamType.BUFFERED

        return None

    @staticmethod
    def _normalize_duration(raw_duration: float) -> float | None:
        """Normalize Viaplay duration to seconds.

        Viaplay responses are inconsistent: some return seconds, while others
        return milliseconds. We treat very large values as milliseconds.
        """
        if raw_duration <= 0:
            return None
        if raw_duration >= 100_000:
            return raw_duration / 1000
        return raw_duration

    @staticmethod
    def _extract_fallbacks(resp: ViaplayStreamResponse) -> tuple[str, ...]:
        """Return fallback CDN URLs from a stream response."""
        if resp.links is None:
            return ()
        return tuple(fb.href for fb in resp.links.fallback_media)
