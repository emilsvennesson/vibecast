"""Provider plugin interfaces and discovery helpers."""

from __future__ import annotations

import re
from abc import ABC, abstractmethod
from dataclasses import dataclass
from importlib.metadata import EntryPoint, EntryPoints, entry_points
from typing import TYPE_CHECKING, Any, cast

from castvibe._log import get_logger

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Iterable
    from pathlib import Path

    from httpx import AsyncClient

    from castvibe._models import LoadRequest
    from castvibe.player import (
        LicenseRequest,
        LicenseResponse,
        LicenseRoute,
        PlaybackMedia,
        PlaybackState,
    )

    type LicenseForwarder = Callable[
        [LicenseRequest, LicenseRoute],
        Awaitable[LicenseResponse],
    ]

log = get_logger("provider")

_ENTRY_POINT_GROUP = "castvibe.providers"


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

    @abstractmethod
    def namespaces(self) -> frozenset[str]:
        """Return custom namespaces handled by this provider."""

    def provider_key(self) -> str:
        """Stable filesystem-safe key for receiver-managed provider data.

        Derived from the class name: strips a ``Provider`` suffix (if
        present) and converts PascalCase to snake_case. For example,
        ``ViaplayProvider`` yields ``"viaplay"``; ``MyCustomProvider``
        yields ``"my_custom"``.
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
        """Handle provider namespace messages (excluding media namespace)."""

    @abstractmethod
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> PlaybackMedia:
        """Translate a Cast ``LOAD`` request into canonical playback media."""

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
    "ReceiverContext",
    "discover_providers",
]
