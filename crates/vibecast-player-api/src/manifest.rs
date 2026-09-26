//! Manifest normalization transforms for the proxy.
//!
//! Shaka Player receives manifests through the bridge, so relative URLs must be
//! absolutized against the upstream URL and DASH `<SegmentTimeline>`
//! `<Pattern>` shorthand (a nonstandard extension some CDNs emit) must be
//! expanded into plain `<S>` runs. DASH manipulation uses the `xot` mutable XML
//! tree; HLS is a line-oriented text rewrite.

use url::Url;
use xot::{NameId, Node, Xot};

/// Cap on `<Pattern r="...">` expansion to bound worst-case memory.
const MAX_PATTERN_REPEAT: i64 = 20_000;

/// Known manifest container types handled by the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestKind {
    /// MPEG-DASH (`.mpd`).
    Dash,
    /// HLS (`.m3u8`).
    Hls,
    /// Anything else — passed through untouched.
    Unknown,
}

/// Infer the manifest kind from the content type and URL.
#[must_use]
pub fn infer_manifest_kind(content_type: Option<&str>, url: &str) -> ManifestKind {
    let lowered_content_type = content_type.unwrap_or("").to_ascii_lowercase();
    let lowered_path = Url::parse(url)
        .map(|parsed| parsed.path().to_ascii_lowercase())
        .unwrap_or_else(|_| url.to_ascii_lowercase());

    if lowered_content_type.contains("dash") || lowered_path.ends_with(".mpd") {
        ManifestKind::Dash
    } else if lowered_content_type.contains("mpegurl") || lowered_path.ends_with(".m3u8") {
        ManifestKind::Hls
    } else {
        ManifestKind::Unknown
    }
}

/// The default content type for a manifest kind.
#[must_use]
pub fn default_manifest_content_type(kind: ManifestKind) -> &'static str {
    match kind {
        ManifestKind::Dash => "application/dash+xml",
        ManifestKind::Hls => "application/vnd.apple.mpegurl",
        ManifestKind::Unknown => "application/octet-stream",
    }
}

/// The deterministic proxy-route filename suffix for a manifest kind.
#[must_use]
pub fn manifest_route_suffix(kind: ManifestKind) -> &'static str {
    match kind {
        ManifestKind::Dash => ".mpd",
        ManifestKind::Hls => ".m3u8",
        ManifestKind::Unknown => ".manifest",
    }
}

/// Normalize one manifest body, returning the rewritten bytes and content type.
#[must_use]
pub fn normalize_manifest_bytes(
    body: &[u8],
    upstream_url: &str,
    content_type: Option<&str>,
) -> (Vec<u8>, String) {
    let kind = infer_manifest_kind(content_type, upstream_url);
    let resolved_content_type = content_type
        .map(str::to_string)
        .unwrap_or_else(|| default_manifest_content_type(kind).to_string());

    if kind == ManifestKind::Unknown {
        return (body.to_vec(), resolved_content_type);
    }

    let text = String::from_utf8_lossy(body).into_owned();
    let transformed = match kind {
        ManifestKind::Dash => transform_dash(&text, upstream_url),
        ManifestKind::Hls => transform_hls(&text, upstream_url),
        ManifestKind::Unknown => text,
    };
    (transformed.into_bytes(), resolved_content_type)
}

/// Append `query` (`name=value[&...]`, already URL-encoded) to every media
/// segment and initialization URL a normalized manifest references, for CDNs
/// that authorize each media request with a token parameter.
///
/// DASH: `SegmentTemplate@media`/`@initialization`, `SegmentURL@media`,
/// `Initialization@sourceURL`, `RepresentationIndex@sourceURL`, and non-directory
/// `BaseURL`s (single-file representations). HLS media playlists: segment URI
/// lines and the `URI="..."` of `EXT-X-MAP`/`EXT-X-PART`/`EXT-X-PRELOAD-HINT`
/// (key URIs are left alone). Master playlists need their child playlists
/// proxied instead; see [`proxy_hls_children`]. Other kinds are returned
/// unchanged.
#[must_use]
pub fn append_segment_query(body: &[u8], kind: ManifestKind, query: &str) -> Vec<u8> {
    if query.is_empty() {
        return body.to_vec();
    }
    let text = String::from_utf8_lossy(body);
    match kind {
        ManifestKind::Dash => append_dash_segment_query(&text, query).into_bytes(),
        ManifestKind::Hls => append_hls_segment_query(&text, query).into_bytes(),
        ManifestKind::Unknown => body.to_vec(),
    }
}

fn with_query(url: &str, query: &str) -> String {
    let (path, fragment) = match url.find('#') {
        Some(index) => url.split_at(index),
        None => (url, ""),
    };
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}{query}{fragment}")
}

fn append_dash_segment_query(manifest: &str, query: &str) -> String {
    let mut xot = Xot::new();
    let Ok(root) = xot.parse(manifest) else {
        return manifest.to_string();
    };
    let Ok(doc_el) = xot.document_element(root) else {
        return manifest.to_string();
    };

    let targets = [
        ("SegmentTemplate", "media"),
        ("SegmentTemplate", "initialization"),
        ("SegmentURL", "media"),
        ("Initialization", "sourceURL"),
        ("RepresentationIndex", "sourceURL"),
    ];
    let mut changed = false;
    for (element, attribute) in targets {
        let attribute = xot.add_name(attribute);
        let mut nodes = Vec::new();
        collect_by_local(&xot, doc_el, element, &mut nodes);
        for node in nodes {
            let Some(value) = xot.attributes(node).get(attribute).cloned() else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            xot.attributes_mut(node)
                .insert(attribute, with_query(&value, query));
            changed = true;
        }
    }

    let mut base_urls = Vec::new();
    collect_by_local(&xot, doc_el, "BaseURL", &mut base_urls);
    for node in base_urls {
        let text = xot
            .text_content_str(node)
            .unwrap_or_default()
            .trim()
            .to_string();
        if text.is_empty() || text.ends_with('/') {
            continue;
        }
        set_element_text(&mut xot, node, &with_query(&text, query));
        changed = true;
    }

    if !changed {
        return manifest.to_string();
    }
    xot.to_string(doc_el)
        .unwrap_or_else(|_| manifest.to_string())
}

fn append_hls_segment_query(manifest: &str, query: &str) -> String {
    let mut out = String::with_capacity(manifest.len());
    for line in split_lines_keepends(manifest) {
        let (body, ending) = split_line_ending(line);
        let stripped = body.trim();
        if stripped.is_empty() {
            out.push_str(line);
        } else if HLS_MEDIA_URI_TAGS
            .iter()
            .any(|tag| stripped.starts_with(tag))
        {
            out.push_str(&map_hls_uri_attrs(body, |uri| with_query(uri, query)));
            out.push_str(ending);
        } else if stripped.starts_with('#') {
            out.push_str(line);
        } else {
            out.push_str(&with_query(stripped, query));
            out.push_str(ending);
        }
    }
    out
}

/// HLS media-playlist tags whose `URI` names segment or initialization data.
const HLS_MEDIA_URI_TAGS: [&str; 3] = ["#EXT-X-MAP:", "#EXT-X-PART:", "#EXT-X-PRELOAD-HINT:"];

/// HLS master-playlist tags whose `URI` names a child playlist.
const HLS_CHILD_URI_TAGS: [&str; 2] = ["#EXT-X-MEDIA:", "#EXT-X-I-FRAME-STREAM-INF:"];

/// Rewrite the child-playlist URIs of a normalized HLS master playlist to
/// `{route_file}?child={n}`, where `n` is the index `register` assigns to the
/// absolute child URL (with `query` appended, as for segments). Relative to the
/// proxied master playlist, that keeps the children on the same proxy route so
/// their segments can be tokenized too.
///
/// Returns `None` when the body is not a master playlist.
#[must_use]
pub fn proxy_hls_children(
    body: &[u8],
    route_file: &str,
    query: &str,
    mut register: impl FnMut(&str) -> usize,
) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(body);
    if !text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("#EXT-X-STREAM-INF:") || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
    }) {
        return None;
    }
    let mut child = |uri: &str| format!("{route_file}?child={}", register(&with_query(uri, query)));
    let mut out = String::with_capacity(text.len());
    for line in split_lines_keepends(&text) {
        let (body, ending) = split_line_ending(line);
        let stripped = body.trim();
        if stripped.is_empty() {
            out.push_str(line);
        } else if HLS_CHILD_URI_TAGS
            .iter()
            .any(|tag| stripped.starts_with(tag))
        {
            out.push_str(&map_hls_uri_attrs(body, &mut child));
            out.push_str(ending);
        } else if stripped.starts_with('#') {
            out.push_str(line);
        } else {
            out.push_str(&child(stripped));
            out.push_str(ending);
        }
    }
    Some(out.into_bytes())
}

fn map_hls_uri_attrs(line: &str, mut map: impl FnMut(&str) -> String) -> String {
    const MARKER: &str = "URI=\"";
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(position) = rest.find(MARKER) {
        let (before, after) = rest.split_at(position);
        out.push_str(before);
        out.push_str(MARKER);
        let after = &after[MARKER.len()..];
        if let Some(end) = after.find('"') {
            out.push_str(&map(&after[..end]));
            out.push('"');
            rest = &after[end + 1..];
        } else {
            out.push_str(after);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

// -- DASH ------------------------------------------------------------------

fn transform_dash(manifest: &str, upstream_url: &str) -> String {
    let mut xot = Xot::new();
    let Ok(root) = xot.parse(manifest) else {
        return manifest.to_string();
    };
    let Ok(doc_el) = xot.document_element(root) else {
        return manifest.to_string();
    };

    let expanded = expand_dash_patterns(&mut xot, doc_el);
    let based = apply_dash_base_url(&mut xot, doc_el, upstream_url);
    if !expanded && !based {
        return manifest.to_string();
    }
    xot.to_string(doc_el)
        .unwrap_or_else(|_| manifest.to_string())
}

fn expand_dash_patterns(xot: &mut Xot, doc_el: Node) -> bool {
    let r_name = xot.add_name("r");
    let t_name = xot.add_name("t");

    let mut timelines = Vec::new();
    collect_by_local(xot, doc_el, "SegmentTimeline", &mut timelines);

    let mut changed = false;
    for timeline in timelines {
        let patterns: Vec<Node> = xot
            .children(timeline)
            .filter(|child| local_name(xot, *child) == Some("Pattern"))
            .collect();
        for pattern in patterns {
            let Some(expanded) = expand_pattern(xot, pattern, r_name, t_name) else {
                continue;
            };
            for segment in expanded {
                let _ = xot.insert_before(pattern, segment);
            }
            let _ = xot.remove(pattern);
            changed = true;
        }
    }
    changed
}

fn expand_pattern(
    xot: &mut Xot,
    pattern: Node,
    r_name: NameId,
    t_name: NameId,
) -> Option<Vec<Node>> {
    let raw_repeat = xot
        .attributes(pattern)
        .get(r_name)
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if raw_repeat < 0 {
        return None;
    }
    let repeat = raw_repeat + 1;
    if repeat > MAX_PATTERN_REPEAT {
        return None;
    }

    let pattern_time: Option<String> = xot
        .attributes(pattern)
        .get(t_name)
        .map(|value| value.to_string());
    let segments: Vec<Node> = xot
        .children(pattern)
        .filter(|child| local_name(xot, *child) == Some("S"))
        .collect();

    let mut expanded = Vec::new();
    for repeat_index in 0..repeat {
        for (segment_index, &segment) in segments.iter().enumerate() {
            let copy = xot.clone_node(segment);
            let needs_time = pattern_time.is_some()
                && repeat_index == 0
                && segment_index == 0
                && xot.attributes(copy).get(t_name).is_none();
            if needs_time {
                let time = pattern_time.clone().unwrap();
                xot.attributes_mut(copy).insert(t_name, time);
            }
            expanded.push(copy);
        }
    }
    Some(expanded)
}

fn apply_dash_base_url(xot: &mut Xot, doc_el: Node, upstream_url: &str) -> bool {
    let base_url = directory_base_url(upstream_url);
    let root_base_urls: Vec<Node> = xot
        .children(doc_el)
        .filter(|child| local_name(xot, *child) == Some("BaseURL"))
        .collect();

    if !root_base_urls.is_empty() {
        let mut changed = false;
        for node in root_base_urls {
            let text = xot.text_content_str(node).unwrap_or_default().to_string();
            let trimmed = text.trim();
            if trimmed.is_empty() || is_absolute_url(trimmed) {
                continue;
            }
            let joined = urljoin(&base_url, trimmed);
            set_element_text(xot, node, &joined);
            changed = true;
        }
        return changed;
    }

    let name = base_url_name(xot, doc_el);
    let element = xot.new_element(name);
    let text_node = xot.new_text(&base_url);
    let _ = xot.append(element, text_node);
    match xot.first_child(doc_el) {
        Some(first) => {
            let _ = xot.insert_before(first, element);
        }
        None => {
            let _ = xot.append(doc_el, element);
        }
    }
    true
}

fn base_url_name(xot: &mut Xot, doc_el: Node) -> NameId {
    let namespace = match xot.node_name(doc_el) {
        Some(name) => xot.name_ns_str(name).1.to_string(),
        None => String::new(),
    };
    if namespace.is_empty() {
        xot.add_name("BaseURL")
    } else {
        let namespace_id = xot.add_namespace(&namespace);
        xot.add_name_ns("BaseURL", namespace_id)
    }
}

fn set_element_text(xot: &mut Xot, node: Node, value: &str) {
    let children: Vec<Node> = xot.children(node).collect();
    for child in children {
        let _ = xot.remove(child);
    }
    let text_node = xot.new_text(value);
    let _ = xot.append(node, text_node);
}

fn collect_by_local(xot: &Xot, node: Node, target: &str, out: &mut Vec<Node>) {
    for child in xot.children(node) {
        if local_name(xot, child) == Some(target) {
            out.push(child);
        }
        collect_by_local(xot, child, target, out);
    }
}

fn local_name(xot: &Xot, node: Node) -> Option<&str> {
    let name = xot.node_name(node)?;
    Some(xot.name_ns_str(name).0)
}

fn directory_base_url(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url) else {
        return url.to_string();
    };
    let path = parsed.path().to_string();
    let directory = if path.is_empty() {
        "/".to_string()
    } else if path.ends_with('/') {
        path
    } else if let Some(index) = path.rfind('/') {
        format!("{}/", &path[..index])
    } else {
        "/".to_string()
    };
    parsed.set_path(&directory);
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

// -- HLS -------------------------------------------------------------------

fn transform_hls(manifest: &str, base_url: &str) -> String {
    let mut changed = false;
    let mut out = String::with_capacity(manifest.len());

    for line in split_lines_keepends(manifest) {
        let (body, ending) = split_line_ending(line);
        let stripped = body.trim();

        if stripped.is_empty() {
            out.push_str(line);
            continue;
        }

        if stripped.starts_with('#') {
            let rewritten = rewrite_hls_uri_attrs(body, base_url);
            if rewritten != body {
                changed = true;
            }
            out.push_str(&rewritten);
            out.push_str(ending);
            continue;
        }

        let target = body.trim();
        let rewritten = absolutize_uri(target, base_url);
        if rewritten != target {
            changed = true;
        }
        out.push_str(&rewritten);
        out.push_str(ending);
    }

    if changed {
        out
    } else {
        manifest.to_string()
    }
}

fn rewrite_hls_uri_attrs(line: &str, base_url: &str) -> String {
    const MARKER: &str = "URI=\"";
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(position) = rest.find(MARKER) {
        let (before, after) = rest.split_at(position);
        out.push_str(before);
        out.push_str(MARKER);
        let after = &after[MARKER.len()..];
        if let Some(end) = after.find('"') {
            out.push_str(&absolutize_uri(&after[..end], base_url));
            out.push('"');
            rest = &after[end + 1..];
        } else {
            out.push_str(after);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

fn split_lines_keepends(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                lines.push(&text[start..=index]);
                index += 1;
                start = index;
            }
            b'\r' => {
                let end = if index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
                    index + 1
                } else {
                    index
                };
                lines.push(&text[start..=end]);
                index = end + 1;
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn split_line_ending(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else if let Some(body) = line.strip_suffix('\r') {
        (body, "\r")
    } else {
        (line, "")
    }
}

// -- shared URL helpers ----------------------------------------------------

fn absolutize_uri(uri: &str, base_url: &str) -> String {
    if uri.is_empty() || is_absolute_url(uri) {
        uri.to_string()
    } else {
        urljoin(base_url, uri)
    }
}

fn urljoin(base: &str, reference: &str) -> String {
    match Url::parse(base).and_then(|base_url| base_url.join(reference)) {
        Ok(joined) => joined.to_string(),
        Err(_) => reference.to_string(),
    }
}

fn is_absolute_url(value: &str) -> bool {
    if value.starts_with("//") {
        return true;
    }
    has_scheme(value)
}

fn has_scheme(value: &str) -> bool {
    let Some(colon) = value.find(':') else {
        return false;
    };
    let scheme = &value[..colon];
    !scheme.is_empty()
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_manifest_kind_from_url_and_content_type() {
        assert_eq!(
            infer_manifest_kind(Some("application/dash+xml"), "https://example.com/manifest"),
            ManifestKind::Dash
        );
        assert_eq!(
            infer_manifest_kind(None, "https://example.com/live/master.m3u8"),
            ManifestKind::Hls
        );
        assert_eq!(
            infer_manifest_kind(Some("video/mp4"), "https://example.com/video.mp4"),
            ManifestKind::Unknown
        );
    }

    #[test]
    fn manifest_route_suffix_matches_kind() {
        assert_eq!(manifest_route_suffix(ManifestKind::Dash), ".mpd");
        assert_eq!(manifest_route_suffix(ManifestKind::Hls), ".m3u8");
        assert_eq!(manifest_route_suffix(ManifestKind::Unknown), ".manifest");
    }

    #[test]
    fn normalize_dash_pattern_and_base_url() {
        let manifest = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic">
  <Period>
    <AdaptationSet mimeType="audio/mp4">
      <Representation id="a1" codecs="mp4a.40.2">
        <SegmentTemplate media="a_$Number$.m4s" initialization="a_init.mp4" timescale="32000">
          <SegmentTimeline>
            <Pattern t="0" r="1">
              <S d="64512"/>
              <S d="63488"/>
            </Pattern>
          </SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"#;

        let (normalized, content_type) = normalize_manifest_bytes(
            manifest.as_bytes(),
            "https://cdn.example.com/live/manifest.mpd?token=abc",
            Some("application/dash+xml"),
        );
        let text = String::from_utf8(normalized).unwrap();

        assert_eq!(content_type, "application/dash+xml");
        // <Pattern> shorthand fully expanded (r=1 -> repeat 2) into 2x2 <S> runs.
        assert!(!text.contains("<Pattern"), "pattern not expanded: {text}");
        assert_eq!(text.matches(r#"d="64512""#).count(), 2, "{text}");
        assert_eq!(text.matches(r#"d="63488""#).count(), 2, "{text}");
        // The pattern's `t` is applied to the first expanded segment only.
        assert!(text.contains(r#"t="0""#), "start time not applied: {text}");
        // A directory BaseURL is injected (query stripped).
        assert!(
            text.contains(">https://cdn.example.com/live/</BaseURL>"),
            "base url not injected: {text}"
        );
    }

    #[test]
    fn normalize_dash_absolutizes_existing_relative_base_url() {
        let manifest =
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><BaseURL>sub/</BaseURL><Period/></MPD>"#;
        let (normalized, _) = normalize_manifest_bytes(
            manifest.as_bytes(),
            "https://cdn.example.com/live/manifest.mpd",
            Some("application/dash+xml"),
        );
        let text = String::from_utf8(normalized).unwrap();
        assert!(
            text.contains(">https://cdn.example.com/live/sub/</BaseURL>"),
            "relative base url not absolutized: {text}"
        );
    }

    #[test]
    fn normalize_hls_absolutizes_relative_uris() {
        let playlist = "#EXTM3U\n\
#EXT-X-VERSION:6\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"keys/key.bin\"\n\
#EXTINF:4.0,\n\
segment-1.ts\n\
#EXTINF:4.0,\n\
segment-2.ts\n";

        let (normalized, content_type) = normalize_manifest_bytes(
            playlist.as_bytes(),
            "https://cdn.example.com/hls/master.m3u8",
            Some("application/vnd.apple.mpegurl"),
        );
        let text = String::from_utf8(normalized).unwrap();

        assert_eq!(content_type, "application/vnd.apple.mpegurl");
        assert!(
            text.contains(r#"URI="https://cdn.example.com/hls/keys/key.bin""#),
            "{text}"
        );
        assert!(
            text.contains("https://cdn.example.com/hls/segment-1.ts"),
            "{text}"
        );
        assert!(
            text.contains("https://cdn.example.com/hls/segment-2.ts"),
            "{text}"
        );
    }

    #[test]
    fn segment_query_is_appended_to_dash_media_urls() {
        let manifest = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><BaseURL>https://cdn.example.com/live/</BaseURL><Period>
  <AdaptationSet><SegmentTemplate media="v_$Number$.m4s" initialization="v-init.dash"/>
    <Representation id="v"/></AdaptationSet>
  <AdaptationSet><Representation id="a"><BaseURL>audio.mp4?x=1</BaseURL>
    <SegmentList><Initialization sourceURL="a-init.mp4"/><SegmentURL media="a1.m4s"/></SegmentList>
  </Representation></AdaptationSet>
</Period></MPD>"#;
        let out = append_segment_query(manifest.as_bytes(), ManifestKind::Dash, "hdntl=a%2Fb");
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(r#"media="v_$Number$.m4s?hdntl=a%2Fb""#),
            "{text}"
        );
        assert!(
            text.contains(r#"initialization="v-init.dash?hdntl=a%2Fb""#),
            "{text}"
        );
        assert!(
            text.contains(r#"sourceURL="a-init.mp4?hdntl=a%2Fb""#),
            "{text}"
        );
        assert!(text.contains(r#"media="a1.m4s?hdntl=a%2Fb""#), "{text}");
        assert!(
            text.contains(">audio.mp4?x=1&amp;hdntl=a%2Fb</BaseURL>"),
            "{text}"
        );
        // Directory BaseURLs stay untouched (segments carry the token).
        assert!(
            text.contains(">https://cdn.example.com/live/</BaseURL>"),
            "{text}"
        );
    }

    #[test]
    fn segment_query_is_appended_to_hls_uris() {
        let playlist = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"https://key.example/k\"\n\
            #EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.0,\nhttps://cdn.example.com/s1.m4s\n";
        let out = append_segment_query(playlist.as_bytes(), ManifestKind::Hls, "t=1");
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains(r#"URI="init.mp4?t=1""#), "{text}");
        assert!(
            text.contains(
                r#"URI="https://key.example/k"
"#
            ),
            "{text}"
        );
        assert!(
            text.contains("https://cdn.example.com/s1.m4s?t=1\n"),
            "{text}"
        );
        assert!(text.starts_with("#EXTM3U\n"), "{text}");
    }

    #[test]
    fn hls_master_children_are_routed_back_through_the_proxy() {
        let master = "#EXTM3U\n\
            #EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,URI=\"https://key.example/k\"\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",URI=\"https://cdn.example.com/audio.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"a\"\n\
            https://cdn.example.com/hi.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2,AUDIO=\"a\"\n\
            https://cdn.example.com/hi.m3u8\n";
        let mut children: Vec<String> = Vec::new();
        let out = proxy_hls_children(master.as_bytes(), "m0.m3u8", "t=1", |uri| {
            children.iter().position(|c| c == uri).unwrap_or_else(|| {
                children.push(uri.to_string());
                children.len() - 1
            })
        })
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            children,
            [
                "https://cdn.example.com/audio.m3u8?t=1",
                "https://cdn.example.com/hi.m3u8?t=1"
            ]
        );
        assert!(text.contains(r#"URI="m0.m3u8?child=0""#), "{text}");
        assert_eq!(text.matches("\nm0.m3u8?child=1\n").count(), 2, "{text}");
        assert!(text.contains(r#"URI="https://key.example/k""#), "{text}");

        let media = b"#EXTM3U\n#EXTINF:4.0,\ns1.m4s\n";
        assert!(proxy_hls_children(media, "m0.m3u8", "t=1", |_| 0).is_none());
    }

    #[test]
    fn unknown_manifest_is_passed_through_untouched() {
        let body = b"not a manifest";
        let (normalized, content_type) =
            normalize_manifest_bytes(body, "https://example.com/video.mp4", Some("video/mp4"));
        assert_eq!(normalized, body);
        assert_eq!(content_type, "video/mp4");
    }
}
