"""Provider plugin interfaces and discovery helpers."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from importlib.metadata import EntryPoint, EntryPoints, entry_points
from typing import TYPE_CHECKING, Any, cast

import castvibe._namespace as ns
from castvibe._log import get_logger
from castvibe._models import MediaRequest, MediaStatus, MediaStatusResponse

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Iterable

log = get_logger("provider")

_ENTRY_POINT_GROUP = "castvibe.providers"


@dataclass(slots=True, frozen=True)
class LaunchCredentials:
    """Credentials supplied with a ``LAUNCH`` request."""

    credentials: str | None = None
    credentials_type: str | None = None


class ProviderSession:
    """Provider callback context for interacting with Cast senders."""

    __slots__ = (
        "app_id",
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
        send_custom: Callable[[str, dict[str, Any]], Awaitable[None]],
        broadcast_custom: Callable[[str, dict[str, Any]], Awaitable[None]],
        send_media_status: Callable[[MediaStatus, int], Awaitable[None]],
    ) -> None:
        self.session_id = session_id
        self.transport_id = transport_id
        self.app_id = app_id
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
    "LaunchCredentials",
    "Provider",
    "ProviderRegistry",
    "ProviderSession",
    "discover_providers",
]
