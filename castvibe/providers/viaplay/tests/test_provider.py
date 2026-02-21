"""Tests for the ViaplayProvider."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import TYPE_CHECKING, Any, cast
from unittest.mock import AsyncMock, patch

import pytest

if TYPE_CHECKING:
    from httpx import AsyncClient

from castvibe._models import (
    LoadRequest,
    MediaGetStatusRequest,
    MediaInfo,
    MediaSetVolumeRequest,
    MediaStopRequest,
    PauseRequest,
    PlayerState,
    PlayRequest,
    QueueGetItemIdsRequest,
    SeekRequest,
    StreamType,
    Volume,
)
from castvibe.provider import (
    LaunchCredentials,
    MediaEventHandler,
    MediaLoadInfo,
    ProviderSession,
    ReceiverContext,
)
from castvibe.providers.viaplay._api import (
    DeviceAuthInfo,
    SessionCheckResult,
    StreamInfo,
    ViaplayUser,
)
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
    receiver_data_dir = Path("/tmp/castvibe-tests/providers/viaplay")
    session = ProviderSession(
        session_id=session_id,
        transport_id=transport_id,
        app_id=app_id,
        http_client=cast("AsyncClient", object()),
        receiver=ReceiverContext(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="receiver-device-id",
            data_dir=receiver_data_dir,
        ),
        send_custom=send_mock,
        broadcast_custom=broadcast_mock,
        send_media_status=AsyncMock(),
    )
    return session, broadcast_mock, send_mock


def _make_handler() -> AsyncMock:
    """Create an AsyncMock that satisfies MediaEventHandler."""
    return AsyncMock(spec=MediaEventHandler)


@pytest.fixture
def _tmp_path(tmp_path: Path) -> Path:  # pyright: ignore[reportUnusedFunction]
    return tmp_path


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
    async def test_on_launch_creates_session(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
        session, _, _ = _make_session()
        creds = LaunchCredentials(credentials="token", credentials_type="iOS")

        await p.on_launch(session, creds)

        assert "sess-1" in p._sessions  # noqa: SLF001

    async def test_on_stop_removes_session(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
        session, _, _ = _make_session()
        creds = LaunchCredentials()

        await p.on_launch(session, creds)
        await p.on_stop(session)

        assert "sess-1" not in p._sessions  # noqa: SLF001

    async def test_on_stop_idempotent(self, _tmp_path: Any) -> None:
        """Stopping a non-existent session should not raise."""
        p = ViaplayProvider()
        session, _, _ = _make_session()
        await p.on_stop(session)  # no prior launch — should be fine


# ---------------------------------------------------------------------------
# on_sender_connected
# ---------------------------------------------------------------------------


class TestOnSenderConnected:
    async def test_broadcasts_empty_media_status_and_receiver_state(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
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
        assert second_data["receiverState"]["isScrubbable"] is True

    async def test_reconnect_with_active_media_sends_full_status(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.player_state = PlayerState.PLAYING
        state.current_time = 42.0
        state.current_media = MediaInfo(
            content_id="https://x",
            content_type="application/dash+xml",
            stream_type=StreamType.BUFFERED,
            duration=120.0,
        )
        state.stream_url = "https://cdn/manifest.mpd"

        await p.on_sender_connected(session, "sender-0")

        assert broadcast.await_count == 2
        media_ns, media_data = broadcast.await_args_list[0].args
        assert media_ns == "urn:x-cast:com.google.cast.media"
        assert media_data["status"][0]["playerState"] == "PLAYING"
        assert media_data["status"][0]["media"]["duration"] == 120.0

        state_ns, state_data = broadcast.await_args_list[1].args
        assert state_ns == _NS_VIAPLAY
        assert state_data["receiverState"]["status"] == "CASTING"


# ---------------------------------------------------------------------------
# on_message — SETUP_INFO
# ---------------------------------------------------------------------------


class TestSetupInfo:
    async def test_setup_info_triggers_auth_flow(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
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

    async def test_setup_info_resets_stale_auth_state(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials(credentials="tok"))

        state = p._sessions["sess-1"]  # noqa: SLF001

        blocker = asyncio.Event()

        async def _pending() -> None:
            _ = await blocker.wait()

        stale_auth_task = asyncio.create_task(_pending())
        stale_poll_task = asyncio.create_task(_pending())
        state.auth_task = stale_auth_task
        state.poll_task = stale_poll_task
        state.authenticated = True
        state.auth_pending = True
        state.auth_event.set()
        state.user_display_name = "Old User"

        with patch.object(p, "_run_auth_flow", new_callable=AsyncMock) as mock_auth:
            await p.on_message(
                session,
                _NS_VIAPLAY,
                {
                    "type": "SETUP_INFO",
                    "contentRoot": "https://content.viaplay.se/stotta",
                    "countryCode": "se",
                    "userId": "user-2",
                    "profileId": "prof-2",
                },
            )

            assert stale_auth_task.done() is True
            assert stale_auth_task.cancelled() is True
            assert stale_poll_task.done() is True
            assert stale_poll_task.cancelled() is True
            assert state.authenticated is False
            assert state.auth_pending is False
            assert state.auth_event.is_set() is False
            assert state.user_display_name == ""
            assert state.auth_task is not stale_auth_task
            assert state.poll_task is None

            if state.auth_task:
                await state.auth_task

        mock_auth.assert_awaited_once()


# ---------------------------------------------------------------------------
# on_message — AUTHORIZATION_DONE
# ---------------------------------------------------------------------------


class TestAuthorizationDone:
    async def test_success_false_does_not_trigger_completion(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        with patch.object(
            p,
            "_complete_device_auth",
            new_callable=AsyncMock,
        ) as mock_complete:
            await p.on_message(
                session,
                _NS_VIAPLAY,
                {
                    "type": "AUTHORIZATION_DONE",
                    "success": False,
                },
            )
            await asyncio.sleep(0)

        mock_complete.assert_not_awaited()

    async def test_success_true_triggers_completion(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        with patch.object(
            p,
            "_complete_device_auth",
            new_callable=AsyncMock,
        ) as mock_complete:
            await p.on_message(
                session,
                _NS_VIAPLAY,
                {
                    "type": "AUTHORIZATION_DONE",
                    "success": True,
                },
            )
            state = p._sessions["sess-1"]  # noqa: SLF001
            if state.auth_task:
                await state.auth_task

        mock_complete.assert_awaited_once()

    async def test_goto_idle_resets_media_state(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.player_state = PlayerState.PLAYING
        state.current_media = MediaInfo(
            content_id="https://x",
            content_type="application/dash+xml",
            stream_type=StreamType.BUFFERED,
        )
        state.stream_url = "https://cdn/manifest.mpd"
        state.current_product_url = (
            "https://content.viaplay.com/{deviceKey}/byguid/1{?productsPerPage}"
        )

        blocker = asyncio.Event()

        async def _pending_load() -> None:
            _ = await blocker.wait()

        pending_load = asyncio.create_task(_pending_load())
        state.load_task = pending_load

        await p.on_message(
            session,
            _NS_VIAPLAY,
            {"type": "GOTO_IDLE", "userId": "u1", "profileId": "p1"},
        )

        assert pending_load.done() is True
        assert pending_load.cancelled() is True
        assert state.player_state == PlayerState.IDLE
        assert state.current_media is None
        assert state.stream_url == ""
        assert state.current_product_url is None
        assert state.load_task is None

        ns_name, data = broadcast.await_args_list[-1].args
        assert ns_name == _NS_VIAPLAY
        assert data["type"] == "RECEIVER_STATE"
        assert data["receiverState"]["status"] == "IDLE"


# ---------------------------------------------------------------------------
# _complete_device_auth
# ---------------------------------------------------------------------------


class TestCompleteDeviceAuth:
    async def test_does_not_authenticate_without_session_user(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.user_id = "expected-user"
        state.auth_pending = True

        with patch.object(
            state.api,
            "check_session",
            new_callable=AsyncMock,
            return_value=SessionCheckResult(user=None),
        ):
            await p._complete_device_auth(session, state)

        assert state.authenticated is False
        assert state.auth_pending is True
        assert state.auth_event.is_set() is False
        broadcast.assert_not_awaited()

    async def test_does_not_authenticate_on_user_mismatch(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.user_id = "expected-user"
        state.auth_pending = True

        with patch.object(
            state.api,
            "check_session",
            new_callable=AsyncMock,
            return_value=SessionCheckResult(user=ViaplayUser(user_id="other-user")),
        ):
            await p._complete_device_auth(session, state)

        assert state.authenticated is False
        assert state.auth_pending is True
        assert state.auth_event.is_set() is False
        broadcast.assert_not_awaited()


# ---------------------------------------------------------------------------
# _run_auth_flow
# ---------------------------------------------------------------------------


class TestAuthFlow:
    async def test_persistent_login_mismatch_falls_back_to_device_auth(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.user_id = "expected-user"
        root = SessionCheckResult(persistent_login_url="https://login.viaplay.com/pl")
        mismatch = SessionCheckResult(user=ViaplayUser(user_id="other-user"))

        with (
            patch.object(
                state.api,
                "check_session",
                new_callable=AsyncMock,
                side_effect=[root, mismatch],
            ),
            patch.object(
                state.api,
                "persistent_login",
                new_callable=AsyncMock,
                return_value=True,
            ),
            patch.object(
                p,
                "_start_device_auth",
                new_callable=AsyncMock,
            ) as mock_start,
        ):
            await p._run_auth_flow(session, state)

        assert state.authenticated is False
        mock_start.assert_awaited_once_with(session, state, root)


# ---------------------------------------------------------------------------
# _start_device_auth
# ---------------------------------------------------------------------------


class TestStartDeviceAuth:
    async def test_uses_expanded_activate_url_without_duplicating_user_code(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.user_id = "user-1"
        state.profile_id = "profile-1"

        activate_url = (
            "https://login.viaplay.com/api/device/activate"
            "?deviceKey=chromecastgoogletv4k-se&userCode=ABCD"
        )
        auth_info = DeviceAuthInfo(
            user_code="ABCD",
            device_token="dt-1",
            activate_url=activate_url,
            authorized_url="https://login.viaplay.com/api/device/authorized{?deviceId,deviceToken,userCode}",
        )

        with (
            patch.object(
                state.api,
                "get_device_authorization",
                new_callable=AsyncMock,
                return_value=auth_info,
            ),
            patch.object(p, "_poll_for_authorization", new_callable=AsyncMock),
        ):
            await p._start_device_auth(session, state, SessionCheckResult())

        # 1st broadcast is AUTHORIZATION_REQUIRED
        auth_required = broadcast.await_args_list[0].args[1]
        assert auth_required["type"] == "AUTHORIZATION_REQUIRED"
        assert auth_required["authorizationUrl"] == activate_url
        assert auth_required["receiverState"]["authorizationUrl"] == activate_url
        assert auth_required["receiverState"]["userCode"] == "ABCD"


# ---------------------------------------------------------------------------
# on_media_message — PLAY, PAUSE, SEEK, STOP, GET_STATUS
# ---------------------------------------------------------------------------


class TestMediaMessages:
    @pytest.fixture
    async def launched(
        self, _tmp_path: Any
    ) -> tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock]:
        handler = _make_handler()
        p = ViaplayProvider(media_handler=handler)
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
        p, session, handler, broadcast = launched
        await p.on_media_message(
            session,
            MediaSetVolumeRequest(
                request_id=15,
                media_session_id=1,
                volume=Volume(level=0.5, muted=True),
            ),
        )

        handler.on_volume.assert_awaited_once_with("sess-1", 0.5, True)
        state = p._sessions["sess-1"]  # noqa: SLF001
        assert state.volume_level == 0.5
        assert state.volume_muted is True

        call_ns, data = broadcast.call_args.args
        assert call_ns == "urn:x-cast:com.google.cast.media"
        assert data["status"][0]["volume"] == {"level": 0.5, "muted": True}

    async def test_volume_partial_update_preserves_existing_level(
        self,
        launched: tuple[ViaplayProvider, ProviderSession, AsyncMock, AsyncMock],
    ) -> None:
        p, session, handler, broadcast = launched
        state = p._sessions["sess-1"]  # noqa: SLF001
        state.volume_level = 0.35
        state.volume_muted = False

        await p.on_media_message(
            session,
            MediaSetVolumeRequest(
                request_id=19,
                media_session_id=1,
                volume=Volume.model_validate({"muted": True}),
            ),
        )

        handler.on_volume.assert_awaited_once_with("sess-1", 0.35, True)
        assert state.volume_level == 0.35
        assert state.volume_muted is True

        _, data = broadcast.call_args.args
        assert data["status"][0]["volume"] == {"level": 0.35, "muted": True}

    async def test_queue_get_item_ids(
        self,
        _tmp_path: Any,
    ) -> None:
        p = ViaplayProvider()
        session, _broadcast, send = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.current_media = MediaInfo(
            content_id="https://x",
            content_type="application/dash+xml",
            stream_type=StreamType.BUFFERED,
        )

        await p.on_media_message(
            session,
            QueueGetItemIdsRequest(request_id=16, media_session_id=1),
        )

        send.assert_awaited_once()
        call_ns, data = send.await_args_list[0].args
        assert call_ns == "urn:x-cast:com.google.cast.media"
        assert data == {
            "type": "QUEUE_ITEM_IDS",
            "requestId": 16,
            "itemIds": [1],
            "sequenceNumber": 0,
        }

    async def test_queue_get_item_ids_during_inflight_load(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
        session, _broadcast, send = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001

        blocker = asyncio.Event()

        async def _pending() -> None:
            _ = await blocker.wait()

        state.load_task = asyncio.create_task(_pending())

        try:
            await p.on_media_message(
                session,
                QueueGetItemIdsRequest(request_id=17, media_session_id=1),
            )
        finally:
            blocker.set()
            if state.load_task:
                await state.load_task

        send.assert_awaited_once()
        call_ns, data = send.await_args_list[0].args
        assert call_ns == "urn:x-cast:com.google.cast.media"
        assert data == {
            "type": "QUEUE_ITEM_IDS",
            "requestId": 17,
            "itemIds": [1],
            "sequenceNumber": 0,
        }

    async def test_queue_get_item_ids_during_auth_flow_without_load(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
        session, _broadcast, send = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001

        blocker = asyncio.Event()

        async def _pending() -> None:
            _ = await blocker.wait()

        state.auth_task = asyncio.create_task(_pending())

        try:
            await p.on_media_message(
                session,
                QueueGetItemIdsRequest(request_id=18, media_session_id=1),
            )
        finally:
            blocker.set()
            if state.auth_task:
                await state.auth_task

        send.assert_awaited_once()
        call_ns, data = send.await_args_list[0].args
        assert call_ns == "urn:x-cast:com.google.cast.media"
        assert data == {
            "type": "QUEUE_ITEM_IDS",
            "requestId": 18,
            "itemIds": [],
            "sequenceNumber": 0,
        }


# ---------------------------------------------------------------------------
# update_playback (external player → Cast senders)
# ---------------------------------------------------------------------------


class TestUpdatePlayback:
    async def test_broadcasts_media_status(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
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

        # update_playback emits both MEDIA_STATUS and RECEIVER_STATE
        assert broadcast.await_count == 2

        media_ns, media_data = broadcast.await_args_list[0].args
        assert media_ns == "urn:x-cast:com.google.cast.media"
        assert media_data["status"][0]["playerState"] == "PLAYING"
        assert media_data["status"][0]["currentTime"] == 10.0

        state_ns, state_data = broadcast.await_args_list[1].args
        assert state_ns == _NS_VIAPLAY
        assert state_data["type"] == "RECEIVER_STATE"
        assert state_data["receiverState"]["status"] == "CASTING"

    async def test_noop_for_unknown_session(self, _tmp_path: Any) -> None:
        """update_playback should silently return for unknown sessions."""
        p = ViaplayProvider()
        await p.update_playback("nonexistent", PlayerState.IDLE)

    async def test_emits_posdur_when_duration_known(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
        session, broadcast, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.current_media = MediaInfo(
            content_id="https://x",
            content_type="application/dash+xml",
            stream_type=StreamType.BUFFERED,
            duration=2535.48,
        )
        state.stream_url = "https://cdn/manifest.mpd"

        await p.update_playback("sess-1", PlayerState.PLAYING, 260.9)

        assert broadcast.await_count == 3
        posdur_ns, posdur_data = broadcast.await_args_list[2].args
        assert posdur_ns == _NS_VIAPLAY
        assert posdur_data["type"] == "POSDUR"
        assert posdur_data["position"] == 260
        assert posdur_data["duration"] == 2535


# ---------------------------------------------------------------------------
# LOAD handling
# ---------------------------------------------------------------------------


class TestLoadHandling:
    async def test_load_resolves_stream_and_notifies_handler(
        self, _tmp_path: Any
    ) -> None:
        handler = _make_handler()
        p = ViaplayProvider(media_handler=handler)
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.auth_event.set()

        # Mock API.fetch_stream
        mock_stream = StreamInfo(
            url="https://cdn/v.mpd",
            content_type="application/dash+xml",
            stream_type=StreamType.LIVE,
            duration=3600.0,
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
                    stream_type=StreamType.NONE,
                    duration=0,
                ),
                custom_data={"playUrl": "https://content.viaplay.se/play/1234"},
            )
            await p.on_media_message(session, load_req)
            # Wait for the spawned task
            if state.load_task:
                await state.load_task

        # Media handler should have been called with load info
        handler.on_load.assert_awaited_once()
        info: MediaLoadInfo = handler.on_load.call_args.args[0]
        assert info.stream_url == "https://cdn/v.mpd"
        assert info.session_id == "sess-1"
        assert info.stream_type == StreamType.LIVE
        assert info.duration == 3600.0
        assert state.current_media is not None
        assert state.current_media.stream_type == StreamType.LIVE
        assert state.current_media.duration == 3600.0

    async def test_load_propagates_drm_info(self, _tmp_path: Any) -> None:
        handler = _make_handler()
        p = ViaplayProvider(media_handler=handler)
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
            if state.load_task:
                await state.load_task

        handler.on_load.assert_awaited_once()
        info: MediaLoadInfo = handler.on_load.call_args.args[0]
        assert info.drm is not None
        assert info.drm.system == "widevine"
        assert info.drm.license_url == "https://drm.example.com/license"

    async def test_load_normalizes_none_stream_type(self, _tmp_path: Any) -> None:
        handler = _make_handler()
        p = ViaplayProvider(media_handler=handler)
        session, _, _ = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.auth_event.set()

        mock_stream = StreamInfo(
            url="https://cdn/v.mpd",
            content_type="application/dash+xml",
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
                    stream_type=StreamType.NONE,
                    duration=0,
                ),
                custom_data={"playUrl": "https://content.viaplay.se/play/1234"},
            )
            await p.on_media_message(session, load_req)
            if state.load_task:
                await state.load_task

        handler.on_load.assert_awaited_once()
        info: MediaLoadInfo = handler.on_load.call_args.args[0]
        assert info.stream_type == StreamType.BUFFERED
        assert info.duration is None
        assert state.current_media is not None
        assert state.current_media.stream_type == StreamType.BUFFERED
        assert state.current_media.duration is None

    async def test_load_handler_error_sends_load_failed_and_resets_state(
        self, _tmp_path: Any
    ) -> None:
        handler = _make_handler()
        handler.on_load.side_effect = RuntimeError("player failed")

        p = ViaplayProvider(media_handler=handler)
        session, broadcast, send = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.auth_event.set()

        mock_stream = StreamInfo(
            url="https://cdn/v.mpd",
            content_type="application/dash+xml",
        )
        with patch.object(
            state.api,
            "fetch_stream",
            new_callable=AsyncMock,
            return_value=mock_stream,
        ):
            load_req = LoadRequest(
                request_id=20,
                media=MediaInfo(
                    content_id="https://x",
                    content_type="video/mp4",
                    stream_type=StreamType.BUFFERED,
                ),
                custom_data={"playUrl": "https://content.viaplay.se/play/1234"},
            )
            await p.on_media_message(session, load_req)
            if state.load_task:
                await state.load_task

        _, data = send.await_args_list[0].args
        assert data["type"] == "LOAD_FAILED"
        assert data["reason"] == "PLAYER_LOAD_FAILED"

        assert state.player_state == PlayerState.IDLE
        assert state.current_media is None
        assert state.stream_url == ""

        media_status_messages = [
            payload
            for namespace, payload in (call.args for call in broadcast.await_args_list)
            if namespace == "urn:x-cast:com.google.cast.media"
            and payload.get("type") == "MEDIA_STATUS"
        ]
        assert media_status_messages
        assert media_status_messages[-1]["status"] == []

        receiver_state_messages = [
            payload
            for namespace, payload in (call.args for call in broadcast.await_args_list)
            if namespace == _NS_VIAPLAY and payload.get("type") == "RECEIVER_STATE"
        ]
        assert receiver_state_messages
        assert receiver_state_messages[-1]["receiverState"]["status"] == "IDLE"

    async def test_load_fails_without_auth(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
        session, _, send = _make_session()
        await p.on_launch(session, LaunchCredentials())

        # NOT authenticated — auth_event is never set, wait_for will time out.
        # Patch wait_for to raise TimeoutError immediately.
        async def _timeout_wait_for(
            awaitable: Any,
            *args: Any,
            **kwargs: Any,
        ) -> None:
            _ = args
            _ = kwargs
            close = getattr(awaitable, "close", None)
            if callable(close):
                _ = close()
            raise TimeoutError

        with patch("asyncio.wait_for", side_effect=_timeout_wait_for):
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
            if state.load_task:
                await state.load_task

        # Should have sent LOAD_FAILED
        _, data = send.call_args.args
        assert data["type"] == "LOAD_FAILED"

    async def test_load_fails_without_play_url(self, _tmp_path: Any) -> None:
        p = ViaplayProvider()
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
        if state.load_task:
            await state.load_task

        _, data = send.call_args.args
        assert data["type"] == "LOAD_FAILED"
        assert data["reason"] == "NO_PLAY_URL"

    async def test_load_buffering_broadcasts_casting_receiver_state(
        self, _tmp_path: Any
    ) -> None:
        p = ViaplayProvider()
        session, broadcast, _send = _make_session()
        await p.on_launch(session, LaunchCredentials())

        state = p._sessions["sess-1"]  # noqa: SLF001
        state.authenticated = True
        state.auth_event.set()

        mock_stream = StreamInfo(
            url="https://cdn/v.mpd",
            content_type="application/dash+xml",
        )
        with patch.object(
            state.api,
            "fetch_stream",
            new_callable=AsyncMock,
            return_value=mock_stream,
        ):
            load_req = LoadRequest(
                request_id=3,
                media=MediaInfo(
                    content_id="https://x",
                    content_type="video/mp4",
                    stream_type=StreamType.BUFFERED,
                ),
                custom_data={"playUrl": "https://content.viaplay.se/play/1234"},
            )
            await p.on_media_message(session, load_req)
            if state.load_task:
                await state.load_task

        receiver_state = [
            payload
            for namespace, payload in (call.args for call in broadcast.await_args_list)
            if namespace == _NS_VIAPLAY and payload.get("type") == "RECEIVER_STATE"
        ]
        assert receiver_state
        assert receiver_state[-1]["receiverState"]["status"] == "CASTING"
