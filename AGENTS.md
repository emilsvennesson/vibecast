# AGENTS.md — CastVibe Development Guide

This document is the primary reference for AI agents working on castvibe in fresh
context windows. It contains everything needed to understand the project, the
Google Cast protocol, design decisions, and how to contribute code that passes
all checks.

## Project Overview

**castvibe** is a Python asyncio library implementing a Google Cast (CastV2)
receiver. It accepts TLS connections from iOS/Android/Chrome Cast senders,
performs device authentication, handles the Cast platform protocol, and delegates
app-specific behavior to modular **providers** (e.g. Viaplay).

Media control uses a mediator architecture:

- `PlaybackCoordinator` (per app session) owns canonical playback state
- `Player` ABC is the internal playback interface
- `PlayerServer` is the default `Player` implementation exposing:
  - WebSocket `GET /player` for commands/state reports
  - HTTP `POST /license/{session_id}` for DRM license proxying

## Backward Compatibility

Do not preserve backward compatibility by default; if a direct API or behavior
change is clearly better for the project, implement it directly.

## Reference Implementations

Two existing codebases serve as implementation references. **Do not port code
1:1** — use them for understanding protocol details and message flows, then write
idiomatic Python.

### go-cast (primary reference)

Location: `/Users/emil/dev/gocast/go-cast`
Repo: `github.com/tristanpenman/go-cast`

Key files:

| File                                  | What it contains                                                                       |
| ------------------------------------- | -------------------------------------------------------------------------------------- |
| `internal/channel/cast_channel.proto` | Protobuf definitions (already copied to `castvibe/_proto/`)                            |
| `internal/cast_channel.go`            | Length-prefixed protobuf framing (4-byte BE uint32 + payload)                          |
| `internal/server.go`                  | TLS server setup (X.509 keypair from manifest, `tls.Listen`)                           |
| `internal/client_connection.go`       | Per-connection handler, device auth challenge/response (lines 150-281)                 |
| `internal/receiver.go`                | Platform namespace handlers (heartbeat, receiver, discovery, multizone, setup)         |
| `internal/device.go`                  | Device hub: transport registry, subscriptions, message routing, `AppSession` interface |
| `internal/generic_session.go`         | Generic app session (media namespace, known apps map)                                  |
| `internal/viaplay_session.go`         | Viaplay session (SETUP_INFO, auth flow, LOAD handling)                                 |
| `internal/viaplay_api.go`             | Viaplay HTTP API client (cookie jar, URI templates, stream resolution)                 |
| `internal/advertisement.go`           | mDNS advertisement (`_googlecast._tcp`, TXT records)                                   |
| `internal/namespaces.go`              | Namespace URI constants                                                                |
| `internal/peer_cert.go`               | Dynamic peer certificate generation                                                    |
| `internal/manifest.go`                | Certificate manifest loading + algorithm detection                                     |
| `internal/auth.go`                    | Shared `BuildAuthResponse()` helper                                                    |

### pychromecast (secondary reference)

Repo: `github.com/home-assistant-libs/pychromecast`

pychromecast is a Cast **sender** (controller), not a receiver. Its value is in:

- `pychromecast/controllers/__init__.py` — `BaseController` ABC pattern for namespace handlers
- `pychromecast/controllers/media.py` — Media namespace message types and flow
- `pychromecast/socket_client.py` — TLS socket, protobuf framing, message dispatch
- `pychromecast/discovery.py` — zeroconf service browsing
- `chromecast_protobuf/` — Python-compiled protobuf files

## Google Cast Protocol Reference

### Transport: TLS over TCP (port 8009)

Cast uses TLS 1.2+ over TCP. The receiver presents a **self-signed certificate**
(the "peer certificate") that is regenerated every 24-48 hours. Senders connect
with certificate verification disabled (`InsecureSkipVerify` / no CA check).

```
Sender ──TLS──> Receiver:8009
         (self-signed cert, no CA verification)
```

### Message Framing

All messages are **length-prefixed protobuf**:

```
┌──────────────────┬─────────────────────────────┐
│ 4 bytes (BE u32) │ N bytes (protobuf payload)   │
│ payload length   │ serialized CastMessage       │
└──────────────────┴─────────────────────────────┘
```

Read: read 4 bytes -> decode as big-endian uint32 -> read that many bytes ->
`CastMessage.ParseFromString(payload)`

Write: `payload = msg.SerializeToString()` -> write `len(payload)` as 4-byte BE
uint32 -> write payload

### CastMessage Protobuf

Defined in `castvibe/_proto/cast_channel.proto`:

```protobuf
message CastMessage {
  required ProtocolVersion protocol_version = 1;  // Always CASTV2_1_0 (0)
  required string source_id = 2;                   // e.g. "sender-0", "receiver-0"
  required string destination_id = 3;              // target, or "*" for broadcast
  required string namespace = 4;                   // multiplexing key
  required PayloadType payload_type = 5;           // STRING (0) or BINARY (1)
  optional string payload_utf8 = 6;                // JSON payload (for STRING)
  optional bytes payload_binary = 7;               // binary payload (for BINARY)
}
```

### Namespaces

Each namespace defines a sub-protocol. Platform namespaces (handled by
`receiver-0`):

| Namespace                                       | Purpose                                       |
| ----------------------------------------------- | --------------------------------------------- |
| `urn:x-cast:com.google.cast.tp.heartbeat`       | Keepalive (PING/PONG)                         |
| `urn:x-cast:com.google.cast.tp.connection`      | Virtual connection management (CONNECT/CLOSE) |
| `urn:x-cast:com.google.cast.tp.deviceauth`      | Device authentication (binary protobuf)       |
| `urn:x-cast:com.google.cast.receiver`           | App management (LAUNCH/STOP/STATUS)           |
| `urn:x-cast:com.google.cast.receiver.discovery` | Device info queries                           |
| `urn:x-cast:com.google.cast.media`              | Media control (LOAD/PLAY/PAUSE/SEEK)          |
| `urn:x-cast:com.google.cast.multizone`          | Multi-room status                             |
| `urn:x-cast:com.google.cast.setup`              | Device setup (eureka_info)                    |

Provider-specific namespaces (handled by app sessions):

| Namespace                          | Provider |
| ---------------------------------- | -------- |
| `urn:x-cast:tv.viaplay.chromecast` | Viaplay  |

### Connection Lifecycle

```
1. DISCOVERY
   Receiver advertises via mDNS (_googlecast._tcp)
   with TXT records: fn, md, id, cd, ca, ve, ...

2. TLS HANDSHAKE
   Sender connects to receiver:8009 over TLS
   Receiver uses peer cert from certificate bundle

3. DEVICE AUTHENTICATION (binary protobuf on deviceauth namespace)
   Sender -> DeviceAuthMessage { challenge: AuthChallenge {} }
   Receiver -> DeviceAuthMessage { response: AuthResponse {
     signature: <SHA1 of peer_cert_DER signed with device_private_key>,
     client_auth_certificate: <device_cert_DER>,
     intermediate_certificate: [<ica_cert_DER>],
     hash_algorithm: SHA1
   }}

4. VIRTUAL CONNECTION (JSON on connection namespace)
   Sender -> { "type": "CONNECT", "origin": {}, "userAgent": "...",
               "senderInfo": { ... } }
   (registers subscription: sender-0 -> receiver-0)

5. STATUS EXCHANGE (JSON on receiver namespace)
   Sender -> { "type": "GET_STATUS", "requestId": 1 }
   Receiver -> { "type": "RECEIVER_STATUS", "requestId": 1, "status": { ... } }

   Sender -> { "type": "GET_APP_AVAILABILITY", "requestId": 2, "appId": ["6313CF39"] }
   Receiver -> { "type": "GET_APP_AVAILABILITY", "requestId": 2,
                 "availability": { "6313CF39": "APP_AVAILABLE" } }

6. APP LAUNCH (JSON on receiver namespace)
   Sender -> { "type": "LAUNCH", "requestId": 3, "appId": "6313CF39",
               "credentials": "<access_token>", "credentialsType": "..." }
   Receiver creates session with transport_id "pid-1"
   Receiver -> { "type": "RECEIVER_STATUS", ... (shows new app) }

   Sender -> CONNECT to transport "pid-1"

7. APP-SPECIFIC COMMUNICATION
   Messages routed to provider session by destination transport_id
   Media namespace handled by PlaybackCoordinator:
   LOAD/PLAY/PAUSE/SEEK/STOP/SET_VOLUME -> MEDIA_STATUS responses
   Coordinator invokes Provider.resolve_media(), Player callbacks,
   and Provider.on_playback_update() as state changes
   Custom namespaces: provider-specific messages

8. TEARDOWN
   Sender -> { "type": "STOP", "requestId": N, "sessionId": "..." }
   Receiver removes session, broadcasts updated RECEIVER_STATUS
```

### Device Authentication Detail

The auth exchange uses **binary protobuf** (not JSON) on the `deviceauth`
namespace. This is the only namespace that uses binary payloads.

For our implementation (SHA1 only, no nonce, static signature):

- The `signature` field contains a pre-computed RSASSA-PKCS1v15 signature of
  `SHA1(peer_cert_DER)` using the device's manufacturing private key
- The `client_auth_certificate` is the device cert DER (manufacturing-provisioned)
- The `intermediate_certificate` list contains the ICA chain DER bytes
- `hash_algorithm` is set to `SHA1`

The certificate bundle is loaded from a go-cast compatible JSON manifest:

```json
{
  "pu": "<peer cert PEM>",
  "pr": "<peer private key PEM>",
  "cpu": "<device auth cert PEM>",
  "ica": "<intermediate CA cert(s) PEM>",
  "sig_sha1": "<base64 SHA1 signature>"
}
```

### Receiver Namespace Messages

**GET_STATUS** response:

```json
{
  "type": "RECEIVER_STATUS",
  "requestId": 1,
  "status": {
    "applications": [
      {
        "appId": "6313CF39",
        "displayName": "Viaplay",
        "sessionId": "uuid-here",
        "transportId": "pid-1",
        "statusText": "",
        "namespaces": [
          { "name": "urn:x-cast:tv.viaplay.chromecast" },
          { "name": "urn:x-cast:com.google.cast.media" }
        ],
        "isIdleScreen": false
      }
    ],
    "volume": {
      "level": 1.0,
      "muted": false,
      "controlType": "attenuation",
      "stepInterval": 0.05
    },
    "isActiveInput": true,
    "isStandBy": false
  }
}
```

**GET_APP_AVAILABILITY** — receiver responds ALL app IDs as `"APP_AVAILABLE"`:

```json
{
  "type": "GET_APP_AVAILABILITY",
  "requestId": 2,
  "availability": { "6313CF39": "APP_AVAILABLE", "CC1AD845": "APP_AVAILABLE" }
}
```

### Media Namespace Messages

**LOAD** request (from sender):

```json
{
  "type": "LOAD",
  "requestId": 1,
  "media": {
    "contentId": "https://example.com/video",
    "contentType": "video/mp4",
    "streamType": "BUFFERED",
    "metadata": { "metadataType": 0, "title": "My Video" }
  },
  "autoplay": true,
  "currentTime": 0,
  "customData": { "playUrl": "https://content.viaplay.se/..." }
}
```

**MEDIA_STATUS** response:

```json
{
  "type": "MEDIA_STATUS",
  "requestId": 1,
  "status": [
    {
      "mediaSessionId": 1,
      "media": {
        "contentId": "...",
        "contentType": "video/mp4",
        "streamType": "BUFFERED"
      },
      "playerState": "PLAYING",
      "currentTime": 0,
      "supportedMediaCommands": 15,
      "volume": { "level": 1.0, "muted": false }
    }
  ]
}
```

### Viaplay Provider Protocol

Viaplay uses a custom namespace `urn:x-cast:tv.viaplay.chromecast` alongside the
standard media namespace.

App IDs: `"6313CF39"`, `"2DB7CC49"`

**Auth flow** (after LAUNCH):

1. Sender sends `SETUP_INFO` with contentRoot, countryCode, userId, profileId
2. Receiver tries token login (using credentials from LAUNCH)
3. If token fails, tries persistent login (stored cookies)
4. If all fail, gets device code via API, sends `AUTHORIZATION_REQUIRED` to sender
5. Sender shows device code to user, sends `AUTHORIZATION_DONE` when activated
6. Receiver polls authorized endpoint, completes auth

**Stream resolution** (`fetch_stream`):
The `customData.playUrl` from LOAD is resolved through the Viaplay API to an
actual streaming manifest URL. The API returns HAL-style JSON; try these paths:

1. `_embedded.viaplay:media.contentUrl`
2. Top-level `contentUrl`
3. `_links.viaplay:encryptedPlaylist.href`
4. `_links.viaplay:playlist.href`
5. `_links.viaplay:stream.href`

Viaplay API headers must mimic a real Chromecast:

- `User-Agent: Mozilla/5.0 ... CrKey/1.56.500000 DeviceType/AndroidTV`
- `CAST-DEVICE-CAPABILITIES` header
- `Origin`/`Referer` matching real Cast receivers

## Architecture & Design Decisions

### Playback Mediation: Coordinator + Player Server

The receiver always starts an internal player bridge server (`PlayerServer`) in
parallel with the Cast TLS server.

- `CastReceiver` owns both servers (`:8009` Cast, `:8010` player by default)
- `Device.start_session()` creates a `PlaybackCoordinator` for each app session
- `AppSession` routes media namespace requests to its coordinator
- Coordinator owns canonical `MEDIA_STATUS` state and broadcasts updates
- Coordinator invokes provider hooks:
  - `resolve_media(session, load_request) -> PlaybackMedia`
  - `on_playback_update(session, state)`
  - `resolve_license(session, request) -> LicenseResponse`

`PlayerServer` behavior:

- Primary WS client reports (`state` / `error`) update coordinator state
- Observer WS clients receive commands but their reports are ignored
- New WS clients are auto-synced (`load` + seek + play/pause) from snapshots
- License POSTs are delegated per session via coordinator -> provider

### Concurrency: asyncio

The library uses `asyncio` throughout. For Kodi integration, run the event loop
in a background thread with `asyncio.run()`.

### Type System: Pydantic v2

All Cast JSON messages are modeled with Pydantic v2 models using:

```python
from pydantic import BaseModel, ConfigDict
from pydantic.alias_generators import to_camel

class CastModel(BaseModel):
    model_config = ConfigDict(
        extra="allow",              # lenient: ignore unknown fields
        populate_by_name=True,      # allow Python snake_case construction
        alias_generator=to_camel,   # snake_case <-> camelCase automatically
        serialize_by_alias=True,    # model_dump() uses camelCase by default
    )
```

**Discriminated unions** dispatch on the `type` field per namespace:

```python
from typing import Annotated, Literal
from pydantic import Discriminator, TypeAdapter

class LaunchRequest(CastModel):
    type: Literal["LAUNCH"]
    request_id: int
    app_id: str

ReceiverRequest = Annotated[
    LaunchRequest | StopRequest | GetStatusRequest | ...,
    Discriminator("type"),
]
receiver_request_adapter: TypeAdapter[ReceiverRequest] = TypeAdapter(ReceiverRequest)
```

**Serialization**: `model.model_dump(exclude_none=True)` — uses camelCase keys
by default thanks to `serialize_by_alias=True`.

### Providers: Entry Points

Providers implement the `Provider` ABC and register via Python entry points:

```toml
# pyproject.toml
[project.entry-points."castvibe.providers"]
viaplay = "castvibe.providers.viaplay:ViaplayProvider"
```

Discovery: `importlib.metadata.entry_points(group="castvibe.providers")`

### Certificate Handling

`CertificateBundle` loads from a go-cast compatible JSON manifest and stores
certs in their wire formats (DER for auth response, PEM for TLS context).

### Package Layout Convention

- `castvibe/` — library root
- Public modules: `receiver.py`, `provider.py`, `player.py`, `__init__.py`
- Private modules: prefixed with `_` (e.g. `_server.py`, `_connection.py`)
- Playback internals: `_coordinator.py`, `_player_server.py`
- `_models/` — Pydantic message models subpackage
- `_proto/` — protobuf definitions and generated code
- `providers/` — bundled provider implementations

## Tooling & Quality Checks

### Package Manager: uv

```bash
uv sync                    # install all deps
uv run python -m castvibe  # run
uv run pytest              # run tests
```

### Type Checking: basedpyright

```bash
uv run basedpyright --warnings
```

Configuration in `pyproject.toml`:

- Mode: `recommended`
- Target: Python 3.12
- Unused imports/variables: error
- Duplicate imports: error
- `reportAny`: hint (not error)

All code must pass basedpyright with zero errors and zero warnings. Treat
warnings as errors. Use proper type annotations everywhere — `dict[str, Any]`,
return types, parameter types.

### Linting & Formatting: ruff

```bash
uv run ruff check .        # lint
uv run ruff format .       # format
```

Enabled rule sets: E, F, UP, B, SIM, I, N, A, T, C4, RET, PTH, TCH, PIE, PERF,
ARG, ASYNC, TRY. Preview mode is on. E501 (line length) is ignored.

Notable rules:

- `T201` (print statements) — forbidden except in `scripts/`
- `I` (isort) — import sorting enforced
- `UP` (pyupgrade) — modern Python syntax required
- `TCH` (type-checking) — type-only imports must be in `TYPE_CHECKING` blocks
- `PERF` — performance anti-patterns flagged
- `N` — naming conventions enforced (snake_case functions, PascalCase classes)

### Dependency Checking: deptry

```bash
uv run deptry .
```

Ensures all imports have corresponding dependencies in `pyproject.toml` and no
unused dependencies exist. Excludes `scripts/` directory.

### Testing: pytest

```bash
uv run pytest
uv run pytest -x           # stop on first failure
uv run pytest -k "test_framing"  # run specific tests
```

Test files go in `tests/` directory, mirroring the source structure. Use
`pytest-asyncio` for async test functions.

### Proto Compilation

```bash
uv run python scripts/compile_proto.py
```

Generated files (`_pb2.py`, `.pyi`) are committed to the repo so library
consumers don't need protoc installed.

## Dependencies

| Package        | Min Version | Purpose                                    |
| -------------- | ----------- | ------------------------------------------ |
| `pydantic`     | `>=2.11`    | Typed message models, discriminated unions |
| `protobuf`     | `>=5.0`     | CastMessage wire format                    |
| `zeroconf`     | `>=0.140`   | mDNS service advertisement                 |
| `cryptography` | `>=44.0`    | PEM/DER parsing, cert digest               |
| `httpx`        | `>=0.28`    | Async HTTP client (provider APIs)          |
| `aiohttp`      | `>=3.11`    | Player bridge server (WebSocket + HTTP)    |
| `uritemplate`  | `>=4.1`     | URI-template expansion for provider APIs   |

Dev dependencies:
| Package | Purpose |
|---------|---------|
| `basedpyright` | Type checking |
| `ruff` | Linting + formatting |
| `deptry` | Dependency auditing |
| `grpcio-tools` | Proto compilation |
| `pytest` | Testing |
| `pytest-asyncio` | Async test support |

## Tool usage

Always use Context7 when you need code generation, setup or configuration steps, or library/API documentation. This means you should automatically use the Context7 MCP tools to resolve library IDs and get library docs without me having to explicitly ask. You can also search the web for up-to-date documentation using the web-search tool. You can also search the web for up-to-date documentation using the web-search tool.
