"""Typed response models for SVT Play HTTP endpoints."""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict, Field

_API_MODEL_CONFIG = ConfigDict(extra="allow", populate_by_name=True)


class SvtVideoReference(BaseModel):
    """Single media reference entry from ``video.svt.se`` responses."""

    model_config = _API_MODEL_CONFIG

    url: str
    resolve: str | None = None
    format: str | None = None


class SvtVariant(BaseModel):
    """Variant-specific media references (default/audio-described/sign-language)."""

    model_config = _API_MODEL_CONFIG

    video_references: list[SvtVideoReference] = Field(
        default_factory=list,
        alias="videoReferences",
    )


class SvtVideoResponse(BaseModel):
    """Top-level response model for ``GET https://video.svt.se/video/{id}``."""

    model_config = _API_MODEL_CONFIG

    svt_id: str = Field(alias="svtId")
    program_title: str | None = Field(default=None, alias="programTitle")
    episode_title: str | None = Field(default=None, alias="episodeTitle")
    content_duration: float | None = Field(default=None, alias="contentDuration")
    video_references: list[SvtVideoReference] = Field(
        default_factory=list,
        alias="videoReferences",
    )
    variants: dict[str, SvtVariant | None] = Field(default_factory=dict)


class SvtResolveResponse(BaseModel):
    """Response model for ``switcher.cdn.svt.se/resolve/*`` endpoints."""

    model_config = _API_MODEL_CONFIG

    location: str


__all__ = [
    "SvtResolveResponse",
    "SvtVariant",
    "SvtVideoReference",
    "SvtVideoResponse",
]
