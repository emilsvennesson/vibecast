# CI/CD

vibecast has two GitHub Actions workflows for validation and one for releasing,
plus a scheduled security audit. PR validation and release publishing are kept
in separate workflows so they never entangle.

## Workflows

| Workflow | File | Triggers | Purpose |
| --- | --- | --- | --- |
| **CI** | `.github/workflows/ci.yml` | `pull_request`; `push` to `main` | Format, lint, test, MSRV, supply chain, Android, Docker recipe. Path-filtered. |
| **Release** | `.github/workflows/release.yml` | `push` to `main` (via release-please) | Build + publish binaries, APK, container images, and the Homebrew formula. |
| **Security audit** | `.github/workflows/audit.yml` | weekly `schedule`; `workflow_dispatch` | Re-run `cargo deny check advisories bans` independent of code changes. |

Actions are **pinned to commit SHAs** (with a `# vX.Y.Z` comment) and kept
current by Dependabot (`.github/dependabot.yml`: cargo, github-actions, gradle).

### CI details

A `changes` job (`dorny/paths-filter`) computes three booleans that gate the
rest, so unrelated edits skip irrelevant work:

- **rust** — `crates/**`, `Cargo.*`, `deny.toml`, `rustfmt.toml`,
  `rust-toolchain.toml`, `.config/nextest.toml`.
- **android** — `android/**`, `crates/**` **except** `crates/vibecast-cli/**`
  (the desktop CLI binary is not part of the FFI cdylib / APK), and `Cargo.*`.
  So a docs-only, Kodi-only, or CLI-only change does **not** run Android.
- **docker** — `Dockerfile`, `docker/**`, `.dockerignore` only (the release
  image is built from prebuilt binaries; the Rust jobs already prove the code
  compiles).

Jobs: `fmt-clippy`, `test` (matrix: ubuntu + macos; nextest + doctests, JUnit
artifact), `msrv` (build on 1.87), `deny`, `android` (Gradle assemble + Android
Lint + ktlint + detekt), `docker-build` (from-source image, amd64, no push).

`ci-success` is an always-run aggregate gate that fails if any needed job
failed/was cancelled (a legitimately *skipped* job counts as success).
**Make `ci-success` the only required status check** in branch protection —
this is what makes path-filtered skips safe (individually-required,
path-filtered checks otherwise leave PRs stuck "waiting").

Caching: `Swatinem/rust-cache` (saves only from `main`), `gradle/actions`.

## How releases work

Releasing is driven by [release-please](https://github.com/googleapis/release-please)
and your Conventional Commits.

1. Every push to `main` updates a **release PR** ("chore: release X.Y.Z") that
   bumps versions + `CHANGELOG.md` from the commits since the last release.
2. **Merging that PR** cuts a **draft** GitHub release + tag `vX.Y.Z`, which
   triggers the build/publish jobs against the release commit.
3. Artifacts build in parallel; `publish` attaches them and only then does the
   **promote** step run (real releases only): tag the image `:latest` +
   `:MAJOR.MINOR`, undraft the release + mark it latest. `homebrew` then updates
   the tap. If any build fails, the release stays a hidden draft and neither
   `:latest` nor the tap move — **no partial/inconsistent release is visible**.

### Published artifacts

- Linux binaries: `vibecast-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`,
  `vibecast-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz`
- macOS binary (Apple Silicon): `vibecast-vX.Y.Z-aarch64-apple-darwin.tar.gz`
- Android: `vibecast-vX.Y.Z.apk` (release-signed)
- `vibecast-vX.Y.Z-SHA256SUMS.txt`
- Container image: `ghcr.io/emilsvennesson/vibecast:{X.Y.Z, X.Y, sha-<sha>, latest}`
  (multi-arch: `linux/amd64` + `linux/arm64`)
- SLSA build-provenance attestations for binaries and the image.

Artifacts are built once and reused: the container image copies the prebuilt
Linux binaries (`docker/release.Dockerfile`) rather than recompiling.

### Versioning

One version drives everything, kept in sync by release-please:

- `[workspace.package] version` in `Cargo.toml` — all crates inherit it via
  `version.workspace = true`. release-please bumps it with a `toml` updater
  (`jsonpath: $.workspace.package.version`).
- `Cargo.lock` — the workspace-member entries are re-synced by
  `cargo update --workspace` on the release PR branch (a step in the
  `release-please` job). The `cargo-workspace` plugin can't be used because it
  rejects `version.workspace = true`, so cargo itself is the source of truth.
- Android `versionName` in `android/app/build.gradle.kts` — bumped by a
  `generic` updater keyed on the `// x-release-please-version` annotation;
  `versionCode` is derived from it (`MAJOR*10000 + MINOR*100 + PATCH`).

Because all crates share one version, `release-type` is `simple` (not `rust`).
Commit → bump mapping (Conventional Commits): `fix:` → patch, `feat:` → minor,
`!`/`BREAKING CHANGE:` → major. Force a version with a `Release-As: X.Y.Z`
commit footer. The initial release is pinned to `0.1.0` via `release-as` in the
config; remove that key after `0.1.0` ships so subsequent versions compute from
commits.

## Required secrets & configuration

Repo settings:
- Repository is **public** (required for unauthenticated `brew`/asset/image
  pulls and free arm64 runners).
- Actions → Workflow permissions → **Allow GitHub Actions to create and approve
  pull requests** (so release-please can open its PR).
- Branch protection on `main`: require the **`ci-success`** check.
- A **public** tap repo `emilsvennesson/homebrew-vibecast` with a `Formula/`
  directory.

Secrets (Settings → Secrets and variables → Actions):

| Secret | Used by | Notes |
| --- | --- | --- |
| `ANDROID_KEYSTORE_BASE64` | build-android | base64 of the release keystore (`.jks`) |
| `ANDROID_KEYSTORE_PASSWORD` | build-android | keystore password |
| `ANDROID_KEY_ALIAS` | build-android | signing key alias |
| `ANDROID_KEY_PASSWORD` | build-android | key password |
| `HOMEBREW_TAP_DEPLOY_KEY` | homebrew | private ed25519 deploy key with write to the tap repo |
| `RELEASE_PLEASE_TOKEN` | release-please | *optional* — fine-grained PAT (Contents + Pull requests: read/write). Makes the release PR trigger CI so a required `ci-success` check can pass on it. Falls back to `GITHUB_TOKEN` when unset (release PR gets no CI). |
| `GITHUB_TOKEN` | most jobs | automatic; GHCR push, release upload, release-please |

### Branch protection + the release PR

PRs opened with the automatic `GITHUB_TOKEN` do **not** trigger workflows (a
GitHub anti-recursion rule), so the release-please PR gets no CI checks by
default. If you require `ci-success` in branch protection, set
`RELEASE_PLEASE_TOKEN` to a fine-grained PAT (Contents + Pull requests
read/write, scoped to this repo) — the release PR will then run CI and can
satisfy the required check. Without it, either don't require the check on release
PRs or admin-merge them.

The Android keystore is stable across releases — losing it blocks future signed
upgrades. Keep the `.jks` + passwords somewhere safe.

## Running the equivalent checks locally

| Check | Command |
| --- | --- |
| Format | `cargo fmt --all --check` |
| Lint | `cargo clippy --all-targets --all-features -- -D warnings` |
| Tests | `cargo nextest run --all-features --profile ci` |
| Doctests | `cargo test --doc --all-features` |
| MSRV | `cargo +1.87 build --all-targets --all-features --locked` |
| Supply chain | `cargo deny check` |
| Android | `cd android && ./gradlew :app:assembleDebug lintDebug ktlintCheck detekt` |
| Fix Kotlin style | `cd android && ./gradlew ktlintFormat` |
| Docker (from source) | `docker build -t vibecast .` |
| Lint workflows | `actionlint` |
| Release dry-run | `npx release-please@17 release-pr --dry-run --repo-url=emilsvennesson/vibecast --token=$(gh auth token) --config-file=release-please-config.json --manifest-file=.release-please-manifest.json` |

## Running the container

```sh
# mDNS discovery needs host networking; mount a data dir for config + certs.
docker run --rm --network host \
  -v "$HOME/.vibecast:/data" \
  ghcr.io/emilsvennesson/vibecast:latest --data-dir /data
```

## Retrying / tearing down a failed release

A failed release never promotes: it stays a hidden **draft** and neither
`:latest` nor the Homebrew tap move. To retry, delete the draft + tag and let
release-please re-open the release PR — merging it rebuilds with the current
workflow:

```sh
TAG=v0.1.0
gh release delete "$TAG" --yes --cleanup-tag   # remove the draft release + tag
# (optional) drop the version-tagged GHCR image that was pushed before the
# failure; needs a token with delete:packages scope:
gh api "/users/emilsvennesson/packages/container/vibecast/versions" \
  --jq '.[] | select(.metadata.container.tags[]? | test("^'"${TAG#v}"'$|^sha-")) | .id'
# gh api -X DELETE "/users/emilsvennesson/packages/container/vibecast/versions/<id>"
```

Then push (or re-run the `release-please` job) so it re-creates the release PR;
merge it to rebuild.
