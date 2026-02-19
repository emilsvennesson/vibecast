"""Shared types, enums, and reusable sub-models for Cast protocol messages."""

from enum import StrEnum
from typing import Any

from castvibe._models._base import CastModel

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
    """Metadata for a media item."""

    metadata_type: int = 0
    title: str | None = None
    subtitle: str | None = None
    images: list[MediaImage] = []


class MediaInfo(CastModel):
    """Description of a media item in LOAD / MEDIA_STATUS messages."""

    content_id: str
    content_type: str = ""
    stream_type: StreamType = StreamType.BUFFERED
    metadata: MediaMetadata | None = None
    duration: float | None = None
    custom_data: dict[str, Any] | None = None


class MediaStatus(CastModel):
    """A single media-session status entry within a MEDIA_STATUS response."""

    media_session_id: int
    media: MediaInfo | None = None
    player_state: PlayerState = PlayerState.IDLE
    current_time: float = 0.0
    supported_media_commands: int = 0
    volume: Volume | None = None
    idle_reason: IdleReason | None = None
    custom_data: dict[str, Any] | None = None
