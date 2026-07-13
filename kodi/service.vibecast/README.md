# service.vibecast

Kodi service add-on that bridges Kodi playback to a vibecast receiver.

It connects to vibecast's player WebSocket endpoint (`/player`), **registers**
its identity and capabilities (name, platform, codecs, resolution, DRM), then
executes playback commands and reports Kodi playback state back to vibecast.
vibecast gives this player its own Cast device named `<player name> [vibecast]`
(the `[vibecast]` suffix is added by the server; the base name defaults to
`Kodi` and is configurable under the add-on's **Player** settings, along with
resolution, codecs, HDR, and Widevine level — all auto-detected by default).
Server-provided app settings are available through **Player** -> **App settings...**.
They remain server-authoritative and become read-only while disconnected.

## Requirements

- Kodi `21.3+`
- vibecast running with player server enabled (default port `8010`)
- Dependencies from Kodi repo:
  - `script.module.inputstreamhelper`
  - `script.module.websocket`

## Install

1. Zip the add-on directory:

   ```bash
   cd kodi
   zip -r service.vibecast.zip service.vibecast
   ```

2. In Kodi: `Add-ons` -> `Install from zip file` -> select
   `service.vibecast.zip`.

3. Configure add-on settings:
   - `vibecast host`: host where vibecast player server is reachable
   - `vibecast port`: usually `8010`

## vibecast side

Start the vibecast receiver so Kodi can reach the player server endpoint. The
player bridge is on by default at `:8010` — no separate flag is needed:

```bash
cargo run -p vibecast-cli -- --name "Living Room"
```

or, after `cargo build -p vibecast-cli --release`:

```bash
./target/release/vibecast --name "Living Room"
```

If you run vibecast and Kodi on different machines, ensure firewall/network rules
allow Kodi to reach `http://<vibecast-host>:8010` and
`ws://<vibecast-host>:8010/player`.
