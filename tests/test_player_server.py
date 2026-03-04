"""Tests for the aiohttp-based player server."""

from __future__ import annotations

import asyncio
import json
from typing import Any, cast

from aiohttp import ClientSession, WSMsgType

from vibecast._manifest_proxy import ManifestProxyRequest, ManifestProxyResponse
from vibecast._models import PlayerState, StreamType
from vibecast._player_server import PlayerServer
from vibecast.player import (
    LicenseRequest,
    LicenseResponse,
    PlaybackError,
    PlaybackMedia,
    PlaybackState,
    PlaybackStream,
    PlayerContext,
)


def _make_context(
    session_id: str = "session-1",
) -> tuple[PlayerContext, list[PlaybackState]]:
    states: list[PlaybackState] = []

    async def _state_sink(state: PlaybackState) -> None:
        states.append(state)

    async def _error_sink(error: PlaybackError) -> None:
        _ = error

    return (
        PlayerContext(
            session_id,
            report_state=_state_sink,
            report_error=_error_sink,
        ),
        states,
    )


def _media(session_id: str = "session-1") -> PlaybackMedia:
    return PlaybackMedia(
        session_id=session_id,
        streams=(
            PlaybackStream(
                url="https://example.com/manifest.mpd",
                content_type="application/dash+xml",
            ),
        ),
        stream_type=StreamType.BUFFERED,
        start_time=0.0,
    )


async def _read_json_message(ws: Any) -> dict[str, object]:
    msg = await ws.receive(timeout=1)
    assert msg.type == WSMsgType.TEXT
    return json.loads(msg.data)


class TestPlayerServer:
    async def test_serves_default_shaka_player_page(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()
        port = server.serving_port
        assert port is not None

        async with ClientSession() as client:
            response = await client.get(f"http://127.0.0.1:{port}/")
            body = await response.text()

        assert response.status == 200
        assert response.content_type == "text/html"
        assert "shaka-player.compiled.js" in body
        assert 'src="/player.js"' in body
        assert '<video id="video" class="video" playsinline></video>' in body
        assert '<video id="video" class="video" controls' not in body
        assert "/player?role=primary" in body

        await server.stop()

    async def test_serves_player_script(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()
        port = server.serving_port
        assert port is not None

        async with ClientSession() as client:
            response = await client.get(f"http://127.0.0.1:{port}/player.js")
            body = await response.text()

        assert response.status == 200
        assert response.content_type == "application/javascript"
        assert "new shaka.Player" in body
        assert "/player?role=primary" in body
        assert 'window.getComputedStyle(el).display !== "none"' in body
        assert 'el.offsetParent !== null || el.style.display !== "none"' not in body

        await server.stop()

    async def test_start_and_stop(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()
        assert server.serving_port is not None
        await server.stop()
        assert server.serving_port is None

    async def test_command_fanout_and_primary_state_reporting(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()
        port = server.serving_port
        assert port is not None

        ctx, states = _make_context()

        async with ClientSession() as client:
            ws_primary = await client.ws_connect(
                f"http://127.0.0.1:{port}/player?role=primary"
            )
            ws_observer = await client.ws_connect(
                f"http://127.0.0.1:{port}/player?role=observer"
            )

            await server.on_load(ctx, _media())

            primary_message = await _read_json_message(ws_primary)
            observer_message = await _read_json_message(ws_observer)
            assert primary_message["type"] == "load"
            assert observer_message["type"] == "load"
            media_payload = cast("dict[str, object]", primary_message["media"])
            streams_payload = cast("list[dict[str, object]]", media_payload["streams"])
            assert streams_payload[0]["url"] == "https://example.com/manifest.mpd"

            await ws_observer.send_json(
                {
                    "type": "state",
                    "sessionId": "session-1",
                    "playerState": "PLAYING",
                    "currentTime": 11,
                }
            )
            await asyncio.sleep(0.05)
            assert states == []

            await ws_primary.send_json(
                {
                    "type": "state",
                    "sessionId": "session-1",
                    "playerState": "PLAYING",
                    "currentTime": 21.5,
                }
            )

            for _ in range(50):
                if states:
                    break
                await asyncio.sleep(0.01)

            assert len(states) == 1
            assert states[0].player_state is PlayerState.PLAYING
            assert states[0].current_time == 21.5

            _ = await ws_primary.close()
            _ = await ws_observer.close()

        await server.stop()

    async def test_auto_sync_on_connect(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()
        port = server.serving_port
        assert port is not None

        ctx, _states = _make_context()
        await server.on_load(ctx, _media())
        await server.on_seek(ctx, 42.0)
        await server.on_pause(ctx)

        async with ClientSession() as client:
            ws = await client.ws_connect(f"http://127.0.0.1:{port}/player")

            first = await _read_json_message(ws)
            second = await _read_json_message(ws)
            third = await _read_json_message(ws)

            assert first["type"] == "load"
            assert second["type"] == "seek"
            assert second["position"] == 42.0
            assert third["type"] == "pause"

            _ = await ws.close()

        await server.stop()

    async def test_license_proxy_round_trip(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()

        class _Handler:
            def __init__(self) -> None:
                self.requests: list[LicenseRequest] = []

            async def handle_license(self, request: LicenseRequest) -> LicenseResponse:
                self.requests.append(request)
                return LicenseResponse(body=request.body + b"-ok")

        handler = _Handler()
        proxy_url = server.register_license_handler("session-1", handler)

        async with ClientSession() as client:
            response = await client.post(
                f"{proxy_url}?route=r7",
                data=b"challenge",
                headers={"Content-Type": "application/octet-stream"},
            )
            body = await response.read()

            assert response.status == 200
            assert response.content_type == "application/octet-stream"
            assert body == b"challenge-ok"
            assert len(handler.requests) == 1
            assert handler.requests[0].session_id == "session-1"
            assert handler.requests[0].route_id == "r7"
            assert handler.requests[0].body == b"challenge"

            missing = await client.post(
                proxy_url.replace("session-1", "missing"), data=b"x"
            )
            assert missing.status == 404

        await server.stop()

    async def test_license_proxy_preserves_explicit_error_response(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()

        class _Handler:
            async def handle_license(self, request: LicenseRequest) -> LicenseResponse:
                _ = request
                return LicenseResponse(
                    status=403,
                    body=b"forbidden",
                    content_type="text/plain",
                )

        proxy_url = server.register_license_handler("session-1", _Handler())

        async with ClientSession() as client:
            response = await client.post(proxy_url, data=b"challenge")
            body = await response.read()

        assert response.status == 403
        assert response.content_type == "text/plain"
        assert body == b"forbidden"

        await server.stop()

    async def test_license_proxy_unhandled_error_returns_500(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()

        class _Handler:
            async def handle_license(self, request: LicenseRequest) -> LicenseResponse:
                _ = request
                msg = "boom"
                raise RuntimeError(msg)

        proxy_url = server.register_license_handler("session-1", _Handler())

        async with ClientSession() as client:
            response = await client.post(proxy_url, data=b"challenge")

        assert response.status == 500

        await server.stop()

    async def test_manifest_proxy_round_trip(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()

        class _Handler:
            def __init__(self) -> None:
                self.requests: list[ManifestProxyRequest] = []

            async def handle_manifest(
                self,
                request: ManifestProxyRequest,
            ) -> ManifestProxyResponse:
                self.requests.append(request)
                return ManifestProxyResponse(
                    body=b"#EXTM3U\n",
                    content_type="application/vnd.apple.mpegurl",
                    status=200,
                    headers={"Cache-Control": "no-store"},
                )

        handler = _Handler()
        proxy_url = server.register_manifest_handler("session-1", handler)

        async with ClientSession() as client:
            response = await client.get(f"{proxy_url}/m7.m3u8")
            body = await response.read()

            assert response.status == 200
            assert response.content_type == "application/vnd.apple.mpegurl"
            assert response.headers.get("Cache-Control") == "no-store"
            assert body == b"#EXTM3U\n"
            assert len(handler.requests) == 1
            assert handler.requests[0].session_id == "session-1"
            assert handler.requests[0].route_id == "m7"
            assert handler.requests[0].method == "GET"

            missing = await client.get(
                proxy_url.replace("session-1", "missing") + "/m0.mpd"
            )
            assert missing.status == 404

        await server.stop()

    async def test_manifest_proxy_head_request(self) -> None:
        server = PlayerServer(host="127.0.0.1", port=0)
        await server.start()

        class _Handler:
            async def handle_manifest(
                self,
                request: ManifestProxyRequest,
            ) -> ManifestProxyResponse:
                assert request.method == "HEAD"
                return ManifestProxyResponse(
                    body=b"ignored",
                    content_type="application/dash+xml",
                    status=200,
                )

        proxy_url = server.register_manifest_handler("session-1", _Handler())

        async with ClientSession() as client:
            response = await client.head(f"{proxy_url}/m0.mpd")
            body = await response.read()

        assert response.status == 200
        assert response.content_type == "application/dash+xml"
        assert body == b""

        await server.stop()
