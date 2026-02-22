"""Receiver namespace messages.

Inbound (from sender):
    GET_STATUS, LAUNCH, STOP, GET_APP_AVAILABILITY, SET_VOLUME

Outbound (from receiver):
    RECEIVER_STATUS, GET_APP_AVAILABILITY, LAUNCH_ERROR, INVALID_REQUEST
"""

from typing import Annotated, Any, Literal

from pydantic import Discriminator, TypeAdapter

from vibecast._models._base import CastModel
from vibecast._models._common import ReceiverStatus, Volume

# ---------------------------------------------------------------------------
# Inbound messages (sender -> receiver)
# ---------------------------------------------------------------------------


class GetStatusRequest(CastModel):
    """Sender requests current receiver status."""

    type: Literal["GET_STATUS"] = "GET_STATUS"
    request_id: int


class _CredentialsData(CastModel):
    """Credentials nested inside appParams.launchCheckerParams."""

    credentials: str | None = None
    credentials_type: str | None = None


class _LaunchCheckerParams(CastModel):
    """Nested inside appParams on a LAUNCH request."""

    credentials_data: _CredentialsData | None = None


class _AppParams(CastModel):
    """Optional structured app launch parameters."""

    launch_checker_params: _LaunchCheckerParams | None = None


class LaunchRequest(CastModel):
    """Sender requests launching an application.

    Credentials may appear at the top level *or* nested under
    ``appParams.launchCheckerParams.credentialsData`` depending on the
    sender implementation.
    """

    type: Literal["LAUNCH"] = "LAUNCH"
    request_id: int
    app_id: str
    credentials: str | None = None
    credentials_type: str | None = None
    language: str | None = None
    app_params: _AppParams | None = None
    custom_data: dict[str, Any] | None = None


class StopRequest(CastModel):
    """Sender requests stopping a running application."""

    type: Literal["STOP"] = "STOP"
    request_id: int
    session_id: str


class GetAppAvailabilityRequest(CastModel):
    """Sender queries which apps are available on the receiver."""

    type: Literal["GET_APP_AVAILABILITY"] = "GET_APP_AVAILABILITY"
    request_id: int
    app_id: list[str]


class SetVolumeRequest(CastModel):
    """Sender requests a volume change."""

    type: Literal["SET_VOLUME"] = "SET_VOLUME"
    request_id: int
    volume: Volume


# ---------------------------------------------------------------------------
# Outbound messages (receiver -> sender)
# ---------------------------------------------------------------------------


class ReceiverStatusResponse(CastModel):
    """Broadcast receiver status (response to GET_STATUS, LAUNCH, STOP, etc.)."""

    type: Literal["RECEIVER_STATUS"] = "RECEIVER_STATUS"
    request_id: int
    status: ReceiverStatus


class AppAvailabilityResponse(CastModel):
    """Response to GET_APP_AVAILABILITY."""

    type: Literal["GET_APP_AVAILABILITY"] = "GET_APP_AVAILABILITY"
    request_id: int
    availability: dict[str, str]


class LaunchErrorResponse(CastModel):
    """Error response when an app launch fails."""

    type: Literal["LAUNCH_ERROR"] = "LAUNCH_ERROR"
    request_id: int
    reason: str | None = None


class InvalidRequestResponse(CastModel):
    """Generic error for malformed/unsupported receiver requests."""

    type: Literal["INVALID_REQUEST"] = "INVALID_REQUEST"
    request_id: int
    reason: str | None = None


# ---------------------------------------------------------------------------
# Discriminated union of inbound receiver requests
# ---------------------------------------------------------------------------

ReceiverRequest = Annotated[
    LaunchRequest
    | StopRequest
    | GetStatusRequest
    | GetAppAvailabilityRequest
    | SetVolumeRequest,
    Discriminator("type"),
]
"""Discriminated union of all inbound receiver namespace messages."""

receiver_request_adapter: TypeAdapter[ReceiverRequest] = TypeAdapter(
    ReceiverRequest,
)
