"""App interfaces and discovery helpers.

Glossary
--------
AppProvider  -- ABC that concrete Cast apps implement (SVT Play, Viaplay, …).
AppContext   -- Per-session context given to an AppProvider with send/broadcast helpers.
AppRegistry  -- Maps Cast application IDs to their AppProvider instances.
AppSession   -- Runtime transport session for an active app (internal, see ``_runtime.device``).
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from enum import StrEnum
from importlib.metadata import EntryPoint, EntryPoints, entry_points
from typing import TYPE_CHECKING, Any, TypeVar, cast, override

from pydantic import TypeAdapter, ValidationError

from vibecast._config import (
    CastConfig,
    cast_device_capabilities_header,
)
from vibecast._log import get_logger
from vibecast._models import LoadRequest, MediaInfo, MediaMetadata, StreamType
from vibecast._models._base import CastModel
from vibecast.player import PlaybackMedia

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Iterable
    from pathlib import Path

    from httpx import AsyncClient

    from vibecast.player import (
        LicenseRequest,
        LicenseResponse,
        LicenseRoute,
        PlaybackState,
    )

    type LicenseForwarder = Callable[
        [LicenseRequest, LicenseRoute],
        Awaitable[LicenseResponse],
    ]

log = get_logger("app")

_ENTRY_POINT_GROUP = "vibecast.apps"
_DEFAULT_CAST_CONFIG = CastConfig()
_DEFAULT_CAST_CAPABILITIES_HEADER = cast_device_capabilities_header(
    _DEFAULT_CAST_CONFIG.device_capabilities
)

_MessageT = TypeVar("_MessageT")


@dataclass(slots=True, frozen=True)
class LaunchCredentials:
    """Credentials supplied with a ``LAUNCH`` request."""

    credentials: str | None = None
    credentials_type: str | None = None


class PlaybackProxy(StrEnum):
    """Playback proxy stages an app can opt into."""

    MANIFEST = "manifest"


_DEFAULT_PLAYBACK_PROXIES: frozenset[PlaybackProxy] = frozenset()


@dataclass(slots=True, frozen=True)
class PlaybackProxyPolicy:
    """Playback proxy stages enabled for an app."""

    enabled: frozenset[PlaybackProxy] = _DEFAULT_PLAYBACK_PROXIES

    def enables(self, proxy: PlaybackProxy) -> bool:
        """Return whether a proxy stage is enabled."""
        return proxy in self.enabled


class MediaResolveFailureCode(StrEnum):
    """Canonical app media-resolution failure reasons."""

    INVALID_REQUEST = "INVALID_REQUEST"
    AUTH_REQUIRED = "AUTH_REQUIRED"
    ACCESS_DENIED = "ACCESS_DENIED"
    MISSING_CONTEXT = "MISSING_CONTEXT"
    CONTENT_UNAVAILABLE = "CONTENT_UNAVAILABLE"
    UPSTREAM_FAILURE = "UPSTREAM_FAILURE"
    PLAYER_FAILURE = "PLAYER_FAILURE"
    INTERNAL_ERROR = "INTERNAL_ERROR"


class AppMessageDisposition(StrEnum):
    """Outcome for app custom namespace message handling."""

    HANDLED = "HANDLED"
    UNHANDLED = "UNHANDLED"


@dataclass(slots=True, frozen=True)
class MediaResolveFailure:
    """Structured app failure result for media resolution."""

    code: MediaResolveFailureCode
    detail_code: str | None = None
    message: str | None = None
    retryable: bool = False


type MediaResolveResult = PlaybackMedia | MediaResolveFailure


class AppHttpStatusError(RuntimeError):
    """Typed upstream HTTP failure that preserves status metadata."""

    __slots__ = ("detail_code", "retryable", "status_code")

    def __init__(
        self,
        status_code: int,
        message: str | None = None,
        *,
        detail_code: str | None = None,
        retryable: bool | None = None,
    ) -> None:
        self.status_code = status_code
        self.detail_code = detail_code
        self.retryable = retryable
        super().__init__(
            message
            if message is not None
            else f"upstream request failed with {status_code}"
        )


def media_failure_from_http_status(
    status_code: int,
    *,
    detail_code: str | None = None,
    message: str | None = None,
    retryable: bool | None = None,
) -> MediaResolveFailure:
    """Map one upstream HTTP status code to a canonical media failure."""
    if status_code == 401:
        code = MediaResolveFailureCode.AUTH_REQUIRED
        default_retryable = False
    elif status_code == 403:
        code = MediaResolveFailureCode.ACCESS_DENIED
        default_retryable = False
    elif status_code == 404:
        code = MediaResolveFailureCode.CONTENT_UNAVAILABLE
        default_retryable = False
    elif status_code == 429 or status_code >= 500:
        code = MediaResolveFailureCode.UPSTREAM_FAILURE
        default_retryable = True
    elif status_code >= 400:
        code = MediaResolveFailureCode.INVALID_REQUEST
        default_retryable = False
    else:
        code = MediaResolveFailureCode.UPSTREAM_FAILURE
        default_retryable = True

    resolved_detail = (
        detail_code if detail_code is not None else f"UPSTREAM_{status_code}"
    )
    resolved_retryable = retryable if retryable is not None else default_retryable
    return MediaResolveFailure(
        code=code,
        detail_code=resolved_detail,
        message=message,
        retryable=resolved_retryable,
    )


def media_failure_from_exception(
    exc: Exception,
    *,
    detail_code: str | None = None,
    default_code: MediaResolveFailureCode = MediaResolveFailureCode.UPSTREAM_FAILURE,
    message: str | None = None,
    retryable: bool | None = None,
) -> MediaResolveFailure:
    """Map one app exception into a canonical media failure."""
    status_code = _status_code_from_exception(exc)
    if status_code is not None:
        resolved_detail = detail_code
        resolved_retryable = retryable
        if isinstance(exc, AppHttpStatusError):
            if resolved_detail is None:
                resolved_detail = exc.detail_code
            if resolved_retryable is None:
                resolved_retryable = exc.retryable
        return media_failure_from_http_status(
            status_code,
            detail_code=resolved_detail,
            message=message if message is not None else str(exc),
            retryable=resolved_retryable,
        )

    resolved_message = message if message is not None else str(exc)
    if not resolved_message:
        resolved_message = exc.__class__.__name__

    resolved_retryable = retryable
    if resolved_retryable is None:
        if isinstance(exc, TimeoutError | OSError):
            resolved_retryable = True
        else:
            resolved_retryable = (
                default_code is MediaResolveFailureCode.UPSTREAM_FAILURE
            )

    return MediaResolveFailure(
        code=default_code,
        detail_code=detail_code,
        message=resolved_message,
        retryable=resolved_retryable,
    )


def _status_code_from_exception(exc: Exception) -> int | None:
    if isinstance(exc, AppHttpStatusError):
        return exc.status_code

    raw_status = getattr(exc, "status_code", None)
    if isinstance(raw_status, int):
        return raw_status

    response = getattr(exc, "response", None)
    if response is None:
        return None
    response_status = getattr(response, "status_code", None)
    if isinstance(response_status, int):
        return response_status
    return None


class AppSessionStateError(RuntimeError):
    """Raised when a stateful app callback has no backing session state."""

    def __init__(self, app_provider: AppProvider, session_id: str) -> None:
        self.app_key = app_provider.app_key()
        self.session_id = session_id
        super().__init__(
            f"missing session state app={self.app_key} session={session_id}"
        )


@dataclass(slots=True, frozen=True)
class ReceiverContext:
    """Receiver metadata made available to app sessions."""

    friendly_name: str
    device_model: str
    device_id: str
    data_dir: Path
    user_agent: str = _DEFAULT_CAST_CONFIG.user_agent
    cast_device_capabilities: str = _DEFAULT_CAST_CAPABILITIES_HEADER
    display_width: int = 1920
    display_height: int = 1080


class AppContext:
    """App callback context for interacting with Cast senders."""

    __slots__ = (
        "app_id",
        "http_client",
        "receiver",
        "session_id",
        "transport_id",
        "_broadcast_custom",
        "_send_custom",
    )

    def __init__(
        self,
        *,
        session_id: str,
        transport_id: str,
        app_id: str,
        http_client: AsyncClient,
        receiver: ReceiverContext,
        send_custom: Callable[[str, dict[str, Any]], Awaitable[None]],
        broadcast_custom: Callable[[str, dict[str, Any]], Awaitable[None]],
    ) -> None:
        self.session_id = session_id
        self.transport_id = transport_id
        self.app_id = app_id
        self.http_client = http_client
        self.receiver = receiver
        self._send_custom = send_custom
        self._broadcast_custom = broadcast_custom

    async def send_custom(self, namespace: str, data: dict[str, Any]) -> None:
        """Send a message to the sender associated with this callback."""
        await self._send_custom(namespace, data)

    async def broadcast_custom(self, namespace: str, data: dict[str, Any]) -> None:
        """Broadcast a message to all senders connected to this transport."""
        await self._broadcast_custom(namespace, data)


class AppProvider(ABC):
    """App interface for app-specific Cast behavior."""

    @abstractmethod
    def app_ids(self) -> frozenset[str]:
        """Return supported Cast app IDs."""

    @abstractmethod
    def display_name(self) -> str:
        """Return human-readable app name shown in receiver status."""

    def namespaces(self) -> frozenset[str]:
        """Return custom namespaces handled by this app."""
        return frozenset()

    def icon_url(self) -> str | None:
        """Return an icon URL for this app shown in receiver status.

        Defaults to ``None``.  Apps may override this to supply the
        Google-hosted icon URL that real Cast apps advertise.
        """
        return None

    @abstractmethod
    def app_key(self) -> str:
        """Return stable app key used for config and data directories."""

    def configure(self, config: dict[str, Any]) -> None:
        """Apply app-specific configuration loaded from TOML."""

        _ = config

    def playback_proxy_policy(self) -> PlaybackProxyPolicy:
        """Return playback proxy stages this app wants the coordinator to use."""
        return PlaybackProxyPolicy()

    @abstractmethod
    async def on_launch(
        self,
        session: AppContext,
        credentials: LaunchCredentials,
    ) -> None:
        """Handle ``LAUNCH`` for one of ``app_ids()``."""

    async def on_message(
        self,
        session: AppContext,
        namespace: str,
        data: dict[str, Any],
    ) -> AppMessageDisposition:
        """Handle app namespace messages (excluding media namespace)."""
        _ = session
        _ = namespace
        _ = data
        return AppMessageDisposition.UNHANDLED

    @abstractmethod
    async def resolve_media(
        self,
        session: AppContext,
        load_request: LoadRequest,
    ) -> MediaResolveResult:
        """Translate a Cast ``LOAD`` request into canonical playback media."""

    @staticmethod
    def normalize_stream_type(stream_type: StreamType) -> StreamType:
        """Normalize app stream types to Cast media semantics."""
        if stream_type is StreamType.NONE:
            return StreamType.BUFFERED
        return stream_type

    def parse_message(
        self,
        adapter: TypeAdapter[_MessageT],
        data: dict[str, Any],
    ) -> _MessageT | None:
        """Parse app custom message payloads using a shared policy."""
        try:
            return adapter.validate_python(data)
        except ValidationError:
            return None

    async def on_sender_connected(
        self,
        session: AppContext,
        sender_id: str,
    ) -> None:
        """Called when a sender connects to this app transport."""
        _ = session
        _ = sender_id

    async def on_stop(self, session: AppContext) -> None:
        """Called before an app session is removed."""
        _ = session

    async def on_playback_update(
        self,
        session: AppContext,
        state: PlaybackState,
    ) -> None:
        """Called when canonical playback state changes."""
        _ = session
        _ = state

    async def resolve_license(
        self,
        session: AppContext,
        request: LicenseRequest,
        route: LicenseRoute,
        forward: LicenseForwarder,
    ) -> LicenseResponse:
        """Resolve a DRM license request proxied by the player bridge."""
        _ = session
        _ = request
        return await forward(request, route)


class StatefulAppProvider[StateT](AppProvider, ABC):
    """App base class that manages per-session state."""

    def __init__(self) -> None:
        self._sessions: dict[str, StateT] = {}

    @abstractmethod
    async def create_session_state(
        self,
        session: AppContext,
        credentials: LaunchCredentials,
    ) -> StateT:
        """Build mutable state for one launched app session."""

    async def teardown_session_state(
        self,
        session: AppContext,
        state: StateT,
    ) -> None:
        """Release resources for one stopped app session."""
        _ = session
        _ = state

    @override
    async def on_launch(
        self,
        session: AppContext,
        credentials: LaunchCredentials,
    ) -> None:
        self._sessions[session.session_id] = await self.create_session_state(
            session,
            credentials,
        )

    @override
    async def on_stop(self, session: AppContext) -> None:
        state = self._sessions.pop(session.session_id, None)
        if state is None:
            return
        await self.teardown_session_state(session, state)

    def state_or_none(self, session: AppContext | str) -> StateT | None:
        """Return session state if present, else ``None``."""
        session_id = session if isinstance(session, str) else session.session_id
        return self._sessions.get(session_id)

    def require_state(self, session: AppContext | str) -> StateT:
        """Return session state or raise ``AppSessionStateError``."""
        session_id = session if isinstance(session, str) else session.session_id
        state = self._sessions.get(session_id)
        if state is None:
            raise AppSessionStateError(self, session_id)
        return state


def discover_apps() -> list[AppProvider]:
    """Discover and instantiate apps from package entry points."""
    apps: list[AppProvider] = []

    eps = entry_points()
    discovered: EntryPoints | tuple[EntryPoint, ...]
    if hasattr(eps, "select"):
        discovered = eps.select(group=_ENTRY_POINT_GROUP)
    else:
        get = getattr(eps, "get", None)
        raw: object = get(_ENTRY_POINT_GROUP, ()) if callable(get) else ()
        if isinstance(raw, EntryPoints):
            discovered = raw
        elif isinstance(raw, list | tuple):
            discovered = cast(
                "tuple[EntryPoint, ...]", tuple(cast("list[object]", raw))
            )
        else:
            discovered = ()

    for entry_point in discovered:
        loaded = entry_point.load()
        instance = loaded() if callable(loaded) else loaded
        if not isinstance(instance, AppProvider):
            log.warning(
                "entry point %s did not produce AppProvider instance",
                entry_point.name,
            )
            continue
        apps.append(instance)
    return apps


class AppRegistry:
    """Registry mapping app IDs to app instances."""

    __slots__ = ("_app_map", "_apps")

    def __init__(self, apps: Iterable[AppProvider] | None = None) -> None:
        self._app_map: dict[str, AppProvider] = {}
        self._apps: list[AppProvider] = []
        if apps is not None:
            for app in apps:
                self.register(app)

    def register(self, app: AppProvider) -> None:
        """Register *app* for each app ID it supports."""
        if app not in self._apps:
            self._apps.append(app)
        for app_id in app.app_ids():
            existing = self._app_map.get(app_id)
            if existing is not None and existing is not app:
                log.warning(
                    "app id %s already registered; replacing %s with %s",
                    app_id,
                    existing.__class__.__name__,
                    app.__class__.__name__,
                )
            self._app_map[app_id] = app

    def get(self, app_id: str) -> AppProvider | None:
        """Return the app registered for *app_id* if available."""
        return self._app_map.get(app_id)

    def all_apps(self) -> list[AppProvider]:
        """Return all registered app instances."""
        return list(self._apps)


__all__ = [
    "CastModel",
    "LaunchCredentials",
    "LoadRequest",
    "MediaResolveFailure",
    "MediaResolveFailureCode",
    "MediaResolveResult",
    "AppHttpStatusError",
    "MediaInfo",
    "MediaMetadata",
    "PlaybackProxy",
    "PlaybackProxyPolicy",
    "AppMessageDisposition",
    "AppProvider",
    "AppSessionStateError",
    "AppRegistry",
    "AppContext",
    "ReceiverContext",
    "StatefulAppProvider",
    "discover_apps",
    "media_failure_from_exception",
    "media_failure_from_http_status",
]
