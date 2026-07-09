# Changelog

## [0.1.0](https://github.com/emilsvennesson/vibecast/compare/v0.1.0...v0.1.0) (2026-07-09)


### ⚠ BREAKING CHANGES

* `PlatformInputs` and the FFI `ServerConfig` gain a `local_ip` field, and `CastAdvertisement` no longer owns the mDNS responder (use `MdnsResponder` under the `mdns` feature).

### Bug Fixes

* **android:** resolve ktlint chain-method-continuation violation ([d896f44](https://github.com/emilsvennesson/vibecast/commit/d896f441f80616e7be5bf450f6a1b4c178435ffd))


### Refactors

* gate mdns-sd behind a feature and inject reported LAN IP ([#51](https://github.com/emilsvennesson/vibecast/issues/51)) ([56bc33a](https://github.com/emilsvennesson/vibecast/commit/56bc33acd43e1c258d0cfc85ff1dd22cbdc2a6ed))


### Documentation

* **agents:** adopt conventional commits and PR title format ([#50](https://github.com/emilsvennesson/vibecast/issues/50)) ([0901aff](https://github.com/emilsvennesson/vibecast/commit/0901affa3c88ba4bb6a2d269a291dea8d542badc))

## [0.1.0](https://github.com/emilsvennesson/vibecast/releases/tag/v0.1.0) (2026-07-09)

Initial release of **vibecast** — a native Google Cast receiver written in Rust
that turns any computer into a Chromecast.

### Features

* Full CastV2 TLS protocol: device authentication, heartbeat, and the receiver
  namespace.
* Advertises as a Chromecast over mDNS and the eureka `/setup/eureka_info`
  HTTP/HTTPS endpoints.
* **Per-player receivers** — each connected player (browser Shaka page, Kodi
  add-on, native frontend) registers its capabilities and gets its own dedicated
  Cast device advertising that player's real DRM systems, codecs, resolution,
  HDR, and HDCP.
* Embedded Shaka Player bridge over HTTP/WebSocket with DRM license and
  DASH/HLS manifest proxying + normalization.
* Bundled apps: SVT Play, TV4 Play, Viaplay, and Prime Video.
* Desktop server (the `vibecast` CLI, Linux/macOS) and a native Android TV
  frontend via a UniFFI facade.
* Kodi add-on client for boxes that prefer Kodi's player.

### Artifacts

* Linux binaries (`x86_64`, `aarch64`) and a macOS binary (Apple Silicon).
* Multi-arch container image on GHCR (`linux/amd64`, `linux/arm64`).
* Signed Android APK.
* Homebrew formula (`brew install emilsvennesson/vibecast/vibecast`).
