"""Tests for manifest proxy normalization transforms."""

from __future__ import annotations

from vibecast._manifest_proxy import (
    ManifestKind,
    infer_manifest_kind,
    manifest_route_suffix,
    normalize_manifest_bytes,
)


def test_infer_manifest_kind_from_url_and_content_type() -> None:
    assert (
        infer_manifest_kind("application/dash+xml", "https://example.com/manifest")
        is ManifestKind.DASH
    )
    assert (
        infer_manifest_kind(None, "https://example.com/live/master.m3u8")
        is ManifestKind.HLS
    )
    assert (
        infer_manifest_kind("video/mp4", "https://example.com/video.mp4")
        is ManifestKind.UNKNOWN
    )


def test_manifest_route_suffix_matches_kind() -> None:
    assert manifest_route_suffix(ManifestKind.DASH) == ".mpd"
    assert manifest_route_suffix(ManifestKind.HLS) == ".m3u8"
    assert manifest_route_suffix(ManifestKind.UNKNOWN) == ".manifest"


def test_normalize_dash_pattern_and_base_url() -> None:
    manifest = """<?xml version=\"1.0\"?>
<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"dynamic\">
  <Period>
    <AdaptationSet mimeType=\"audio/mp4\">
      <Representation id=\"a1\" codecs=\"mp4a.40.2\">
        <SegmentTemplate media=\"a_$Number$.m4s\" initialization=\"a_init.mp4\" timescale=\"32000\">
          <SegmentTimeline>
            <Pattern t=\"0\" r=\"1\">
              <S d=\"64512\"/>
              <S d=\"63488\"/>
            </Pattern>
          </SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"""

    normalized, content_type = normalize_manifest_bytes(
        manifest.encode("utf-8"),
        upstream_url="https://cdn.example.com/live/manifest.mpd?token=abc",
        content_type="application/dash+xml",
        provider_key="primevideo",
    )

    text = normalized.decode("utf-8")
    assert content_type == "application/dash+xml"
    assert "<Pattern" not in text
    assert text.count('<S d="64512"') == 2
    assert "<BaseURL>https://cdn.example.com/live/</BaseURL>" in text


def test_normalize_hls_absolutizes_relative_uris() -> None:
    playlist = """#EXTM3U
#EXT-X-VERSION:6
#EXT-X-KEY:METHOD=AES-128,URI=\"keys/key.bin\"
#EXTINF:4.0,
segment-1.ts
#EXTINF:4.0,
segment-2.ts
"""

    normalized, content_type = normalize_manifest_bytes(
        playlist.encode("utf-8"),
        upstream_url="https://cdn.example.com/hls/master.m3u8",
        content_type="application/vnd.apple.mpegurl",
        provider_key="test",
    )

    text = normalized.decode("utf-8")
    assert content_type == "application/vnd.apple.mpegurl"
    assert 'URI="https://cdn.example.com/hls/keys/key.bin"' in text
    assert "https://cdn.example.com/hls/segment-1.ts" in text
    assert "https://cdn.example.com/hls/segment-2.ts" in text
