"""Player abstractions and universal WebSocket protocol models.

Glossary
--------
Player        -- ABC for playback control (play, pause, seek, load).
PlayerBridge  -- Default Player implementation; an HTTP/WS server that bridges
                 commands to an external Renderer (see ``_playback.player_bridge``).
Renderer      -- The external process (browser page or Kodi) that actually
                 decodes and displays video, connected to PlayerBridge via WebSocket.
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from enum import StrEnum
from typing import TYPE_CHECKING, Annotated, Any, Literal, override

from pydantic import Discriminator, TypeAdapter

from vibecast._models import CastModel, IdleReason, MediaImage, PlayerState, StreamType

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable


@dataclass(slots=True, frozen=True)
class DrmInfo:
    """DRM configuration for protected media."""

    system: DrmSystem
    license_url: str
    headers: dict[str, str] = field(default_factory=dict)


class DrmSystem(StrEnum):
    """Supported DRM key-system identifiers."""

    WIDEVINE = "com.widevine.alpha"
    CLEARKEY = "org.w3.clearkey"


@dataclass(slots=True, frozen=True)
class PlaybackStream:
    """Single playable stream candidate with optional DRM configuration."""

    url: str
    content_type: str
    drm: DrmInfo | None = None


@dataclass(slots=True, frozen=True)
class PlaybackMedia:
    """Canonical media description passed from coordinator to player.

    ``content_id`` carries the original content identifier from the LOAD
    request (e.g. a content page URL), distinct from the resolved stream
    URLs in ``streams``.
    """

    session_id: str
    streams: tuple[PlaybackStream, ...]
    stream_type: StreamType
    content_id: str | None = None
    title: str | None = None
    subtitle: str | None = None
    images: tuple[MediaImage, ...] = ()
    duration: float | None = None
    autoplay: bool = True
    start_time: float = 0.0
    custom_data: dict[str, Any] = field(default_factory=dict)


@dataclass(slots=True, frozen=True)
class PlaybackState:
    """Canonical playback state reported by a player."""

    player_state: PlayerState
    current_time: float = 0.0
    duration: float | None = None
    idle_reason: IdleReason | None = None


@dataclass(slots=True, frozen=True)
class PlaybackError:
    """Player error payload."""

    code: str
    message: str


@dataclass(slots=True, frozen=True)
class LicenseRequest:
    """License request forwarded from the external player."""

    session_id: str
    body: bytes
    content_type: str = "application/octet-stream"
    route_id: str | None = None
    headers: dict[str, str] = field(default_factory=dict)


@dataclass(slots=True, frozen=True)
class LicenseRoute:
    """Resolved coordinator route metadata for one DRM license target."""

    route_id: str
    system: DrmSystem
    upstream_url: str
    headers: dict[str, str] = field(default_factory=dict)


@dataclass(slots=True, frozen=True)
class LicenseResponse:
    """License response returned to the external player."""

    body: bytes
    content_type: str = "application/octet-stream"
    status: int = 200


class DrmPayload(CastModel):
    """WebSocket wire model for DRM config."""

    system: DrmSystem
    license_url: str
    headers: dict[str, str] = {}


class PlaybackStreamPayload(CastModel):
    """WebSocket wire model for one stream candidate."""

    url: str
    content_type: str
    drm: DrmPayload | None = None


class PlaybackMediaPayload(CastModel):
    """WebSocket wire model for media load payload."""

    streams: list[PlaybackStreamPayload] = []
    stream_type: StreamType
    title: str | None = None
    subtitle: str | None = None
    images: list[MediaImage] = []
    duration: float | None = None
    autoplay: bool = True
    start_time: float = 0.0
    custom_data: dict[str, Any] = {}


class LoadCommand(CastModel):
    """Command: load media."""

    type: Literal["load"] = "load"
    session_id: str
    media: PlaybackMediaPayload


class PlayCommand(CastModel):
    """Command: resume playback."""

    type: Literal["play"] = "play"
    session_id: str


class PauseCommand(CastModel):
    """Command: pause playback."""

    type: Literal["pause"] = "pause"
    session_id: str


class SeekCommand(CastModel):
    """Command: seek playback."""

    type: Literal["seek"] = "seek"
    session_id: str
    position: float


class StopCommand(CastModel):
    """Command: stop playback."""

    type: Literal["stop"] = "stop"
    session_id: str


class VolumeCommand(CastModel):
    """Command: set player volume."""

    type: Literal["volume"] = "volume"
    session_id: str
    level: float
    muted: bool


PlayerCommand = Annotated[
    LoadCommand
    | PlayCommand
    | PauseCommand
    | SeekCommand
    | StopCommand
    | VolumeCommand,
    Discriminator("type"),
]

player_command_adapter: TypeAdapter[PlayerCommand] = TypeAdapter(PlayerCommand)


class StateReport(CastModel):
    """Report: player state update."""

    type: Literal["state"] = "state"
    session_id: str
    player_state: PlayerState
    current_time: float = 0.0
    duration: float | None = None
    idle_reason: IdleReason | None = None


class ErrorReport(CastModel):
    """Report: player error."""

    type: Literal["error"] = "error"
    session_id: str
    code: str
    message: str


PlayerReport = Annotated[StateReport | ErrorReport, Discriminator("type")]

player_report_adapter: TypeAdapter[PlayerReport] = TypeAdapter(PlayerReport)


class PlayerContext:
    """Context object passed to :class:`Player` command handlers."""

    __slots__ = ("session_id", "_report_error", "_report_state")

    def __init__(
        self,
        session_id: str,
        *,
        report_state: Callable[[PlaybackState], Awaitable[None]],
        report_error: Callable[[PlaybackError], Awaitable[None]],
    ) -> None:
        self.session_id = session_id
        self._report_state = report_state
        self._report_error = report_error

    async def report_state(self, state: PlaybackState) -> None:
        """Report player-state updates back to the coordinator."""
        await self._report_state(state)

    async def report_error(self, error: PlaybackError) -> None:
        """Report player errors back to the coordinator."""
        await self._report_error(error)


class Player(ABC):
    """Internal player interface used by :class:`PlaybackCoordinator`."""

    @abstractmethod
    async def on_load(self, ctx: PlayerContext, media: PlaybackMedia) -> None:
        """Load and prepare media playback."""

    @abstractmethod
    async def on_play(self, ctx: PlayerContext) -> None:
        """Resume playback."""

    @abstractmethod
    async def on_pause(self, ctx: PlayerContext) -> None:
        """Pause playback."""

    @abstractmethod
    async def on_seek(self, ctx: PlayerContext, position: float) -> None:
        """Seek playback."""

    @abstractmethod
    async def on_stop(self, ctx: PlayerContext) -> None:
        """Stop playback."""

    async def on_volume(self, ctx: PlayerContext, level: float, muted: bool) -> None:
        """Update player volume (optional no-op)."""
        _ = ctx
        _ = level
        _ = muted


class DefaultPlayer(Player):
    """No-op player implementation useful for tests."""

    @override
    async def on_load(self, ctx: PlayerContext, media: PlaybackMedia) -> None:
        _ = ctx
        _ = media

    @override
    async def on_play(self, ctx: PlayerContext) -> None:
        _ = ctx

    @override
    async def on_pause(self, ctx: PlayerContext) -> None:
        _ = ctx

    @override
    async def on_seek(self, ctx: PlayerContext, position: float) -> None:
        _ = ctx
        _ = position

    @override
    async def on_stop(self, ctx: PlayerContext) -> None:
        _ = ctx


__all__ = [
    "DefaultPlayer",
    "DrmInfo",
    "DrmPayload",
    "DrmSystem",
    "ErrorReport",
    "IdleReason",
    "LicenseRequest",
    "LicenseRoute",
    "LicenseResponse",
    "LoadCommand",
    "MediaImage",
    "PauseCommand",
    "PlaybackError",
    "PlaybackMedia",
    "PlaybackMediaPayload",
    "PlaybackStream",
    "PlaybackStreamPayload",
    "PlaybackState",
    "PlayCommand",
    "Player",
    "PlayerCommand",
    "PlayerContext",
    "PlayerReport",
    "PlayerState",
    "SeekCommand",
    "StateReport",
    "StopCommand",
    "StreamType",
    "VolumeCommand",
    "player_command_adapter",
    "player_report_adapter",
]
