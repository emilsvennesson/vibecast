# vibecast

[![CI](https://github.com/emilsvennesson/vibecast/actions/workflows/ci.yml/badge.svg)](https://github.com/emilsvennesson/vibecast/actions/workflows/ci.yml)
[![Release](https://github.com/emilsvennesson/vibecast/actions/workflows/release.yml/badge.svg)](https://github.com/emilsvennesson/vibecast/actions/workflows/release.yml)

Turn any computer into a Chromecast. vibecast is a native Google Cast
receiver — it impersonates a Chromecast on your network so the Cast button in
supported apps works against a PC, HTPC, or media server instead of a dongle.
Cast from your phone; playback happens on the machine running vibecast.

It's a single Rust binary with no cloud dependency: it speaks the full CastV2
TLS protocol, advertises itself over mDNS, and runs an embedded Shaka Player
for playback. A Kodi add-on is included for boxes that prefer Kodi's player.

## Quick start

```sh
cargo run -p vibecast-cli
```

Open `http://localhost:8010/` for the bundled browser player, or connect the
Kodi add-on. Each connected player becomes its own advertised Cast receiver
named `<player name> [vibecast]`; Cast and eureka ports are assigned dynamically.

You'll need a Cast device-auth certificate bundle (`certs.json`) in the data
directory (`$HOME/.vibecast` by default). Vibecast uses pre-harvested static
signatures for device auth — no runtime RSA signing.

## Install

Prebuilt artifacts are published on each [release](https://github.com/emilsvennesson/vibecast/releases).

```sh
# Homebrew (macOS Apple Silicon + Linux)
brew install emilsvennesson/vibecast/vibecast

# Docker / GHCR (multi-arch). mDNS needs host networking; mount a data dir.
docker run --rm --network host \
  -v "$HOME/.vibecast:/data" \
  ghcr.io/emilsvennesson/vibecast:latest --data-dir /data
```

Or grab a binary tarball / the Android APK directly from the release assets.
Build, CI, and release details live in [`docs/ci-cd.md`](docs/ci-cd.md).

## Bundled apps

| App | Notes |
| --- | --- |
| SVT Play | DASH + ditto manifests, ClearKey/Widevine |
| TV4 Play | OAuth refresh, Yospace ad-stitching, Widevine |
| Viaplay | Device-code auth, Widevine |
| Prime Video | Custom Widevine license flow, VOD + live |
| YouTube | Lounge control, generated DASH manifests, codec preference, optional SponsorBlock |

## Configuration

Receiver config lives at `{data_dir}/config.toml` (default data dir:
`$HOME/.vibecast`). A missing file yields Chromecast-like defaults; partial
config overrides only the keys you name. CLI flags override config for one run.

```toml
[device]
model = "Chromecast"

[network]
player_port = 8010
```

Apps declare typed runtime settings in their manifests. Values are stored in
`{data_dir}/settings.json` and synchronized with each connected player. The
bundled browser player and Kodi add-on render those settings generically.

## Writing an app

App crates depend only on `vibecast-sdk`. Implement `AppProvider` (a manifest +
factory) and `AppSession` (an owned per-launch session) — `resolve_media` turns
a Cast `LOAD` request into playable streams + DRM info. Model new apps on
`vibecast-apps-svtplay` and register them in
`crates/vibecast-platform/src/lib.rs::build_app_providers`.

```sh
cargo doc -p vibecast-sdk --open   # full app-author docs
```

## Kodi

`kodi/service.vibecast/` is a Python Kodi add-on that bridges Kodi's player to
vibecast's WebSocket endpoint. It's a **client** of the receiver, not part of
it — the Rust receiver serves the `/player` endpoint by default. See
[`kodi/service.vibecast/README.md`](kodi/service.vibecast/README.md).

## Status

Working receiver with the four bundled apps above. Limitations:

- No Windows CI (ubuntu + macos).
- `vibecast-bridge` uses `std::sync::Mutex::lock().unwrap()` in production
  paths (poison = panic; deliberate for a server).
- `cargo-deny` ignores one build-time-only advisory (`RUSTSEC-2024-0370`,
  proc-macro-error via the `xot` manifest crate); the shipped binary has no
  ignored vulnerabilities.

See [`AGENTS.md`](AGENTS.md) for the full developer guide (architecture,
layering, build/test/lint commands, conventions).

## License

MIT. See [`LICENSE`](LICENSE).
