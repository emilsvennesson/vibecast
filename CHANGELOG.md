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
