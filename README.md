# vibecast

[![CI](https://github.com/emilsvennesson/vibecast/actions/workflows/ci.yml/badge.svg)](https://github.com/emilsvennesson/vibecast/actions/workflows/ci.yml)
[![Release](https://github.com/emilsvennesson/vibecast/actions/workflows/release.yml/badge.svg)](https://github.com/emilsvennesson/vibecast/actions/workflows/release.yml)

vibecast is a native Google Cast receiver written in Rust. It implements the
CastV2 protocol and device authentication and advertises itself over mDNS, so
unmodified sender apps can cast to any connected player as if it were a
Chromecast.

Players connect to vibecast over a WebSocket and register their capabilities
(resolution, codecs, HDR, DRM security level). Each one is advertised as a
separate Cast device. vibecast handles the Cast session and app logic, and the
player only has to play the stream it's given.

```mermaid
flowchart LR
    sender["Sender app<br/>(phone, browser)"]

    subgraph vibecast
        receiver["Cast receiver<br/>(one per player)"]
        apps["Apps<br/>(YouTube, Prime Video, …)"]
        bridge["Player bridge<br/>(DRM + manifest proxy)"]
        receiver --> apps --> bridge
    end

    sender -- "mDNS + CastV2" --> receiver
    bridge -- "WebSocket" --> kodi["Kodi"]
    bridge -- "WebSocket" --> browser["Browser"]
    bridge -- "WebSocket" --> other["Other players"]
```

## Supported apps

- YouTube (with optional SponsorBlock)
- Prime Video
- SVT Play
- TV4 Play
- Viaplay
- DAZN

## Platforms

**Receiver (server)**

- **Linux** (x86_64, aarch64) and **macOS** (Apple Silicon): the `vibecast`
  binary, available through Homebrew, Docker, or release tarballs.
- **Android TV**: an APK that runs the receiver as a foreground service,
  alongside the device's built-in Cast receiver. It has no native player yet, so
  playback goes through one of the players below. See
  [`android/README.md`](android/README.md).

**Players**

- **Browser**: the receiver serves a Shaka Player page at `http://<host>:8010/`.
  It's intended for development and testing.
- **Kodi** (21.3+): the add-on in [`kodi/service.vibecast/`](kodi/service.vibecast/README.md)
  plays streams with Kodi's own player and Widevine through inputstream.adaptive.

## Install

Prebuilt binaries, the Android APK, and Docker images are published with each
[release](https://github.com/emilsvennesson/vibecast/releases).

```sh
# Homebrew (macOS Apple Silicon + Linux)
brew install emilsvennesson/vibecast/vibecast

# Docker (multi-arch). mDNS requires host networking.
docker run --rm --network host \
  -v "$HOME/.vibecast:/data" \
  ghcr.io/emilsvennesson/vibecast:latest --data-dir /data

# From source
cargo run -p vibecast-cli --release
```

Start `vibecast`, then open the browser player or connect the Kodi add-on.
The Cast device appears in your phone's Cast menu once a player is connected.

## Certificates

Senders require Cast device authentication: a certificate chain rooted in
Google's Cast CA plus a signed auth response. Passing it requires device-auth
material from a real Cast device, and **none is included in this repository or
its releases**.

Put the material in `certs.json` in the data directory (`~/.vibecast/certs.json`
by default, or set a different path with `--certs`):

```jsonc
{
  "cpu": "-----BEGIN CERTIFICATE-----…",   // device certificate (PEM)
  "ica": "-----BEGIN CERTIFICATE-----…",   // intermediate CA chain (PEM)
  "crl": "…",                              // optional, base64
  "certs": [
    {
      "pu": "-----BEGIN CERTIFICATE-----…", // peer (TLS) certificate (PEM)
      "pr": "-----BEGIN PRIVATE KEY-----…", // its private key (PEM)
      "sig_sha1": "…",                      // pre-computed auth signature, base64
      "sig_sha256": "…"                     // pre-computed auth signature, base64
    }
  ]
}
```

Each peer certificate is only valid for a limited time. You can list several
entries under `certs`, and vibecast uses whichever one is currently valid,
switching automatically when it expires. When no entry is valid, senders will
reject the device.

On Android, copy the same file into the app's private storage with adb. See
[`android/README.md`](android/README.md#install-and-provision-certificates).

## Configuration

Everything is optional. vibecast reads `~/.vibecast/config.toml`:

```toml
[device]
model = "Chromecast"

[network]
player_port = 8010
```

Command-line flags override the config file: `--data-dir`, `--certs`,
`--model`, `--bind-host`, `--player-port`, `--log-level`.

## License

MIT. See [`LICENSE`](LICENSE).
