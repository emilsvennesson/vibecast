"""Tests for provider discovery and registry."""

from __future__ import annotations

from typing import Any, cast, override

import pytest

from vibecast._models import LoadRequest, StreamType
from vibecast.player import PlaybackMedia, PlaybackStream
from vibecast.provider import (
    LaunchCredentials,
    MediaResolveFailure,
    MediaResolveFailureCode,
    Provider,
    ProviderHttpStatusError,
    ProviderMessageDisposition,
    ProviderRegistry,
    ProviderSession,
    ProviderSessionStateError,
    StatefulProvider,
    discover_providers,
    media_failure_from_exception,
    media_failure_from_http_status,
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
    def provider_key(self) -> str:
        return self._name.lower()

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
    ) -> ProviderMessageDisposition:
        _ = session
        _ = namespace
        _ = data
        return ProviderMessageDisposition.HANDLED

    @override
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> PlaybackMedia:
        _ = session
        _ = load_request
        return PlaybackMedia(
            session_id="session",
            streams=(
                PlaybackStream(
                    url="https://example.com/video.mpd",
                    content_type="application/dash+xml",
                ),
            ),
            stream_type=StreamType.BUFFERED,
        )


class MinimalProvider(Provider):
    @override
    def app_ids(self) -> frozenset[str]:
        return frozenset({"minimal"})

    @override
    def display_name(self) -> str:
        return "Minimal"

    @override
    def provider_key(self) -> str:
        return "minimal"

    @override
    async def on_launch(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> None:
        _ = session
        _ = credentials

    @override
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> PlaybackMedia | MediaResolveFailure:
        _ = session
        _ = load_request
        return MediaResolveFailure(code=MediaResolveFailureCode.CONTENT_UNAVAILABLE)


class StatefulStringProvider(StatefulProvider[str]):
    @override
    def app_ids(self) -> frozenset[str]:
        return frozenset({"stateful"})

    @override
    def display_name(self) -> str:
        return "Stateful"

    @override
    def provider_key(self) -> str:
        return "stateful"

    @override
    async def create_session_state(
        self,
        session: ProviderSession,
        credentials: LaunchCredentials,
    ) -> str:
        _ = credentials
        return f"state:{session.session_id}"

    @override
    async def resolve_media(
        self,
        session: ProviderSession,
        load_request: LoadRequest,
    ) -> PlaybackMedia:
        _ = session
        _ = load_request
        return PlaybackMedia(
            session_id="session",
            streams=(
                PlaybackStream(
                    url="https://example.com/video.mpd",
                    content_type="application/dash+xml",
                ),
            ),
            stream_type=StreamType.BUFFERED,
        )


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
        assert group == "vibecast.providers"
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
            "vibecast.provider.entry_points",
            lambda: FakeEntryPoints(entries),
        )

        providers = discover_providers()

        names = {provider.display_name() for provider in providers}
        assert names == {"One", "Two"}


class TestProviderDefaults:
    async def test_minimal_provider_defaults(self) -> None:
        provider = MinimalProvider()
        assert provider.namespaces() == frozenset()

        session = ProviderSession(
            session_id="s1",
            transport_id="pid-1",
            app_id="minimal",
            http_client=cast("Any", object()),
            receiver=cast("Any", object()),
            send_custom=lambda _namespace, _data: _noop_async(),
            broadcast_custom=lambda _namespace, _data: _noop_async(),
        )

        _ = await provider.on_message(session, "urn:x-cast:test", {"type": "unknown"})


class TestStatefulProvider:
    async def test_state_lifecycle(self) -> None:
        provider = StatefulStringProvider()
        session = ProviderSession(
            session_id="s1",
            transport_id="pid-1",
            app_id="stateful",
            http_client=cast("Any", object()),
            receiver=cast("Any", object()),
            send_custom=lambda _namespace, _data: _noop_async(),
            broadcast_custom=lambda _namespace, _data: _noop_async(),
        )

        await provider.on_launch(session, LaunchCredentials())
        assert provider.state_or_none(session) == "state:s1"
        assert provider.require_state(session) == "state:s1"

        await provider.on_stop(session)
        assert provider.state_or_none(session) is None

    def test_require_state_raises(self) -> None:
        provider = StatefulStringProvider()
        with pytest.raises(ProviderSessionStateError):
            _ = provider.require_state("missing")


class TestMediaFailureHelpers:
    def test_maps_http_statuses(self) -> None:
        assert (
            media_failure_from_http_status(401).code
            is MediaResolveFailureCode.AUTH_REQUIRED
        )
        assert (
            media_failure_from_http_status(403).code
            is MediaResolveFailureCode.ACCESS_DENIED
        )
        assert (
            media_failure_from_http_status(404).code
            is MediaResolveFailureCode.CONTENT_UNAVAILABLE
        )
        assert (
            media_failure_from_http_status(422).code
            is MediaResolveFailureCode.INVALID_REQUEST
        )

        too_many = media_failure_from_http_status(429)
        assert too_many.code is MediaResolveFailureCode.UPSTREAM_FAILURE
        assert too_many.retryable is True

        server_error = media_failure_from_http_status(503)
        assert server_error.code is MediaResolveFailureCode.UPSTREAM_FAILURE
        assert server_error.retryable is True

    def test_maps_typed_http_exception(self) -> None:
        failure = media_failure_from_exception(
            ProviderHttpStatusError(
                404,
                "not found",
                detail_code="VIAPLAY_STREAM_FETCH",
            )
        )

        assert failure.code is MediaResolveFailureCode.CONTENT_UNAVAILABLE
        assert failure.detail_code == "VIAPLAY_STREAM_FETCH"
        assert failure.message == "not found"

    def test_maps_response_status_from_generic_exception(self) -> None:
        class _FakeResponse:
            status_code = 403

        class _FakeHttpError(RuntimeError):
            def __init__(self) -> None:
                self.response = _FakeResponse()
                super().__init__("forbidden")

        failure = media_failure_from_exception(_FakeHttpError())

        assert failure.code is MediaResolveFailureCode.ACCESS_DENIED
        assert failure.detail_code == "UPSTREAM_403"

    def test_marks_timeout_exception_retryable(self) -> None:
        failure = media_failure_from_exception(TimeoutError("request timed out"))

        assert failure.code is MediaResolveFailureCode.UPSTREAM_FAILURE
        assert failure.retryable is True


async def _noop_async() -> None:
    return None
