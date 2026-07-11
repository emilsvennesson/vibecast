# vibecast capture (dev tool)

A standalone developer tool for reverse-engineering Google Cast apps. It is
**not** part of the vibecast binary and pulls in no Python at build time — it is
a separate [uv](https://docs.astral.sh/uv/)-managed project that consumes the
`vibecast-primitives-ffi` Rust crate through its generated Python bindings.

What it does, in one process:

- **Cast MITM proxy** — relays CastV2 between a sender (phone) and a genuine
  receiver (e.g. a Shield), answering device-auth locally with the harvested
  certificate, advertising itself over mDNS, and logging every message to
  `cast.jsonl`. All the Cast protocol logic comes from the Rust primitives
  (`CertBundle`, `try_parse_frame`/`serialize_frame`, `decode_payload_json`,
  `CastAdvertiser`) — there is no protocol re-implementation here.
- **HTTP/HTTPS capture** — runs [mitmproxy](https://mitmproxy.org) as a library
  in **WireGuard mode**; the receiver's decrypted egress is logged to
  `http.jsonl`.

Merge `cast.jsonl` + `http.jsonl` by their `ts` field to see how an app drives
the device and talks to its backend.

## One-time setup

On the host (macOS/Linux):

1. Install [uv](https://docs.astral.sh/uv/) and
   [mitmproxy](https://mitmproxy.org) (`brew install mitmproxy`). Running
   `mitmdump` once creates `~/.mitmproxy/` (CA + WireGuard keys).
2. Generate the Rust bindings: `./regenerate-bindings.sh`.
3. `uv sync`.

On the Android device (rooted; this tool assumes a Shield reachable over adb):

1. Trust the mitmproxy CA in the **system** store (e.g. a Magisk cert module) —
   user-store CAs are not trusted by streaming apps.
2. Install the WireGuard app and import a tunnel (default name `wg_mitm`) whose
   `Endpoint` is this host's LAN IP `:51820` and whose `[Peer]` keys match
   `~/.mitmproxy/wireguard.conf`. Set `AllowedIPs` to route WAN traffic but
   **exclude this LAN subnet**, so CastV2 + mDNS stay local (subnet-subtract your
   `/24` from `0.0.0.0/0`).
3. Enable the tunnel yourself in the WireGuard app before casting (and disable it
   when done). **This tool never toggles the tunnel** — it only prints the
   requirement and, if adb is reachable, a read-only "detected up" hint.

## Usage

```sh
uv run capture.py --certs ~/.vibecast/certs.json --upstream 192.168.2.6
```

Then cast to the advertised device from a sender and drive the app. `Ctrl-C`
stops and writes the session to `captures/<name>/`.

Cast-only (no HTTP capture, no WireGuard/adb):

```sh
uv run capture.py --certs ~/.vibecast/certs.json --upstream 192.168.2.6 --no-http
```

Key flags: `--listen-port` (default 8009), `--upstream-port` (8009), `--out`,
`--name`, `--model`, `--tunnel` (`wg_mitm`), `--wg-port` (51820), `--adb-serial`,
`--local-ip`. See `uv run capture.py --help`.

### Presenting as a different device

The proxy advertises itself as a **distinct** Cast device (its own model, name,
and stable device id) at two layers, both driven by the same flags:

- **mDNS** — the `md`/`fn`/`id` TXT record senders read *before* connecting.
  Many apps filter the device list here (e.g. Netflix hides `md=SHIELD Android
  TV` but shows `md=Chromecast`), so this is the gate that decides whether the
  proxy even appears.
- **CastV2 `DEVICE_INFO`** — the `deviceModel`/`friendlyName`/`deviceId` the
  sender reads *after* connecting.

```sh
uv run capture.py --certs ~/.vibecast/certs.json --upstream 192.168.2.6 \
    --model "Chromecast" --friendly-name "vibecast Living Room"
```

- `--model` → mDNS `md` **and** `DEVICE_INFO.deviceModel`.
- `--friendly-name` → mDNS `fn` **and** `DEVICE_INFO.friendlyName`.
- `--spoof KEY=VALUE` → any other `DEVICE_INFO` field (repeatable; value parsed
  as JSON, else string), e.g. `--spoof deviceCapabilities=2115`.

The proxy always rewrites `DEVICE_INFO.deviceId` to its **own** id (not the
upstream's). If it passed the real receiver's id through, senders would see the
proxy and the real device sharing one id and merge them — flip-flopping between
the two names. Rewrites are logged as `device_info_rewritten` in `cast.jsonl`
and printed live as `edit  DEVICE_INFO …`.

### Forcing app availability

Even when the device is visible, a sender checks `GET_APP_AVAILABILITY` for its
app id before offering the cast, and hides the device if the receiver replies
`APP_UNAVAILABLE`. The proxy can flip those replies to `APP_AVAILABLE`:

```sh
uv run capture.py --certs ~/.vibecast/certs.json --upstream 192.168.2.6 \
    --model "Chromecast" --available CA5E8412        # CA5E8412 = Netflix
```

- `--available APPID` → force that app id available (repeatable).
- `--all-available` → force every queried app id available.

The `LAUNCH` the sender then sends is still relayed to the real receiver, so
whether playback actually starts depends on the upstream — but this is enough to
make the sender offer the device and start the flow you want to capture. Logged
as `app_availability_rewritten` / `edit  APP_AVAILABILITY …`.

## Layout

```
capture.py               # entry point: cast proxy + mitmproxy + tunnel control
mitm_addon.py            # mitmproxy addon: writes http.jsonl
regenerate-bindings.sh   # build cdylib + generate ./generated bindings
generated/               # git-ignored: vibecast_primitives_ffi.py + native lib
captures/                # git-ignored: per-session cast.jsonl + http.jsonl
```

Re-run `./regenerate-bindings.sh` after changing `crates/vibecast-primitives-ffi`.
