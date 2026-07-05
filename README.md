# vibecast

A native Google Cast receiver written in Rust. vibecast impersonates a
Chromecast: it speaks the CastV2 TLS protocol (device authentication, heartbeat,
receiver namespace), advertises itself over mDNS + the eureka `/setup/eureka_info`
HTTP endpoint, and routes `LAUNCH`/`LOAD` requests to bundled app providers. A
player bridge serves an embedded [Shaka Player](https://github.com/shaka-project/shaka-player)
page over HTTP/WebSocket and proxies DRM license + DASH/HLS manifest requests
(with normalization).

## Status

Working receiver with bundled apps for SVT Play, TV4 Play, Viaplay, and Amazon
Prime Video. See [Limitations](#limitations) for known gaps.

## Requirements

- Rust 1.85+ (stable). No `protoc` binary needed — protobuf is compiled at build
  time via `protox` (pure Rust).
- A Cast device-auth certificate bundle (`certs.json`) in the data directory.
  Device auth uses pre-harvested static signatures; no runtime RSA signing.

## Build & run

```sh
cargo run -p vibecast-cli                          # binary named `vibecast`
cargo run -p vibecast-cli -- --name "Living Room"  # override friendly name
cargo build -p vibecast-cli --release              # release binary
```

### CLI flags

Flags override matching `config.toml` values:

| Flag | Purpose |
| --- | --- |
| `--certs <path>` | Certificate manifest path (overrides `[device].certs`) |
| `--data-dir <path>` | Data directory (default: `$HOME/.vibecast`) |
| `--name <name>` | Friendly name advertised to senders |
| `--model <model>` | Device model string |
| `--bind-host <host>` | Host/interface to bind listeners (default: `0.0.0.0`) |
| `--cast-port <port>` | CastV2 TLS port (standard: `8009`) |
| `--device-id <id>` | Stable device id (default: random UUID) |
| `--log-level <level>` | `trace|debug|info|warn|error` (overrides `RUST_LOG`) |

### Configuration

Config lives at `{data_dir}/config.toml` (data dir defaults to `$HOME/.vibecast`).
A missing file yields Chromecast-like defaults. Sections:

- `[device]` — friendly name, model, manufacturer, locale, certs path, display
  dimensions, eureka capabilities.
- `[network]` — bind host, ports (CastV2 `8009`, player `8010`, eureka HTTP
  `8008`, eureka HTTPS `8443`), HTTP timeout, cert-rotation poll interval.
- `[volume]` — initial level, muted, step granularity.
- `[cast]` — firmware build version/revision, User-Agent, device-capabilities header.
- `[apps.<app_key>]` — per-app config tables passed to `AppProvider::configure`.

All sections use `#[serde(default)]` per-field fallbacks and
`deny_unknown_fields`, so partial config overrides only the named keys and
unknown keys are rejected with a clear error.

Example:

```toml
[device]
friendly_name = "Living Room"

[network]
cast_port = 8009

[apps.primevideo]
marketplace_id = "ATVPDKIKX0DER"
locale = "en-US"
```

### Default ports

| Service | Port |
| --- | --- |
| CastV2 TLS | 8009 |
| Player bridge (HTTP/WebSocket) | 8010 |
| Eureka discovery HTTP | 8008 |
| Eureka discovery HTTPS | 8443 |

## Architecture

vibecast is a Cargo workspace of 13 focused crates with strict layering:

```
vibecast-proto        CastV2 protobuf + length-prefixed framing          (leaf)
vibecast-security     Device-auth material + TLS cert rotation           (leaf)
vibecast-cast         CastV2 TLS transport: connection actor             (proto, security)
vibecast-discovery    mDNS advertisement + eureka HTTP/HTTPS             (security)
vibecast-messages     Cast JSON message models (serde)                   (leaf)
vibecast-bridge       Player bridge: WebSocket relay + DRM/manifest proxy (messages)
vibecast-sdk          Stable app-author SDK                              (messages)
vibecast-apps-*       Bundled apps (SVT Play, TV4 Play, Viaplay, Prime)  (sdk ONLY)
vibecast-core         Receiver runtime: device hub + coordinator         (cast, messages, bridge, sdk)
vibecast-cli          Platform binary: wires everything into a server    (all)
```

The portable core (`proto` → `security`/`cast`/`discovery`/`messages`/`bridge`/
`sdk`/`core`) never sees config, IO, TLS, or `anyhow` — those live in the CLI
binary. The runtime is built on actors + channels rather than shared-mutable
state: each Cast connection is an actor, the device hub is a single-task actor,
and slow app callbacks run on per-session ordered tasks so one app can't stall
routing.

### Bundled apps

| App | Cast app id | Notes |
| --- | --- | --- |
| SVT Play | `95370A1C` | DASH + ditto manifests, ClearKey/Widevine |
| TV4 Play | `B6470434` | OAuth refresh, Yospace ad-stitching, Widevine |
| Viaplay | `6313CF39`, `2DB7CC49` | Device-code auth, Widevine |
| Prime Video | `17608BC8` | Custom Widevine license flow, VOD + live |

### Security model

Device authentication uses **pre-harvested static signatures** (SHA-1 and
SHA-256) loaded from a certificate manifest — no runtime RSA signing, so the
vulnerable `rsa` crate (RUSTSEC-2023-0071 Marvin attack) is explicitly banned in
`deny.toml`. TLS keys are parsed via `rustls-pki-types` `PemObject` (the
unmaintained `rustls-pemfile` is avoided — RUSTSEC-2025-0134). The
`CertResolver` is lock-free and hot-swappable via `arc-swap`, so certificate
rotation doesn't drop in-flight handshakes.

No secrets are logged: tokens, license challenges/responses, cert private keys,
and device-auth signatures never appear in tracing output.

## Writing an app

App crates depend **only** on `vibecast-sdk`. Implement `AppProvider` (a factory)
and `AppSession` (an owned, per-launch session):

```rust
use std::sync::Arc;
use vibecast_sdk::{
    AppContext, AppConfig, AppConfigError, AppProvider, AppSession, LaunchCredentials,
    LaunchError, LoadRequest, MediaResolveError, PlaybackMedia,
};

pub struct MyApp;

#[async_trait::async_trait]
impl AppProvider for MyApp {
    fn app_ids(&self) -> &'static [&'static str] { &["DEADBEEF"] }
    fn display_name(&self) -> &'static str { "My App" }
    fn app_key(&self) -> &'static str { "myapp" }

    async fn launch(
        &self,
        ctx: &AppContext,
        credentials: LaunchCredentials,
    ) -> Result<Arc<dyn AppSession>, LaunchError> {
        Ok(Arc::new(MySession))
    }
}

pub struct MySession;

#[async_trait::async_trait]
impl AppSession for MySession {
    async fn resolve_media(
        &self,
        ctx: &AppContext,
        request: &LoadRequest,
    ) -> Result<PlaybackMedia, MediaResolveError> {
        // resolve request.media.content_id into a PlaybackMedia with streams + DRM
        todo!("resolve media")
    }
}
```

Then register the provider in `crates/vibecast-cli/src/main.rs::apps`.
`vibecast-apps-svtplay` is the reference app — model new apps on it.

Generate SDK docs with: `cargo doc -p vibecast-sdk --no-deps --open`.

## Kodi add-on

`kodi/service.vibecast/` is a Python Kodi add-on that bridges Kodi playback to
vibecast's player WebSocket endpoint (`/player?role=primary`). It is a **client**
of the receiver, not part of the receiver. The Rust receiver serves the `/player`
endpoint the add-on connects to; no special flags are needed. See
[`kodi/service.vibecast/README.md`](kodi/service.vibecast/README.md).

## Testing

```sh
cargo nextest run --all-features --profile ci     # main suite (CI profile)
cargo test --doc --all-features                   # doctests
```

Tests validate real behavior — protocol round-trips, state transitions, error
paths, regressions. The cast crate tests over in-memory duplex streams and a real
TLS handshake; the bridge crate tests WebSocket fanout and proxy round-trips; the
core crate drives an end-to-end launch → load → play harness; the app crates use
`wiremock` to exercise real resolve flows.

## Supply-chain checks

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check                                  # advisories + licenses + bans
cargo doc --no-deps --all-features                # must be warning-free
```

`cargo deny` guards advisories, licenses, and bans. The `rsa` crate is explicitly
denied. One build-time-only advisory (`RUSTSEC-2024-0370`, proc-macro-error,
pulled in transitively by the `xot` manifest-normalization crate) is ignored;
it's never linked into the shipped binary.

## Limitations

- `vibecast-bridge` uses `std::sync::Mutex::lock().unwrap()` in production paths
  (poison = panic). Deliberate for a server; revisit only if poison-panic becomes
  undesirable.
- CI matrix is ubuntu + macos (no Windows).
- Capture/dev tooling in `scripts/` is gitignored and untracked — reverse-
  engineering utilities used during development, not part of the receiver.

## License

MIT. See [`LICENSE`](LICENSE).
