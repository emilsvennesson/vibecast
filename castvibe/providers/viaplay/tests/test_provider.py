"""Tests for the ViaplayProvider."""

from __future__ import annotations

from typing import Any
from unittest.mock import AsyncMock, patch

import pytest

from castvibe._models import (
    LoadRequest,
    MediaGetStatusRequest,
    MediaInfo,
    MediaSetVolumeRequest,
    MediaStopRequest,
    PauseRequest,
    PlayerState,
    PlayRequest,
    SeekRequest,
    StreamType,
    Volume,
)
from castvibe.provider import (
    LaunchCredentials,
    MediaEventHandler,
    MediaLoadInfo,
    ProviderSession,
)
from castvibe.providers.viaplay._api import StreamInfo
from castvibe.providers.viaplay._provider import _NS_VIAPLAY, ViaplayProvider

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_session(
    session_id: str = "sess-1",
    transport_id: str = "pid-1",
    app_id: str = "6313CF39",
) -> tuple[ProviderSession, AsyncMock, AsyncMock]:
    """Create a ProviderSession with AsyncMock callbacks.

    Returns (session, broadcast_mock, send_mock) so callers can inspect
    calls without touching private attributes.
    """
    broadcast_mock = AsyncMock()
    send_mock = AsyncMock()
    session = ProviderSession(
        session_id=session_id,
        transport_id=transport_id,
        app_id=app_id,
        send_custom=send_mock,
        broadcast_custom=broadcast_mock,
        send_media_status=AsyncMock(),
    )
    return session, broadcast_mock, send_mock


def _make_handler() -> AsyncMock:
    """Create an AsyncMock that satisfies MediaEventHandler."""
    return AsyncMock(spec=MediaEventHandler)


# ---------------------------------------------------------------------------
# Basic properties
# ---------------------------------------------------------------------------


class TestProviderProperties:
    def test_app_ids(self) -> None:
        p = ViaplayProvider()
        assert p.app_ids() == frozenset({"6313CF39", "2DB7CC49"})

    def test_display_name(self) -> None:
        p = ViaplayProvider()
        assert p.display_name() == "Viaplay"

    def test_namespaces(self) -> None:
        p = ViaplayProvider()
        assert _NS_VIAPLAY in p.namespaces()


# ---------------------------------------------------------------------------
# on_launch / on_stop lifecycle
# ---------------------------------------------------------------------------


class TestLaunchStop:
    async def test_on_launch_creates_session(self, tmp_path: Any) -> None:
        p = ViaplayProvider(data_dir=tmp_path)
        session, _, _ = _make_session()
        creds = LaunchCredentials(credentials="token", credentials_type="iOS")

        await p.on_launch(session, creds)

        assert "sess-1" in p._sessions  # noqa: SLF001

    async def test_on_stop_removes_session(self, tmp_path: Any) -> None:
        p = ViaplayProvider(data_dir=tmp_path)
        session, _, _ = _make_session()
        creds = LaunchCredentials()

        await p.on_launch(session, creds)
        await p.on_stop(session)

        assert "sess-1" not in p._sessions  # noqa: SLF001

    async def test_on_stop_idempotent(self, tmp_path: Any) -> None:
        """Stopping a non-existent session should not raise."""
        p = ViaplayProvider(data_dir=tmp_path)
        session, _, _ = _make_session()
        await p.on_stop(session)  # no prior launch — should be fine


# ---------------------------------------------------------------------------
# on_sender_connected
# ---------------------------------------------------------------------------


class TestOnSenderConnected:
    async def test_broadcasts_empty_media_status_and_receiver_state(
        self, tmp_path: Any
    ) -> None:
        p = ViaplayProvider(data_dir=tmp_path)
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        await p.on_sender_connected(session, "sender-0")

        assert broadcast.call_count == 2

        # First call: empty MEDIA_STATUS
        first_ns, first_data = broadcast.call_args_list[0].args
        assert first_ns == "urn:x-cast:com.google.cast.media"
        assert first_data["type"] == "MEDIA_STATUS"
        assert first_data["status"] == []

        # Second call: RECEIVER_STATE on viaplay namespace
        second_ns, second_data = broadcast.call_args_list[1].args
        assert second_ns == _NS_VIAPLAY
        assert second_data["type"] == "RECEIVER_STATE"
        assert second_data["receiverState"]["status"] == "IDLE"


# ---------------------------------------------------------------------------
# on_message — SETUP_INFO
# ---------------------------------------------------------------------------


class TestSetupInfo:
    async def test_setup_info_triggers_auth_flow(self, tmp_path: Any) -> None:
        p = ViaplayProvider(data_dir=tmp_path)
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials(credentials="tok"))

        # Mock the auth flow to avoid real HTTP
        with patch.object(p, "_run_auth_flow", new_callable=AsyncMock) as mock_auth:
            await p.on_message(
                session,
                _NS_VIAPLAY,
                {
                    "type": "SETUP_INFO",
                    "contentRoot": "https://content.viaplay.se/stotta",
                    "countryCode": "se",
                    "userId": "user-1",
                    "profileId": "prof-1",
                },
            )
            # Wait for the spawned task
            state = p._sessions["sess-1"]  # noqa: SLF001
            if state.auth_task:
                await state.auth_task

        # The auth flow coroutine should have been awaited via the task
        mock_auth.assert_awaited_once()


# ---------------------------------------------------------------------------
# on_media_message — PLAY, PAUSE, SEEK, STOP, GET_STATUS
# ---------------------------------------------------------------------------


class TestMediaMessages:
    @pytest.fixture
    async def launched(
        self, tmp_path: Any
    ) -> tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock]:
        handler = _make_handler()
        p = ViaplayProvider(media_handler=handler, data_dir=tmp_path)
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())
        # Mark as authenticated and give it some media
        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.current_media = MediaInfo(
            content_id="https://x",
            content_type="application/dash+xml",
            stream_type=StreamType.BUFFERED,
        )
        state.stream_url = "https://cdn/manifest.mpd"
        state.player_state = PlayerState.PLAYING
        return p, session, handler, broadcast

    async def test_play(
        self,
        launched: tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock],
    ) -> None:
        p, session, handler, broadcast = launched
        await p.on_media_message(
            session, PlayRequest(request_id=10, media_session_id=1)
        )

        handler.on_play.assert_awaited_once_with("sess-1")
        last_data = broadcast.call_args.args[1]
        assert last_data["type"] == "MEDIA_STATUS"

    async def test_pause(
        self,
        launched: tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock],
    ) -> None:
        p, session, handler, _broadcast = launched
        await p.on_media_message(
            session, PauseRequest(request_id=11, media_session_id=1)
        )

        handler.on_pause.assert_awaited_once_with("sess-1")
        state = p._sessions["sess-1"]  # noqa: SLF001
        assert state.player_state == PlayerState.PAUSED

    async def test_seek(
        self,
        launched: tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock],
    ) -> None:
        p, session, handler, _broadcast = launched
        await p.on_media_message(
            session,
            SeekRequest(request_id=12, media_session_id=1, current_time=42.5),
        )

        handler.on_seek.assert_awaited_once_with("sess-1", 42.5)
        state = p._sessions["sess-1"]  # noqa: SLF001
        assert state.current_time == 42.5

    async def test_stop(
        self,
        launched: tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock],
    ) -> None:
        p, session, handler, _broadcast = launched
        await p.on_media_message(
            session, MediaStopRequest(request_id=13, media_session_id=1)
        )

        handler.on_stop.assert_awaited_once_with("sess-1")
        state = p._sessions["sess-1"]  # noqa: SLF001
        assert state.player_state == PlayerState.IDLE
        assert state.current_media is None

    async def test_get_status(
        self,
        launched: tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock],
    ) -> None:
        p, session, _handler, broadcast = launched
        await p.on_media_message(session, MediaGetStatusRequest(request_id=14))

        call_ns, data = broadcast.call_args.args
        assert call_ns == "urn:x-cast:com.google.cast.media"
        assert data["type"] == "MEDIA_STATUS"
        assert len(data["status"]) == 1

    async def test_volume(
        self,
        launched: tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock],
    ) -> None:
        p, session, handler, _broadcast = launched
        await p.on_media_message(
            session,
            MediaSetVolumeRequest(
                request_id=15,
                media_session_id=1,
                volume=Volume(level=0.5, muted=True),
            ),
        )

        handler.on_volume.assert_awaited_once_with("sess-1", 0.5, True)


# ---------------------------------------------------------------------------
# update_playback (external player → Cast senders)
# ---------------------------------------------------------------------------


class TestUpdatePlayback:
    async def test_broadcasts_media_status(self, tmp_path: Any) -> None:
        p = ViaplayProvider(data_dir=tmp_path)
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.current_media = MediaInfo(
            content_id="https://x",
            content_type="application/dash+xml",
            stream_type=StreamType.BUFFERED,
        )
        state.stream_url = "https://cdn/manifest.mpd"

        await p.update_playback("sess-1", PlayerState.PLAYING, 10.0)

        # state.broadcast wraps session.broadcast_custom which calls the mock
        broadcast.assert_awaited_once()
        call_ns, data = broadcast.call_args.args
        assert call_ns == "urn:x-cast:com.google.cast.media"
        assert data["status"][0]["playerState"] == "PLAYING"
        assert data["status"][0]["currentTime"] == 10.0

    async def test_noop_for_unknown_session(self, tmp_path: Any) -> None:
        """update_playback should silently return for unknown sessions."""
        p = ViaplayProvider(data_dir=tmp_path)
        await p.update_playback("nonexistent", PlayerState.IDLE)


# ---------------------------------------------------------------------------
# LOAD handling
# ---------------------------------------------------------------------------


class TestLoadHandling:
    async def test_load_resolves_stream_and_notifies_handler(
        self, tmp_path: Any
    ) -> None:
        handler = _make_handler()
        p = ViaplayProvider(media_handler=handler, data_dir=tmp_path)
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.auth_event.set()

        # Mock API.fetch_stream
        mock_stream = StreamInfo(
            url="https://cdn/v.mpd", content_type="application/dash+xml"
        )
        with patch.object(
            state.api,
            "fetch_stream",
            new_callable=AsyncMock,
            return_value=mock_stream,
        ):
            load_req = LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="https://x",
                    content_type="video/mp4",
                    stream_type=StreamType.BUFFERED,
                ),
                custom_data={"playUrl": "https://content.viaplay.se/play/1234"},
            )
            await p.on_media_message(session, load_req)
            # Wait for the spawned task
            if state.auth_task:
                await state.auth_task

        # Media handler should have been called with load info
        handler.on_load.assert_awaited_once()
        info: MediaLoadInfo = handler.on_load.call_args.args[0]
        assert info.stream_url == "https://cdn/v.mpd"
        assert info.session_id == "sess-1"

    async def test_load_propagates_drm_info(self, tmp_path: Any) -> None:
        handler = _make_handler()
        p = ViaplayProvider(media_handler=handler, data_dir=tmp_path)
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.auth_event.set()

        mock_stream = StreamInfo(
            url="https://cdn/v.mpd",
            content_type="application/dash+xml",
            drm_license_url="https://drm.example.com/license",
        )
        with patch.object(
            state.api,
            "fetch_stream",
            new_callable=AsyncMock,
            return_value=mock_stream,
        ):
            load_req = LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="https://x",
                    content_type="video/mp4",
                    stream_type=StreamType.BUFFERED,
                ),
                custom_data={"playUrl": "https://content.viaplay.se/play/1234"},
            )
            await p.on_media_message(session, load_req)
            if state.auth_task:
                await state.auth_task

        handler.on_load.assert_awaited_once()
        info: MediaLoadInfo = handler.on_load.call_args.args[0]
        assert info.drm is not None
        assert info.drm.system == "widevine"
        assert info.drm.license_url == "https://drm.example.com/license"

    async def test_load_fails_without_auth(self, tmp_path: Any) -> None:
        p = ViaplayProvider(data_dir=tmp_path)
        session, _, send = _make_session()
        await p.on_launch(session, LaunchCredentials())

        # NOT authenticated — auth_event is never set, wait_for will time out.
        # Patch wait_for to raise TimeoutError immediately.
        with patch("asyncio.wait_for", side_effect=TimeoutError):
            load_req = LoadRequest(
                request_id=1,
                media=MediaInfo(
                    content_id="https://x",
                    content_type="video/mp4",
                    stream_type=StreamType.BUFFERED,
                ),
                custom_data={"playUrl": "https://content.viaplay.se/play/1234"},
            )
            await p.on_media_message(session, load_req)
            state = p._sessions["sess-1"]  # noqa: SLF001
            if state.auth_task:
                await state.auth_task

        # Should have sent LOAD_FAILED
        _, data = send.call_args.args
        assert data["type"] == "LOAD_FAILED"

    async def test_load_fails_without_play_url(self, tmp_path: Any) -> None:
        p = ViaplayProvider(data_dir=tmp_path)
        session, _, send = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.auth_event.set()

        load_req = LoadRequest(
            request_id=2,
            media=MediaInfo(
                content_id="https://x",
                content_type="video/mp4",
                stream_type=StreamType.BUFFERED,
            ),
            custom_data={},
        )
        await p.on_media_message(session, load_req)
        if state.auth_task:
            await state.auth_task

        _, data = send.call_args.args
        assert data["type"] == "LOAD_FAILED"
        assert data["reason"] == "NO_PLAY_URL"
