# AGENTS.md

Canonical agent & developer guide for vibecast. `CLAUDE.md` imports this file via
`@AGENTS.md` so Claude Code and other coding agents (Codex, Cursor, Aider, etc.)
read the same single source.

## Behavioral guidelines

These bias toward caution over speed. For trivial tasks, use judgment.

### Think before coding

Don't assume. Don't hide confusion. Surface tradeoffs.

- State assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them — don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

### Simplicity first

Minimum code that solves the problem. Nothing speculative.

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

### Surgical changes

Touch only what you must. Clean up only your own mess.

- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it — don't delete it.
- Remove imports/variables/functions that YOUR changes made unused.

Every changed line should trace directly to the request.

### Goal-driven execution

Define success criteria. Loop until verified.

- "Add validation" → write tests for invalid inputs, then make them pass.
- "Fix the bug" → write a test that reproduces it, then make it pass.
- "Refactor X" → ensure tests pass before and after.

For multi-step tasks, state a brief plan with a verification check per step.

## Tool usage

Always use Context7 when you need code generation, setup/configuration steps, or
library/API documentation — even for well-known libraries. Resolve the library ID
first, then query its docs. Use web search for up-to-date docs when Context7
lacks coverage. The `.mcp.json` config provisions Context7.

Do not use Context7 for: refactoring, writing scripts from scratch, debugging
business logic, code review, or general programming concepts.

## Project overview

vibecast is a **native Google Cast receiver** written in Rust. It impersonates a
Chromecast: it speaks the CastV2 TLS protocol (device auth, heartbeat, receiver
namespace), advertises itself over mDNS + the eureka `/setup/eureka_info` HTTP
endpoint, and routes `LAUNCH`/`LOAD` to bundled app providers. A player bridge
serves an embedded Shaka Player page over HTTP/WebSocket and proxies DRM license
+ DASH/HLS manifest requests (with normalization).

**Per-player receivers.** vibecast is not one advertised device: each *player*
(the browser Shaka page, the Kodi add-on, a native player) connects to the
shared player bridge's `/player` WebSocket and **registers** its identity + a
`PlayerCapabilities` (platform, DRM systems + security level, codecs, max
resolution, HDR, HDCP). The orchestrator (`vibecast-platform::manager`) then
spins up a *dedicated* Cast receiver for that player — its own friendly name
(`<reported name> [vibecast]`), fresh device id, and dynamically-assigned
CastV2/eureka ports — so senders see one distinct Chromecast per player,
advertising that player's real capabilities. The receiver is torn down when the
player disconnects (ephemeral). All per-player receivers share one harvested
device certificate (varying only the identity strings). A player's capabilities
reach app sessions via `ctx.receiver.capabilities`, so apps make conditional
decisions per selected player (e.g. Prime Video derives its whole device profile
from them).

The compose/orchestration logic — assemble certs + shared bridge + the
per-player orchestrator, then start/observe/stop — lives in `vibecast-platform`,
shared by two platform bindings: `vibecast-cli` (binary name `vibecast`; the
desktop Linux/macOS server, mDNS discovery, TOML config, Ctrl-C lifecycle) and
`vibecast-ffi` (a `cdylib` + UniFFI facade generating Kotlin/Swift/… bindings
for native Android/iOS frontends; per-player discovery is delegated to the
frontend via `PlayerObserver`, e.g. Android `NsdManager`). See `android/` for
the Android TV frontend.

## Workspace layout

Cargo workspace of 18 focused crates under `crates/`. Layering is strict:

```
vibecast-proto        CastV2 protobuf + length-prefixed framing          (leaf)
vibecast-security     Device-auth material + TLS cert rotation           (leaf)
vibecast-cast         CastV2 TLS transport: connection actor             (proto, security)
vibecast-discovery    Cast identity/TXT + eureka HTTP/HTTPS; mDNS (feat) (security)
vibecast-messages     Cast JSON message models (serde)                   (leaf)
vibecast-player-api   Player/proxy seams + wire protocol + manifest utils (messages)
vibecast-sdk          Stable app-author SDK (+ PlayerCapabilities)       (messages)
vibecast-apps-*       Bundled apps (SVT Play, TV4 Play, Viaplay, Prime)  (sdk ONLY)
vibecast-bridge       Player bridge: registration + routing + proxy      (player-api, sdk)
vibecast-core         Receiver runtime: device hub + coordinator         (cast, messages, player-api, sdk)
vibecast-receiver     Generic per-receiver composition (reusable)        (cast, core, discovery, security, player-api, sdk)
vibecast-platform     Compose + per-player orchestrator + Config         (receiver, bridge, core, apps, …)
vibecast-cli          Desktop binding: args + Ctrl-C lifecycle           (platform)
vibecast-ffi          cdylib + UniFFI facade (Kotlin/Swift/…)            (platform)
uniffi-bindgen        Version-locked uniffi-bindgen CLI (dev tool)       (uniffi[cli])
```

`vibecast-core` does **not** depend on `vibecast-bridge`: the `Player` command
sink and `ProxyRegistrar` proxy seams live in `vibecast-player-api`, so the
generic runtime is decoupled from the concrete Shaka/Kodi bridge.
`vibecast-receiver` is the app-agnostic "one Cast receiver" composition and can
be reused independently of vibecast's apps/bridge/branding.

`vibecast-discovery` always exposes the portable `CastAdvertisement`
identity/TXT + eureka endpoints; the `mdns-sd` responder (`MdnsResponder`) is
behind an `mdns` cargo feature that only `vibecast-cli` enables, so the
`vibecast-ffi` cdylib never links `mdns-sd` (it advertises via `PlayerObserver`
facts). The reported LAN IP is injected via `PlatformInputs.local_ip`
(`None` = derive from the routed interface).

**App crates depend ONLY on `vibecast-sdk`.** No transport, TLS, or bridge
types leak into app code. To add an app, model it on `vibecast-apps-svtplay`
(the reference app) and register it in
`crates/vibecast-platform/src/lib.rs::build_app_providers` (both bindings then
pick it up automatically).

## Build, run, test

```sh
# Build / run the server (players register over the bridge; each becomes a device)
cargo run -p vibecast-cli                          # binary named `vibecast`
cargo run -p vibecast-cli -- --model "Chromecast"  # override the reported model
cargo build -p vibecast-cli --release              # release binary

# Tests
cargo nextest run --all-features --profile ci     # main suite (CI profile)
cargo test --doc --all-features                   # doctests

# Lint / format / supply chain
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check                                  # advisories + licenses + bans
cargo doc --no-deps --all-features                # rustdoc build (must be warning-free)
actionlint .github/workflows/*.yml                # lint GitHub Actions (brew install actionlint)
```

CLI flags (override `config.toml`): `--certs`, `--data-dir`, `--model`,
`--bind-host`, `--player-port`, `--log-level`. Data dir defaults to
`$HOME/.vibecast`; config lives at `{data_dir}/config.toml`; certs at
`{data_dir}/certs.json`. The shared player bridge defaults to `:8010`; each
per-player receiver binds **OS-assigned** CastV2/eureka ports (advertised via
mDNS), so senders find them regardless of port.

```sh
# Android (Android TV) frontend — needs cargo-ndk + NDK r28+ + JDK 17.
# Cross-compiles vibecast-ffi per ABI, generates UniFFI Kotlin bindings, builds the APK.
cargo ndk -t arm64-v8a -t x86_64 -P 24 build --release -p vibecast-ffi   # .so per ABI
cd android && ./gradlew :app:assembleDebug lintDebug ktlintCheck detekt   # app + lint gate
```

See `android/README.md` for cert provisioning and on-device validation over adb.

## CI/CD

Two workflows (details in `docs/ci-cd.md`):

- **`ci.yml`** — PR + push-to-main. Path-filtered (`rust`/`android`/`docker`) so
  unrelated changes skip; a single always-run `ci-success` job is the required
  status check.
- **`release.yml`** — release-please cuts a **draft** release on merge of its
  release PR, then builds Linux (x86_64/aarch64) + macOS (arm64) binaries, a
  signed APK, and a multi-arch GHCR image (assembled from the prebuilt binaries),
  and only promotes (`:latest`, undraft, Homebrew tap) once every artifact
  succeeds — atomic, no partial releases.

Releasing is automatic: land Conventional Commits (`fix`→patch, `feat`→minor,
`!`→major), then merge the release PR release-please opens. One version drives
`[workspace.package].version` (toml updater), `Cargo.lock` (`cargo update
--workspace`), and Android `versionName`. Third-party actions are SHA-pinned;
don't unpin. Linux binaries need glibc ≥ 2.38 (aws-lc-rs), hence the
distroless-debian13 runtime.

## Invariants

- **`#![forbid(unsafe_code)]`** in every library crate. Do not relax this. The
  sole exception is `vibecast-ffi`: `uniffi::setup_scaffolding!()` emits
  `unsafe extern "C"` scaffolding, so it uses `#![deny(unsafe_code)]` instead
  (still forbids hand-written unsafe; the generated code carries its own
  `#[allow]`). No hand-written `unsafe` is permitted anywhere.
- **No secrets in logs/tracing.** Never log tokens, license challenges/responses,
  cert private keys, or device-auth signatures. Tracing fields must be non-sensitive.
- **`thiserror` per crate** for error enums. Apps may hand-roll `Display`+`Error`
  for dynamic-message errors, but prefer `thiserror` where the message is static.
- **Errors are useful and typed.** No stringly-typed errors; use the crate's error
  enum (`vibecast-platform` exposes `PlatformError`; `vibecast-ffi` maps it to the
  UniFFI `ReceiverError`). `anyhow` is confined to the `vibecast-cli` binary.
- **Tests validate real behavior** — protocol round-trips, state transitions, error
  paths, regressions. Do not write tests that assert log output or that a specific
  private helper was called.
- **Certificate material is gitignored.** Never commit `*.pem`, `*.key`, `certs/`,
  or harvested device-auth bundles. The `scripts/` directory (capture/credential
  dev tooling) is gitignored and untracked — keep it that way.

## Conventions

- Edition 2021, `rust-version = "1.87"` (UniFFI 0.32 floor), `max_width = 100` (rustfmt).
- Workspace deps declared once in the root `[workspace.dependencies]`; crates
  inherit via `xxx.workspace = true`. Add new deps there, not per-crate.
- `publish = false` workspace-wide (unpublished internal crates).
- Deny `rsa` crate (`deny.toml`) — device auth uses pre-harvested static signatures,
  not runtime RSA signing (avoids RUSTSEC-2023-0071 Marvin attack).
- `RUSTSEC-2024-0370` (proc-macro-error) is ignored in `deny.toml` — it's a
  build-time-only transitive dep of `xot` (manifest normalization), never linked
  into the shipped binary.
- Commit messages follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/):
  `<type>(<scope>): <description>` (e.g. `feat(svtplay): add app provider`,
  `fix(viaplay): slow LOAD`, `refactor(bridge)!: split routing` = breaking).
  Scope is the crate/area. Do not commit unless explicitly asked.
- PR titles mirror this format; squash-merging yields one conventional commit.

## Kodi add-on

`kodi/service.vibecast/` is a Python Kodi add-on that bridges Kodi playback to
vibecast's player WebSocket endpoint (`/player`). It is a **client** of the
receiver, not part of the receiver. Kodi add-ons are inherently Python, so this
directory stays Python. The Rust receiver serves the `/player` endpoint the
add-on connects to; no special flags are needed (player bridge is on by default).

On connect the add-on sends a `register` frame (`{type:"register", playerId,
name, capabilities}`) as its first message — its name defaults to `Kodi`
(configurable, plus resolution/codecs/HDR/Widevine level under the add-on's
**Player** settings; auto-detected by default) — and vibecast advertises it as
`Kodi [vibecast]`. The embedded browser player (`crates/vibecast-bridge/assets/
player.js`) registers the same way.

## Known limitations

- `vibecast-bridge` uses `std::sync::Mutex::lock().unwrap()` in production paths
  (poison = panic). Deliberate for a server; revisit only if poison-panic becomes
  undesirable.
- CI matrix is ubuntu + macos (no Windows).
- `cargo-deny` ignores one build-time-only advisory (see above); the shipped
  binary has no ignored vulnerabilities.
