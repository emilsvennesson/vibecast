"""Tests for the SVT Play provider."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock
from urllib.parse import parse_qs, urlsplit

import httpx

from vibecast._models import LoadRequest, MediaInfo, MediaMetadata, StreamType
from vibecast.player import DrmSystem
from vibecast.provider import LaunchCredentials, ProviderSession, ReceiverContext
from vibecast.providers.svt_play._provider import SvtPlayProvider


def _make_session(
    client: httpx.AsyncClient,
    *,
    session_id: str = "sess-1",
    transport_id: str = "pid-1",
    app_id: str = "95370A1C",
) -> ProviderSession:
    return ProviderSession(
        session_id=session_id,
        transport_id=transport_id,
        app_id=app_id,
        http_client=client,
        receiver=ReceiverContext(
            friendly_name="Living Room",
            device_model="Chromecast",
            device_id="receiver-device-id",
            data_dir=Path("/tmp/vibecast-tests/providers/svt_play"),
        ),
        send_custom=AsyncMock(),
        broadcast_custom=AsyncMock(),
    )


class TestProperties:
    def test_provider_metadata(self) -> None:
        provider = SvtPlayProvider()
        assert provider.app_ids() == frozenset({"95370A1C"})
        assert provider.display_name() == "SVT Play"
        assert provider.namespaces() == frozenset()


class TestLifecycle:
    async def test_on_launch_and_stop_manage_session(self) -> None:
        provider = SvtPlayProvider()

        async def _handler(request: httpx.Request) -> httpx.Response:
            return httpx.Response(404, json={"error": "unused"}, request=request)

        async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
            session = _make_session(client)
            await provider.on_launch(session, LaunchCredentials())
            assert session.session_id in provider._sessions  # noqa: SLF001

            await provider.on_stop(session)
            assert session.session_id not in provider._sessions  # noqa: SLF001


class TestResolveMedia:
    async def test_resolves_manifest_from_media_custom_data(self) -> None:
        provider = SvtPlayProvider()

        default_resolve = "https://switcher.cdn.svt.se/resolve/default-id/dash-full.mpd"
        sign_resolve = "https://switcher.cdn.svt.se/resolve/sign-id/dash-full.mpd"
        audio_resolve = "https://switcher.cdn.svt.se/resolve/audio-id/dash-full.mpd"

        default_manifest = "https://ed7.cdn.svt.se/d0/se/default-id/dash-full.mpd"
        sign_manifest = "https://ed17.cdn.svt.se/d0/se/sign-id/dash-full.mpd"
        audio_manifest = "https://ed5.cdn.svt.se/d0/se/audio-id/dash-full.mpd"

        video_payload = {
            "svtId": "egWnL16",
            "programTitle": "Hundarna",
            "episodeTitle": '1. "Nu kor vi!"',
            "contentDuration": 2489,
            "videoReferences": [
                {
                    "format": "dash-full",
                    "url": "https://svt-vod.example/default-id/dash-full.mpd",
                    "resolve": default_resolve,
                }
            ],
            "variants": {
                "default": {
                    "videoReferences": [
                        {
                            "format": "dash-full",
                            "url": "https://svt-vod.example/default-id/dash-full.mpd",
                            "resolve": default_resolve,
                        }
                    ]
                },
                "audioDescribed": {
                    "videoReferences": [
                        {
                            "format": "dash-full",
                            "url": "https://svt-vod.example/audio-id/dash-full.mpd",
                            "resolve": audio_resolve,
                        }
                    ]
                },
                "signInterpreted": {
                    "videoReferences": [
                        {
                            "format": "dash-full",
                            "url": "https://svt-vod.example/sign-id/dash-full.mpd",
                            "resolve": sign_resolve,
                        }
                    ]
                },
            },
        }

        async def _handler(request: httpx.Request) -> httpx.Response:
            url = str(request.url)
            if url == "https://video.svt.se/video/egWnL16":
                return httpx.Response(200, json=video_payload, request=request)
            if url == default_resolve:
                return httpx.Response(
                    200, json={"location": default_manifest}, request=request
                )
            if url == sign_resolve:
                return httpx.Response(
                    200, json={"location": sign_manifest}, request=request
                )
            if url == audio_resolve:
                return httpx.Response(
                    200, json={"location": audio_manifest}, request=request
                )
            return httpx.Response(404, json={"error": "unexpected"}, request=request)

        async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
            session = _make_session(client)
            await provider.on_launch(session, LaunchCredentials())

            media_custom_data = {
                "client": "svt-play",
                "videoTrackPreference": {"kind": "Original"},
            }

            media = await provider.resolve_media(
                session,
                LoadRequest(
                    request_id=8,
                    media=MediaInfo(
                        content_id="egWnL16",
                        content_type="video/mp4",
                        stream_type=StreamType.BUFFERED,
                        metadata=MediaMetadata(metadata_type=0, subtitle="Laddar..."),
                        custom_data=media_custom_data,
                    ),
                    autoplay=True,
                    current_time=12.5,
                    custom_data={"topLevelShouldBeIgnored": True},
                ),
            )

        assert len(media.streams) >= 2

        parsed = parse_qs(urlsplit(media.streams[0].url).query)
        assert parsed["manifestUrl"] == [default_manifest]
        assert parsed["manifestUrlSignLanguage"] == [sign_manifest]
        assert parsed["manifestUrlAudioDescription"] == [audio_manifest]
        assert parsed["preferredVideoTrack"] == ["original"]
        assert parsed["platform"] == ["chromecast;cc-androidtv"]
        assert parsed["includeAudioCodecs"] == ["mp4a.40.2"]
        assert parsed["b"] == ["-6334"]

        assert media.streams[1].url == default_manifest

        assert media.title == "Hundarna"
        assert media.subtitle == '1. "Nu kor vi!"'
        assert media.duration == 2489
        assert media.start_time == 12.5
        assert media.custom_data["videoTrackPreference"] == {"kind": "Original"}
        assert "topLevelShouldBeIgnored" not in media.custom_data

    async def test_uses_fallback_metadata_without_alt_variants(self) -> None:
        provider = SvtPlayProvider()

        default_resolve = "https://switcher.cdn.svt.se/resolve/main-id/dash-full.mpd"
        default_manifest = "https://ed8.cdn.svt.se/d0/se/main-id/dash-full.mpd"

        video_payload = {
            "svtId": "eXv13pb",
            "contentDuration": 56,
            "videoReferences": [
                {
                    "format": "dash-full",
                    "url": "https://svt-vod.example/main-id/dash-full.mpd",
                    "resolve": default_resolve,
                }
            ],
            "variants": {
                "default": {
                    "videoReferences": [
                        {
                            "format": "dash-full",
                            "url": "https://svt-vod.example/main-id/dash-full.mpd",
                            "resolve": default_resolve,
                        }
                    ]
                },
                "audioDescribed": None,
                "signInterpreted": None,
            },
        }

        requested_urls: list[str] = []

        async def _handler(request: httpx.Request) -> httpx.Response:
            url = str(request.url)
            requested_urls.append(url)
            if url == "https://video.svt.se/video/eXv13pb":
                return httpx.Response(200, json=video_payload, request=request)
            if url == default_resolve:
                return httpx.Response(
                    200,
                    json={"location": default_manifest},
                    request=request,
                )
            return httpx.Response(404, json={"error": "unexpected"}, request=request)

        async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
            session = _make_session(client)
            await provider.on_launch(session, LaunchCredentials())

            media = await provider.resolve_media(
                session,
                LoadRequest(
                    request_id=1,
                    media=MediaInfo(
                        content_id="https://video.svt.se/video/eXv13pb",
                        content_type="video/mp4",
                        stream_type=StreamType.BUFFERED,
                        metadata=MediaMetadata(
                            metadata_type=1,
                            title="Fallback title",
                            subtitle="Fallback subtitle",
                        ),
                        custom_data={
                            "videoTrackPreference": {"kind": "Original"},
                        },
                    ),
                ),
            )

        assert requested_urls[0] == "https://video.svt.se/video/eXv13pb"

        assert len(media.streams) >= 2

        parsed = parse_qs(urlsplit(media.streams[0].url).query)
        assert parsed["manifestUrl"] == [default_manifest]
        assert "manifestUrlSignLanguage" not in parsed
        assert "manifestUrlAudioDescription" not in parsed
        assert "preferredVideoTrack" not in parsed

        assert media.streams[1].url == default_manifest

        assert media.title == "Fallback title"
        assert media.subtitle == "Fallback subtitle"
        assert media.duration == 56

    async def test_detects_clearkey_and_includes_unencrypted_fallback(self) -> None:
        provider = SvtPlayProvider()

        primary_resolve = "https://switcher.cdn.svt.se/resolve/crypt-id/dash-full.mpd"
        fallback_resolve = (
            "https://switcher.cdn.svt.se/resolve/crypt-id/dash-hbbtv-avc.mpd"
        )
        primary_manifest = (
            "https://ed8.cdn.svt.se/d0/crypt/20260222/crypt-id/dash-full.mpd"
        )
        fallback_manifest = (
            "https://ed8.cdn.svt.se/d0/crypt/20260222/crypt-id/dash-hbbtv-avc.mpd"
        )

        video_payload = {
            "svtId": "cryptAsset",
            "programTitle": "Encrypted Test",
            "contentDuration": 111,
            "videoReferences": [
                {
                    "format": "dash-full",
                    "url": "https://svt-vod.example/crypt-id/dash-full.mpd",
                    "resolve": primary_resolve,
                },
                {
                    "format": "dash-hbbtv-avc",
                    "url": "https://svt-vod.example/crypt-id/dash-hbbtv-avc.mpd",
                    "resolve": fallback_resolve,
                },
            ],
            "variants": {
                "default": {
                    "videoReferences": [
                        {
                            "format": "dash-full",
                            "url": "https://svt-vod.example/crypt-id/dash-full.mpd",
                            "resolve": primary_resolve,
                        },
                        {
                            "format": "dash-hbbtv-avc",
                            "url": "https://svt-vod.example/crypt-id/dash-hbbtv-avc.mpd",
                            "resolve": fallback_resolve,
                        },
                    ]
                }
            },
        }

        clearkey_manifest = (
            "<MPD><Period><AdaptationSet><ContentProtection "
            'schemeIdUri="urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e">'
            '<dashif:Laurl xmlns:dashif="https://dashif.org/guidelines/clearKey">'
            "https://license.example.com/clearkey"
            "</dashif:Laurl>"
            "</ContentProtection></AdaptationSet></Period></MPD>"
        )
        unencrypted_manifest = "<MPD><Period><AdaptationSet /></Period></MPD>"

        async def _handler(request: httpx.Request) -> httpx.Response:
            url = str(request.url)
            if url == "https://video.svt.se/video/cryptAsset":
                return httpx.Response(200, json=video_payload, request=request)
            if url == primary_resolve:
                return httpx.Response(
                    200,
                    json={"location": primary_manifest},
                    request=request,
                )
            if url == fallback_resolve:
                return httpx.Response(
                    200,
                    json={"location": fallback_manifest},
                    request=request,
                )
            if url.startswith("https://api.svt.se/ditto/api/v3/manifest?"):
                return httpx.Response(
                    200,
                    text=clearkey_manifest,
                    headers={"content-type": "application/dash+xml"},
                    request=request,
                )
            if url == primary_manifest:
                return httpx.Response(
                    200,
                    text=clearkey_manifest,
                    headers={"content-type": "application/dash+xml"},
                    request=request,
                )
            if url == fallback_manifest:
                return httpx.Response(
                    200,
                    text=unencrypted_manifest,
                    headers={"content-type": "application/dash+xml"},
                    request=request,
                )
            return httpx.Response(404, request=request)

        async with httpx.AsyncClient(transport=httpx.MockTransport(_handler)) as client:
            session = _make_session(client)
            await provider.on_launch(session, LaunchCredentials())

            media = await provider.resolve_media(
                session,
                LoadRequest(
                    request_id=11,
                    media=MediaInfo(
                        content_id="cryptAsset",
                        content_type="video/mp4",
                        stream_type=StreamType.BUFFERED,
                    ),
                ),
            )

        assert len(media.streams) >= 3
        assert any(
            stream.drm is not None and stream.drm.system is DrmSystem.CLEARKEY
            for stream in media.streams
        )
        assert any(stream.drm is None for stream in media.streams)

        clearkey_stream = next(
            stream for stream in media.streams if stream.drm is not None
        )
        assert clearkey_stream.drm is not None
        assert clearkey_stream.drm.system is DrmSystem.CLEARKEY
        assert clearkey_stream.drm.license_url == "https://license.example.com/clearkey"
