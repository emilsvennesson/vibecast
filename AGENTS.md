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

The receiver binary is `vibecast-cli` (binary name `vibecast`). It wires the
portable core crates into a runnable server: CastV2 TLS listener, device hub,
player bridge, mDNS + eureka discovery, TOML config, certificate rotation, and
graceful shutdown.

## Workspace layout

Cargo workspace of 13 focused crates under `crates/`. Layering is strict:

```
vibecast-proto        CastV2 protobuf + length-prefixed framing          (leaf)
vibecast-security     Device-auth material + TLS cert rotation           (leaf)
vibecast-cast         CastV2 TLS transport: connection actor             (proto, security)
vibecast-discovery    mDNS advertisement + eureka HTTP/HTTPS             (security)
vibecast-messages     Cast JSON message models (serde)                   (leaf)
vibecast-bridge       Player bridge: WebSocket relay + DRM/manifest proxy (messages)
vibecast-sdk          Stable app-author SDK                              (messages)
vibecast-apps-*       Bundled apps (SVT Play, TV4 Play, Viaplay, Prime)  (sdk ONLY)
vibecast-core         Receiver runtime: device hub + coordinator         (cast, messages, bridge, sdk)
vibecast-cli          Platform binary: wires everything into a server    (all)
```

**App crates depend ONLY on `vibecast-sdk`.** No transport, TLS, or bridge
types leak into app code. To add an app, model it on `vibecast-apps-svtplay`
(the reference app) and register it in `crates/vibecast-cli/src/main.rs::apps`.

## Build, run, test

```sh
# Build / run the receiver
cargo run -p vibeast-cli                          # binary named `vibecast`
cargo run -p vibeast-cli -- --name "Living Room"  # override friendly name
cargo build -p vibeast-cli --release              # release binary

# Tests
cargo nextest run --all-features --profile ci     # main suite (CI profile)
cargo test --doc --all-features                   # doctests

# Lint / format / supply chain
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo deny check                                  # advisories + licenses + bans
cargo doc --no-deps --all-features                # rustdoc build (must be warning-free)
```

CLI flags (override `config.toml`): `--certs`, `--data-dir`, `--name`, `--model`,
`--bind-host`, `--cast-port`, `--device-id`, `--log-level`. Data dir defaults to
`$HOME/.vibecast`; config lives at `{data_dir}/config.toml`; certs at
`{data_dir}/certs.json`. Player bridge defaults to `:8010`, eureka HTTP `:8008`,
eureka HTTPS `:8443`, CastV2 TLS `:8009`.

## Invariants

- **`#![forbid(unsafe_code)]`** in every library crate. Do not relax this.
- **No secrets in logs/tracing.** Never log tokens, license challenges/responses,
  cert private keys, or device-auth signatures. Tracing fields must be non-sensitive.
- **`thiserror` per crate** for error enums. Apps may hand-roll `Display`+`Error`
  for dynamic-message errors, but prefer `thiserror` where the message is static.
- **Errors are useful and typed.** No stringly-typed errors; use the crate's error
  enum. `anyhow` is confined to the CLI binary (the platform layer).
- **Tests validate real behavior** — protocol round-trips, state transitions, error
  paths, regressions. Do not write tests that assert log output or that a specific
  private helper was called.
- **Certificate material is gitignored.** Never commit `*.pem`, `*.key`, `certs/`,
  or harvested device-auth bundles. The `scripts/` directory (capture/credential
  dev tooling) is gitignored and untracked — keep it that way.

## Conventions

- Edition 2021, `rust-version = "1.85"`, `max_width = 100` (rustfmt).
- Workspace deps declared once in the root `[workspace.dependencies]`; crates
  inherit via `xxx.workspace = true`. Add new deps there, not per-crate.
- `publish = false` workspace-wide (unpublished internal crates).
- Deny `rsa` crate (`deny.toml`) — device auth uses pre-harvested static signatures,
  not runtime RSA signing (avoids RUSTSEC-2023-0071 Marvin attack).
- `RUSTSEC-2024-0370` (proc-macro-error) is ignored in `deny.toml` — it's a
  build-time-only transitive dep of `xot` (manifest normalization), never linked
  into the shipped binary.
- Commit messages: concise, match repo style (e.g. `Add TV4 Play app provider`,
  `Fix slow Viaplay LOAD`). Do not commit unless explicitly asked.
- PR title format: `<area>: <summary>` (e.g. `Rust port: finalize as sole implementation`).

## Kodi add-on

`kodi/service.vibecast/` is a Python Kodi add-on that bridges Kodi playback to
vibecast's player WebSocket endpoint (`/player?role=primary`). It is a **client**
of the receiver, not part of the receiver. Kodi add-ons are inherently Python, so
this directory stays Python. The Rust receiver serves the `/player` endpoint the
add-on connects to; no special flags are needed (player bridge is on by default).

## Known limitations

- `vibecast-bridge` uses `std::sync::Mutex::lock().unwrap()` in production paths
  (poison = panic). Deliberate for a server; revisit only if poison-panic becomes
  undesirable.
- CI matrix is ubuntu + macos (no Windows).
- `cargo-deny` ignores one build-time-only advisory (see above); the shipped
  binary has no ignored vulnerabilities.
