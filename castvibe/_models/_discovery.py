"""Discovery namespace messages (GET_DEVICE_INFO / DEVICE_INFO)."""

from typing import Literal

from castvibe._models._base import CastModel


class GetDeviceInfoRequest(CastModel):
    """Sender requests device information."""

    type: Literal["GET_DEVICE_INFO"] = "GET_DEVICE_INFO"
    request_id: int


class DeviceInfoResponse(CastModel):
    """Receiver responds with device metadata.

    Field values are modeled after a real Chromecast / Shield TV as seen
    in go-cast ``receiver.go``.
    """

    type: Literal["DEVICE_INFO"] = "DEVICE_INFO"
    request_id: int
    device_id: str
    device_model: str = ""
    friendly_name: str = ""
    device_capabilities: int = 4101
    device_icon_url: str = ""
    control_notifications: int = 1
    receiver_metrics_id: str = ""
    wifi_proximity_id: str = ""
