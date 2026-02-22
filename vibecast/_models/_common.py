"""Shared types, enums, and reusable sub-models for Cast protocol messages."""

from __future__ import annotations

from enum import IntEnum, IntFlag, StrEnum
from typing import Any

from vibecast._models._base import CastModel

# ---------------------------------------------------------------------------
# Enums
# ---------------------------------------------------------------------------


class StreamType(StrEnum):
    """Media stream types used in LOAD requests."""

    BUFFERED = "BUFFERED"
    LIVE = "LIVE"
    NONE = "NONE"


class PlayerState(StrEnum):
    """Playback states reported in MEDIA_STATUS."""

    IDLE = "IDLE"
    PLAYING = "PLAYING"
    PAUSED = "PAUSED"
    BUFFERING = "BUFFERING"


class IdleReason(StrEnum):
    """Reason the player entered the IDLE state."""

    CANCELLED = "CANCELLED"
    INTERRUPTED = "INTERRUPTED"
    FINISHED = "FINISHED"
    ERROR = "ERROR"


class RepeatMode(StrEnum):
    """Queue repeat mode in MEDIA_STATUS."""

    REPEAT_OFF = "REPEAT_OFF"
    REPEAT_ALL = "REPEAT_ALL"
    REPEAT_SINGLE = "REPEAT_SINGLE"
    REPEAT_ALL_AND_SHUFFLE = "REPEAT_ALL_AND_SHUFFLE"


class MetadataType(IntEnum):
    """Well-known metadata types for media items."""

    GENERIC = 0
    MOVIE = 1
    TV_SHOW = 2
    MUSIC_TRACK = 3
    PHOTO = 4


class HdrType(StrEnum):
    """HDR type reported in ``VideoInfo``."""

    SDR = "sdr"
    HDR = "hdr"
    DV = "dv"


class MediaCategory(StrEnum):
    """Media category reported in ``MediaInfo``."""

    VIDEO = "VIDEO"
    AUDIO = "AUDIO"
    IMAGE = "IMAGE"


class MediaCommand(IntFlag):
    """Bitmask values for ``MediaStatus.supportedMediaCommands``.

    ``LOAD``, ``PLAY``, ``STOP``, and ``GET_STATUS`` are always implicitly
    supported and do not appear in the bitmask.
    """

    PAUSE = 1
    SEEK = 2
    STREAM_VOLUME = 4
    STREAM_MUTE = 8
    SKIP_FORWARD = 16
    SKIP_BACKWARD = 32
    QUEUE_NEXT = 64
    QUEUE_PREV = 128
    QUEUE_SHUFFLE = 256
    SKIP_AD = 512
    QUEUE_REPEAT_ALL = 1024
    QUEUE_REPEAT_ONE = 2048
    EDIT_TRACKS = 4096
    PLAYBACK_RATE = 8192
    LIKE = 16384
    DISLIKE = 32768
    FOLLOW = 65536
    UNFOLLOW = 131072
    STREAM_TRANSFER = 262144


# ---------------------------------------------------------------------------
# Sub-models
# ---------------------------------------------------------------------------


class Volume(CastModel):
    """Audio volume state."""

    level: float = 1.0
    muted: bool = False
    control_type: str | None = None
    step_interval: float | None = None


class CastNamespace(CastModel):
    """A namespace entry in an application status block.

    Named ``CastNamespace`` rather than ``Namespace`` to avoid shadowing
    the Python builtin.
    """

    name: str


class ApplicationStatus(CastModel):
    """Status of a running Cast application."""

    app_id: str
    display_name: str
    session_id: str
    transport_id: str
    status_text: str = ""
    namespaces: list[CastNamespace] = []
    is_idle_screen: bool = False
    app_type: str | None = None
    icon_url: str | None = None
    launched_from_cloud: bool | None = None
    sender_connected: bool | None = None
    universal_app_id: str | None = None


class ReceiverStatus(CastModel):
    """Top-level receiver status included in RECEIVER_STATUS responses."""

    applications: list[ApplicationStatus] = []
    volume: Volume = Volume()
    is_active_input: bool | None = None
    is_stand_by: bool | None = None
    user_eq: dict[str, Any] | None = None


class MediaImage(CastModel):
    """An image reference within media metadata."""

    url: str
    height: int | None = None
    width: int | None = None


class MediaMetadata(CastModel):
    """Metadata for a media item.

    ``metadata_type`` selects the metadata schema:

    - :attr:`MetadataType.GENERIC` (0) — title, subtitle
    - :attr:`MetadataType.MOVIE` (1) — title, subtitle, studio
    - :attr:`MetadataType.TV_SHOW` (2) — series_title, title, season, episode
    - :attr:`MetadataType.MUSIC_TRACK` (3) — title, album, artist
    - :attr:`MetadataType.PHOTO` (4) — title, location
    """

    metadata_type: MetadataType = MetadataType.GENERIC
    title: str | None = None
    subtitle: str | None = None
    series_title: str | None = None
    season: int | None = None
    episode: int | None = None
    images: list[MediaImage] = []


class MediaInfo(CastModel):
    """Description of a media item in LOAD / MEDIA_STATUS messages.

    ``content_id`` is the logical content identifier (e.g. a content page URL).
    ``content_url`` is the resolved playback manifest URL (DASH/HLS).
    """

    content_id: str
    content_type: str = ""
    stream_type: StreamType = StreamType.BUFFERED
    metadata: MediaMetadata | None = None
    duration: float | None = None
    custom_data: dict[str, Any] | None = None
    content_url: str | None = None
    media_category: MediaCategory | None = None
    start_absolute_time: float | None = None
    is_live_media: bool | None = None


class VideoInfo(CastModel):
    """Video resolution and HDR type reported by the player."""

    width: int
    height: int
    hdr_type: HdrType = HdrType.SDR


class LiveSeekableRange(CastModel):
    """Seekable range for a live stream."""

    start: float = 0.0
    end: float = 0.0
    is_moving_window: bool = False
    is_live_done: bool = False


class ExtendedStatus(CastModel):
    """Extended status used during loading to show progress.

    While the main ``MediaStatus.player_state`` is ``IDLE``, the
    ``extended_status.player_state`` can be ``LOADING`` to signal that
    the receiver is resolving and preparing the media.
    """

    player_state: str
    media: MediaInfo | None = None
    media_session_id: int | None = None


class MediaStatus(CastModel):
    """A single media-session status entry within a MEDIA_STATUS response."""

    media_session_id: int
    media: MediaInfo | None = None
    player_state: PlayerState = PlayerState.IDLE
    current_time: float = 0.0
    supported_media_commands: MediaCommand = MediaCommand(0)
    volume: Volume | None = None
    idle_reason: IdleReason | None = None
    custom_data: dict[str, Any] | None = None
    playback_rate: float | None = None
    current_item_id: int | None = None
    repeat_mode: RepeatMode | None = None
    extended_status: ExtendedStatus | None = None
    live_seekable_range: LiveSeekableRange | None = None
    video_info: VideoInfo | None = None
    active_track_ids: list[int] | None = None
