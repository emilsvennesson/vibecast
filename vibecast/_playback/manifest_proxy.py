"""Manifest proxy request models and normalization transforms."""

from __future__ import annotations

import re
import xml.etree.ElementTree as ET
from copy import deepcopy
from dataclasses import dataclass, field
from enum import StrEnum
from typing import Protocol
from urllib.parse import urljoin, urlsplit, urlunsplit

_MAX_PATTERN_REPEAT = 20_000
_HLS_URI_ATTR_RE = re.compile(r'URI="([^"]+)"')


class ManifestKind(StrEnum):
    """Known manifest container types handled by the proxy."""

    DASH = "dash"
    HLS = "hls"
    UNKNOWN = "unknown"


@dataclass(slots=True, frozen=True)
class ManifestProxyRequest:
    """Manifest request received by the player server proxy route."""

    session_id: str
    route_id: str
    method: str = "GET"
    headers: dict[str, str] = field(default_factory=dict)


@dataclass(slots=True, frozen=True)
class ManifestProxyResponse:
    """Manifest response returned by the coordinator to player server."""

    body: bytes
    content_type: str
    status: int = 200
    headers: dict[str, str] = field(default_factory=dict)


@dataclass(slots=True, frozen=True)
class ManifestTransformContext:
    """Context passed to manifest transformers."""

    kind: ManifestKind
    upstream_url: str
    content_type: str
    app_key: str | None = None


class ManifestTransformer(Protocol):
    """Protocol for manifest text transforms."""

    def applies(self, context: ManifestTransformContext) -> bool: ...

    def transform(self, manifest: str, context: ManifestTransformContext) -> str: ...


class _DashSegmentTimelinePatternTransformer:
    def applies(self, context: ManifestTransformContext) -> bool:
        return context.kind is ManifestKind.DASH

    def transform(self, manifest: str, context: ManifestTransformContext) -> str:
        _ = context
        root = _parse_xml(manifest)
        if root is None:
            return manifest

        changed = False
        for node_segment_timeline in root.iter():
            if _local_name(node_segment_timeline.tag) != "SegmentTimeline":
                continue

            expanded_children: list[ET.Element] = []
            node_changed = False
            for child in list(node_segment_timeline):
                if _local_name(child.tag) != "Pattern":
                    expanded_children.append(child)
                    continue

                expanded_pattern = _expand_pattern(child)
                if expanded_pattern is None:
                    expanded_children.append(child)
                    continue

                expanded_children.extend(expanded_pattern)
                node_changed = True

            if not node_changed:
                continue

            node_segment_timeline[:] = expanded_children
            changed = True

        if not changed:
            return manifest
        return _serialize_xml(root)


class _DashBaseUrlTransformer:
    def applies(self, context: ManifestTransformContext) -> bool:
        return context.kind is ManifestKind.DASH

    def transform(self, manifest: str, context: ManifestTransformContext) -> str:
        root = _parse_xml(manifest)
        if root is None:
            return manifest

        base_url = _directory_base_url(context.upstream_url)
        root_base_urls = [
            child for child in list(root) if _local_name(child.tag) == "BaseURL"
        ]

        changed = False
        if root_base_urls:
            for node_base_url in root_base_urls:
                text = (node_base_url.text or "").strip()
                if not text or _is_absolute_url(text):
                    continue
                node_base_url.text = urljoin(base_url, text)
                changed = True
        else:
            node_base_url = ET.Element(_qualified_tag(root.tag, "BaseURL"))
            node_base_url.text = base_url
            root.insert(0, node_base_url)
            changed = True

        if not changed:
            return manifest
        return _serialize_xml(root)


class _HlsAbsoluteUriTransformer:
    def applies(self, context: ManifestTransformContext) -> bool:
        return context.kind is ManifestKind.HLS

    def transform(self, manifest: str, context: ManifestTransformContext) -> str:
        base_url = context.upstream_url
        changed = False
        out_lines: list[str] = []

        for line in manifest.splitlines(keepends=True):
            line_body, line_ending = _split_line_ending(line)
            stripped = line_body.strip()

            if not stripped:
                out_lines.append(line)
                continue

            if stripped.startswith("#"):
                rewritten = _HLS_URI_ATTR_RE.sub(
                    lambda match: _rewrite_hls_uri_attr(match, base_url),
                    line_body,
                )
                if rewritten != line_body:
                    changed = True
                out_lines.append(f"{rewritten}{line_ending}")
                continue

            rewritten_uri = _absolutize_uri(line_body.strip(), base_url)
            if rewritten_uri != line_body.strip():
                changed = True
            out_lines.append(f"{rewritten_uri}{line_ending}")

        if not changed:
            return manifest
        return "".join(out_lines)


_TRANSFORMERS: tuple[ManifestTransformer, ...] = (
    _DashSegmentTimelinePatternTransformer(),
    _DashBaseUrlTransformer(),
    _HlsAbsoluteUriTransformer(),
)


def infer_manifest_kind(content_type: str | None, url: str) -> ManifestKind:
    """Infer manifest kind from URL and media type hints."""
    lowered_content_type = (content_type or "").lower()
    lowered_path = urlsplit(url).path.lower()

    if "dash" in lowered_content_type or lowered_path.endswith(".mpd"):
        return ManifestKind.DASH
    if "mpegurl" in lowered_content_type or lowered_path.endswith(".m3u8"):
        return ManifestKind.HLS
    return ManifestKind.UNKNOWN


def default_manifest_content_type(kind: ManifestKind) -> str:
    """Return default content type for manifest kind."""
    if kind is ManifestKind.DASH:
        return "application/dash+xml"
    if kind is ManifestKind.HLS:
        return "application/vnd.apple.mpegurl"
    return "application/octet-stream"


def manifest_route_suffix(kind: ManifestKind) -> str:
    """Return deterministic proxy route filename suffix for one kind."""
    if kind is ManifestKind.DASH:
        return ".mpd"
    if kind is ManifestKind.HLS:
        return ".m3u8"
    return ".manifest"


def normalize_manifest_bytes(
    body: bytes,
    *,
    upstream_url: str,
    content_type: str | None,
    app_key: str | None,
) -> tuple[bytes, str]:
    """Normalize one manifest body and return bytes + content type."""
    kind = infer_manifest_kind(content_type, upstream_url)
    resolved_content_type = content_type or default_manifest_content_type(kind)

    if kind is ManifestKind.UNKNOWN:
        return body, resolved_content_type

    manifest_text = body.decode("utf-8", errors="replace")
    context = ManifestTransformContext(
        kind=kind,
        upstream_url=upstream_url,
        content_type=resolved_content_type,
        app_key=app_key,
    )

    for transformer in _TRANSFORMERS:
        if not transformer.applies(context):
            continue
        manifest_text = transformer.transform(manifest_text, context)

    return manifest_text.encode("utf-8"), resolved_content_type


def _expand_pattern(node_pattern: ET.Element) -> list[ET.Element] | None:
    raw_repeat = _parse_int(node_pattern.attrib.get("r"), default=0)
    if raw_repeat < 0:
        return None

    pattern_repeat = raw_repeat + 1
    if pattern_repeat > _MAX_PATTERN_REPEAT:
        return None

    pattern_segments = [
        child for child in list(node_pattern) if _local_name(child.tag) == "S"
    ]

    expanded_segments: list[ET.Element] = []
    pattern_time = node_pattern.attrib.get("t")
    for repeat_index in range(pattern_repeat):
        for segment_index, pattern_segment in enumerate(pattern_segments):
            segment = deepcopy(pattern_segment)
            if (
                pattern_time is not None
                and repeat_index == 0
                and segment_index == 0
                and "t" not in segment.attrib
            ):
                segment.attrib["t"] = pattern_time
            expanded_segments.append(segment)

    return expanded_segments


def _parse_xml(manifest: str) -> ET.Element | None:
    try:
        return ET.fromstring(manifest)
    except ET.ParseError:
        return None


def _serialize_xml(root: ET.Element) -> str:
    namespace = _namespace_from_tag(root.tag)
    if namespace is not None:
        ET.register_namespace("", namespace)
    return ET.tostring(root, encoding="unicode")


def _local_name(tag: str) -> str:
    if "}" in tag:
        return tag.rsplit("}", 1)[1]
    if ":" in tag:
        return tag.rsplit(":", 1)[1]
    return tag


def _namespace_from_tag(tag: str) -> str | None:
    if not tag.startswith("{"):
        return None
    namespace, _, _ = tag[1:].partition("}")
    return namespace or None


def _qualified_tag(parent_tag: str, child_name: str) -> str:
    namespace = _namespace_from_tag(parent_tag)
    if namespace is None:
        return child_name
    return f"{{{namespace}}}{child_name}"


def _directory_base_url(url: str) -> str:
    parts = urlsplit(url)
    path = parts.path
    if not path:
        directory = "/"
    elif path.endswith("/"):
        directory = path
    elif "/" in path:
        directory = f"{path.rsplit('/', 1)[0]}/"
    else:
        directory = "/"
    return urlunsplit((parts.scheme, parts.netloc, directory, "", ""))


def _split_line_ending(line: str) -> tuple[str, str]:
    if line.endswith("\r\n"):
        return line[:-2], "\r\n"
    if line.endswith("\n"):
        return line[:-1], "\n"
    return line, ""


def _rewrite_hls_uri_attr(match: re.Match[str], base_url: str) -> str:
    uri = match.group(1)
    absolute_uri = _absolutize_uri(uri, base_url)
    return f'URI="{absolute_uri}"'


def _absolutize_uri(uri: str, base_url: str) -> str:
    if not uri or _is_absolute_url(uri):
        return uri
    return urljoin(base_url, uri)


def _is_absolute_url(value: str) -> bool:
    if value.startswith("//"):
        return True
    return bool(urlsplit(value).scheme)


def _parse_int(value: str | None, *, default: int) -> int:
    if value is None:
        return default
    try:
        return int(value)
    except ValueError:
        return default


__all__ = [
    "ManifestKind",
    "ManifestProxyRequest",
    "ManifestProxyResponse",
    "default_manifest_content_type",
    "infer_manifest_kind",
    "manifest_route_suffix",
    "normalize_manifest_bytes",
]
