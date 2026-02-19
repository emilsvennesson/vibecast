"""Tests for receiver namespace models."""

from castvibe._models import (
    AppAvailabilityResponse,
    ApplicationStatus,
    CastNamespace,
    GetAppAvailabilityRequest,
    GetStatusRequest,
    InvalidRequestResponse,
    LaunchErrorResponse,
    LaunchRequest,
    ReceiverStatus,
    ReceiverStatusResponse,
    SetVolumeRequest,
    StopRequest,
    Volume,
    receiver_request_adapter,
)


class TestGetStatusRequest:
    def test_round_trip(self) -> None:
        raw = {"type": "GET_STATUS", "requestId": 1}
        msg = GetStatusRequest.model_validate(raw)
        assert msg.request_id == 1
        data = msg.model_dump(exclude_none=True)
        assert data["type"] == "GET_STATUS"
        assert data["requestId"] == 1


class TestLaunchRequest:
    def test_basic_launch(self) -> None:
        raw = {
            "type": "LAUNCH",
            "requestId": 3,
            "appId": "6313CF39",
        }
        msg = LaunchRequest.model_validate(raw)
        assert msg.app_id == "6313CF39"
        assert msg.request_id == 3

    def test_launch_with_credentials(self) -> None:
        raw = {
            "type": "LAUNCH",
            "requestId": 3,
            "appId": "6313CF39",
            "credentials": "token123",
            "credentialsType": "bearer",
            "language": "en",
        }
        msg = LaunchRequest.model_validate(raw)
        assert msg.credentials == "token123"
        assert msg.credentials_type == "bearer"
        assert msg.language == "en"

    def test_launch_with_app_params(self) -> None:
        """Credentials can arrive nested in appParams."""
        raw = {
            "type": "LAUNCH",
            "requestId": 5,
            "appId": "6313CF39",
            "appParams": {
                "launchCheckerParams": {
                    "credentialsData": {
                        "credentials": "nested-token",
                        "credentialsType": "oauth",
                    }
                }
            },
        }
        msg = LaunchRequest.model_validate(raw)
        assert msg.app_params is not None
        lcp = msg.app_params.launch_checker_params
        assert lcp is not None
        cd = lcp.credentials_data
        assert cd is not None
        assert cd.credentials == "nested-token"
        assert cd.credentials_type == "oauth"

    def test_round_trip(self) -> None:
        original = LaunchRequest(request_id=10, app_id="CC1AD845")
        data = original.model_dump(exclude_none=True)
        restored = LaunchRequest.model_validate(data)
        assert restored.app_id == "CC1AD845"
        assert restored.request_id == 10


class TestStopRequest:
    def test_round_trip(self) -> None:
        raw = {
            "type": "STOP",
            "requestId": 7,
            "sessionId": "abc-123",
        }
        msg = StopRequest.model_validate(raw)
        assert msg.session_id == "abc-123"
        data = msg.model_dump(exclude_none=True)
        assert data["sessionId"] == "abc-123"


class TestGetAppAvailabilityRequest:
    def test_round_trip(self) -> None:
        raw = {
            "type": "GET_APP_AVAILABILITY",
            "requestId": 2,
            "appId": ["6313CF39", "CC1AD845"],
        }
        msg = GetAppAvailabilityRequest.model_validate(raw)
        assert msg.app_id == ["6313CF39", "CC1AD845"]
        data = msg.model_dump(exclude_none=True)
        assert data["appId"] == ["6313CF39", "CC1AD845"]


class TestSetVolumeRequest:
    def test_round_trip(self) -> None:
        raw = {
            "type": "SET_VOLUME",
            "requestId": 4,
            "volume": {"level": 0.5, "muted": False},
        }
        msg = SetVolumeRequest.model_validate(raw)
        assert msg.volume.level == 0.5
        assert msg.volume.muted is False


class TestReceiverStatusResponse:
    """Verify RECEIVER_STATUS output matches protocol JSON examples."""

    def test_serializes_to_protocol_format(self) -> None:
        response = ReceiverStatusResponse(
            request_id=1,
            status=ReceiverStatus(
                applications=[
                    ApplicationStatus(
                        app_id="6313CF39",
                        display_name="Viaplay",
                        session_id="uuid-here",
                        transport_id="pid-1",
                        status_text="",
                        namespaces=[
                            CastNamespace(name="urn:x-cast:tv.viaplay.chromecast"),
                            CastNamespace(name="urn:x-cast:com.google.cast.media"),
                        ],
                        is_idle_screen=False,
                    ),
                ],
                volume=Volume(
                    level=1.0,
                    muted=False,
                    control_type="attenuation",
                    step_interval=0.05,
                ),
            ),
        )
        data = response.model_dump(exclude_none=True)
        assert data["type"] == "RECEIVER_STATUS"
        assert data["requestId"] == 1

        status = data["status"]
        assert len(status["applications"]) == 1

        app = status["applications"][0]
        assert app["appId"] == "6313CF39"
        assert app["displayName"] == "Viaplay"
        assert app["sessionId"] == "uuid-here"
        assert app["transportId"] == "pid-1"
        assert app["isIdleScreen"] is False
        assert len(app["namespaces"]) == 2
        assert app["namespaces"][0]["name"] == "urn:x-cast:tv.viaplay.chromecast"

        vol = status["volume"]
        assert vol["level"] == 1.0
        assert vol["muted"] is False
        assert vol["controlType"] == "attenuation"
        assert vol["stepInterval"] == 0.05


class TestAppAvailabilityResponse:
    def test_round_trip(self) -> None:
        response = AppAvailabilityResponse(
            request_id=2,
            availability={"6313CF39": "APP_AVAILABLE", "CC1AD845": "APP_AVAILABLE"},
        )
        data = response.model_dump(exclude_none=True)
        assert data["type"] == "GET_APP_AVAILABILITY"
        assert data["availability"]["6313CF39"] == "APP_AVAILABLE"

        restored = AppAvailabilityResponse.model_validate(data)
        assert restored.availability == response.availability


class TestErrorResponses:
    def test_launch_error(self) -> None:
        msg = LaunchErrorResponse(request_id=3, reason="App not found")
        data = msg.model_dump(exclude_none=True)
        assert data["type"] == "LAUNCH_ERROR"
        assert data["reason"] == "App not found"

    def test_invalid_request(self) -> None:
        msg = InvalidRequestResponse(request_id=4, reason="Unknown type")
        data = msg.model_dump(exclude_none=True)
        assert data["type"] == "INVALID_REQUEST"


class TestReceiverRequestDiscriminator:
    """receiver_request_adapter dispatches on type field."""

    def test_get_status_dispatch(self) -> None:
        msg = receiver_request_adapter.validate_python(
            {"type": "GET_STATUS", "requestId": 1}
        )
        assert isinstance(msg, GetStatusRequest)

    def test_launch_dispatch(self) -> None:
        msg = receiver_request_adapter.validate_python(
            {"type": "LAUNCH", "requestId": 2, "appId": "CC1AD845"}
        )
        assert isinstance(msg, LaunchRequest)
        assert msg.app_id == "CC1AD845"

    def test_stop_dispatch(self) -> None:
        msg = receiver_request_adapter.validate_python(
            {"type": "STOP", "requestId": 3, "sessionId": "s1"}
        )
        assert isinstance(msg, StopRequest)

    def test_get_app_availability_dispatch(self) -> None:
        msg = receiver_request_adapter.validate_python(
            {
                "type": "GET_APP_AVAILABILITY",
                "requestId": 4,
                "appId": ["6313CF39"],
            }
        )
        assert isinstance(msg, GetAppAvailabilityRequest)

    def test_set_volume_dispatch(self) -> None:
        msg = receiver_request_adapter.validate_python(
            {
                "type": "SET_VOLUME",
                "requestId": 5,
                "volume": {"level": 0.8, "muted": False},
            }
        )
        assert isinstance(msg, SetVolumeRequest)
