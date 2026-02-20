"""Tests for setup namespace models."""

from castvibe._models import SetupData, SetupDeviceInfo, SetupRequest, SetupResponse


class TestSetupRequest:
    def test_accepts_snake_case_request_id(self) -> None:
        msg = SetupRequest.model_validate({"type": "eureka_info", "request_id": 1})
        assert msg.request_id == 1

    def test_accepts_camel_case_request_id(self) -> None:
        msg = SetupRequest.model_validate({"type": "eureka_info", "requestId": 2})
        assert msg.request_id == 2


class TestSetupResponse:
    def test_serializes_to_setup_wire_shape(self) -> None:
        response = SetupResponse(
            request_id=10,
            response_code=200,
            response_string="OK",
            data=SetupData(
                name="Living Room",
                version=8,
                device_info=SetupDeviceInfo(ssdp_udn="device-1234"),
            ),
        )

        data = response.model_dump(exclude_none=True)
        assert data == {
            "type": "eureka_info",
            "request_id": 10,
            "response_code": 200,
            "response_string": "OK",
            "data": {
                "name": "Living Room",
                "version": 8,
                "device_info": {
                    "ssdp_udn": "device-1234",
                },
            },
        }
