"""Tests for media namespace models."""

from castvibe._models import (
    LoadRequest,
    MediaGetStatusRequest,
    MediaInfo,
    MediaMetadata,
    MediaStatus,
    MediaStatusResponse,
    MediaStopRequest,
    PauseRequest,
    PlayerState,
    PlayRequest,
    QueueLoadRequest,
    SeekRequest,
    StreamType,
    media_request_adapter,
)


class TestLoadRequest:
    """LoadRequest with nested MediaInfo."""

    def test_basic_load(self) -> None:
        raw = {
            "type": "LOAD",
            "requestId": 1,
            "media": {
                "contentId": "https://example.com/video",
                "contentType": "video/mp4",
                "streamType": "BUFFERED",
                "metadata": {"metadataType": 0, "title": "My Video"},
            },
            "autoplay": True,
            "currentTime": 0,
        }
        msg = LoadRequest.model_validate(raw)
        assert msg.media.content_id == "https://example.com/video"
        assert msg.media.content_type == "video/mp4"
        assert msg.media.stream_type == "BUFFERED"
        assert msg.media.metadata is not None
        assert msg.media.metadata.title == "My Video"
        assert msg.autoplay is True
        assert msg.current_time == 0.0

    def test_load_with_custom_data(self) -> None:
        raw = {
            "type": "LOAD",
            "requestId": 1,
            "media": {
                "contentId": "content-1",
                "contentType": "video/mp4",
            },
            "customData": {"playUrl": "https://content.viaplay.se/stream"},
        }
        msg = LoadRequest.model_validate(raw)
        assert msg.custom_data is not None
        assert msg.custom_data["playUrl"] == "https://content.viaplay.se/stream"

    def test_round_trip(self) -> None:
        original = LoadRequest(
            request_id=1,
            media=MediaInfo(
                content_id="https://example.com/video",
                content_type="video/mp4",
                metadata=MediaMetadata(title="Test"),
            ),
        )
        data = original.model_dump(exclude_none=True)
        restored = LoadRequest.model_validate(data)
        assert restored.media.content_id == "https://example.com/video"
        assert restored.media.metadata is not None
        assert restored.media.metadata.title == "Test"


class TestMediaStatusResponse:
    """Verify MEDIA_STATUS output matches expected protocol JSON."""

    def test_empty_status(self) -> None:
        """Empty status list (as sent by go-cast for GET_STATUS / initial)."""
        response = MediaStatusResponse(request_id=1, status=[])
        data = response.model_dump(exclude_none=True)
        assert data == {"type": "MEDIA_STATUS", "requestId": 1, "status": []}

    def test_with_playing_status(self) -> None:
        response = MediaStatusResponse(
            request_id=1,
            status=[
                MediaStatus(
                    media_session_id=1,
                    media=MediaInfo(
                        content_id="https://example.com/video",
                        content_type="video/mp4",
                        stream_type=StreamType.BUFFERED,
                    ),
                    player_state=PlayerState.PLAYING,
                    current_time=0,
                    supported_media_commands=15,
                ),
            ],
        )
        data = response.model_dump(exclude_none=True)
        assert data["type"] == "MEDIA_STATUS"
        assert len(data["status"]) == 1
        entry = data["status"][0]
        assert entry["mediaSessionId"] == 1
        assert entry["playerState"] == "PLAYING"
        assert entry["media"]["contentId"] == "https://example.com/video"
        assert entry["supportedMediaCommands"] == 15


class TestMediaRequestDiscriminator:
    """media_request_adapter dispatches on type field."""

    def test_get_status_dispatch(self) -> None:
        msg = media_request_adapter.validate_python(
            {"type": "GET_STATUS", "requestId": 1}
        )
        assert isinstance(msg, MediaGetStatusRequest)

    def test_load_dispatch(self) -> None:
        msg = media_request_adapter.validate_python(
            {
                "type": "LOAD",
                "requestId": 2,
                "media": {"contentId": "x", "contentType": "video/mp4"},
            }
        )
        assert isinstance(msg, LoadRequest)

    def test_play_dispatch(self) -> None:
        msg = media_request_adapter.validate_python(
            {"type": "PLAY", "requestId": 3, "mediaSessionId": 1}
        )
        assert isinstance(msg, PlayRequest)

    def test_pause_dispatch(self) -> None:
        msg = media_request_adapter.validate_python(
            {"type": "PAUSE", "requestId": 4, "mediaSessionId": 1}
        )
        assert isinstance(msg, PauseRequest)

    def test_seek_dispatch(self) -> None:
        msg = media_request_adapter.validate_python(
            {
                "type": "SEEK",
                "requestId": 5,
                "mediaSessionId": 1,
                "currentTime": 30.0,
            }
        )
        assert isinstance(msg, SeekRequest)
        assert msg.current_time == 30.0

    def test_stop_dispatch(self) -> None:
        msg = media_request_adapter.validate_python(
            {"type": "STOP", "requestId": 6, "mediaSessionId": 1}
        )
        assert isinstance(msg, MediaStopRequest)

    def test_queue_load_dispatch(self) -> None:
        msg = media_request_adapter.validate_python(
            {"type": "QUEUE_LOAD", "requestId": 7}
        )
        assert isinstance(msg, QueueLoadRequest)
