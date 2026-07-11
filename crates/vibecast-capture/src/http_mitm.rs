//! Transparent HTTP/HTTPS MITM for the receiver's egress.
//!
//! Traffic is redirected here by the device's iptables rules (see [`crate::adb`]).
//! Because the redirect is transparent (no `CONNECT`), the target host is
//! recovered from the TLS SNI (HTTPS) or the `Host` header (HTTP). For HTTPS we
//! terminate TLS with a per-host leaf minted by [`crate::ca`], forward upstream
//! with `reqwest`, and log the full request/response pair.

use std::collections::HashSet;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::Stream;
use http::header::{HeaderMap, HeaderName};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ServerBuilder;
use serde_json::{Map, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::LazyConfigAcceptor;

/// The response body type produced by the proxy (a boxed streaming body).
/// Unsync because it wraps `reqwest`'s byte stream (Send but not Sync).
type ProxyBody = UnsyncBoxBody<Bytes, std::io::Error>;

use crate::ca::CaptureCa;
use crate::recorder::Recorder;

/// Bodies larger than this are summarized rather than logged inline (256 KiB).
const MAX_BODY_LOG: usize = 256 * 1024;

/// Hop-by-hop headers that must not be forwarded across the proxy boundary.
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "proxy-connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "host",
];

/// Host suffixes that are always passed through undecrypted: pinned Google /
/// Cast infrastructure that rejects a MITM cert and is needed for the Cast
/// session to establish. App traffic (providers/CDNs/APIs) is still decrypted.
const PASSTHROUGH_SUFFIXES: &[&str] = &[
    "googleapis.com",
    "google.com",
    "gstatic.com",
    "gvt1.com",
    "gvt2.com",
    "ggpht.com",
    "googlevideo.com",
    "doubleclick.net",
    "app-measurement.com",
];

/// Transparent MITM state shared across accepted connections.
pub struct HttpMitm {
    recorder: Arc<Recorder>,
    ca: Arc<CaptureCa>,
    client: reqwest::Client,
    /// Hosts learned to reject our cert (pinned); passed through on retry.
    learned_pinned: Mutex<HashSet<String>>,
}

impl HttpMitm {
    /// Build the MITM. The upstream client does not follow redirects (so the
    /// sender observes real 3xx responses) and verifies upstream certificates
    /// normally.
    pub fn new(
        recorder: Arc<Recorder>,
        ca: Arc<CaptureCa>,
    ) -> Result<Self, crate::error::CaptureError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| crate::error::CaptureError::Tls(e.to_string()))?;
        Ok(Self {
            recorder,
            ca,
            client,
            learned_pinned: Mutex::new(HashSet::new()),
        })
    }

    /// Whether `host` should be tunnelled undecrypted (known-pinned infra or a
    /// host previously observed to reject our cert).
    fn should_passthrough(&self, host: &str) -> bool {
        if is_infra_host(host) {
            return true;
        }
        self.learned_pinned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(host)
    }

    /// Record a host as pinned so subsequent connections pass through.
    fn learn_pinned(&self, host: &str) {
        let inserted = self
            .learned_pinned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(host.to_owned());
        if inserted {
            tracing::debug!(%host, "learned pinned host; passing through on retry");
            self.recorder.meta(
                "passthrough_learned",
                [("host".to_owned(), host.into())].into_iter().collect(),
            );
        }
    }

    /// Serve the HTTPS listener: SNI-routed TLS termination + MITM.
    pub async fn serve_https(self: Arc<Self>, listener: TcpListener) {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                return;
            };
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(error) = me.handle_https(stream).await {
                    tracing::debug!(%error, "https mitm connection ended");
                }
            });
        }
    }

    /// Serve the plaintext HTTP listener: `Host`-routed MITM.
    pub async fn serve_http(self: Arc<Self>, listener: TcpListener) {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                return;
            };
            let me = Arc::clone(&self);
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = me.make_service("http", None);
                if let Err(error) = ServerBuilder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await
                {
                    tracing::debug!(%error, "http mitm connection ended");
                }
            });
        }
    }

    async fn handle_https(self: Arc<Self>, stream: TcpStream) -> std::io::Result<()> {
        // Peek the ClientHello (without consuming) to recover the SNI, so we can
        // decide whether to decrypt or tunnel before terminating TLS.
        let Some(host) = peek_sni(&stream).await else {
            tracing::debug!("dropping HTTPS connection without SNI");
            return Ok(());
        };

        if self.should_passthrough(&host) {
            tracing::debug!(%host, "passing through (not decrypting)");
            return passthrough(stream, &host).await;
        }

        let acceptor = LazyConfigAcceptor::new(rustls::server::Acceptor::default(), stream);
        let start = acceptor.await?;
        let config = self
            .ca
            .server_config(&host)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let tls = match start.into_stream(config).await {
            Ok(tls) => tls,
            Err(error) => {
                // The client rejected our (validly-chained) leaf — it pins this
                // host. Learn it so the client's retry is tunnelled and works.
                tracing::debug!(%host, %error, "TLS rejected (pinned); will pass through");
                self.learn_pinned(&host);
                return Ok(());
            }
        };
        tracing::debug!(%host, "TLS established (decrypting)");

        let io = TokioIo::new(tls);
        let service = self.make_service("https", Some(host.clone()));
        if let Err(error) = ServerBuilder::new(TokioExecutor::new())
            .serve_connection(io, service)
            .await
        {
            tracing::debug!(%host, %error, "https mitm connection ended");
        }
        Ok(())
    }

    /// Build a per-connection `service_fn`. `sni` fixes the host for HTTPS; for
    /// plain HTTP the host is read from each request's `Host` header.
    fn make_service(
        self: &Arc<Self>,
        scheme: &'static str,
        sni: Option<String>,
    ) -> impl hyper::service::Service<
        Request<Incoming>,
        Response = Response<ProxyBody>,
        Error = Infallible,
        Future = impl std::future::Future<Output = Result<Response<ProxyBody>, Infallible>>,
    > + Clone {
        let me = Arc::clone(self);
        service_fn(move |req: Request<Incoming>| {
            let me = Arc::clone(&me);
            let sni = sni.clone();
            async move { Ok(me.proxy(scheme, sni, req).await) }
        })
    }

    async fn proxy(
        self: Arc<Self>,
        scheme: &'static str,
        sni: Option<String>,
        req: Request<Incoming>,
    ) -> Response<ProxyBody> {
        let (parts, body) = req.into_parts();

        let host = sni
            .or_else(|| {
                parts
                    .headers
                    .get(http::header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .map(|h| h.split(':').next().unwrap_or(h).to_owned())
            })
            .or_else(|| parts.uri.host().map(str::to_owned));

        let Some(host) = host else {
            return status_response(StatusCode::BAD_GATEWAY);
        };

        let path = parts
            .uri
            .path_and_query()
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| "/".to_owned());
        let url = format!("{scheme}://{host}{path}");
        let method = parts.method.clone();
        let req_ct = content_type(&parts.headers);

        let req_bytes = body
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();

        tracing::debug!(method = %method, url = %url, "http request");
        let started = Instant::now();
        let upstream = self
            .client
            .request(method.clone(), &url)
            .headers(forward_headers(&parts.headers))
            .body(req_bytes.clone())
            .send()
            .await;

        let mut entry = Map::new();
        entry.insert("layer".into(), "http".into());
        entry.insert("method".into(), method.as_str().into());
        entry.insert("url".into(), url.clone().into());
        entry.insert("host".into(), host.into());
        entry.insert("scheme".into(), scheme.into());
        entry.insert("request_headers".into(), headers_to_value(&parts.headers));
        entry.insert(
            "request_body".into(),
            body_to_value(&req_bytes, req_ct.as_deref()),
        );

        match upstream {
            Ok(resp) => {
                let status = resp.status();
                let resp_headers = resp.headers().clone();
                let resp_ct = content_type(&resp_headers);

                entry.insert("status".into(), status.as_u16().into());
                entry.insert("response_headers".into(), headers_to_value(&resp_headers));

                // Stream the body straight through to the client (so streaming /
                // long-poll responses don't stall the receiver), tee-ing a capped
                // copy for the log, which is written when the stream completes.
                let tee = TeeBody::new(
                    resp.bytes_stream(),
                    entry,
                    resp_ct,
                    started,
                    Arc::clone(&self.recorder),
                );
                let body = StreamBody::new(tee).boxed_unsync();
                client_response(status, &resp_headers, body)
            }
            Err(error) => {
                entry.insert("error".into(), error.to_string().into());
                entry.insert(
                    "duration_ms".into(),
                    (started.elapsed().as_millis() as u64).into(),
                );
                self.recorder.http(entry);
                status_response(StatusCode::BAD_GATEWAY)
            }
        }
    }
}

/// A response body that streams `inner`'s chunks to the client while capturing
/// up to [`MAX_BODY_LOG`] bytes; the log entry is written once the stream ends
/// (or the connection drops), so streaming responses never stall the client.
struct TeeBody {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    captured: Vec<u8>,
    truncated: bool,
    logged: bool,
    entry: Option<Map<String, Value>>,
    resp_ct: Option<String>,
    started: Instant,
    recorder: Arc<Recorder>,
}

impl TeeBody {
    fn new(
        inner: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
        entry: Map<String, Value>,
        resp_ct: Option<String>,
        started: Instant,
        recorder: Arc<Recorder>,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            captured: Vec::new(),
            truncated: false,
            logged: false,
            entry: Some(entry),
            resp_ct,
            started,
            recorder,
        }
    }

    fn capture(&mut self, chunk: &[u8]) {
        let room = MAX_BODY_LOG.saturating_sub(self.captured.len());
        if room == 0 {
            self.truncated = self.truncated || !chunk.is_empty();
            return;
        }
        let take = room.min(chunk.len());
        self.captured.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            self.truncated = true;
        }
    }

    fn finish(&mut self, error: Option<String>) {
        if self.logged {
            return;
        }
        self.logged = true;
        let Some(mut entry) = self.entry.take() else {
            return;
        };
        entry.insert(
            "response_body".into(),
            body_to_value(&self.captured, self.resp_ct.as_deref()),
        );
        if self.truncated {
            entry.insert("response_body_truncated".into(), true.into());
        }
        if let Some(error) = error {
            entry.insert("response_error".into(), error.into());
        }
        entry.insert(
            "duration_ms".into(),
            (self.started.elapsed().as_millis() as u64).into(),
        );
        self.recorder.http(entry);
    }
}

impl Stream for TeeBody {
    type Item = Result<Frame<Bytes>, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.capture(&chunk);
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            Poll::Ready(Some(Err(error))) => {
                let msg = error.to_string();
                this.finish(Some(msg.clone()));
                Poll::Ready(Some(Err(std::io::Error::other(msg))))
            }
            Poll::Ready(None) => {
                this.finish(None);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for TeeBody {
    fn drop(&mut self) {
        // Log even if the client disconnected before the stream finished.
        self.finish(Some("connection closed before completion".into()));
    }
}

/// Whether `host` is pinned Google/Cast infrastructure that must be tunnelled
/// (matches a passthrough suffix exactly or as a dotted subdomain).
fn is_infra_host(host: &str) -> bool {
    PASSTHROUGH_SUFFIXES
        .iter()
        .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// Peek the TLS ClientHello (without consuming it) and extract the SNI host.
async fn peek_sni(stream: &TcpStream) -> Option<String> {
    let mut buf = vec![0u8; 8192];
    for _ in 0..12 {
        let n = stream.peek(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        if let Some(host) = parse_sni(&buf[..n]) {
            return Some(host.to_owned());
        }
        if n >= buf.len() {
            return None; // ClientHello larger than our peek buffer
        }
        // Not enough bytes buffered yet; wait briefly for the rest.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// Parse the SNI server name from a TLS ClientHello record. Returns `None` if
/// the bytes are incomplete or contain no SNI.
fn parse_sni(buf: &[u8]) -> Option<&str> {
    // TLS record header: type(1)=0x16 handshake, version(2), length(2).
    if buf.len() < 5 || buf[0] != 0x16 {
        return None;
    }
    let rec_len = ((buf[3] as usize) << 8) | buf[4] as usize;
    let hs = buf.get(5..(5 + rec_len).min(buf.len()))?;
    // Handshake header: type(1)=0x01 ClientHello, length(3).
    if hs.len() < 4 || hs[0] != 0x01 {
        return None;
    }
    let mut p = 4 + 2 + 32; // handshake header + client_version + random
    let sid = *hs.get(p)? as usize;
    p += 1 + sid;
    let cs = ((*hs.get(p)? as usize) << 8) | *hs.get(p + 1)? as usize;
    p += 2 + cs;
    let cm = *hs.get(p)? as usize;
    p += 1 + cm;
    let ext_len = ((*hs.get(p)? as usize) << 8) | *hs.get(p + 1)? as usize;
    p += 2;
    let ext_end = (p + ext_len).min(hs.len());
    while p + 4 <= ext_end {
        let etype = ((hs[p] as usize) << 8) | hs[p + 1] as usize;
        let elen = ((hs[p + 2] as usize) << 8) | hs[p + 3] as usize;
        p += 4;
        if etype == 0 {
            // server_name extension: list_len(2), then name_type(1)+len(2)+name.
            let sni = hs.get(p..(p + elen).min(hs.len()))?;
            let mut q = 2;
            while q + 3 <= sni.len() {
                let ntype = sni[q];
                let nlen = ((sni[q + 1] as usize) << 8) | sni[q + 2] as usize;
                q += 3;
                if ntype == 0 {
                    return std::str::from_utf8(sni.get(q..q + nlen)?).ok();
                }
                q += nlen;
            }
            return None;
        }
        p += elen;
    }
    None
}

/// Blindly tunnel a TLS connection to `host:443` without decrypting it. The
/// peeked ClientHello is still in the socket buffer, so it is forwarded.
async fn passthrough(mut stream: TcpStream, host: &str) -> std::io::Result<()> {
    let mut upstream = TcpStream::connect((host, 443)).await?;
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
    Ok(())
}

/// Copy request headers for the upstream call, dropping hop-by-hop headers
/// (and `Host`, which `reqwest` derives from the URL).
fn forward_headers(src: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in src {
        // Drop hop-by-hop, content-length (reqwest sets it), and accept-encoding:
        // leaving accept-encoding out lets reqwest advertise+auto-decode gzip, so
        // we forward identity bytes to the client (and log readable bodies).
        // Forwarding the client's accept-encoding would make reqwest pass the
        // compressed body through undecoded, which we'd then mislabel as identity.
        if is_hop_by_hop(name) || matches!(name.as_str(), "content-length" | "accept-encoding") {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// Build the response returned to the sender. `reqwest` already decoded any
/// content-encoding, so those framing headers are dropped and the body length
/// is recomputed by the server.
fn client_response(
    status: StatusCode,
    headers: &HeaderMap,
    body: ProxyBody,
) -> Response<ProxyBody> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        if is_hop_by_hop(name)
            || matches!(
                name.as_str(),
                "content-length" | "content-encoding" | "transfer-encoding"
            )
        {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| status_response(StatusCode::BAD_GATEWAY))
}

fn status_response(status: StatusCode) -> Response<ProxyBody> {
    Response::builder()
        .status(status)
        .body(empty_body())
        .expect("static status response is valid")
}

fn empty_body() -> ProxyBody {
    Full::new(Bytes::new())
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    HOP_BY_HOP.contains(&lower.as_str())
}

fn content_type(headers: &HeaderMap) -> Option<String> {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn headers_to_value(headers: &HeaderMap) -> Value {
    let mut map = Map::new();
    for (name, value) in headers {
        let v = String::from_utf8_lossy(value.as_bytes()).into_owned();
        match map.get_mut(name.as_str()) {
            Some(Value::Array(arr)) => arr.push(v.into()),
            Some(existing) => {
                let first = existing.take();
                *existing = Value::Array(vec![first, v.into()]);
            }
            None => {
                map.insert(name.as_str().to_owned(), v.into());
            }
        }
    }
    Value::Object(map)
}

/// Decode a body for logging: JSON is parsed inline, other text kept as a
/// string, binary/oversize bodies summarized.
fn body_to_value(bytes: &[u8], content_type: Option<&str>) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    let base = ct.split(';').next().unwrap_or("").trim();

    let textual = base.starts_with("text/")
        || base.contains("json")
        || base.contains("xml")
        || base == "application/x-www-form-urlencoded"
        || base == "application/dash+xml"
        || base == "application/vnd.apple.mpegurl";

    if !textual {
        return serde_json::json!({ "_binary": true, "content_type": base, "size": bytes.len() });
    }
    if bytes.len() > MAX_BODY_LOG {
        return serde_json::json!({
            "_truncated": true,
            "size": bytes.len(),
            "content_type": base,
            "preview": String::from_utf8_lossy(&bytes[..2048]),
        });
    }

    let text = String::from_utf8_lossy(bytes);
    if base.contains("json") {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            return value;
        }
    }
    Value::from(text.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::HeaderValue;

    #[test]
    fn infra_hosts_are_passed_through() {
        assert!(is_infra_host("play.googleapis.com"));
        assert!(is_infra_host("www.gstatic.com"));
        assert!(is_infra_host("google.com"));
        assert!(is_infra_host("clients3.google.com"));
        // App/provider hosts are decrypted, not passed through.
        assert!(!is_infra_host("contento.svt.se"));
        assert!(!is_infra_host("viaplay-chromecast.viaplay.com"));
        // Suffix must be a real dotted boundary, not a substring.
        assert!(!is_infra_host("notgoogle.com"));
        assert!(!is_infra_host("evilgoogleapis.com.attacker.net"));
    }

    #[test]
    fn parses_sni_from_clienthello() {
        // Minimal TLS 1.2 ClientHello with an SNI extension for "svt.se".
        let host = b"svt.se";
        let mut ext = Vec::new();
        // server_name extension body: list_len, name_type(0), name_len, name
        let mut sni = vec![0u8]; // name_type = host_name
        sni.extend_from_slice(&(host.len() as u16).to_be_bytes());
        sni.extend_from_slice(host);
        let mut ext_body = (sni.len() as u16).to_be_bytes().to_vec(); // list length
        ext_body.extend_from_slice(&sni);
        ext.extend_from_slice(&0u16.to_be_bytes()); // ext type 0 = SNI
        ext.extend_from_slice(&(ext_body.len() as u16).to_be_bytes());
        ext.extend_from_slice(&ext_body);

        let mut hs = Vec::new();
        hs.extend_from_slice(&[0u8; 2]); // client_version
        hs.extend_from_slice(&[0u8; 32]); // random
        hs.push(0); // session_id len
        hs.extend_from_slice(&2u16.to_be_bytes()); // cipher suites len
        hs.extend_from_slice(&[0x00, 0x2f]); // one cipher suite
        hs.push(1); // compression methods len
        hs.push(0); // null compression
        hs.extend_from_slice(&(ext.len() as u16).to_be_bytes()); // extensions len
        hs.extend_from_slice(&ext);

        let mut handshake = vec![0x01]; // ClientHello
        let len = hs.len();
        handshake.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8]);
        handshake.extend_from_slice(&hs);

        let mut record = vec![0x16, 0x03, 0x01];
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);

        assert_eq!(parse_sni(&record), Some("svt.se"));
        // Truncated input yields None (incomplete), not a panic.
        assert_eq!(parse_sni(&record[..10]), None);
        assert_eq!(parse_sni(b"not tls"), None);
    }

    #[test]
    fn json_body_is_parsed_inline() {
        let v = body_to_value(br#"{"a":1}"#, Some("application/json; charset=utf-8"));
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn binary_body_is_summarized() {
        let v = body_to_value(&[0, 1, 2, 3], Some("application/octet-stream"));
        assert_eq!(v["_binary"], true);
        assert_eq!(v["size"], 4);
    }

    #[test]
    fn multi_valued_headers_become_arrays() {
        let mut h = HeaderMap::new();
        h.append(
            HeaderName::from_static("set-cookie"),
            HeaderValue::from_static("a=1"),
        );
        h.append(
            HeaderName::from_static("set-cookie"),
            HeaderValue::from_static("b=2"),
        );
        let v = headers_to_value(&h);
        assert_eq!(v["set-cookie"], serde_json::json!(["a=1", "b=2"]));
    }

    #[test]
    fn client_response_drops_framing_and_hop_by_hop_headers() {
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("content-encoding"),
            HeaderValue::from_static("gzip"),
        );
        h.insert(
            HeaderName::from_static("content-length"),
            HeaderValue::from_static("5"),
        );
        h.insert(
            HeaderName::from_static("connection"),
            HeaderValue::from_static("keep-alive"),
        );
        h.insert(
            HeaderName::from_static("x-custom"),
            HeaderValue::from_static("v"),
        );

        let resp = client_response(StatusCode::OK, &h, empty_body());
        let headers = resp.headers();
        assert_eq!(headers.get("x-custom").unwrap(), "v");
        assert!(headers.get("content-encoding").is_none());
        assert!(headers.get("content-length").is_none());
        assert!(headers.get("connection").is_none());
    }

    #[test]
    fn forward_headers_strip_host_and_content_length() {
        let mut h = HeaderMap::new();
        h.insert(http::header::HOST, HeaderValue::from_static("example.com"));
        h.insert(
            HeaderName::from_static("content-length"),
            HeaderValue::from_static("3"),
        );
        h.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer x"),
        );
        h.insert(
            HeaderName::from_static("accept-encoding"),
            HeaderValue::from_static("gzip, br"),
        );
        let out = forward_headers(&h);
        assert!(out.get(http::header::HOST).is_none());
        assert!(out.get("content-length").is_none());
        // Stripped so reqwest manages + auto-decodes gzip (we log identity bodies).
        assert!(out.get("accept-encoding").is_none());
        assert_eq!(out.get("authorization").unwrap(), "Bearer x");
    }
}
