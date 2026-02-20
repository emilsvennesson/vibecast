"""Tests for Viaplay custom namespace Pydantic models."""

from __future__ import annotations

from castvibe.providers.viaplay._models import (
    AuthorizationDone,
    AuthorizationRequiredMessage,
    ReceiverStateMessage,
    SessionOkMessage,
    SetupInfo,
    ViaplayReceiverState,
    viaplay_request_adapter,
)


class TestSetupInfo:
    def test_round_trip_camel_case(self) -> None:
        msg = SetupInfo(
            content_root="https://content.viaplay.se/stotta",
            country_code="se",
            user_id="u123",
            profile_id="p456",
            receiver_name="Living Room",
            receiver_language_code="sv",
        )
        dumped = msg.model_dump(exclude_none=True)
        assert dumped["type"] == "SETUP_INFO"
        assert dumped["contentRoot"] == "https://content.viaplay.se/stotta"
        assert dumped["countryCode"] == "se"
        assert dumped["userId"] == "u123"
        assert dumped["profileId"] == "p456"

    def test_parse_from_camel_case(self) -> None:
        raw = {
            "type": "SETUP_INFO",
            "contentRoot": "https://content.viaplay.no/initial",
            "countryCode": "no",
            "userId": "user1",
            "profileId": "prof1",
            "featureFlags": ["flag1", "flag2"],
        }
        msg = SetupInfo.model_validate(raw)
        assert msg.content_root == "https://content.viaplay.no/initial"
        assert msg.country_code == "no"
        assert msg.feature_flags == ["flag1", "flag2"]

    def test_extra_fields_allowed(self) -> None:
        """SetupInfo inherits CastModel which uses extra='allow'."""
        raw = {"type": "SETUP_INFO", "someUnknownField": 42}
        msg = SetupInfo.model_validate(raw)
        assert msg.type == "SETUP_INFO"


class TestAuthorizationDone:
    def test_type_literal(self) -> None:
        msg = AuthorizationDone()
        assert msg.type == "AUTHORIZATION_DONE"
        dumped = msg.model_dump(exclude_none=True)
        assert dumped["type"] == "AUTHORIZATION_DONE"


class TestReceiverStateMessage:
    def test_defaults(self) -> None:
        rs = ViaplayReceiverState()
        msg = ReceiverStateMessage(receiver_state=rs)
        dumped = msg.model_dump(exclude_none=True)
        assert dumped["type"] == "RECEIVER_STATE"
        assert dumped["receiverState"]["status"] == "IDLE"
        assert dumped["receiverState"]["isScrubbable"] is True

    def test_playing_state(self) -> None:
        rs = ViaplayReceiverState(
            status="PLAYING",
            is_scrubbable=True,
            user_id="u1",
            country_code="se",
        )
        msg = ReceiverStateMessage(receiver_state=rs)
        dumped = msg.model_dump(exclude_none=True)
        assert dumped["receiverState"]["status"] == "PLAYING"
        assert dumped["receiverState"]["userId"] == "u1"


class TestSessionOkMessage:
    def test_with_user_info(self) -> None:
        msg = SessionOkMessage(
            user_id="u1",
            profile_id="p1",
            user_display_name="Test User",
        )
        dumped = msg.model_dump(exclude_none=True)
        assert dumped["type"] == "SESSION_OK"
        assert dumped["userId"] == "u1"
        assert dumped["userDisplayName"] == "Test User"


class TestAuthorizationRequiredMessage:
    def test_includes_receiver_state(self) -> None:
        rs = ViaplayReceiverState(
            status="AUTHORIZATION_REQUIRED",
            user_code="ABC123",
            authorization_url="https://viaplay.com/activate?userCode=ABC123",
        )
        msg = AuthorizationRequiredMessage(receiver_state=rs)
        dumped = msg.model_dump(exclude_none=True)
        assert dumped["type"] == "AUTHORIZATION_REQUIRED"
        assert dumped["receiverState"]["userCode"] == "ABC123"


class TestViaplayRequestDiscriminator:
    def test_dispatches_setup_info(self) -> None:
        raw = {"type": "SETUP_INFO", "contentRoot": "https://example.com"}
        msg = viaplay_request_adapter.validate_python(raw)
        assert isinstance(msg, SetupInfo)
        assert msg.content_root == "https://example.com"

    def test_dispatches_authorization_done(self) -> None:
        raw = {"type": "AUTHORIZATION_DONE"}
        msg = viaplay_request_adapter.validate_python(raw)
        assert isinstance(msg, AuthorizationDone)
