# service.vibecast

Kodi service add-on that bridges Kodi playback to a vibecast receiver.

It connects to vibecast's player WebSocket endpoint (`/player?role=primary`),
executes playback commands, and reports Kodi playback state back to vibecast.

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

Start vibecast so Kodi can reach the player server endpoint, for example:

```bash
uv run python -m vibecast --manifest /path/to/manifest.json --name "Living Room" --player-host 0.0.0.0 --player-port 8010
```

If you run vibecast and Kodi on different machines, ensure firewall/network rules
allow Kodi to reach `http://<vibecast-host>:8010` and
`ws://<vibecast-host>:8010/player`.
