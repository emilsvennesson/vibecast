"""Tests for multizone namespace models."""

from vibecast._models import (
    MultizoneGetStatusRequest,
    MultizoneStatus,
    MultizoneStatusResponse,
)


class TestMultizoneGetStatusRequest:
    def test_round_trip(self) -> None:
        raw = {"type": "GET_STATUS", "requestId": 7}
        msg = MultizoneGetStatusRequest.model_validate(raw)
        assert msg.request_id == 7
        data = msg.model_dump(exclude_none=True)
        assert data == {"type": "GET_STATUS", "requestId": 7}


class TestMultizoneStatusResponse:
    def test_empty_status_serialization(self) -> None:
        response = MultizoneStatusResponse(
            request_id=11,
            status=MultizoneStatus(devices=[], is_multichannel=False),
        )

        data = response.model_dump(exclude_none=True)
        assert data == {
            "type": "MULTIZONE_STATUS",
            "requestId": 11,
            "status": {
                "devices": [],
                "isMultichannel": False,
            },
        }
