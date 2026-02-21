"""Provider plugin interfaces and discovery helpers."""

from __future__ import annotations

import re
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from importlib.metadata import EntryPoint, EntryPoints, entry_points
from typing import TYPE_CHECKING, Any, Protocol, cast, runtime_checkable

import castvibe._namespace as ns
from castvibe._log import get_logger
from castvibe._models import (
    IdleReason,
    MediaImage,
    MediaRequest,
    MediaStatus,
    MediaStatusResponse,
    PlayerState,
    StreamType,
)

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Iterable
    from pathlib import Path

    from httpx import AsyncClient

log = get_logger("provider")

_ENTRY_POINT_GROUP = "castvibe.providers"


# ---------------------------------------------------------------------------
# Data types for external media-player integration
# ---------------------------------------------------------------------------


@dataclass(slots=True, frozen=True)
class DrmInfo:
    """DRM configuration for protected content."""

    system: str
    """DRM system identifier (e.g. ``"widevine"``, ``"playready"``)."""

    license_url: str
    """URL of the license server."""

    headers: dict[str, str] = field(default_factory=dict)
    """Extra headers required for license requests."""


@dataclass(slots=True, frozen=True)
class MediaLoadInfo:
    """Everything an external player needs to start playback."""

    session_id: str
    stream_url: str
    content_type: str
    stream_type: StreamType
    title: str | None = None
    subtitle: str | None = None
    images: tuple[MediaImage, ...] = ()
    duration: float | None = None
    autoplay: bool = True
    start_time: float = 0.0
    drm: DrmInfo | None = None
    custom_data: dict[str, Any] = field(default_factory=dict)


@runtime_checkable
class MediaEventHandler(Protocol):
    """Integration point for external media players.

    Implement this protocol to receive media events from any provider.
    A Kodi addon, for example, would implement ``on_load`` to start
    playback and ``on_pause`` to pause the player.
    """

    async def on_load(self, info: MediaLoadInfo) -> None: ...
    async def on_play(self, session_id: str) -> None: ...
    async def on_pause(self, session_id: str) -> None: ...
    async def on_seek(self, session_id: str, position: float) -> None: ...
    async def on_stop(self, session_id: str) -> None: ...
    async def on_volume(self, session_id: str, level: float, muted: bool) -> None: ...


class DefaultMediaEventHandler:
    """No-op :class:`MediaEventHandler` used when no handler is provided."""

    async def on_load(self, info: MediaLoadInfo) -> None:
        _ = info

    async def on_play(self, session_id: str) -> None:
        _ = session_id

    async def on_pause(self, session_id: str) -> None:
        _ = session_id

    async def on_seek(self, session_id: str, position: float) -> None:
        _ = session_id
        _ = position

    async def on_stop(self, session_id: str) -> None:
        _ = session_id

    async def on_volume(self, session_id: str, level: float, muted: bool) -> None:
        _ = session_id
        _ = level
        _ = muted


# ---------------------------------------------------------------------------
# Credentials
# ---------------------------------------------------------------------------


@dataclass(slots=True, frozen=True)
class LaunchCredentials:
    """Credentials supplied with a ``LAUNCH`` request."""

    credentials: str | None = None
    credentials_type: str | None = None


@dataclass(slots=True, frozen=True)
class ReceiverContext:
    """Receiver metadata made available to provider sessions."""

    friendly_name: str
    device_model: str
    device_id: str
    data_dir: Path


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
        "_send_media_status",
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
        send_media_status: Callable[[MediaStatus, int], Awaitable[None]],
    ) -> None:
        self.session_id = session_id
        self.transport_id = transport_id
        self.app_id = app_id
        self.http_client = http_client
        self.receiver = receiver
        self._send_custom = send_custom
        self._broadcast_custom = broadcast_custom
        self._send_media_status = send_media_status

    async def send_custom(self, namespace: str, data: dict[str, Any]) -> None:
        """Send a message to the sender associated with this callback."""
        await self._send_custom(namespace, data)

    async def broadcast_custom(self, namespace: str, data: dict[str, Any]) -> None:
        """Broadcast a message to all senders connected to this transport."""
        await self._broadcast_custom(namespace, data)

    async def send_media_status(self, status: MediaStatus, request_id: int) -> None:
        """Send a ``MEDIA_STATUS`` response with a single status entry."""
        await self._send_media_status(status, request_id)


class Provider(ABC):
    """Provider plugin interface for app-specific Cast behavior."""

    @abstractmethod
    def app_ids(self) -> frozenset[str]:
        """Return supported Cast app IDs."""

    @abstractmethod
    def display_name(self) -> str:
        """Return human-readable app name shown in receiver status."""

    @abstractmethod
    def namespaces(self) -> frozenset[str]:
        """Return custom namespaces handled by this provider."""

    def provider_key(self) -> str:
        """Stable filesystem-safe key for receiver-managed provider data.

        Derived from the class name: strips a ``Provider`` suffix (if
        present) and converts PascalCase to snake_case.  For example,
        ``ViaplayProvider`` yields ``"viaplay"``; ``MyCustomProvider``
        yields ``"my_custom"``.

        Override this method if the default derivation is unsuitable
        (e.g. acronym-heavy names like ``ABCProvider`` produce
        ``"a_b_c"``), or if two providers share a class name.
        """
        name = self.__class__.__name__
        if name.endswith("Provider"):
            name = name[:-8]
        if not name:
            name = self.__class__.__name__
        return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()

    @abstractmethod
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        """Handle ``LAUNCH`` for one of ``app_ids()``."""

    @abstractmethod
    async def on_message(
        self,
        session: ProviderSession,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        """Handle non-media messages for provider namespaces."""

    async def on_media_message(
        self,
        session: ProviderSession,
        message: MediaRequest,
    ) -> None:
        """Handle media namespace messages.

        Default behavior mirrors the minimum receiver behavior by responding
        with an empty ``MEDIA_STATUS`` payload.
        """
        response = MediaStatusResponse(request_id=message.request_id, status=[])
        await session.send_custom(ns.MEDIA, response.model_dump(exclude_none=True))

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

    async def update_playback(
        self,
        session_id: str,
        player_state: PlayerState,
        current_time: float = 0.0,
        idle_reason: IdleReason | None = None,
    ) -> None:
        """Push playback state from an external player.

        Integration code (e.g. a Kodi addon) calls this when the player's
        state changes.  The provider translates it into a ``MEDIA_STATUS``
        broadcast so connected senders stay in sync.

        The default implementation is a no-op.  Override in providers that
        support external playback control.
        """
        _ = session_id
        _ = player_state
        _ = current_time
        _ = idle_reason


def discover_providers() -> list[Provider]:
    """Discover and instantiate providers from package entry points.

    Providers are constructed with **no arguments**.  Providers that
    require constructor parameters (e.g. ``media_handler``) should be
    instantiated manually and registered via :class:`ProviderRegistry`.
    """
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
    "DefaultMediaEventHandler",
    "DrmInfo",
    "LaunchCredentials",
    "MediaEventHandler",
    "MediaLoadInfo",
    "Provider",
    "ProviderRegistry",
    "ProviderSession",
    "ReceiverContext",
    "discover_providers",
]
