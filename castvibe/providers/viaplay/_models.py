"""Pydantic models for the Viaplay provider.

Cast namespace models (``urn:x-cast:tv.viaplay.chromecast``):

Inbound (sender -> receiver):
    SETUP_INFO, AUTHORIZATION_DONE

Outbound (receiver -> sender):
    SESSION_OK, RECEIVER_STATE, AUTHORIZATION_REQUIRED

API response models:
    ViaplayStreamResponse — stream resolution endpoint
    ViaplayLoginResponse  — persistent / token login endpoint
"""

from __future__ import annotations

from typing import Annotated, Any, Literal

from pydantic import BaseModel, ConfigDict, Discriminator, Field, TypeAdapter

from castvibe._models._base import CastModel

# ---------------------------------------------------------------------------
# Shared sub-models (Cast namespace)
# ---------------------------------------------------------------------------


class SubtitleState(CastModel):
    """Subtitle configuration in receiver state."""

    active_language_code: str | None = None
    available_language_codes: list[str] = []
    enabled: bool | dict[str, Any] | None = True


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
    pne_in_progress: bool = False  # "Preview Next Episode" transition active
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
    success: bool = True
    user_id: str = ""
    profile_id: str = ""


class GotoIdle(CastModel):
    """``GOTO_IDLE`` — sender signals app should return to idle state."""

    type: Literal["GOTO_IDLE"] = "GOTO_IDLE"
    user_id: str = ""
    profile_id: str = ""


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
    authorization_url: str | None = None
    receiver_state: ViaplayReceiverState


class PosDurMessage(CastModel):
    """``POSDUR`` progress update used by Viaplay senders."""

    type: Literal["POSDUR"] = "POSDUR"
    position: int
    duration: int
    receiver_state: ViaplayReceiverState


# ---------------------------------------------------------------------------
# Discriminated union for inbound Viaplay messages
# ---------------------------------------------------------------------------

ViaplayRequest = Annotated[
    SetupInfo | AuthorizationDone | GotoIdle,
    Discriminator("type"),
]

viaplay_request_adapter: TypeAdapter[ViaplayRequest] = TypeAdapter(ViaplayRequest)


# ---------------------------------------------------------------------------
# Viaplay HTTP API response models
# ---------------------------------------------------------------------------
# These model the JSON returned by Viaplay's REST API — not Cast wire
# messages — so they use plain BaseModel with explicit aliases for HAL
# ``_links`` keys that contain colons (e.g. ``viaplay:encryptedPlaylist``).
# ---------------------------------------------------------------------------

_API_MODEL_CONFIG = ConfigDict(extra="allow", populate_by_name=True)


class StreamPlaylistLink(BaseModel):
    """HAL link for an encrypted playlist (DASH or HLS manifest)."""

    model_config = _API_MODEL_CONFIG

    href: str
    embedded_subtitles: bool = Field(default=False, alias="embeddedSubtitles")
    streaming_format: str = Field(default="", alias="streamingFormat")


class StreamLicenseLink(BaseModel):
    """HAL link for a DRM license server."""

    model_config = _API_MODEL_CONFIG

    href: str
    templated: bool = False
    release_pid: str = Field(default="", alias="releasePid")


class StreamFallbackLink(BaseModel):
    """HAL link for a CDN fallback stream."""

    model_config = _API_MODEL_CONFIG

    href: str
    streaming_format: str = Field(default="Dash", alias="streamingFormat")


class StreamHrefLink(BaseModel):
    """Minimal HAL link with only an ``href``."""

    model_config = _API_MODEL_CONFIG

    href: str


class StreamResponseLinks(BaseModel):
    """``_links`` section of a stream resolution response.

    Keys use the ``viaplay:`` prefix in the wire format, mapped to
    Python-friendly attribute names via explicit aliases.
    """

    model_config = _API_MODEL_CONFIG

    encrypted_playlist: StreamPlaylistLink | None = Field(
        default=None, alias="viaplay:encryptedPlaylist"
    )
    playlist: StreamHrefLink | None = Field(default=None, alias="viaplay:playlist")
    stream: StreamHrefLink | None = Field(default=None, alias="viaplay:stream")
    license_link: StreamLicenseLink | None = Field(
        default=None, alias="viaplay:license"
    )
    widevine_license: StreamLicenseLink | None = Field(
        default=None, alias="viaplay:widevineLicense"
    )
    fallback_media: list[StreamFallbackLink] = Field(
        default_factory=list, alias="viaplay:fallbackMedia"
    )


class StreamProductContent(BaseModel):
    """``product.content`` in a stream resolution response."""

    model_config = _API_MODEL_CONFIG

    title: str = ""
    type: str = ""


class StreamProduct(BaseModel):
    """``product`` in a stream resolution response."""

    model_config = _API_MODEL_CONFIG

    content: StreamProductContent = StreamProductContent()
    stream_type: str = Field(default="", alias="streamType")
    product_type: str = Field(default="", alias="productType")


class EmbeddedMedia(BaseModel):
    """Media object inside ``_embedded.viaplay:media``."""

    model_config = _API_MODEL_CONFIG

    content_url: str | None = Field(default=None, alias="contentUrl")
    content_type: str | None = Field(default=None, alias="contentType")


class StreamResponseEmbedded(BaseModel):
    """``_embedded`` section of a stream resolution response."""

    model_config = _API_MODEL_CONFIG

    media: EmbeddedMedia | None = Field(default=None, alias="viaplay:media")


class ViaplayStreamResponse(BaseModel):
    """Top-level response from the Viaplay stream resolution API.

    Parsed from ``GET /api/stream/bymediaguid`` or play-URL endpoints.
    """

    model_config = _API_MODEL_CONFIG

    duration: float = 0
    product: StreamProduct | None = None
    content_url: str | None = Field(default=None, alias="contentUrl")
    content_type: str | None = Field(default=None, alias="contentType")
    streaming_format: str | None = Field(default=None, alias="streamingFormat")
    links: StreamResponseLinks | None = Field(default=None, alias="_links")
    embedded: StreamResponseEmbedded | None = Field(default=None, alias="_embedded")


# -- Login response ---------------------------------------------------------


class LoginUserData(BaseModel):
    """``userData`` section of a persistent/token login response."""

    model_config = _API_MODEL_CONFIG

    user_id: str = Field(default="", alias="userId")
    first_name: str = Field(default="", alias="firstName")
    last_name: str = Field(default="", alias="lastName")
    access_token: str = Field(default="", alias="accessToken")


class ViaplayLoginResponse(BaseModel):
    """Response from persistent login or token login endpoints."""

    model_config = _API_MODEL_CONFIG

    success: bool = False
    user_data: LoginUserData | None = Field(default=None, alias="userData")
    code: int = 0


# -- Session check response -------------------------------------------------


class SessionUser(BaseModel):
    """User data from a content-root session check response."""

    model_config = _API_MODEL_CONFIG

    user_id: str = Field(default="", alias="userId")
    first_name: str = Field(default="", alias="firstName")
    last_name: str = Field(default="", alias="lastName")


class SessionLinks(BaseModel):
    """``_links`` section of a session check response."""

    model_config = _API_MODEL_CONFIG

    persistent_login: StreamHrefLink | None = Field(
        default=None, alias="viaplay:persistentLogin"
    )
    token_login: StreamHrefLink | None = Field(default=None, alias="viaplay:tokenLogin")
    device_authorization: StreamHrefLink | None = Field(
        default=None, alias="viaplay:deviceAuthorization"
    )


class ViaplaySessionResponse(BaseModel):
    """Response from the content-root session check endpoint."""

    model_config = _API_MODEL_CONFIG

    user: SessionUser | None = None
    links: SessionLinks | None = Field(default=None, alias="_links")


# -- Device authorization response ------------------------------------------


class DeviceAuthLinks(BaseModel):
    """``_links`` in a device authorization response."""

    model_config = _API_MODEL_CONFIG

    activate: StreamHrefLink | None = Field(default=None, alias="viaplay:activate")
    authorized: StreamHrefLink | None = Field(default=None, alias="viaplay:authorized")


class ViaplayDeviceAuthResponse(BaseModel):
    """Response from the device authorization endpoint."""

    model_config = _API_MODEL_CONFIG

    user_code: str = Field(default="", alias="userCode")
    device_token: str = Field(default="", alias="deviceToken")
    verification_url: str = Field(default="", alias="verificationUrl")
    links: DeviceAuthLinks | None = Field(default=None, alias="_links")


# -- Authorized poll response -----------------------------------------------


class AuthorizedPollLinks(BaseModel):
    """``_links`` in an authorized poll response."""

    model_config = _API_MODEL_CONFIG

    persistent_login: StreamHrefLink | None = Field(
        default=None, alias="viaplay:persistentLogin"
    )


class ViaplayAuthorizedPollResponse(BaseModel):
    """Response from the authorized poll endpoint."""

    model_config = _API_MODEL_CONFIG

    links: AuthorizedPollLinks | None = Field(default=None, alias="_links")
