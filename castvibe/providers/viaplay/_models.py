"""Pydantic models for messages on the Viaplay custom namespace.

Namespace: ``urn:x-cast:tv.viaplay.chromecast``

Inbound (sender -> receiver):
    SETUP_INFO, AUTHORIZATION_DONE

Outbound (receiver -> sender):
    SESSION_OK, RECEIVER_STATE, AUTHORIZATION_REQUIRED
"""

from __future__ import annotations

from typing import Annotated, Any, Literal

from pydantic import Discriminator, TypeAdapter

from castvibe._models._base import CastModel

# ---------------------------------------------------------------------------
# Shared sub-models
# ---------------------------------------------------------------------------


class SubtitleState(CastModel):
    """Subtitle configuration in receiver state."""

    active_language_code: str | None = None
    available_language_codes: list[str] = []
    enabled: bool | None = None


class AudioTrackState(CastModel):
    """Audio track configuration in receiver state."""

    active_audio_track: str | None = None
    available_audio_tracks: list[str] = []


class UserProfile(CastModel):
    """User profile info embedded in receiver state."""

    id: str | None = None
    type: str = "unknown"


class ViaplayReceiverState(CastModel):
    """Full receiver state broadcast on the Viaplay namespace."""

    status: str = "IDLE"
    is_scrubbable: bool = True
    pne_in_progress: bool = False
    user_id: str | None = None
    user_profile: UserProfile | None = None
    user_consent: Any | None = None
    user_session_id: str | None = None
    user_display_name: str | None = None
    country_code: str = "se"
    receiver_name: str = ""
    receiver_language_code: str = "en"
    current_product_url: str | None = None
    loading_product_url: str | None = None
    authorization_url: str | None = None
    user_code: str | None = None
    subtitles: SubtitleState = SubtitleState()
    audio_tracks: AudioTrackState = AudioTrackState()
    intro: dict[str, Any] = {}
    recap: dict[str, Any] = {}
    tracking_debug: bool = False
    feature_flags: dict[str, Any] | list[str] = {}


# ---------------------------------------------------------------------------
# Inbound messages (sender -> receiver)
# ---------------------------------------------------------------------------


class SetupInfo(CastModel):
    """``SETUP_INFO`` — sent by the sender after connecting to the app transport."""

    type: Literal["SETUP_INFO"] = "SETUP_INFO"
    content_root: str = ""
    country_code: str = ""
    user_id: str = ""
    profile_id: str = ""
    receiver_name: str = ""
    receiver_language_code: str = "en"
    platform: str = ""
    feature_flags: list[str] | dict[str, Any] = []


class AuthorizationDone(CastModel):
    """``AUTHORIZATION_DONE`` — sender signals the user activated the device code."""

    type: Literal["AUTHORIZATION_DONE"] = "AUTHORIZATION_DONE"


# ---------------------------------------------------------------------------
# Outbound messages (receiver -> sender)
# ---------------------------------------------------------------------------


class ReceiverStateMessage(CastModel):
    """``RECEIVER_STATE`` broadcast on the Viaplay namespace."""

    type: Literal["RECEIVER_STATE"] = "RECEIVER_STATE"
    receiver_state: ViaplayReceiverState


class SessionOkMessage(CastModel):
    """``SESSION_OK`` broadcast when authentication succeeds."""

    type: Literal["SESSION_OK"] = "SESSION_OK"
    user_id: str | None = None
    profile_id: str | None = None
    user_display_name: str | None = None
    receiver_state: ViaplayReceiverState | None = None


class AuthorizationRequiredMessage(CastModel):
    """``AUTHORIZATION_REQUIRED`` broadcast when device-code auth is needed."""

    type: Literal["AUTHORIZATION_REQUIRED"] = "AUTHORIZATION_REQUIRED"
    receiver_state: ViaplayReceiverState


# ---------------------------------------------------------------------------
# Discriminated union for inbound Viaplay messages
# ---------------------------------------------------------------------------

ViaplayRequest = Annotated[
    SetupInfo | AuthorizationDone,
    Discriminator("type"),
]

viaplay_request_adapter: TypeAdapter[ViaplayRequest] = TypeAdapter(ViaplayRequest)
