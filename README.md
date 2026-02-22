# vibecast

`vibecast` is a Python asyncio Google Cast receiver implementation.

It accepts TLS CastV2 connections from real senders (Chrome/iOS/Android),
handles device auth + platform namespaces, and routes app-specific behavior to
providers (built-in providers: Viaplay and SVT Play).

## Current capabilities

- TLS Cast receiver on port `8009` (configurable)
- Built-in player mediator server on port `8010` (configurable):
  - `GET /` and `GET /index.html` (embedded Shaka web player)
  - `GET /player.js` (embedded player script)
  - `GET /player` (WebSocket command/report channel)
  - `POST /license/{session_id}?route=<route_id>` (DRM license proxy)
- Device auth response with cert chain + CRL (`sig_sha1`/SHA1 or legacy
  `sig`/SHA256 signatures)
- Core platform namespaces:
  - `urn:x-cast:com.google.cast.tp.connection`
  - `urn:x-cast:com.google.cast.tp.heartbeat`
  - `urn:x-cast:com.google.cast.receiver`
  - `urn:x-cast:com.google.cast.receiver.discovery`
  - `urn:x-cast:com.google.cast.multizone`
  - `urn:x-cast:com.google.cast.setup`
- Provider API with app launch/session callbacks
- Playback coordinator handling generic media namespace flows for providers
- Persistent receiver state under `--data-dir` (stable device ID, provider data)
- mDNS advertisement (`_googlecast._tcp.local`)

## Requirements

- Python `3.12+`
- [`uv`](https://docs.astral.sh/uv/)
- Cast certificate manifest JSON (see below)
- Optional: `mitmproxy` (for provider protocol capture script)

## Install

```bash
uv sync
```

For local development, install editable so provider entry points resolve in the
active environment:

```bash
uv pip install -e .
```

## Certificate manifest

`vibecast` expects a go-cast style manifest JSON with at least:

- `pu`: peer cert PEM
- `pr`: peer private key PEM
- `cpu`: device auth cert PEM
- `ica`: intermediate cert chain PEM (can include multiple cert blocks)
- one signature field:
  - preferred `sig_sha1` (base64 signature for `SHA1(peer_cert_der)`), or
  - fallback `sig` (base64 signature for `SHA256(peer_cert_der)`)

Optional:

- `crl`: base64 CRL blob; if absent, `vibecast` fetches CRL at startup.

## Getting fresh Shield certs

If you use the Shield extraction workflow from
[go-cast](https://github.com/tristanpenman/go-cast):

```bash
cd /path/to/shield-analysis
python3 extract_cast_creds.py --output-dir output
```

This produces `output/manifest.json`.

If your signing daemon is running and you want to add a `sig_sha1` entry to
that manifest, generate it using the daemon and update the manifest locally.

Do not commit extracted cert material or local cert scripts.

## Run receiver

Basic:

```bash
uv run python -m vibecast \
  --manifest /path/to/manifest.json \
  --name "Living Room" \
  --log-level INFO
```

Bind explicit Cast + player server host/ports:

```bash
uv run python -m vibecast \
  --manifest /path/to/manifest.json \
  --name "Living Room" \
  --host 0.0.0.0 \
  --port 8009 \
  --player-host 0.0.0.0 \
  --player-port 8010 \
  --log-level DEBUG
```

CLI options:

- `--manifest` (required): path to manifest JSON
- `--name` (required): friendly Cast device name
- `--model`: advertised model (default `Chromecast`)
- `--device-id`: stable mDNS/discovery device ID (default: persisted in
  `~/.vibecast/cast_receiver_device_id`)
- `--data-dir`: persistent receiver data directory (default `~/.vibecast`)
- `--host`: bind host/interface (default `0.0.0.0`)
- `--port`: bind port (default `8009`)
- `--player-host`: bind host/interface for player server (default `0.0.0.0`)
- `--player-port`: bind port for player server (default `8010`)
- `--log-level`: `DEBUG|INFO|WARNING|ERROR` (default `INFO`)

## External player endpoints

Once the receiver is running, external players connect to:

- Browser player: `http://<receiver-ip>:<player-port>/`
  - same page is also available at `http://<receiver-ip>:<player-port>/index.html`
  - built-in Shaka player auto-connects as `role=primary`
- Script asset: `http://<receiver-ip>:<player-port>/player.js`
- WebSocket: `ws://<receiver-ip>:<player-port>/player`
  - optional role query: `?role=primary` or `?role=observer`
- License proxy: `http://<receiver-ip>:<player-port>/license/<session-id>`

Kodi integration:

- A ready-to-install Kodi service add-on is available under
  `kodi/service.vibecast`.

## Quick verification

1. Confirm listener:

```bash
lsof -nP -iTCP:8009 -sTCP:LISTEN
lsof -nP -iTCP:8010 -sTCP:LISTEN
```

2. Confirm Cast mDNS service appears:

```bash
dns-sd -B _googlecast._tcp local
```

3. Check startup logs include:

- CRL fetch/source
- server listening host/port
- registered mDNS service and advertised addresses

## Providers

Provider discovery uses Python entry points under `vibecast.providers`.

Built-in providers in this repo:

- `SvtPlayProvider` (`appId` `95370A1C`)
- `ViaplayProvider` (`appId` `6313CF39`, `2DB7CC49`)

## Capturing new provider protocols (optional)

Use `scripts/capture_provider.py` to proxy Cast traffic between a real sender
and a real Cast receiver while writing a structured JSONL capture.

```bash
uv run python scripts/capture_provider.py \
  --manifest /path/to/manifest.json \
  --upstream <real-receiver-ip>
```

Optional full capture with HTTP interception:

```bash
uv run python scripts/capture_provider.py \
  --manifest /path/to/manifest.json \
  --upstream <real-receiver-ip> \
  --enable-mitm
```

Sanity-check discovery:

```bash
uv run python - <<'PY'
from vibecast.provider import discover_providers
providers = discover_providers()
print([type(p).__name__ for p in providers])
PY
```

If this prints `[]`, install editable (`uv pip install -e .`) in the same env
you use to run `vibecast`.

## Development checks

```bash
uv run pytest
uv run ruff check .
uv run ruff format --check .
uv run basedpyright --warnings
uv run deptry .
```

## Troubleshooting

### No logs printed

Run with `--log-level INFO` or `--log-level DEBUG`.

### Device appears briefly then disconnects

Check receiver logs around:

- device auth challenge/response fields (`hash`, `sig`, `nonce_len`, `crl_len`)
- sender `LAUNCH` payload and follow-up requests

For iOS senders especially, ensure:

- fresh cert material
- valid signature material is present (`sig_sha1` preferred, `sig` supported)
- CRL is included

### `LAUNCH_ERROR: Application not available`

Provider for that app ID is not registered/discovered. Verify entry points and
active environment (`uv pip install -e .`, then re-check discovery).

### iPhone cannot discover receiver

- Same subnet/SSID for phone + host
- No VPN/Private Relay blocking mDNS
- App has Local Network permission
- Firewall allows incoming Python process
