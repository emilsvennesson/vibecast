# Changelog

## [0.3.0](https://github.com/emilsvennesson/vibecast/compare/v0.2.0...v0.3.0) (2026-09-27)


### ⚠ BREAKING CHANGES

* **settings:** add typed per-player app settings ([#76](https://github.com/emilsvennesson/vibecast/issues/76))
* `PlatformInputs` and the FFI `ServerConfig` gain a `local_ip` field, and `CastAdvertisement` no longer owns the mDNS responder (use `MdnsResponder` under the `mdns` feature).

### Features

* **dazn:** add DAZN app provider ([#94](https://github.com/emilsvennesson/vibecast/issues/94)) ([d6061c0](https://github.com/emilsvennesson/vibecast/commit/d6061c0f5ebfdf6f9248a8364ed91475a6d623f0))
* **settings:** add typed per-player app settings ([#76](https://github.com/emilsvennesson/vibecast/issues/76)) ([bdc8cdb](https://github.com/emilsvennesson/vibecast/commit/bdc8cdb469cd429f351425ccb8c0b83e21bb5dc5))
* **tools:** standalone Cast dev tooling + primitives FFI facade ([#73](https://github.com/emilsvennesson/vibecast/issues/73)) ([e334de9](https://github.com/emilsvennesson/vibecast/commit/e334de954c578a4e86dca8627fc9eb04a16104e2))
* **youtube:** add optional SponsorBlock skipping ([#78](https://github.com/emilsvennesson/vibecast/issues/78)) ([1f89fba](https://github.com/emilsvennesson/vibecast/commit/1f89fba49289be2d9e88824a4140a5cbbb2a341c))
* **youtube:** YouTube receiver app with adaptive DASH streaming ([#74](https://github.com/emilsvennesson/vibecast/issues/74)) ([872c901](https://github.com/emilsvennesson/vibecast/commit/872c9015231608def674657ad81f2c8c5879df17))


### Bug Fixes

* **android:** resolve ktlint chain-method-continuation violation ([d896f44](https://github.com/emilsvennesson/vibecast/commit/d896f441f80616e7be5bf450f6a1b4c178435ffd))
* **ci:** correct Android path filter; document CI/CD in AGENTS.md ([#69](https://github.com/emilsvennesson/vibecast/issues/69)) ([250bd67](https://github.com/emilsvennesson/vibecast/commit/250bd676aea9bbfa408ad273950d9039d1adab71))
* **discovery:** stable per-player identity and base-only mDNS advertisement ([#72](https://github.com/emilsvennesson/vibecast/issues/72)) ([8af03f6](https://github.com/emilsvennesson/vibecast/commit/8af03f67d98dc6abea274c365bfb6bb613bdaa57)), closes [#49](https://github.com/emilsvennesson/vibecast/issues/49)
* **platform:** widen concurrent installation-id creation retry window ([#102](https://github.com/emilsvennesson/vibecast/issues/102)) ([4a73ca9](https://github.com/emilsvennesson/vibecast/commit/4a73ca985b10563f7a213b23dcec5fb78bff20da))
* **youtube:** playable streams via ANDROID client + Lounge control fixes ([#97](https://github.com/emilsvennesson/vibecast/issues/97)) ([c46d05d](https://github.com/emilsvennesson/vibecast/commit/c46d05dc9b451e328f6aafb91b1eff676b55eec8))
* **youtube:** session parity with a real Cast device (Now playing, reconnect, bot check) ([#98](https://github.com/emilsvennesson/vibecast/issues/98)) ([85c8074](https://github.com/emilsvennesson/vibecast/commit/85c807402cdce388182cf5e82a08e980bc14c9b9))
* **youtube:** stop Lounge discovery request loop ([#77](https://github.com/emilsvennesson/vibecast/issues/77)) ([b4616f8](https://github.com/emilsvennesson/vibecast/commit/b4616f8f399be706a1409ed21922aa2df892e303))


### Refactors

* gate mdns-sd behind a feature and inject reported LAN IP ([#51](https://github.com/emilsvennesson/vibecast/issues/51)) ([56bc33a](https://github.com/emilsvennesson/vibecast/commit/56bc33acd43e1c258d0cfc85ff1dd22cbdc2a6ed))


### Documentation

* **agents:** adopt conventional commits and PR title format ([#50](https://github.com/emilsvennesson/vibecast/issues/50)) ([0901aff](https://github.com/emilsvennesson/vibecast/commit/0901affa3c88ba4bb6a2d269a291dea8d542badc))
* rewrite README and Android README ([#99](https://github.com/emilsvennesson/vibecast/issues/99)) ([4a5acb8](https://github.com/emilsvennesson/vibecast/commit/4a5acb8f243d38881b76febb66da053cc10dde8d))

## [0.2.0](https://github.com/emilsvennesson/vibecast/compare/v0.1.0...v0.2.0) (2026-09-27)


### ⚠ BREAKING CHANGES

* **settings:** add typed per-player app settings ([#76](https://github.com/emilsvennesson/vibecast/issues/76))

### Features

* **dazn:** add DAZN app provider ([#94](https://github.com/emilsvennesson/vibecast/issues/94)) ([d6061c0](https://github.com/emilsvennesson/vibecast/commit/d6061c0f5ebfdf6f9248a8364ed91475a6d623f0))
* **settings:** add typed per-player app settings ([#76](https://github.com/emilsvennesson/vibecast/issues/76)) ([bdc8cdb](https://github.com/emilsvennesson/vibecast/commit/bdc8cdb469cd429f351425ccb8c0b83e21bb5dc5))
* **tools:** standalone Cast dev tooling + primitives FFI facade ([#73](https://github.com/emilsvennesson/vibecast/issues/73)) ([e334de9](https://github.com/emilsvennesson/vibecast/commit/e334de954c578a4e86dca8627fc9eb04a16104e2))
* **youtube:** add optional SponsorBlock skipping ([#78](https://github.com/emilsvennesson/vibecast/issues/78)) ([1f89fba](https://github.com/emilsvennesson/vibecast/commit/1f89fba49289be2d9e88824a4140a5cbbb2a341c))
* **youtube:** YouTube receiver app with adaptive DASH streaming ([#74](https://github.com/emilsvennesson/vibecast/issues/74)) ([872c901](https://github.com/emilsvennesson/vibecast/commit/872c9015231608def674657ad81f2c8c5879df17))


### Bug Fixes

* **ci:** correct Android path filter; document CI/CD in AGENTS.md ([#69](https://github.com/emilsvennesson/vibecast/issues/69)) ([250bd67](https://github.com/emilsvennesson/vibecast/commit/250bd676aea9bbfa408ad273950d9039d1adab71))
* **discovery:** stable per-player identity and base-only mDNS advertisement ([#72](https://github.com/emilsvennesson/vibecast/issues/72)) ([8af03f6](https://github.com/emilsvennesson/vibecast/commit/8af03f67d98dc6abea274c365bfb6bb613bdaa57)), closes [#49](https://github.com/emilsvennesson/vibecast/issues/49)
* **platform:** widen concurrent installation-id creation retry window ([#102](https://github.com/emilsvennesson/vibecast/issues/102)) ([4a73ca9](https://github.com/emilsvennesson/vibecast/commit/4a73ca985b10563f7a213b23dcec5fb78bff20da))
* **youtube:** playable streams via ANDROID client + Lounge control fixes ([#97](https://github.com/emilsvennesson/vibecast/issues/97)) ([c46d05d](https://github.com/emilsvennesson/vibecast/commit/c46d05dc9b451e328f6aafb91b1eff676b55eec8))
* **youtube:** session parity with a real Cast device (Now playing, reconnect, bot check) ([#98](https://github.com/emilsvennesson/vibecast/issues/98)) ([85c8074](https://github.com/emilsvennesson/vibecast/commit/85c807402cdce388182cf5e82a08e980bc14c9b9))
* **youtube:** stop Lounge discovery request loop ([#77](https://github.com/emilsvennesson/vibecast/issues/77)) ([b4616f8](https://github.com/emilsvennesson/vibecast/commit/b4616f8f399be706a1409ed21922aa2df892e303))


### Documentation

* rewrite README and Android README ([#99](https://github.com/emilsvennesson/vibecast/issues/99)) ([4a5acb8](https://github.com/emilsvennesson/vibecast/commit/4a5acb8f243d38881b76febb66da053cc10dde8d))

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
