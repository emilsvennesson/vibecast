# Changelog

## [1.0.0](https://github.com/emilsvennesson/vibecast/compare/v0.1.0...v1.0.0) (2026-07-13)


### ⚠ BREAKING CHANGES

* **settings:** add typed per-player app settings ([#76](https://github.com/emilsvennesson/vibecast/issues/76))

### Features

* **settings:** add typed per-player app settings ([#76](https://github.com/emilsvennesson/vibecast/issues/76)) ([bdc8cdb](https://github.com/emilsvennesson/vibecast/commit/bdc8cdb469cd429f351425ccb8c0b83e21bb5dc5))
* **tools:** standalone Cast dev tooling + primitives FFI facade ([#73](https://github.com/emilsvennesson/vibecast/issues/73)) ([e334de9](https://github.com/emilsvennesson/vibecast/commit/e334de954c578a4e86dca8627fc9eb04a16104e2))
* **youtube:** YouTube receiver app with adaptive DASH streaming ([#74](https://github.com/emilsvennesson/vibecast/issues/74)) ([872c901](https://github.com/emilsvennesson/vibecast/commit/872c9015231608def674657ad81f2c8c5879df17))


### Bug Fixes

* **ci:** correct Android path filter; document CI/CD in AGENTS.md ([#69](https://github.com/emilsvennesson/vibecast/issues/69)) ([250bd67](https://github.com/emilsvennesson/vibecast/commit/250bd676aea9bbfa408ad273950d9039d1adab71))
* **discovery:** stable per-player identity and base-only mDNS advertisement ([#72](https://github.com/emilsvennesson/vibecast/issues/72)) ([8af03f6](https://github.com/emilsvennesson/vibecast/commit/8af03f67d98dc6abea274c365bfb6bb613bdaa57)), closes [#49](https://github.com/emilsvennesson/vibecast/issues/49)
* **youtube:** stop Lounge discovery request loop ([#77](https://github.com/emilsvennesson/vibecast/issues/77)) ([b4616f8](https://github.com/emilsvennesson/vibecast/commit/b4616f8f399be706a1409ed21922aa2df892e303))

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
