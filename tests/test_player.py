"""Tests for player abstractions and protocol models."""

from __future__ import annotations

from vibecast._models import IdleReason, MediaImage, PlayerState, StreamType
from vibecast.player import (
    DefaultPlayer,
    LoadCommand,
    PlaybackError,
    PlaybackMedia,
    PlaybackMediaPayload,
    PlaybackState,
    PlaybackStream,
    PlaybackStreamPayload,
    PlayerContext,
    StateReport,
    player_report_adapter,
)


def test_load_command_serializes_with_camel_case() -> None:
    command = LoadCommand(
        session_id="session-1",
        media=PlaybackMediaPayload(
            streams=[
                PlaybackStreamPayload(
                    url="https://example.com/manifest.mpd",
                    content_type="application/dash+xml",
                )
            ],
            stream_type=StreamType.BUFFERED,
            title="Example",
            images=[MediaImage(url="https://example.com/poster.jpg")],
            start_time=12.0,
            custom_data={"foo": "bar"},
        ),
    )

    dumped = command.model_dump(exclude_none=True)
    assert dumped["sessionId"] == "session-1"
    assert dumped["media"]["streams"][0]["contentType"] == "application/dash+xml"
    assert dumped["media"]["streamType"] == "BUFFERED"
    assert dumped["media"]["startTime"] == 12.0
    assert dumped["media"]["customData"] == {"foo": "bar"}


def test_player_report_adapter_validates_state_report() -> None:
    report = player_report_adapter.validate_json(
        """
        {
          "type": "state",
          "sessionId": "session-1",
          "playerState": "PLAYING",
          "currentTime": 33.5,
          "idleReason": "INTERRUPTED"
        }
        """
    )

    assert isinstance(report, StateReport)
    assert report.session_id == "session-1"
    assert report.player_state is PlayerState.PLAYING
    assert report.current_time == 33.5
    assert report.idle_reason is IdleReason.INTERRUPTED


async def test_player_context_forwards_reports() -> None:
    states: list[PlaybackState] = []
    errors: list[PlaybackError] = []

    async def _state_sink(state: PlaybackState) -> None:
        states.append(state)

    async def _error_sink(error: PlaybackError) -> None:
        errors.append(error)

    ctx = PlayerContext(
        "session-1",
        report_state=_state_sink,
        report_error=_error_sink,
    )

    await ctx.report_state(
        PlaybackState(player_state=PlayerState.PAUSED, current_time=9.0)
    )
    await ctx.report_error(PlaybackError(code="E_TEST", message="boom"))

    assert states == [PlaybackState(player_state=PlayerState.PAUSED, current_time=9.0)]
    assert errors == [PlaybackError(code="E_TEST", message="boom")]


async def test_default_player_is_noop() -> None:
    async def _state_sink(state: PlaybackState) -> None:
        _ = state

    async def _error_sink(error: PlaybackError) -> None:
        _ = error

    ctx = PlayerContext(
        "session-1",
        report_state=_state_sink,
        report_error=_error_sink,
    )
    player = DefaultPlayer()

    await player.on_load(
        ctx,
        PlaybackMedia(
            session_id="session-1",
            streams=(
                PlaybackStream(
                    url="https://example.com/manifest.mpd",
                    content_type="application/dash+xml",
                ),
            ),
            stream_type=StreamType.BUFFERED,
        ),
    )
    await player.on_play(ctx)
    await player.on_pause(ctx)
    await player.on_seek(ctx, 10.0)
    await player.on_volume(ctx, 0.8, muted=False)
    await player.on_stop(ctx)
