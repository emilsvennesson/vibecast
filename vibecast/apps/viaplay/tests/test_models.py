"""Tests for Viaplay Pydantic models (Cast namespace + API responses)."""

from __future__ import annotations

from typing import Any

from vibecast.apps.viaplay._models import (
    AuthorizationDone,
    AuthorizationRequiredMessage,
    AuthorizedPollLinks,
    DeviceAuthLinks,
    EmbeddedMedia,
    GotoIdle,
    LoginUserData,
    PosDurMessage,
    ReceiverStateMessage,
    SessionLinks,
    SessionOkMessage,
    SessionUser,
    SetupInfo,
    StreamResponseEmbedded,
    StreamResponseLinks,
    ViaplayAuthorizedPollResponse,
    ViaplayDeviceAuthResponse,
    ViaplayLoginResponse,
    ViaplayReceiverState,
    ViaplaySessionResponse,
    ViaplayStreamResponse,
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

    def test_parses_success_flag(self) -> None:
        msg = AuthorizationDone.model_validate(
            {
                "type": "AUTHORIZATION_DONE",
                "success": False,
                "userId": "u1",
                "profileId": "p1",
            }
        )
        assert msg.success is False
        assert msg.user_id == "u1"
        assert msg.profile_id == "p1"


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

    def test_subtitle_enabled_can_be_style_dict(self) -> None:
        rs = ViaplayReceiverState.model_validate(
            {
                "status": "CASTING",
                "subtitles": {
                    "activeLanguageCode": "sv",
                    "enabled": {
                        "fontSize": 1,
                        "backgroundColor": "#0000007F",
                        "foregroundColor": "#FFFFFFFF",
                    },
                },
            }
        )
        msg = ReceiverStateMessage(receiver_state=rs)
        dumped = msg.model_dump(exclude_none=True)
        enabled = dumped["receiverState"]["subtitles"]["enabled"]
        assert isinstance(enabled, dict)
        assert enabled["fontSize"] == 1


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
        msg = AuthorizationRequiredMessage(
            authorization_url="https://viaplay.com/activate?userCode=ABC123",
            receiver_state=rs,
        )
        dumped = msg.model_dump(exclude_none=True)
        assert dumped["type"] == "AUTHORIZATION_REQUIRED"
        assert (
            dumped["authorizationUrl"] == "https://viaplay.com/activate?userCode=ABC123"
        )
        assert dumped["receiverState"]["userCode"] == "ABC123"


class TestPosDurMessage:
    def test_serialization(self) -> None:
        rs = ViaplayReceiverState(status="CASTING")
        msg = PosDurMessage(position=12, duration=2535, receiver_state=rs)
        dumped = msg.model_dump(exclude_none=True)
        assert dumped["type"] == "POSDUR"
        assert dumped["position"] == 12
        assert dumped["duration"] == 2535
        assert dumped["receiverState"]["status"] == "CASTING"


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

    def test_dispatches_goto_idle(self) -> None:
        raw = {"type": "GOTO_IDLE", "userId": "u1", "profileId": "p1"}
        msg = viaplay_request_adapter.validate_python(raw)
        assert isinstance(msg, GotoIdle)
        assert msg.user_id == "u1"


# ---------------------------------------------------------------------------
# API response models
# ---------------------------------------------------------------------------


class TestViaplayStreamResponse:
    def test_parses_full_stream_response(self) -> None:
        raw: dict[str, Any] = {
            "duration": 0,
            "product": {
                "content": {"title": "Poland Darts Open", "type": "live"},
                "streamType": "live",
                "productType": "sports",
            },
            "_links": {
                "viaplay:encryptedPlaylist": {
                    "href": "https://cdn.example.com/manifest.mpd",
                    "embeddedSubtitles": False,
                    "streamingFormat": "Dash",
                },
                "viaplay:widevineLicense": {
                    "href": "https://drm.example.com/license",
                    "releasePid": "abc123",
                },
                "viaplay:fallbackMedia": [
                    {
                        "href": "https://cdn2.example.com/manifest.mpd",
                        "streamingFormat": "Dash",
                    },
                ],
            },
        }
        resp = ViaplayStreamResponse.model_validate(raw)

        assert resp.product is not None
        assert resp.product.content.title == "Poland Darts Open"
        assert resp.product.stream_type == "live"
        assert resp.links is not None
        assert resp.links.encrypted_playlist is not None
        assert (
            resp.links.encrypted_playlist.href == "https://cdn.example.com/manifest.mpd"
        )
        assert resp.links.widevine_license is not None
        assert resp.links.widevine_license.href == "https://drm.example.com/license"
        assert resp.links.widevine_license.release_pid == "abc123"
        assert len(resp.links.fallback_media) == 1

    def test_parses_top_level_content_url(self) -> None:
        raw: dict[str, Any] = {
            "contentUrl": "https://cdn.example.com/video.mp4",
            "contentType": "video/mp4",
        }
        resp = ViaplayStreamResponse.model_validate(raw)
        assert resp.content_url == "https://cdn.example.com/video.mp4"
        assert resp.content_type == "video/mp4"

    def test_tolerates_missing_fields(self) -> None:
        resp = ViaplayStreamResponse.model_validate({})
        assert resp.product is None
        assert resp.links is None
        assert resp.content_url is None

    def test_extra_fields_preserved(self) -> None:
        raw: dict[str, Any] = {
            "serverTime": "2026-02-20T21:16:18.100Z",
            "contentUrl": "https://x",
        }
        resp = ViaplayStreamResponse.model_validate(raw)
        assert resp.content_url == "https://x"


class TestStreamResponseLinks:
    def test_parses_colon_prefixed_keys(self) -> None:
        raw: dict[str, Any] = {
            "viaplay:encryptedPlaylist": {
                "href": "https://cdn/manifest.mpd",
                "streamingFormat": "Dash",
            },
            "viaplay:license": {
                "href": "https://drm/license",
                "releasePid": "pid1",
            },
        }
        links = StreamResponseLinks.model_validate(raw)
        assert links.encrypted_playlist is not None
        assert links.encrypted_playlist.href == "https://cdn/manifest.mpd"
        assert links.license_link is not None
        assert links.license_link.release_pid == "pid1"

    def test_empty_links(self) -> None:
        links = StreamResponseLinks.model_validate({})
        assert links.encrypted_playlist is None
        assert links.playlist is None
        assert links.fallback_media == []


class TestViaplayLoginResponse:
    def test_parses_successful_login(self) -> None:
        raw: dict[str, Any] = {
            "success": True,
            "userData": {
                "userId": "u1",
                "firstName": "Alice",
                "lastName": "Smith",
                "accessToken": "jwt-token-123",
            },
            "code": 1,
        }
        resp = ViaplayLoginResponse.model_validate(raw)
        assert resp.success is True
        assert resp.user_data is not None
        assert resp.user_data.user_id == "u1"
        assert resp.user_data.first_name == "Alice"
        assert resp.user_data.access_token == "jwt-token-123"

    def test_parses_failed_login(self) -> None:
        raw: dict[str, Any] = {"success": False, "code": 0}
        resp = ViaplayLoginResponse.model_validate(raw)
        assert resp.success is False
        assert resp.user_data is None

    def test_login_user_data_defaults(self) -> None:
        data = LoginUserData.model_validate({})
        assert data.user_id == ""
        assert data.first_name == ""
        assert data.access_token == ""


# ---------------------------------------------------------------------------
# Embedded media models
# ---------------------------------------------------------------------------


class TestEmbeddedMedia:
    def test_parses_content_fields(self) -> None:
        raw: dict[str, Any] = {
            "contentUrl": "https://cdn.example.com/manifest.mpd",
            "contentType": "application/dash+xml",
        }
        media = EmbeddedMedia.model_validate(raw)
        assert media.content_url == "https://cdn.example.com/manifest.mpd"
        assert media.content_type == "application/dash+xml"

    def test_defaults_to_none(self) -> None:
        media = EmbeddedMedia.model_validate({})
        assert media.content_url is None
        assert media.content_type is None


class TestStreamResponseEmbedded:
    def test_parses_viaplay_media(self) -> None:
        raw: dict[str, Any] = {
            "viaplay:media": {
                "contentUrl": "https://cdn.example.com/video.mpd",
                "contentType": "video/mp4",
            },
        }
        embedded = StreamResponseEmbedded.model_validate(raw)
        assert embedded.media is not None
        assert embedded.media.content_url == "https://cdn.example.com/video.mpd"

    def test_missing_media(self) -> None:
        embedded = StreamResponseEmbedded.model_validate({})
        assert embedded.media is None


class TestStreamResponseWithEmbedded:
    def test_parses_embedded_field(self) -> None:
        raw: dict[str, Any] = {
            "_embedded": {
                "viaplay:media": {
                    "contentUrl": "https://cdn.example.com/manifest.mpd",
                    "contentType": "application/dash+xml",
                },
            },
        }
        resp = ViaplayStreamResponse.model_validate(raw)
        assert resp.embedded is not None
        assert resp.embedded.media is not None
        assert resp.embedded.media.content_url == "https://cdn.example.com/manifest.mpd"

    def test_missing_embedded(self) -> None:
        resp = ViaplayStreamResponse.model_validate({})
        assert resp.embedded is None


# ---------------------------------------------------------------------------
# Session check response models
# ---------------------------------------------------------------------------


class TestSessionUser:
    def test_parses_user_fields(self) -> None:
        raw: dict[str, Any] = {
            "userId": "u1",
            "firstName": "Alice",
            "lastName": "Smith",
        }
        user = SessionUser.model_validate(raw)
        assert user.user_id == "u1"
        assert user.first_name == "Alice"
        assert user.last_name == "Smith"

    def test_defaults(self) -> None:
        user = SessionUser.model_validate({})
        assert user.user_id == ""
        assert user.first_name == ""


class TestSessionLinks:
    def test_parses_known_links(self) -> None:
        raw: dict[str, Any] = {
            "viaplay:persistentLogin": {"href": "https://login/pl"},
            "viaplay:tokenLogin": {"href": "https://login/tl"},
            "viaplay:deviceAuthorization": {"href": "https://login/da"},
        }
        links = SessionLinks.model_validate(raw)
        assert links.persistent_login is not None
        assert links.persistent_login.href == "https://login/pl"
        assert links.token_login is not None
        assert links.token_login.href == "https://login/tl"
        assert links.device_authorization is not None
        assert links.device_authorization.href == "https://login/da"

    def test_empty_links(self) -> None:
        links = SessionLinks.model_validate({})
        assert links.persistent_login is None
        assert links.token_login is None
        assert links.device_authorization is None


class TestViaplaySessionResponse:
    def test_parses_user_and_links(self) -> None:
        raw: dict[str, Any] = {
            "user": {
                "userId": "u1",
                "firstName": "Alice",
                "lastName": "Smith",
            },
            "_links": {
                "viaplay:persistentLogin": {"href": "https://login/pl"},
            },
        }
        resp = ViaplaySessionResponse.model_validate(raw)
        assert resp.user is not None
        assert resp.user.user_id == "u1"
        assert resp.links is not None
        assert resp.links.persistent_login is not None
        assert resp.links.persistent_login.href == "https://login/pl"

    def test_no_user(self) -> None:
        resp = ViaplaySessionResponse.model_validate({})
        assert resp.user is None
        assert resp.links is None


# ---------------------------------------------------------------------------
# Device authorization response models
# ---------------------------------------------------------------------------


class TestDeviceAuthLinks:
    def test_parses_links(self) -> None:
        raw: dict[str, Any] = {
            "viaplay:activate": {"href": "https://viaplay.com/activate"},
            "viaplay:authorized": {"href": "https://login/authorized"},
        }
        links = DeviceAuthLinks.model_validate(raw)
        assert links.activate is not None
        assert links.activate.href == "https://viaplay.com/activate"
        assert links.authorized is not None
        assert links.authorized.href == "https://login/authorized"


class TestViaplayDeviceAuthResponse:
    def test_parses_full_response(self) -> None:
        raw: dict[str, Any] = {
            "userCode": "ABCD1234",
            "deviceToken": "dt-999",
            "_links": {
                "viaplay:activate": {"href": "https://viaplay.com/activate"},
                "viaplay:authorized": {"href": "https://login/authorized"},
            },
        }
        resp = ViaplayDeviceAuthResponse.model_validate(raw)
        assert resp.user_code == "ABCD1234"
        assert resp.device_token == "dt-999"
        assert resp.links is not None
        assert resp.links.activate is not None

    def test_defaults(self) -> None:
        resp = ViaplayDeviceAuthResponse.model_validate({})
        assert resp.user_code == ""
        assert resp.device_token == ""
        assert resp.links is None


# ---------------------------------------------------------------------------
# Authorized poll response models
# ---------------------------------------------------------------------------


class TestAuthorizedPollLinks:
    def test_parses_persistent_login(self) -> None:
        raw: dict[str, Any] = {
            "viaplay:persistentLogin": {"href": "https://login/pl"},
        }
        links = AuthorizedPollLinks.model_validate(raw)
        assert links.persistent_login is not None
        assert links.persistent_login.href == "https://login/pl"


class TestViaplayAuthorizedPollResponse:
    def test_parses_with_links(self) -> None:
        raw: dict[str, Any] = {
            "_links": {
                "viaplay:persistentLogin": {"href": "https://login/pl"},
            },
        }
        resp = ViaplayAuthorizedPollResponse.model_validate(raw)
        assert resp.links is not None
        assert resp.links.persistent_login is not None

    def test_empty(self) -> None:
        resp = ViaplayAuthorizedPollResponse.model_validate({})
        assert resp.links is None
