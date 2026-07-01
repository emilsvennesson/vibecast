"""Models for the TV4 Play app and HTTP APIs."""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field

_API_MODEL_CONFIG = ConfigDict(extra="allow", populate_by_name=True)


class Tv4AuthTokenResponse(BaseModel):
    """Response from ``auth.tv4.a2d.tv/v2/auth/token``."""

    model_config = _API_MODEL_CONFIG

    access_token: str = ""
    refresh_token: str = ""
    token_type: str = ""
    expires_in: int | None = None


class Tv4Image(BaseModel):
    """Image object returned by the TV4 GraphQL API."""

    model_config = _API_MODEL_CONFIG

    source: str | None = None


class Tv4Images(BaseModel):
    """Known TV4 image variants."""

    model_config = _API_MODEL_CONFIG

    main_16x9: Tv4Image | None = Field(default=None, alias="main16x9")
    poster_2x3: Tv4Image | None = Field(default=None, alias="poster2x3")
    logo: Tv4Image | None = None


class Tv4Synopsis(BaseModel):
    """Synopsis text returned by GraphQL."""

    model_config = _API_MODEL_CONFIG
    medium: str | None = None


class Tv4IsoDate(BaseModel):
    """GraphQL ISO date wrapper."""

    model_config = _API_MODEL_CONFIG
    iso_string: str | None = Field(default=None, alias="isoString")


class Tv4Series(BaseModel):
    """Series metadata embedded on episode responses."""

    model_config = _API_MODEL_CONFIG
    id: str | None = None
    title: str | None = None
    images: Tv4Images | None = None


class Tv4Media(BaseModel):
    """GraphQL media union fields used by the receiver."""

    model_config = _API_MODEL_CONFIG

    typename: str = Field(default="", alias="__typename")
    title: str | None = None
    extended_title: str | None = Field(default=None, alias="extendedTitle")
    channel_type: str | None = Field(default=None, alias="channelType")
    is_drm_protected: bool = Field(default=False, alias="isDrmProtected")
    images: Tv4Images | None = None
    synopsis: Tv4Synopsis | None = None
    live_event_end: Tv4IsoDate | None = Field(default=None, alias="liveEventEnd")
    series: Tv4Series | None = None


class Tv4GraphqlData(BaseModel):
    """Top-level GraphQL data object."""

    model_config = _API_MODEL_CONFIG
    media: Tv4Media | None = None


class Tv4GraphqlResponse(BaseModel):
    """GraphQL response envelope."""

    model_config = _API_MODEL_CONFIG
    data: Tv4GraphqlData | None = None


class Tv4PlaybackMetadata(BaseModel):
    """Playback metadata returned by ``playback2``."""

    model_config = _API_MODEL_CONFIG

    title: str | None = None
    series_title: str | None = Field(default=None, alias="seriesTitle")
    description: str | None = None
    type: str | None = None
    duration: float | None = None
    is_live: bool = Field(default=False, alias="isLive")
    is_drm_protected: bool = Field(default=False, alias="isDrmProtected")
    image: str | None = None
    video_id: str | None = Field(default=None, alias="videoId")


class Tv4PlaybackLicense(BaseModel):
    """Widevine license information returned by ``playback2``."""

    model_config = _API_MODEL_CONFIG

    castlabs_asset_id: str | None = Field(default=None, alias="castlabsAssetId")
    castlabs_server: str | None = Field(default=None, alias="castlabsServer")
    castlabs_token: str | None = Field(default=None, alias="castlabsToken")
    castlabs_cert_server: str | None = Field(default=None, alias="castlabsCertServer")
    license_expiry: str | None = Field(default=None, alias="licenseExpiry")
    type: str | None = None


class Tv4Subtitle(BaseModel):
    """Subtitle or text-track metadata."""

    model_config = _API_MODEL_CONFIG
    type: str | None = None
    url: str | None = None
    language: str | None = None
    name: str | None = None


class Tv4Thumbnail(BaseModel):
    """Thumbnail sprite metadata."""

    model_config = _API_MODEL_CONFIG
    type: str | None = None
    url: str | None = None
    width: int | None = None
    height: int | None = None


class Tv4PlaybackItem(BaseModel):
    """Resolved playback item from ``playback2``."""

    model_config = _API_MODEL_CONFIG

    type: str | None = None
    state: str | None = None
    manifest_url: str | None = Field(default=None, alias="manifestUrl")
    access_url: str | None = Field(default=None, alias="accessUrl")
    access_url_type: str | None = Field(default=None, alias="accessUrlType")
    origin_url: str | None = Field(default=None, alias="originUrl")
    license: Tv4PlaybackLicense | None = None
    subtitles: list[Tv4Subtitle] = []
    subs: list[Tv4Subtitle] = []
    thumbnails: list[Tv4Thumbnail] = []


class Tv4PlaybackCapabilities(BaseModel):
    """Playback capabilities returned by ``playback2``."""

    model_config = _API_MODEL_CONFIG
    pause: bool = True
    seek: bool = True
    stream_switch: bool = False


class Tv4PlaybackResponse(BaseModel):
    """Top-level playback response."""

    model_config = _API_MODEL_CONFIG

    id: str = ""
    metadata: Tv4PlaybackMetadata | None = None
    playback_item: Tv4PlaybackItem | None = Field(default=None, alias="playbackItem")
    capabilities: Tv4PlaybackCapabilities = Tv4PlaybackCapabilities()
    session_id: str | None = Field(default=None, alias="sessionId")
    user_tier: str | None = Field(default=None, alias="userTier")


__all__ = [
    "Tv4AuthTokenResponse",
    "Tv4GraphqlResponse",
    "Tv4Media",
    "Tv4PlaybackResponse",
]
