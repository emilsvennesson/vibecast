"""Async HTTP client for the Viaplay API.

Handles authentication flows (persistent login, token login, device-code
authorization) and stream resolution.  Uses :mod:`aiohttp` with a cookie
jar for session persistence.
"""

from __future__ import annotations

import logging
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, cast

import aiohttp
from uritemplate import expand as uri_expand

log = logging.getLogger("castvibe.viaplay.api")

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_USER_AGENT = (
    "Mozilla/5.0 (Linux; Android 11.0; Build/RQ1A.210105.003) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/92.0.4515.0 "
    "Safari/537.36 CrKey/1.56.500000 DeviceType/AndroidTV"
)

_DEVICE_CODE_FALLBACK = "https://login.viaplay.com/api/device/code{?deviceKey,deviceId}"

# Domains whose cookies we persist to disk.
_COOKIE_DOMAINS = [
    "https://viaplay.com",
    "https://www.viaplay.com",
    "https://content.viaplay.com",
    "https://login.viaplay.com",
    "https://play-live.viaplay.com",
]


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
    links: dict[str, str] = field(default_factory=dict)
    raw: dict[str, Any] = field(default_factory=dict)


@dataclass(slots=True)
class DeviceAuthInfo:
    """Device-code authorization data."""

    user_code: str
    device_token: str
    activate_url: str
    raw: dict[str, Any]


@dataclass(slots=True)
class StreamInfo:
    """Resolved stream information."""

    url: str
    content_type: str


# ---------------------------------------------------------------------------
# ViaplayAPI
# ---------------------------------------------------------------------------


class ViaplayAPI:
    """Async HTTP client for the Viaplay API."""

    def __init__(self, data_dir: Path | None = None) -> None:
        self._data_dir = data_dir or Path.home() / ".castvibe"
        self._data_dir.mkdir(parents=True, exist_ok=True)

        self._device_id = self._load_or_create_device_id()
        self._jar = aiohttp.CookieJar(unsafe=True)
        self._session: aiohttp.ClientSession | None = None

        # Populated by set_setup_info()
        self._content_root = ""
        self._country_code = ""
        self._user_id = ""
        self._profile_id = ""

        self._load_cookies()

    # -- lifecycle -----------------------------------------------------------

    def _ensure_session(self) -> aiohttp.ClientSession:
        if self._session is None or self._session.closed:
            self._session = aiohttp.ClientSession(
                cookie_jar=self._jar,
                timeout=aiohttp.ClientTimeout(total=15),
            )
        return self._session

    async def close(self) -> None:
        """Close the HTTP session and save cookies."""
        self._save_cookies()
        if self._session is not None and not self._session.closed:
            await self._session.close()
            self._session = None

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
            "profileId": self._profile_id,
            "cse": "true",
        }
        if extra:
            variables.update(extra)
        return variables

    def _expand(self, template: str, extra: dict[str, str] | None = None) -> str:
        return uri_expand(template, var_dict=self._template_vars(extra))

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
        session = self._ensure_session()
        async with session.get(url, headers={"User-Agent": _USER_AGENT}) as resp:
            body = await resp.json(content_type=None)
            return body, resp.status

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
        session = self._ensure_session()
        async with session.get(url, headers={"User-Agent": _USER_AGENT}) as resp:
            body = await resp.read()
            return body, resp.status

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
        result = _parse_session_response(body)

        if status != 200:
            log.debug("session check returned status %d", status)
        elif result.user and result.user.user_id == self._user_id:
            log.info("session valid for user %s", self._user_id)
            self._save_cookies()
        else:
            log.debug("session check: no matching user")

        return result

    async def persistent_login(self, url: str) -> bool:
        """Attempt persistent login at *url*.  Returns True on success."""
        _, status = await self._get(url)
        if status == 200:
            self._save_cookies()
            log.info("persistent login succeeded")
            return True
        log.debug("persistent login returned %d", status)
        return False

    async def token_login(self, url_template: str, access_token: str) -> bool:
        """Attempt token login.  Returns True on success."""
        url = self._expand(url_template, {"accessToken": access_token})
        _, status = await self._get_raw(url, expand=False)
        if status == 200:
            self._save_cookies()
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
            auth_url = root_result.links.get("viaplay:deviceAuthorization")

        if not auth_url:
            # Re-fetch root to get links
            root_result = await self.check_session()
            auth_url = root_result.links.get("viaplay:deviceAuthorization")

        if not auth_url:
            auth_url = _DEVICE_CODE_FALLBACK

        body, status = await self._get(auth_url)
        if status != 200:
            msg = f"device authorization request failed with status {status}"
            raise RuntimeError(msg)

        user_code = body.get("userCode", "")
        device_token = body.get("deviceToken", "")
        if not user_code:
            msg = "no userCode in device authorization response"
            raise RuntimeError(msg)

        # Extract activate URL
        activate_url = _find_link(body, "viaplay:activate") or ""

        log.info("device auth: code=%s", user_code)
        return DeviceAuthInfo(
            user_code=user_code,
            device_token=device_token,
            activate_url=activate_url,
            raw=body,
        )

    async def poll_authorized(
        self,
        auth_info: DeviceAuthInfo,
        device_token: str,
        user_code: str,
    ) -> bool:
        """Poll the authorized endpoint.  Returns True when the code is activated."""
        authorized_url = _find_link(auth_info.raw, "viaplay:authorized")
        if not authorized_url:
            return False

        extra = {"deviceToken": device_token, "userCode": user_code}
        body, status = await self._get(authorized_url, extra_vars=extra)

        if status == 200:
            self._save_cookies()
            # Try persistent login from the response
            pl_url = _find_link(body, "viaplay:persistentLogin")
            if pl_url:
                _ = await self.persistent_login(pl_url)
            return True

        if status == 403:
            return False  # not yet activated

        log.debug("poll authorized returned %d", status)
        return False

    # -- stream resolution ---------------------------------------------------

    async def fetch_stream(self, play_url: str) -> StreamInfo:
        """Resolve *play_url* to a streaming manifest.

        Tries multiple HAL paths in order, matching the go-cast resolution
        strategy observed in real captures.

        Raises :class:`RuntimeError` if no stream URL can be found.
        """
        body, status = await self._get(play_url)
        if status != 200:
            msg = f"stream fetch failed with status {status}"
            raise RuntimeError(msg)

        # Path 1: _embedded.viaplay:media.contentUrl
        embedded: dict[str, Any] = body.get("_embedded", {})
        media_obj: object = embedded.get("viaplay:media")
        if isinstance(media_obj, dict):
            media = cast("dict[str, Any]", media_obj)
            content_url: object = media.get("contentUrl")
            if isinstance(content_url, str) and content_url:
                ct_val: object = media.get("contentType", "application/dash+xml")
                return StreamInfo(url=content_url, content_type=str(ct_val))

        # Path 2: top-level contentUrl
        top_url: object = body.get("contentUrl")
        if isinstance(top_url, str) and top_url:
            top_ct: object = body.get("contentType", "application/dash+xml")
            return StreamInfo(url=top_url, content_type=str(top_ct))

        # Path 3: _links.viaplay:encryptedPlaylist
        url = _find_link(body, "viaplay:encryptedPlaylist")
        if url:
            fmt = body.get("streamingFormat", "")
            ct = "application/x-mpegURL" if fmt == "HLS" else "application/dash+xml"
            return StreamInfo(url=url, content_type=ct)

        # Path 4: _links.viaplay:playlist
        url = _find_link(body, "viaplay:playlist")
        if url:
            return StreamInfo(url=url, content_type="application/dash+xml")

        # Path 5: _links.viaplay:stream
        url = _find_link(body, "viaplay:stream")
        if url:
            return StreamInfo(url=url, content_type="")

        msg = "no stream URL found in API response"
        raise RuntimeError(msg)

    # -- device ID / cookie persistence --------------------------------------

    def _load_or_create_device_id(self) -> str:
        path = self._data_dir / "viaplay_device_id"
        if path.exists():
            device_id = path.read_text().strip()
            if device_id:
                return device_id
        device_id = str(uuid.uuid4())
        _ = path.write_text(device_id)
        return device_id

    def _cookie_path(self) -> Path:
        return self._data_dir / "viaplay_cookies.json"

    def _save_cookies(self) -> None:
        """Persist cookies using aiohttp's built-in file saving."""
        try:
            self._jar.save(self._cookie_path())
        except Exception:
            log.debug("failed to save cookies", exc_info=True)

    def _load_cookies(self) -> None:
        """Load previously persisted cookies."""
        path = self._cookie_path()
        if not path.exists():
            return
        try:
            self._jar.load(path)
        except Exception:
            log.debug("failed to load cookies", exc_info=True)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _find_link(body: dict[str, Any], link_name: str) -> str | None:
    """Extract a HAL link ``href`` from a JSON response body."""
    links: object = body.get("_links", {})
    if not isinstance(links, dict):
        return None
    links_dict = cast("dict[str, Any]", links)
    link: object = links_dict.get(link_name)
    if not isinstance(link, dict):
        return None
    link_dict = cast("dict[str, Any]", link)
    href: object = link_dict.get("href")
    if isinstance(href, str):
        return href
    return None


def _extract_links(body: dict[str, Any]) -> dict[str, str]:
    """Extract all HAL ``_links`` as a flat name->href mapping."""
    result: dict[str, str] = {}
    links: object = body.get("_links", {})
    if not isinstance(links, dict):
        return result
    links_dict = cast("dict[str, Any]", links)
    for key, val in links_dict.items():
        if not isinstance(val, dict):
            continue
        entry = cast("dict[str, Any]", val)
        href: object = entry.get("href")
        if isinstance(href, str):
            result[key] = href
    return result


def _parse_session_response(body: dict[str, Any]) -> SessionCheckResult:
    """Parse a content-root API response into a :class:`SessionCheckResult`."""
    user: ViaplayUser | None = None
    user_data: object = body.get("user")
    if isinstance(user_data, dict):
        ud = cast("dict[str, Any]", user_data)
        uid: object = ud.get("userId", "")
        fname: object = ud.get("firstName", "")
        lname: object = ud.get("lastName", "")
        user = ViaplayUser(
            user_id=str(uid),
            first_name=str(fname),
            last_name=str(lname),
        )
    return SessionCheckResult(
        user=user,
        links=_extract_links(body),
        raw=body,
    )
