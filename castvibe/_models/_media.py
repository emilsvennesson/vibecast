"""Media namespace messages.

Inbound (from sender):
    GET_STATUS, LOAD, PLAY, PAUSE, SEEK, STOP, SET_VOLUME,
    QUEUE_LOAD, QUEUE_GET_ITEM_IDS

Outbound (from receiver):
    MEDIA_STATUS, LOAD_FAILED, INVALID_REQUEST, QUEUE_ITEM_IDS
"""

from typing import Annotated, Any, Literal

from pydantic import Discriminator, TypeAdapter

from castvibe._models._base import CastModel
from castvibe._models._common import MediaInfo, MediaStatus, Volume

# ---------------------------------------------------------------------------
# Inbound messages (sender -> receiver)
# ---------------------------------------------------------------------------


class MediaGetStatusRequest(CastModel):
    """Sender requests current media playback status."""

    type: Literal["GET_STATUS"] = "GET_STATUS"
    request_id: int
    media_session_id: int | None = None


class LoadRequest(CastModel):
    """Sender requests loading a media item."""

    type: Literal["LOAD"] = "LOAD"
    request_id: int
    media: MediaInfo
    autoplay: bool = True
    current_time: float = 0.0
    custom_data: dict[str, Any] | None = None


class PlayRequest(CastModel):
    """Sender requests resuming playback."""

    type: Literal["PLAY"] = "PLAY"
    request_id: int
    media_session_id: int


class PauseRequest(CastModel):
    """Sender requests pausing playback."""

    type: Literal["PAUSE"] = "PAUSE"
    request_id: int
    media_session_id: int


class SeekRequest(CastModel):
    """Sender requests seeking to a position."""

    type: Literal["SEEK"] = "SEEK"
    request_id: int
    media_session_id: int
    current_time: float
    resume_state: str | None = None


class MediaStopRequest(CastModel):
    """Sender requests stopping media playback (distinct from receiver STOP)."""

    type: Literal["STOP"] = "STOP"
    request_id: int
    media_session_id: int


class MediaSetVolumeRequest(CastModel):
    """Sender requests changing the media stream volume."""

    type: Literal["SET_VOLUME"] = "SET_VOLUME"
    request_id: int
    media_session_id: int
    volume: Volume


class QueueLoadRequest(CastModel):
    """Sender requests loading a media queue."""

    type: Literal["QUEUE_LOAD"] = "QUEUE_LOAD"
    request_id: int
    items: list[dict[str, Any]] = []
    start_index: int = 0
    repeat_mode: str | None = None
    custom_data: dict[str, Any] | None = None


class QueueGetItemIdsRequest(CastModel):
    """Sender requests queue item IDs for current media session."""

    type: Literal["QUEUE_GET_ITEM_IDS"] = "QUEUE_GET_ITEM_IDS"
    request_id: int
    media_session_id: int | None = None


# ---------------------------------------------------------------------------
# Outbound messages (receiver -> sender)
# ---------------------------------------------------------------------------


class MediaStatusResponse(CastModel):
    """Broadcast media status (response to GET_STATUS, LOAD, etc.)."""

    type: Literal["MEDIA_STATUS"] = "MEDIA_STATUS"
    request_id: int
    status: list[MediaStatus] = []


class QueueItemIdsResponse(CastModel):
    """Queue item ID response for ``QUEUE_GET_ITEM_IDS``."""

    type: Literal["QUEUE_ITEM_IDS"] = "QUEUE_ITEM_IDS"
    request_id: int
    item_ids: list[int] = []
    sequence_number: int = 0


class LoadFailedResponse(CastModel):
    """Error response when media loading fails."""

    type: Literal["LOAD_FAILED"] = "LOAD_FAILED"
    request_id: int
    reason: str | None = None


class MediaInvalidRequestResponse(CastModel):
    """Error for malformed/unsupported media requests."""

    type: Literal["INVALID_REQUEST"] = "INVALID_REQUEST"
    request_id: int
    reason: str | None = None


# ---------------------------------------------------------------------------
# Discriminated union of inbound media requests
# ---------------------------------------------------------------------------

MediaRequest = Annotated[
    (
        MediaGetStatusRequest
        | LoadRequest
        | PlayRequest
        | PauseRequest
        | SeekRequest
        | MediaStopRequest
        | MediaSetVolumeRequest
        | QueueLoadRequest
        | QueueGetItemIdsRequest
    ),
    Discriminator("type"),
]
"""Discriminated union of all inbound media namespace messages."""

media_request_adapter: TypeAdapter[MediaRequest] = TypeAdapter(MediaRequest)
