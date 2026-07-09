# Changelog

## [0.1.0](https://github.com/emilsvennesson/vibecast/compare/v0.1.0...v0.1.0) (2026-07-09)


### ⚠ BREAKING CHANGES

* `PlatformInputs` and the FFI `ServerConfig` gain a `local_ip` field, and `CastAdvertisement` no longer owns the mDNS responder (use `MdnsResponder` under the `mdns` feature).

### Bug Fixes

* **android:** resolve ktlint chain-method-continuation violation in ([e45e2c9](https://github.com/emilsvennesson/vibecast/commit/e45e2c941e9f24cb2ce9244c1f436686a9b0981d))
* **ci:** keep release-please job non-skipped so release builds run ([#61](https://github.com/emilsvennesson/vibecast/issues/61)) ([a780cc5](https://github.com/emilsvennesson/vibecast/commit/a780cc5c25611ae2c82fb5ff4102bc0620e4d013))
* **ci:** run release build jobs on dispatch despite skipped release-please ([#60](https://github.com/emilsvennesson/vibecast/issues/60)) ([a2ea878](https://github.com/emilsvennesson/vibecast/commit/a2ea878e14bf56c962de7bf462f81aaee0269c7c))


### Refactors

* gate mdns-sd behind a feature and inject reported LAN IP ([#51](https://github.com/emilsvennesson/vibecast/issues/51)) ([56bc33a](https://github.com/emilsvennesson/vibecast/commit/56bc33acd43e1c258d0cfc85ff1dd22cbdc2a6ed))


### Documentation

* **agents:** adopt conventional commits and PR title format ([#50](https://github.com/emilsvennesson/vibecast/issues/50)) ([0901aff](https://github.com/emilsvennesson/vibecast/commit/0901affa3c88ba4bb6a2d269a291dea8d542badc))
