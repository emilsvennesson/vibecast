"""Bundled Viaplay provider.

This implementation is intentionally minimal and only provides enough behavior
for app launch/session wiring and protocol tests.
"""

from __future__ import annotations

from typing import Any, override

from castvibe.provider import LaunchCredentials, Provider, ProviderSession


class ViaplayProvider(Provider):
    """Minimal provider for the Viaplay Cast app IDs."""

    _APP_IDS = frozenset({"6313CF39", "2DB7CC49"})
    _NAMESPACES = frozenset({"urn:x-cast:tv.viaplay.chromecast"})

    @override
    def app_ids(self) -> frozenset[str]:
        return self._APP_IDS

    @override
    def display_name(self) -> str:
        return "Viaplay"

    @override
    def namespaces(self) -> frozenset[str]:
        return self._NAMESPACES

    @override
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        _ = session
        _ = credentials

    @override
    async def on_message(
        self,
        session: ProviderSession,
        namespace: str,
        data: dict[str, Any],
    ) -> None:
        _ = session
        _ = namespace
        _ = data


__all__ = ["ViaplayProvider"]
