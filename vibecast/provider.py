"""Provider plugin interfaces and discovery helpers."""

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

log = get_logger("provider")

_ENTRY_POINT_GROUP = "vibecast.providers"
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


class MediaResolveFailureCode(StrEnum):
    """Canonical provider media-resolution failure reasons."""

    INVALID_REQUEST = "INVALID_REQUEST"
    AUTH_REQUIRED = "AUTH_REQUIRED"
    ACCESS_DENIED = "ACCESS_DENIED"
    MISSING_CONTEXT = "MISSING_CONTEXT"
    CONTENT_UNAVAILABLE = "CONTENT_UNAVAILABLE"
    UPSTREAM_FAILURE = "UPSTREAM_FAILURE"
    PLAYER_FAILURE = "PLAYER_FAILURE"
    INTERNAL_ERROR = "INTERNAL_ERROR"


class ProviderMessageDisposition(StrEnum):
    """Outcome for provider custom namespace message handling."""

    HANDLED = "HANDLED"
    UNHANDLED = "UNHANDLED"


@dataclass(slots=True, frozen=True)
class MediaResolveFailure:
    """Structured provider failure result for media resolution."""

    code: MediaResolveFailureCode
    detail_code: str | None = None
    message: str | None = None
    retryable: bool = False


type MediaResolveResult = PlaybackMedia | MediaResolveFailure


class ProviderHttpStatusError(RuntimeError):
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
    """Map one provider exception into a canonical media failure."""
    status_code = _status_code_from_exception(exc)
    if status_code is not None:
        resolved_detail = detail_code
        resolved_retryable = retryable
        if isinstance(exc, ProviderHttpStatusError):
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
    if isinstance(exc, ProviderHttpStatusError):
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


class ProviderSessionStateError(RuntimeError):
    """Raised when a stateful provider callback has no backing session state."""

    def __init__(self, provider: Provider, session_id: str) -> None:
        self.provider_key = provider.provider_key()
        self.session_id = session_id
        super().__init__(
            f"missing session state provider={self.provider_key} session={session_id}"
        )


@dataclass(slots=True, frozen=True)
class ReceiverContext:
    """Receiver metadata made available to provider sessions."""

    friendly_name: str
    device_model: str
    device_id: str
    data_dir: Path
    user_agent: str = _DEFAULT_CAST_CONFIG.user_agent
    cast_device_capabilities: str = _DEFAULT_CAST_CAPABILITIES_HEADER
    display_width: int = 1920
    display_height: int = 1080


class ProviderSession:
    """Provider callback context for interacting with Cast senders."""

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


class Provider(ABC):
    """Provider plugin interface for app-specific Cast behavior."""

    @abstractmethod
    def app_ids(self) -> frozenset[str]:
        """Return supported Cast app IDs."""

    @abstractmethod
    def display_name(self) -> str:
        """Return human-readable app name shown in receiver status."""

    def namespaces(self) -> frozenset[str]:
        """Return custom namespaces handled by this provider."""
        return frozenset()

    def icon_url(self) -> str | None:
        """Return an icon URL for this app shown in receiver status.

        Defaults to ``None``.  Providers may override this to supply the
        Google-hosted icon URL that real Cast apps advertise.
        """
        return None

    @abstractmethod
    def provider_key(self) -> str:
        """Return stable provider key used for config and data directories."""

    def configure(self, config: dict[str, Any]) -> None:
        """Apply provider-specific configuration loaded from TOML."""

        _ = config

    @abstractmethod
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        """Handle ``LAUNCH`` for one of ``app_ids()``."""

    async def on_message(
        self,
        session: ProviderSession,
        namespace: str,
        data: dict[str, Any],
    ) -> ProviderMessageDisposition:
        """Handle provider namespace messages (excluding media namespace)."""
        _ = session
        _ = namespace
        _ = data
        return ProviderMessageDisposition.UNHANDLED

    @abstractmethod
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> MediaResolveResult:
        """Translate a Cast ``LOAD`` request into canonical playback media."""

    @staticmethod
    def normalize_stream_type(stream_type: StreamType) -> StreamType:
        """Normalize provider stream types to Cast media semantics."""
        if stream_type is StreamType.NONE:
            return StreamType.BUFFERED
        return stream_type

    def parse_message(
        self,
        adapter: TypeAdapter[_MessageT],
        data: dict[str, Any],
    ) -> _MessageT | None:
        """Parse provider custom message payloads using a shared policy."""
        try:
            return adapter.validate_python(data)
        except ValidationError:
            return None

    async def on_sender_connected(
        self,
        session: ProviderSession,
        sender_id: str,
    ) -> None:
        """Called when a sender connects to this app transport."""
        _ = session
        _ = sender_id

    async def on_stop(self, session: ProviderSession) -> None:
        """Called before an app session is removed."""
        _ = session

    async def on_playback_update(
        self,
        session: ProviderSession,
        state: PlaybackState,
    ) -> None:
        """Called when canonical playback state changes."""
        _ = session
        _ = state

    async def resolve_license(
        self,
        session: ProviderSession,
        request: LicenseRequest,
        route: LicenseRoute,
        forward: LicenseForwarder,
    ) -> LicenseResponse:
        """Resolve a DRM license request proxied by the player server."""
        _ = session
        _ = request
        return await forward(request, route)


class StatefulProvider[StateT](Provider, ABC):
    """Provider base class that manages per-session provider state."""

    def __init__(self) -> None:
        self._sessions: dict[str, StateT] = {}

    @abstractmethod
    async def create_session_state(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> StateT:
        """Build mutable provider state for one launched app session."""

    async def teardown_session_state(
        self,
        session: ProviderSession,
        state: StateT,
    ) -> None:
        """Release resources for one stopped app session."""
        _ = session
        _ = state

    @override
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        self._sessions[session.session_id] = await self.create_session_state(
            session,
            credentials,
        )

    @override
    async def on_stop(self, session: ProviderSession) -> None:
        state = self._sessions.pop(session.session_id, None)
        if state is None:
            return
        await self.teardown_session_state(session, state)

    def state_or_none(self, session: ProviderSession | str) -> StateT | None:
        """Return session state if present, else ``None``."""
        session_id = session if isinstance(session, str) else session.session_id
        return self._sessions.get(session_id)

    def require_state(self, session: ProviderSession | str) -> StateT:
        """Return session state or raise ``ProviderSessionStateError``."""
        session_id = session if isinstance(session, str) else session.session_id
        state = self._sessions.get(session_id)
        if state is None:
            raise ProviderSessionStateError(self, session_id)
        return state


def discover_providers() -> list[Provider]:
    """Discover and instantiate providers from package entry points."""
    providers: list[Provider] = []

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
        if not isinstance(instance, Provider):
            log.warning(
                "entry point %s did not produce Provider instance",
                entry_point.name,
            )
            continue
        providers.append(instance)
    return providers


class ProviderRegistry:
    """Registry mapping app IDs to provider instances."""

    __slots__ = ("_app_map", "_providers")

    def __init__(self, providers: Iterable[Provider] | None = None) -> None:
        self._app_map: dict[str, Provider] = {}
        self._providers: list[Provider] = []
        if providers is not None:
            for provider in providers:
                self.register(provider)

    def register(self, provider: Provider) -> None:
        """Register *provider* for each app ID it supports."""
        if provider not in self._providers:
            self._providers.append(provider)
        for app_id in provider.app_ids():
            existing = self._app_map.get(app_id)
            if existing is not None and existing is not provider:
                log.warning(
                    "app id %s already registered; replacing %s with %s",
                    app_id,
                    existing.__class__.__name__,
                    provider.__class__.__name__,
                )
            self._app_map[app_id] = provider

    def get(self, app_id: str) -> Provider | None:
        """Return provider registered for *app_id* if available."""
        return self._app_map.get(app_id)

    def all_providers(self) -> list[Provider]:
        """Return all registered provider instances."""
        return list(self._providers)


__all__ = [
    "CastModel",
    "LaunchCredentials",
    "LoadRequest",
    "MediaResolveFailure",
    "MediaResolveFailureCode",
    "MediaResolveResult",
    "ProviderHttpStatusError",
    "MediaInfo",
    "MediaMetadata",
    "ProviderMessageDisposition",
    "Provider",
    "ProviderSessionStateError",
    "ProviderRegistry",
    "ProviderSession",
    "ReceiverContext",
    "StatefulProvider",
    "discover_providers",
    "media_failure_from_exception",
    "media_failure_from_http_status",
]
