# vibecast

Turn any computer into a Chromecast. vibecast is a native Google Cast
receiver — it impersonates a Chromecast on your network so the Cast button in
supported apps works against a PC, HTPC, or media server instead of a dongle.
Cast from your phone; playback happens on the machine running vibecast.

It's a single Rust binary with no cloud dependency: it speaks the full CastV2
TLS protocol, advertises itself over mDNS, and runs an embedded Shaka Player
for playback. A Kodi add-on is included for boxes that prefer Kodi's player.

## Quick start

```sh
cargo run -p vibecast-cli -- --name "Living Room"
```

That's it — the receiver binds the standard Cast ports (8009/8008/8010) and
shows up in nearby Cast senders. Drop a `--name` flag or set
`[device] friendly_name` in `config.toml` to customize it.

You'll need a Cast device-auth certificate bundle (`certs.json`) in the data
directory (`$HOME/.vibecast` by default). Vibecast uses pre-harvested static
signatures for device auth — no runtime RSA signing.

## Bundled apps

| App | Notes |
| --- | --- |
| SVT Play | DASH + ditto manifests, ClearKey/Widevine |
| TV4 Play | OAuth refresh, Yospace ad-stitching, Widevine |
| Viaplay | Device-code auth, Widevine |
| Prime Video | Custom Widevine license flow, VOD + live |

## Configuration

Config lives at `{data_dir}/config.toml` (default data dir: `$HOME/.vibecast`).
A missing file yields Chromecast-like defaults; partial config overrides only
the keys you name. CLI flags (`--name`, `--cast-port`, `--bind-host`, etc.)
override config for one run.

```toml
[device]
friendly_name = "Living Room"

[apps.primevideo]
marketplace_id = "ATVPDKIKX0DER"
locale = "en-US"
```

## Writing an app

App crates depend only on `vibecast-sdk`. Implement `AppProvider` (a factory)
and `AppSession` (an owned per-launch session) — `resolve_media` turns a Cast
`LOAD` request into playable streams + DRM info. Model new apps on
`vibecast-apps-svtplay` (the reference app) and register them in
`crates/vibecast-cli/src/main.rs::apps`.

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
