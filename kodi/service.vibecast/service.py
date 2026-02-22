from __future__ import annotations

import json
import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlencode, urlparse, urlunparse

import websocket
import xbmc
import xbmcaddon
import xbmcgui

ADDON_ID = "service.vibecast"
ADDON_NAME = "vibecast"

INTERNAL_EVENT_TYPE = "__internal__"

CONNECTION_STATE_STARTING = "starting"
CONNECTION_STATE_WAITING = "waiting"
CONNECTION_STATE_CONNECTED = "connected"
CONNECTION_STATE_DISCONNECTED = "disconnected"

PLAYER_STATE_IDLE = "IDLE"
PLAYER_STATE_PLAYING = "PLAYING"
PLAYER_STATE_PAUSED = "PAUSED"
PLAYER_STATE_BUFFERING = "BUFFERING"

IDLE_REASON_CANCELLED = "CANCELLED"
IDLE_REASON_INTERRUPTED = "INTERRUPTED"
IDLE_REASON_FINISHED = "FINISHED"
IDLE_REASON_ERROR = "ERROR"

LOOPBACK_HOSTS = frozenset({"127.0.0.1", "localhost", "::1"})

COMMAND_TICK_SECONDS = 0.1
STATE_REPORT_INTERVAL_SECONDS = 1.0
SEEK_COMMAND_DEBOUNCE_SECONDS = 0.25
SEEK_SETTLE_REPORT_SECONDS = 0.4


def log(message: str, level: int = xbmc.LOGINFO) -> None:
    xbmc.log(f"[{ADDON_ID}] {message}", level)


def _setting_string(addon: xbmcaddon.Addon, key: str, default: str) -> str:
    getter = getattr(addon, "getSettingString", None)
    value_raw = getter(key) if callable(getter) else addon.getSetting(key)
    value = str(value_raw).strip()
    return value if value else default


def _setting_bool(addon: xbmcaddon.Addon, key: str, default: bool) -> bool:
    getter = getattr(addon, "getSettingBool", None)
    if callable(getter):
        return bool(getter(key))

    value = addon.getSetting(key).strip().lower()
    if value in {"true", "1", "yes", "on"}:
        return True
    if value in {"false", "0", "no", "off"}:
        return False
    return default


def _parse_float(value: str, default: float, *, minimum: float) -> float:
    try:
        parsed = float(value)
    except ValueError:
        return default
    if parsed < minimum:
        return minimum
    return parsed


def _parse_port(value: str, default: int) -> int:
    try:
        parsed = int(value)
    except ValueError:
        return default
    if parsed < 1:
        return 1
    if parsed > 65535:
        return 65535
    return parsed


def _coerce_float(value: Any, *, default: float | None = None) -> float | None:
    if isinstance(value, bool):
        return default
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        stripped = value.strip()
        if not stripped:
            return default
        try:
            return float(stripped)
        except ValueError:
            return default
    return default


def _coerce_str(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    stripped = value.strip()
    if not stripped:
        return None
    return stripped


@dataclass(slots=True, frozen=True)
class ServiceConfig:
    host: str
    port: int
    use_tls: bool
    reconnect_seconds: float
    ping_interval: float
    ping_timeout: float
    rewrite_loopback_urls: bool
    show_notifications: bool
    debug_logging: bool

    @classmethod
    def from_addon(cls, addon: xbmcaddon.Addon) -> ServiceConfig:
        host = _setting_string(addon, "server_host", "127.0.0.1")
        port = _parse_port(_setting_string(addon, "server_port", "8010"), 8010)
        use_tls = _setting_bool(addon, "use_tls", False)
        reconnect_seconds = _parse_float(
            _setting_string(addon, "reconnect_seconds", "2.0"),
            2.0,
            minimum=0.2,
        )
        ping_timeout = _parse_float(
            _setting_string(addon, "ping_timeout", "10.0"),
            10.0,
            minimum=1.0,
        )
        ping_interval = _parse_float(
            _setting_string(addon, "ping_interval", "20.0"),
            20.0,
            minimum=1.0,
        )
        if ping_interval <= ping_timeout:
            ping_interval = ping_timeout + 1.0

        return cls(
            host=host,
            port=port,
            use_tls=use_tls,
            reconnect_seconds=reconnect_seconds,
            ping_interval=ping_interval,
            ping_timeout=ping_timeout,
            rewrite_loopback_urls=_setting_bool(
                addon,
                "rewrite_loopback_urls",
                True,
            ),
            show_notifications=_setting_bool(addon, "show_notifications", True),
            debug_logging=_setting_bool(addon, "debug_logging", False),
        )

    @property
    def ws_url(self) -> str:
        scheme = "wss" if self.use_tls else "ws"
        return f"{scheme}://{self.host}:{self.port}/player?role=primary"


class VibecastPlayer(xbmc.Player):
    def __init__(self, service: VibecastService) -> None:
        super().__init__()
        self._service = service

    def onAVStarted(self) -> None:  # noqa: N802
        self._service.on_av_started()

    def onPlayBackStarted(self) -> None:  # noqa: N802
        self._service.on_playback_started()

    def onPlayBackPaused(self) -> None:  # noqa: N802
        self._service.on_playback_paused()

    def onPlayBackResumed(self) -> None:  # noqa: N802
        self._service.on_playback_resumed()

    def onPlayBackSeek(self, _time: int, _seek_offset: int) -> None:  # noqa: N802
        self._service.on_playback_seek()

    def onPlayBackSeekChapter(self, _chapter: int) -> None:  # noqa: N802
        self._service.on_playback_seek()

    def onPlayBackStopped(self) -> None:  # noqa: N802
        self._service.on_playback_stopped()

    def onPlayBackEnded(self) -> None:  # noqa: N802
        self._service.on_playback_ended()

    def onPlayBackError(self) -> None:  # noqa: N802
        self._service.on_playback_error()


class VibecastService:
    def __init__(self) -> None:
        self._addon = xbmcaddon.Addon(id=ADDON_ID)
        self._config = ServiceConfig.from_addon(self._addon)
        self._monitor = xbmc.Monitor()
        self._player = VibecastPlayer(self)

        self._state_lock = threading.RLock()
        self._ws_lock = threading.Lock()
        self._queue_lock = threading.Lock()
        self._stop_event = threading.Event()

        self._ws: websocket.WebSocketApp | None = None
        self._ws_connected = False
        self._ws_thread: threading.Thread | None = None
        self._last_ws_error: str | None = None
        self._connection_state = CONNECTION_STATE_STARTING

        self._active_session_id: str | None = None
        self._active_media: dict[str, Any] | None = None
        self._stream_queue: deque[dict[str, Any]] = deque()
        self._command_queue: deque[dict[str, Any]] = deque()

        self._state_hint = PLAYER_STATE_IDLE
        self._is_paused = False
        self._pending_seek = 0.0
        self._autoplay_enabled = True

        self._expected_stop_session_id: str | None = None
        self._expected_stop_reason: str | None = None

        self._last_state_key: tuple[Any, ...] | None = None
        self._last_known_time = 0.0
        self._last_known_duration: float | None = None
        self._next_state_report_at = 0.0

        self._deferred_seek_session_id: str | None = None
        self._deferred_seek_position: float | None = None
        self._deferred_seek_apply_at: float | None = None
        self._deferred_seek_report_at: float | None = None

    def run(self) -> None:
        log(
            f"starting ({ADDON_NAME}) ws={self._config.ws_url} "
            f"reconnect={self._config.reconnect_seconds:.1f}s"
        )
        self._start_ws_thread()

        while not self._monitor.waitForAbort(COMMAND_TICK_SECONDS):
            self._tick()

        self.stop()

    def stop(self) -> None:
        self._stop_event.set()
        self._close_websocket()
        ws_thread = self._ws_thread
        if ws_thread is not None and ws_thread.is_alive():
            ws_thread.join(timeout=5.0)
        log("stopped")

    def _start_ws_thread(self) -> None:
        self._ws_thread = threading.Thread(
            target=self._ws_loop,
            name="vibecast-ws",
            daemon=True,
        )
        self._ws_thread.start()

    def _ws_loop(self) -> None:
        while not self._stop_event.is_set():
            ws = websocket.WebSocketApp(
                self._config.ws_url,
                on_open=self._on_ws_open,
                on_message=self._on_ws_message,
                on_error=self._on_ws_error,
                on_close=self._on_ws_close,
            )

            with self._ws_lock:
                self._ws = ws

            try:
                ws.run_forever(
                    ping_interval=self._config.ping_interval,
                    ping_timeout=self._config.ping_timeout,
                    ping_payload="vibecast",
                )
            except (OSError, websocket.WebSocketException) as exc:
                log(f"websocket loop failed: {exc}", xbmc.LOGWARNING)
            finally:
                with self._ws_lock:
                    if self._ws is ws:
                        self._ws = None
                        self._ws_connected = False

            if self._stop_event.wait(self._config.reconnect_seconds):
                break

    def _on_ws_open(self, _ws: websocket.WebSocketApp) -> None:
        with self._ws_lock:
            self._ws_connected = True
            self._last_ws_error = None
        self._enqueue_internal_event("ws_open")
        if self._config.debug_logging:
            log("websocket connected", xbmc.LOGDEBUG)

    def _on_ws_message(self, _ws: websocket.WebSocketApp, message: Any) -> None:
        if not isinstance(message, str):
            return

        try:
            payload = json.loads(message)
        except json.JSONDecodeError:
            log("ignoring malformed websocket payload", xbmc.LOGWARNING)
            return

        if not isinstance(payload, dict):
            return

        with self._queue_lock:
            self._command_queue.append(payload)

    def _on_ws_error(self, _ws: websocket.WebSocketApp, error: Any) -> None:
        error_text = str(error)
        with self._ws_lock:
            self._last_ws_error = error_text
        self._enqueue_internal_event("ws_error", error=error_text)
        log(f"websocket error: {error_text}", xbmc.LOGWARNING)

    def _on_ws_close(
        self,
        _ws: websocket.WebSocketApp,
        status_code: Any,
        close_message: Any,
    ) -> None:
        with self._ws_lock:
            self._ws_connected = False
        if self._stop_event.is_set():
            return
        self._enqueue_internal_event(
            "ws_close",
            statusCode=status_code,
            closeMessage=close_message,
        )
        if self._config.debug_logging:
            log(
                f"websocket closed status={status_code} message={close_message}",
                xbmc.LOGDEBUG,
            )

    def _close_websocket(self) -> None:
        with self._ws_lock:
            ws = self._ws
        if ws is None:
            return
        try:
            ws.close()
        except (OSError, RuntimeError):
            return

    def _enqueue_internal_event(self, event: str, **data: Any) -> None:
        payload: dict[str, Any] = {
            "type": INTERNAL_EVENT_TYPE,
            "event": event,
        }
        payload.update(data)
        with self._queue_lock:
            self._command_queue.append(payload)

    def _handle_command(self, payload: dict[str, Any]) -> None:
        command_type = _coerce_str(payload.get("type"))
        if command_type is None:
            return

        if command_type == INTERNAL_EVENT_TYPE:
            self._handle_internal_event(payload)
            return

        if self._config.debug_logging:
            log(f"command: {command_type}", xbmc.LOGDEBUG)

        if command_type == "load":
            self._handle_load(payload)
            return
        if command_type == "play":
            self._handle_play(payload)
            return
        if command_type == "pause":
            self._handle_pause(payload)
            return
        if command_type == "seek":
            self._handle_seek(payload)
            return
        if command_type == "stop":
            self._handle_stop(payload)
            return
        if command_type == "volume":
            self._handle_volume(payload)

    def _handle_internal_event(self, payload: dict[str, Any]) -> None:
        event_name = _coerce_str(payload.get("event"))
        if event_name is None:
            return

        if event_name == "ws_open":
            self._handle_ws_open_event()
            return
        if event_name == "ws_close":
            status_code = payload.get("statusCode")
            close_message = payload.get("closeMessage")
            self._handle_ws_close_event(status_code, close_message)
            return
        if event_name == "ws_error" and self._config.debug_logging:
            error_text = _coerce_str(payload.get("error"))
            if error_text is not None:
                log(f"websocket transient error: {error_text}", xbmc.LOGDEBUG)

    def _handle_ws_open_event(self) -> None:
        if self._connection_state == CONNECTION_STATE_CONNECTED:
            return

        self._connection_state = CONNECTION_STATE_CONNECTED
        endpoint = f"{self._config.host}:{self._config.port}"
        log(f"connected to vibecast player endpoint {endpoint}")
        self._notify(
            f"Connected to vibecast ({endpoint})",
            xbmcgui.NOTIFICATION_INFO,
        )

    def _handle_ws_close_event(self, status_code: Any, close_message: Any) -> None:
        endpoint = f"{self._config.host}:{self._config.port}"
        last_error: str | None
        with self._ws_lock:
            last_error = self._last_ws_error
            self._last_ws_error = None

        details = f" status={status_code} message={close_message}"
        if last_error:
            details = f"{details} error={last_error}"

        if self._connection_state == CONNECTION_STATE_CONNECTED:
            self._connection_state = CONNECTION_STATE_DISCONNECTED
            log(
                f"disconnected from vibecast player endpoint {endpoint}; retrying.{details}",
                xbmc.LOGWARNING,
            )
            self._notify(
                f"Disconnected from vibecast ({endpoint}); retrying.",
                xbmcgui.NOTIFICATION_WARNING,
            )
            return

        if self._connection_state == CONNECTION_STATE_STARTING:
            self._connection_state = CONNECTION_STATE_WAITING
            log(
                f"unable to connect to vibecast player endpoint {endpoint} yet; retrying.{details}",
                xbmc.LOGWARNING,
            )
            self._notify(
                f"Could not connect to vibecast yet ({endpoint}).",
                xbmcgui.NOTIFICATION_WARNING,
            )

    def _notify(self, message: str, icon: int) -> None:
        if not self._config.show_notifications:
            return
        xbmcgui.Dialog().notification(
            ADDON_NAME,
            message,
            icon,
            4000,
            sound=False,
        )

    def _handle_load(self, payload: dict[str, Any]) -> None:
        session_id = _coerce_str(payload.get("sessionId"))
        if session_id is None:
            return

        media = payload.get("media")
        if not isinstance(media, dict):
            self._report_error(
                session_id, "PLAYBACK_INVALID_LOAD", "Missing media object."
            )
            return

        raw_streams = media.get("streams")
        if not isinstance(raw_streams, list):
            self._report_error(
                session_id, "PLAYBACK_INVALID_LOAD", "Missing streams list."
            )
            return

        stream_queue = deque(
            stream for stream in raw_streams if isinstance(stream, dict)
        )
        if not stream_queue:
            self._report_error(
                session_id, "PLAYBACK_INVALID_LOAD", "No valid stream entries."
            )
            return

        with self._state_lock:
            previous_session_id = self._active_session_id
            if previous_session_id is not None and previous_session_id != session_id:
                self._interrupt_session(previous_session_id)

            self._active_session_id = session_id
            self._active_media = media
            self._stream_queue = stream_queue
            self._state_hint = PLAYER_STATE_BUFFERING
            self._is_paused = False
            self._pending_seek = max(
                _coerce_float(media.get("startTime"), default=0.0) or 0.0, 0.0
            )
            self._autoplay_enabled = media.get("autoplay") is not False
            self._last_state_key = None
            self._last_known_time = self._pending_seek
            self._last_known_duration = _coerce_float(
                media.get("duration"), default=None
            )
            self._clear_deferred_seek_locked()

        self._report_state(session_id, PLAYER_STATE_BUFFERING, force=True)
        if self._play_next_stream_candidate(session_id):
            return

        self._report_error(
            session_id,
            "PLAYBACK_LOAD_FAILED",
            "No stream candidate could be started.",
        )
        self._report_state(
            session_id,
            PLAYER_STATE_IDLE,
            idle_reason=IDLE_REASON_ERROR,
            force=True,
        )
        self._clear_active_session(session_id)

    def _handle_play(self, payload: dict[str, Any]) -> None:
        session_id = _coerce_str(payload.get("sessionId"))
        if not self._is_active_session(session_id):
            return

        with self._state_lock:
            self._state_hint = PLAYER_STATE_PLAYING
            should_toggle = self._is_paused and self._player.isPlaying()
            self._is_paused = False
            self._clear_deferred_seek_locked()

        if should_toggle:
            self._player.pause()
        self._report_current_state(force=True)

    def _handle_pause(self, payload: dict[str, Any]) -> None:
        session_id = _coerce_str(payload.get("sessionId"))
        if not self._is_active_session(session_id):
            return

        with self._state_lock:
            self._state_hint = PLAYER_STATE_PAUSED
            should_toggle = (not self._is_paused) and self._player.isPlaying()
            self._is_paused = True
            self._clear_deferred_seek_locked()

        if should_toggle:
            self._player.pause()
        self._report_current_state(force=True)

    def _handle_seek(self, payload: dict[str, Any]) -> None:
        session_id = _coerce_str(payload.get("sessionId"))
        if not self._is_active_session(session_id):
            return
        if session_id is None:
            return

        position = _coerce_float(payload.get("position"), default=0.0)
        if position is None:
            position = 0.0
        if position < 0:
            position = 0.0

        now = time.monotonic()
        with self._state_lock:
            self._pending_seek = position
            self._last_known_time = position
            self._state_hint = PLAYER_STATE_BUFFERING
            self._deferred_seek_session_id = session_id
            self._deferred_seek_position = position
            self._deferred_seek_apply_at = now + SEEK_COMMAND_DEBOUNCE_SECONDS
            self._deferred_seek_report_at = now + SEEK_SETTLE_REPORT_SECONDS

        # Report position immediately so the sender UI tracks the drag.
        # The actual Kodi seekTime() is deferred until the drag settles.
        self._report_state(session_id, PLAYER_STATE_BUFFERING, force=True)

    def _handle_stop(self, payload: dict[str, Any]) -> None:
        session_id = _coerce_str(payload.get("sessionId"))
        if not self._is_active_session(session_id):
            return
        if session_id is None:
            return

        with self._state_lock:
            self._expected_stop_session_id = session_id
            self._expected_stop_reason = IDLE_REASON_CANCELLED
            self._clear_deferred_seek_locked()

        if self._player.isPlaying():
            self._player.stop()
            return

        self._report_state(
            session_id,
            PLAYER_STATE_IDLE,
            idle_reason=IDLE_REASON_CANCELLED,
            force=True,
        )
        self._clear_active_session(session_id)

    def _handle_volume(self, payload: dict[str, Any]) -> None:
        session_id = _coerce_str(payload.get("sessionId"))
        if not self._is_active_session(session_id):
            return

        level = _coerce_float(payload.get("level"), default=None)
        if level is not None:
            if level < 0:
                level = 0.0
            if level > 1:
                level = 1.0
            volume_percent = int(round(level * 100))
            self._execute_json_rpc("Application.SetVolume", {"volume": volume_percent})

        muted = payload.get("muted")
        if isinstance(muted, bool):
            self._execute_json_rpc("Application.SetMute", {"mute": muted})

    def _play_next_stream_candidate(self, session_id: str) -> bool:
        while True:
            with self._state_lock:
                if not self._is_active_session(session_id):
                    return False
                if not self._stream_queue:
                    return False
                candidate = self._stream_queue.popleft()

            ok, error_message = self._start_stream(session_id, candidate)
            if ok:
                return True

            if self._config.debug_logging:
                log(f"stream candidate rejected: {error_message}", xbmc.LOGDEBUG)

    def _start_stream(
        self, session_id: str, stream: dict[str, Any]
    ) -> tuple[bool, str]:
        stream_url = _coerce_str(stream.get("url"))
        if stream_url is None:
            return False, "missing stream URL"

        stream_url = self._rewrite_if_loopback(stream_url)

        content_type = _coerce_str(stream.get("contentType"))
        protocol, mime_type = self._infer_protocol(stream_url, content_type)

        drm = stream.get("drm")
        if not self._ensure_inputstream(protocol, drm):
            return False, "inputstream setup failed"

        list_item = xbmcgui.ListItem(path=stream_url, offscreen=True)
        list_item.setMimeType(mime_type)
        list_item.setContentLookup(False)
        list_item.setProperty("inputstream", "inputstream.adaptive")

        with self._state_lock:
            media = self._active_media

        self._populate_metadata(list_item, media)

        if isinstance(drm, dict):
            success, reason = self._configure_drm(list_item, drm)
            if not success:
                return False, reason

        if self._config.debug_logging:
            log(
                f"starting playback url={stream_url} protocol={protocol} session={session_id}",
                xbmc.LOGDEBUG,
            )

        self._player.play(item=stream_url, listitem=list_item)
        return True, ""

    def _ensure_inputstream(self, protocol: str, drm: Any) -> bool:
        try:
            from inputstreamhelper import (
                Helper,  # pylint: disable=import-outside-toplevel
            )
        except ImportError:
            log("inputstreamhelper import failed", xbmc.LOGERROR)
            return False

        drm_argument: str | None = None
        if isinstance(drm, dict):
            drm_system = _coerce_str(drm.get("system"))
            if drm_system == "com.widevine.alpha":
                drm_argument = "com.widevine.alpha"

        helper = (
            Helper(protocol, drm=drm_argument) if drm_argument else Helper(protocol)
        )
        return bool(helper.check_inputstream())

    def _configure_drm(
        self, list_item: xbmcgui.ListItem, drm: dict[str, Any]
    ) -> tuple[bool, str]:
        drm_system = _coerce_str(drm.get("system"))
        if drm_system is None:
            return False, "missing DRM system"

        license_url = _coerce_str(drm.get("licenseUrl"))
        if license_url is None:
            return False, "missing DRM license URL"

        license_url = self._rewrite_if_loopback(license_url)

        headers_value = ""
        raw_headers = drm.get("headers")
        if isinstance(raw_headers, dict):
            clean_headers = {
                str(key): str(value)
                for key, value in raw_headers.items()
                if _coerce_str(key) is not None and _coerce_str(value) is not None
            }
            if clean_headers:
                headers_value = urlencode(clean_headers)

        # Kodi 21+ handles Widevine request formatting more reliably via
        # drm_legacy than the older license_key placeholder path.
        drm_legacy_parts = [drm_system, license_url]
        if headers_value:
            drm_legacy_parts.append(headers_value)
        list_item.setProperty(
            "inputstream.adaptive.drm_legacy",
            "|".join(drm_legacy_parts),
        )
        return True, ""

    def _populate_metadata(
        self,
        list_item: xbmcgui.ListItem,
        media: dict[str, Any] | None,
    ) -> None:
        if media is None:
            return

        title = _coerce_str(media.get("title"))
        subtitle = _coerce_str(media.get("subtitle"))

        info: dict[str, str] = {}
        if title is not None:
            info["title"] = title
        if subtitle is not None:
            info["plot"] = subtitle
        if info:
            list_item.setInfo("video", info)

        images = media.get("images")
        if not isinstance(images, list):
            return

        for image in images:
            if not isinstance(image, dict):
                continue
            image_url = _coerce_str(image.get("url"))
            if image_url is None:
                continue
            list_item.setArt(
                {
                    "thumb": image_url,
                    "icon": image_url,
                    "fanart": image_url,
                }
            )
            return

    def _infer_protocol(
        self, stream_url: str, content_type: str | None
    ) -> tuple[str, str]:
        lowered_type = (content_type or "").lower()
        lowered_url = stream_url.lower()

        if "mpegurl" in lowered_type or lowered_url.endswith(".m3u8"):
            return "hls", "application/vnd.apple.mpegurl"
        if "ms-sstr" in lowered_type or ".ism" in lowered_url:
            return "ism", "application/vnd.ms-sstr+xml"
        if "dash" in lowered_type or lowered_url.endswith(".mpd"):
            return "mpd", "application/dash+xml"
        return "mpd", "application/dash+xml"

    def _is_active_session(self, session_id: str | None) -> bool:
        if session_id is None:
            return False
        with self._state_lock:
            return session_id == self._active_session_id

    def _interrupt_session(self, previous_session_id: str) -> None:
        self._expected_stop_session_id = previous_session_id
        self._expected_stop_reason = IDLE_REASON_INTERRUPTED

        if self._player.isPlaying():
            self._player.stop()
        else:
            self._report_state(
                previous_session_id,
                PLAYER_STATE_IDLE,
                idle_reason=IDLE_REASON_INTERRUPTED,
                force=True,
            )

    def _clear_active_session(self, session_id: str | None) -> None:
        with self._state_lock:
            if session_id is not None and self._active_session_id != session_id:
                return

            self._active_session_id = None
            self._active_media = None
            self._stream_queue.clear()
            self._state_hint = PLAYER_STATE_IDLE
            self._is_paused = False
            self._pending_seek = 0.0
            self._autoplay_enabled = True
            self._last_state_key = None
            self._clear_deferred_seek_locked()

    def _report_error(self, session_id: str, code: str, message: str) -> None:
        payload = {
            "type": "error",
            "sessionId": session_id,
            "code": code,
            "message": message,
        }
        self._send_payload(payload)

    def _report_current_state(self, *, force: bool) -> None:
        with self._state_lock:
            session_id = self._active_session_id
            state = self._state_hint
            is_paused = self._is_paused

        if session_id is None:
            return

        if self._player.isPlaying():
            state = PLAYER_STATE_PAUSED if is_paused else PLAYER_STATE_PLAYING
        self._report_state(session_id, state, force=force)

    def _report_state(
        self,
        session_id: str,
        player_state: str,
        *,
        idle_reason: str | None = None,
        force: bool,
    ) -> None:
        current_time, duration = self._player_metrics(player_state)
        rounded_duration: float | str = (
            "none" if duration is None else round(duration, 1)
        )

        state_key = (
            session_id,
            player_state,
            idle_reason,
            round(current_time, 1),
            rounded_duration,
        )

        with self._state_lock:
            if not force and state_key == self._last_state_key:
                return
            self._last_state_key = state_key

        payload: dict[str, Any] = {
            "type": "state",
            "sessionId": session_id,
            "playerState": player_state,
            "currentTime": current_time,
        }
        if duration is not None:
            payload["duration"] = duration
        if idle_reason is not None:
            payload["idleReason"] = idle_reason

        self._send_payload(payload)

    def _player_metrics(self, player_state: str) -> tuple[float, float | None]:
        if player_state == PLAYER_STATE_IDLE:
            return self._last_known_time, self._last_known_duration

        if not self._player.isPlaying():
            return self._last_known_time, self._last_known_duration

        current_time = self._safe_player_time()
        if current_time is not None:
            self._last_known_time = current_time

        duration = self._safe_player_duration()
        if duration is not None and duration > 0:
            self._last_known_duration = duration

        return self._last_known_time, self._last_known_duration

    def _safe_player_time(self) -> float | None:
        try:
            return max(self._player.getTime(), 0.0)
        except RuntimeError:
            return None

    def _safe_player_duration(self) -> float | None:
        try:
            duration = self._player.getTotalTime()
        except RuntimeError:
            return None
        if duration <= 0:
            return None
        return duration

    def _send_payload(self, payload: dict[str, Any]) -> None:
        message = json.dumps(payload, separators=(",", ":"))
        with self._ws_lock:
            ws = self._ws
            connected = self._ws_connected
        if ws is None or not connected:
            return

        try:
            ws.send(message)
        except (OSError, websocket.WebSocketException):
            return

    def _execute_json_rpc(self, method: str, params: dict[str, Any]) -> None:
        payload = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }
        xbmc.executeJSONRPC(json.dumps(payload))

    def _rewrite_if_loopback(self, url: str) -> str:
        if not self._config.rewrite_loopback_urls:
            return url

        parsed = urlparse(url)
        host = parsed.hostname
        if host is None or host not in LOOPBACK_HOSTS:
            return url

        replacement_host = self._config.host
        if not replacement_host:
            return url

        netloc_host = replacement_host
        if ":" in replacement_host and not replacement_host.startswith("["):
            netloc_host = f"[{replacement_host}]"
        if parsed.port is not None:
            netloc_host = f"{netloc_host}:{parsed.port}"

        rewritten = parsed._replace(netloc=netloc_host)
        return urlunparse(rewritten)

    def _coalesce_seek_commands(
        self, commands: list[dict[str, Any]]
    ) -> list[dict[str, Any]]:
        reduced: list[dict[str, Any]] = []
        for command in commands:
            command_type = _coerce_str(command.get("type"))
            if command_type != "seek":
                reduced.append(command)
                continue

            session_id = _coerce_str(command.get("sessionId"))
            if session_id is None or not reduced:
                reduced.append(command)
                continue

            previous = reduced[-1]
            previous_type = _coerce_str(previous.get("type"))
            previous_session = _coerce_str(previous.get("sessionId"))
            if previous_type == "seek" and previous_session == session_id:
                reduced[-1] = command
                continue

            reduced.append(command)
        return reduced

    def _process_deferred_seek(self) -> None:
        now = time.monotonic()
        with self._state_lock:
            session_id = self._deferred_seek_session_id
            position = self._deferred_seek_position
            apply_at = self._deferred_seek_apply_at

        if session_id is None or position is None or apply_at is None:
            return
        if now < apply_at:
            return
        if not self._is_active_session(session_id):
            with self._state_lock:
                self._clear_deferred_seek_locked()
            return

        if self._player.isPlaying():
            self._player.seekTime(position)

        with self._state_lock:
            self._deferred_seek_position = None
            self._deferred_seek_apply_at = None

    def _flush_settled_seek_report(self) -> None:
        now = time.monotonic()
        with self._state_lock:
            report_at = self._deferred_seek_report_at
            session_id = self._active_session_id
            is_paused = self._is_paused
            state_hint = self._state_hint

        if report_at is None or now < report_at:
            return

        if session_id is None:
            with self._state_lock:
                self._deferred_seek_report_at = None
            return

        report_state = state_hint
        if self._player.isPlaying():
            report_state = PLAYER_STATE_PAUSED if is_paused else PLAYER_STATE_PLAYING

        with self._state_lock:
            self._deferred_seek_report_at = None
        self._report_state(session_id, report_state, force=True)

    def _clear_deferred_seek_locked(self) -> None:
        self._deferred_seek_session_id = None
        self._deferred_seek_position = None
        self._deferred_seek_apply_at = None
        self._deferred_seek_report_at = None

    def _tick(self) -> None:
        queued_commands: list[dict[str, Any]] = []
        with self._queue_lock:
            while self._command_queue:
                queued_commands.append(self._command_queue.popleft())

        for command in self._coalesce_seek_commands(queued_commands):
            self._handle_command(command)

        self._process_deferred_seek()
        self._flush_settled_seek_report()

        with self._ws_lock:
            connected = self._ws_connected
        if not connected:
            return

        with self._state_lock:
            seeking = (
                self._deferred_seek_report_at is not None
                or self._deferred_seek_apply_at is not None
            )
        if seeking:
            return

        now = time.monotonic()
        if now < self._next_state_report_at:
            return
        self._next_state_report_at = now + STATE_REPORT_INTERVAL_SECONDS
        self._report_current_state(force=False)

    def on_av_started(self) -> None:
        with self._state_lock:
            session_id = self._active_session_id
            pending_seek = self._pending_seek
            autoplay_enabled = self._autoplay_enabled
            self._clear_deferred_seek_locked()
            self._pending_seek = 0.0
            self._state_hint = PLAYER_STATE_PLAYING
            self._is_paused = False

        if session_id is None:
            return

        if pending_seek > 0 and self._player.isPlaying():
            self._player.seekTime(pending_seek)

        if not autoplay_enabled and self._player.isPlaying():
            self._player.pause()
            with self._state_lock:
                self._state_hint = PLAYER_STATE_PAUSED
                self._is_paused = True
            self._report_state(session_id, PLAYER_STATE_PAUSED, force=True)
            return

        self._report_state(session_id, PLAYER_STATE_PLAYING, force=True)

    def on_playback_started(self) -> None:
        self.on_av_started()

    def on_playback_paused(self) -> None:
        with self._state_lock:
            session_id = self._active_session_id
            self._state_hint = PLAYER_STATE_PAUSED
            self._is_paused = True
            self._clear_deferred_seek_locked()
        if session_id is not None:
            self._report_state(session_id, PLAYER_STATE_PAUSED, force=True)

    def on_playback_resumed(self) -> None:
        with self._state_lock:
            session_id = self._active_session_id
            self._state_hint = PLAYER_STATE_PLAYING
            self._is_paused = False
            self._clear_deferred_seek_locked()
        if session_id is not None:
            self._report_state(session_id, PLAYER_STATE_PLAYING, force=True)

    def on_playback_seek(self) -> None:
        with self._state_lock:
            session_id = self._active_session_id
            self._state_hint = PLAYER_STATE_BUFFERING
            if session_id is not None:
                self._deferred_seek_report_at = (
                    time.monotonic() + SEEK_SETTLE_REPORT_SECONDS
                )

    def on_playback_stopped(self) -> None:
        with self._state_lock:
            session_id = self._expected_stop_session_id or self._active_session_id
            idle_reason = self._expected_stop_reason or IDLE_REASON_CANCELLED
            self._expected_stop_session_id = None
            self._expected_stop_reason = None

        if session_id is None:
            return

        self._report_state(
            session_id,
            PLAYER_STATE_IDLE,
            idle_reason=idle_reason,
            force=True,
        )
        self._clear_active_session(session_id)

    def on_playback_ended(self) -> None:
        with self._state_lock:
            session_id = self._active_session_id
        if session_id is None:
            return

        self._report_state(
            session_id,
            PLAYER_STATE_IDLE,
            idle_reason=IDLE_REASON_FINISHED,
            force=True,
        )
        self._clear_active_session(session_id)

    def on_playback_error(self) -> None:
        with self._state_lock:
            session_id = self._active_session_id
        if session_id is None:
            return

        if self._play_next_stream_candidate(session_id):
            self._report_state(session_id, PLAYER_STATE_BUFFERING, force=True)
            return

        self._report_error(
            session_id, "PLAYBACK_ERROR", "Kodi player reported an error."
        )
        self._report_state(
            session_id,
            PLAYER_STATE_IDLE,
            idle_reason=IDLE_REASON_ERROR,
            force=True,
        )
        self._clear_active_session(session_id)


if __name__ == "__main__":
    VibecastService().run()
