"""Multizone namespace messages (GET_STATUS / MULTIZONE_STATUS)."""

from typing import Literal

from pydantic import TypeAdapter

from vibecast._models._base import CastModel


class MultizoneGetStatusRequest(CastModel):
    """Sender requests multizone status information."""

    type: Literal["GET_STATUS"] = "GET_STATUS"
    request_id: int


class MultizoneDeviceVolume(CastModel):
    """Volume info for a multizone device member."""

    level: float = 1.0
    muted: bool = False


class MultizoneDevice(CastModel):
    """Member device entry in a multizone group."""

    capabilities: int
    device_id: str
    name: str
    volume: MultizoneDeviceVolume


class MultizonePlaybackSession(CastModel):
    """Playback session metadata for grouped playback."""

    app_allows_grouping: bool = True
    immutable_devices: list[MultizoneDevice] = []
    is_video_content: bool = True
    stream_transfer_supported: bool = False


class MultizoneStatus(CastModel):
    """Multizone status payload."""

    devices: list[MultizoneDevice] = []
    is_multichannel: bool = False
    playback_session: MultizonePlaybackSession | None = None


class MultizoneStatusResponse(CastModel):
    """Response to multizone GET_STATUS."""

    type: Literal["MULTIZONE_STATUS"] = "MULTIZONE_STATUS"
    request_id: int
    status: MultizoneStatus


multizone_get_status_adapter: TypeAdapter[MultizoneGetStatusRequest] = TypeAdapter(
    MultizoneGetStatusRequest
)
