"""Tests for provider discovery and registry."""

from __future__ import annotations

from typing import Any, override

from castvibe.provider import (
    LaunchCredentials,
    Provider,
    ProviderRegistry,
    ProviderSession,
    discover_providers,
)


class DummyProvider(Provider):
    def __init__(self, name: str, app_ids: frozenset[str]) -> None:
        self._name = name
        self._app_ids = app_ids

    @override
    def app_ids(self) -> frozenset[str]:
        return self._app_ids

    @override
    def display_name(self) -> str:
        return self._name

    @override
    def namespaces(self) -> frozenset[str]:
        return frozenset({"urn:x-cast:test"})

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


class FakeEntryPoint:
    def __init__(self, name: str, loaded: Any) -> None:
        self.name = name
        self._loaded = loaded

    def load(self) -> Any:
        return self._loaded


class FakeEntryPoints:
    def __init__(self, entries: list[FakeEntryPoint]) -> None:
        self._entries = entries

    def select(self, *, group: str) -> list[FakeEntryPoint]:
        assert group == "castvibe.providers"
        return self._entries


class TestProviderRegistry:
    def test_register_and_get(self) -> None:
        provider = DummyProvider("One", frozenset({"app.1", "app.2"}))
        registry = ProviderRegistry()

        registry.register(provider)

        assert registry.get("app.1") is provider
        assert registry.get("app.2") is provider

    def test_register_multiple_providers(self) -> None:
        first = DummyProvider("First", frozenset({"a"}))
        second = DummyProvider("Second", frozenset({"b"}))
        registry = ProviderRegistry()

        registry.register(first)
        registry.register(second)

        assert registry.get("a") is first
        assert registry.get("b") is second
        assert registry.all_providers() == [first, second]

    def test_unknown_app_returns_none(self) -> None:
        registry = ProviderRegistry([DummyProvider("First", frozenset({"a"}))])
        assert registry.get("missing") is None


class TestDiscoverProviders:
    def test_discovers_entry_points(self, monkeypatch: Any) -> None:
        entries = [
            FakeEntryPoint("one", lambda: DummyProvider("One", frozenset({"one"}))),
            FakeEntryPoint("two", lambda: DummyProvider("Two", frozenset({"two"}))),
        ]
        monkeypatch.setattr(
            "castvibe.provider.entry_points",
            lambda: FakeEntryPoints(entries),
        )

        providers = discover_providers()

        names = {provider.display_name() for provider in providers}
        assert names == {"One", "Two"}
