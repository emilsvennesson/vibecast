"""Pydantic models for the Amazon Prime provider."""

from __future__ import annotations

from typing import Annotated, Any, Literal

from pydantic import BaseModel, ConfigDict, Discriminator, Field, TypeAdapter

from vibecast.provider import CastModel


class RegisterMessage(CastModel):
    """Prime sender registration message."""

    type: Literal["Register"] = "Register"
    marketplace_id: str | None = None
    message_protocol_version: int | None = None
    actor_id: str | None = None
    pre_authorized_link_code: str | None = None
    device_type_id: str | None = None
    device_id: str | None = None


class AmIRegisteredMessage(CastModel):
    """Prime sender registration check message."""

    type: Literal["AmIRegistered"] = "AmIRegistered"
    message_protocol_version: int | None = None
    device_id: str | None = None


class AmIRegisteredError(CastModel):
    """Error payload for ``AmIRegisteredResponse``."""

    code: str
    internal_name: str | None = None
    message: str | None = None
    is_fatal: bool = False


class AmIRegisteredResponseMessage(CastModel):
    """Prime registration check response."""

    type: Literal["AmIRegisteredResponse"] = "AmIRegisteredResponse"
    error: AmIRegisteredError | None = None


class ApplySettingsMessage(CastModel):
    """Prime sender settings update message."""

    type: Literal["ApplySettings"] = "ApplySettings"
    message_protocol_version: int | None = None
    device_id: str | None = None
    settings: dict[str, Any] | None = None


class PreloadEnvelope(CastModel):
    """Embedded playback envelope payload sent by Prime sender."""

    envelope: str
    correlation_id: str | None = None
    expiration: str | int | None = None


class PreloadMessage(CastModel):
    """Prime sender preload message."""

    type: Literal["Preload"] = "Preload"
    content_id: str | None = None
    device_id: str | None = None
    video_material_type: str | None = None
    current_time_sec: float | None = None
    initial_tracks: dict[str, Any] | None = None
    playback_envelope: PreloadEnvelope | None = None


PrimeMessage = Annotated[
    AmIRegisteredMessage | RegisterMessage | ApplySettingsMessage | PreloadMessage,
    Discriminator("type"),
]

prime_message_adapter: TypeAdapter[PrimeMessage] = TypeAdapter(PrimeMessage)


class RegisterResponseMessage(CastModel):
    """Prime registration response."""

    type: Literal["RegisterResponse"] = "RegisterResponse"


class ApplySettingsResponseMessage(CastModel):
    """Prime settings response."""

    type: Literal["ApplySettingsResponse"] = "ApplySettingsResponse"


class PreloadResponseMessage(CastModel):
    """Prime preload response."""

    type: Literal["PreloadResponse"] = "PreloadResponse"


_API_MODEL_CONFIG = ConfigDict(extra="allow", populate_by_name=True)


class AuthRegisterBearerToken(BaseModel):
    """Bearer token object from ``/auth/register``."""

    model_config = _API_MODEL_CONFIG

    access_token: str = ""
    refresh_token: str = ""
    expires_in: int | str | None = None


class AuthRegisterTokens(BaseModel):
    """Token container from ``/auth/register``."""

    model_config = _API_MODEL_CONFIG

    bearer: AuthRegisterBearerToken | None = None


class AuthRegisterSuccess(BaseModel):
    """Success payload from ``/auth/register``."""

    model_config = _API_MODEL_CONFIG

    tokens: AuthRegisterTokens | None = None


class AuthRegisterResponseContainer(BaseModel):
    """Envelope payload from ``/auth/register``."""

    model_config = _API_MODEL_CONFIG

    success: AuthRegisterSuccess | None = None


class AuthRegisterResponse(BaseModel):
    """Top-level ``/auth/register`` response payload."""

    model_config = _API_MODEL_CONFIG

    response: AuthRegisterResponseContainer | None = None


class TokenValue(BaseModel):
    """Token wrapper used by ``/auth/token`` response."""

    model_config = _API_MODEL_CONFIG

    token: str = ""


class ActorAccessToken(TokenValue):
    """Actor access token from ``/auth/token`` response."""

    expires_in: int | None = Field(default=None, alias="expires_in")


class AuthTokenDeviceToken(BaseModel):
    """Device token entry from ``/auth/token`` response."""

    model_config = _API_MODEL_CONFIG

    actor_access_token: ActorAccessToken | None = Field(
        default=None, alias="actor_access_token"
    )
    actor_refresh_token: TokenValue | None = Field(
        default=None, alias="actor_refresh_token"
    )


class AuthTokenResponse(BaseModel):
    """Top-level ``/auth/token`` response payload."""

    model_config = _API_MODEL_CONFIG

    device_tokens: list[AuthTokenDeviceToken] = Field(
        default_factory=list,
        alias="device_tokens",
    )


class RefreshedPlaybackExperience(BaseModel):
    """Playback experience payload returned by envelope refresh API."""

    model_config = _API_MODEL_CONFIG

    correlation_id: str | None = Field(default=None, alias="correlationId")
    playback_envelope: str | None = Field(default=None, alias="playbackEnvelope")


class RefreshedEnvelopeItem(BaseModel):
    """Per-title response item for envelope refresh API."""

    model_config = _API_MODEL_CONFIG

    playback_experience: RefreshedPlaybackExperience | None = Field(
        default=None,
        alias="playbackExperience",
    )


class RefreshedEnvelopeResponse(BaseModel):
    """Top-level envelope refresh response payload."""

    model_config = _API_MODEL_CONFIG

    response: dict[str, RefreshedEnvelopeItem] = Field(default_factory=dict)


class AccessTokensPayload(BaseModel):
    """Edge access-token metadata for one playback URL set."""

    model_config = _API_MODEL_CONFIG

    initial_access_token: str | None = Field(default=None, alias="initialAccessToken")
    initial_access_token_seconds_of_validity: int | None = Field(
        default=None,
        alias="initialAccessTokenSecondsOfValidity",
    )
    access_token_get_url: str | None = Field(default=None, alias="accessTokenGetUrl")


class EdgeDeliveryAuthorizationPayload(BaseModel):
    """Authorization payload for one playback URL set."""

    model_config = _API_MODEL_CONFIG

    authorization_scheme: str | None = Field(default=None, alias="authorizationScheme")
    access_tokens: AccessTokensPayload | None = Field(
        default=None,
        alias="accessTokens",
    )


class PlaybackUrlSetPayload(BaseModel):
    """One playback URL set entry returned by playback resources API."""

    model_config = _API_MODEL_CONFIG

    url_set_id: str = Field(alias="urlSetId")
    url: str
    edge_delivery_authorization: EdgeDeliveryAuthorizationPayload | None = Field(
        default=None,
        alias="edgeDeliveryAuthorization",
    )


class PlaybackUrlsPayload(BaseModel):
    """Playback URL set container from playback resources API."""

    model_config = _API_MODEL_CONFIG

    default_url_set_id: str | None = Field(default=None, alias="defaultUrlSetId")
    default_audio_track_id: str | None = Field(
        default=None, alias="defaultAudioTrackId"
    )
    url_sets: list[PlaybackUrlSetPayload] = Field(default_factory=list, alias="urlSets")


class VodPlaybackUrlsResult(BaseModel):
    """Result payload for ``vodPlaybackUrls`` section."""

    model_config = _API_MODEL_CONFIG

    playback_urls: PlaybackUrlsPayload | None = Field(
        default=None, alias="playbackUrls"
    )


class VodPlaybackUrlsSection(BaseModel):
    """``vodPlaybackUrls`` wrapper section."""

    model_config = _API_MODEL_CONFIG

    result: VodPlaybackUrlsResult | None = None


class SessionizationPayload(BaseModel):
    """Sessionization payload from playback resources API."""

    model_config = _API_MODEL_CONFIG

    session_handoff_token: str | None = Field(default=None, alias="sessionHandoffToken")


class VodPlaybackResourcesResponse(BaseModel):
    """Top-level playback resources response payload."""

    model_config = _API_MODEL_CONFIG

    sessionization: SessionizationPayload | None = None
    vod_playback_urls: VodPlaybackUrlsSection | None = Field(
        default=None,
        alias="vodPlaybackUrls",
    )


class WidevineLicensePayload(BaseModel):
    """Widevine license payload returned by Prime DRM endpoint."""

    model_config = _API_MODEL_CONFIG

    license: str = ""


class WidevineLicenseResponse(BaseModel):
    """Top-level Widevine license response payload."""

    model_config = _API_MODEL_CONFIG

    widevine_license: WidevineLicensePayload | None = Field(
        default=None,
        alias="widevineLicense",
    )


__all__ = [
    "AmIRegisteredError",
    "AmIRegisteredMessage",
    "AmIRegisteredResponseMessage",
    "ActorAccessToken",
    "ApplySettingsMessage",
    "ApplySettingsResponseMessage",
    "AuthRegisterResponse",
    "AuthTokenResponse",
    "PlaybackUrlSetPayload",
    "PreloadMessage",
    "PreloadResponseMessage",
    "RefreshedEnvelopeResponse",
    "RegisterMessage",
    "RegisterResponseMessage",
    "VodPlaybackResourcesResponse",
    "WidevineLicenseResponse",
    "prime_message_adapter",
]
