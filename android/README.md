# vibecast — Android (Android TV) frontend

A thin Kotlin frontend that hosts the portable Rust receiver core on Android. It
loads the `vibecast-ffi` cdylib through UniFFI-generated Kotlin bindings, runs a
`connectedDevice` foreground service, advertises the receiver via `NsdManager`,
and holds a Wi-Fi lock while serving. It **coexists** with a device's built-in
Cast receiver by binding alternate ports and advertising a distinct instance.

This phase hosts the **server only** — CastV2 TLS + device auth, the eureka
endpoint, and the player-bridge HTTP/WS server. No on-device media renderer is
wired to the bridge yet.

## Layout

```
android/
  app/
    build.gradle.kts                  cargo-ndk + uniffi-bindgen wiring, Android config
    src/main/AndroidManifest.xml      permissions, FGS (connectedDevice), leanback launcher
    src/main/kotlin/com/vibecast/receiver/
      MainActivity.kt                 status screen (start/stop)
      CastReceiverService.kt          FGS + ReceiverObserver + NsdManager + WifiLock
      ReceiverState.kt                shared UI state
      Settings.kt                     name/ports (SharedPreferences)
      Provisioning.kt                 certs.json discovery in filesDir
  gradle/libs.versions.toml           version catalog
  config/detekt/detekt.yml            detekt rules
```

The Rust `.so` and generated Kotlin bindings are produced into `app/build/` by
Gradle tasks (`cargoBuildAndroid`, `cargoBuildHostFfi`, `generateUniffiKotlin`)
and are **not** committed.

## Prerequisites

- Android SDK (`ANDROID_HOME` / `local.properties` `sdk.dir`), platform 34, build-tools 34.
- Android **NDK r28+** (16 KB page default). Set `ANDROID_NDK_HOME`, or install it
  under `$ANDROID_HOME/ndk/` and the build auto-detects the newest.
- Rust with the Android targets and `cargo-ndk`:
  ```sh
  rustup target add aarch64-linux-android x86_64-linux-android
  cargo install cargo-ndk    # or: cargo binstall cargo-ndk
  ```
- JDK 17 (Gradle daemon). The wrapper pins Gradle 8.9 / AGP 8.5.2.

## Build

```sh
cd android
./gradlew :app:assembleDebug                          # APK with both ABIs' .so
./gradlew :app:assembleDebug lintDebug ktlintCheck detekt   # full quality gate
```

The build cross-compiles `vibecast-ffi` for `arm64-v8a` + `x86_64`, generates the
Kotlin bindings from an unstripped host build (the shipped `.so` is stripped and
drops the UniFFI metadata bindgen needs), and packages everything.

## Provision device-auth certs (development)

`certs.json` is harvested device-auth material — **never committed, never bundled
in the APK**. Provision it into the app's private files dir over adb (debuggable
build). SELinux permitting, the simplest reliable path is push-then-`run-as`:

```sh
PKG=com.vibecast.receiver
adb shell run-as "$PKG" mkdir -p files
adb push ~/.vibecast/certs.json /data/local/tmp/certs.json
adb shell run-as "$PKG" cp /data/local/tmp/certs.json files/certs.json
adb shell rm /data/local/tmp/certs.json
```

## Run + validate on device

```sh
adb install -r app/build/outputs/apk/debug/app-debug.apk
# Launch the status screen and press "Start receiver" (the service is not exported).
adb shell am start -n com.vibecast.receiver/.MainActivity

adb logcat -s vibecast          # Rust (tracing-logcat) + Kotlin logs, tag "vibecast"
adb shell ss -tln | grep 9009   # confirm alt ports bound (9009/9008/9443/8010)
```

Discovery coexistence (from another host):

```sh
dns-sd -B _googlecast._tcp      # vibecast-<id> appears alongside the built-in Cast
```

Then cast from any sender (Chrome, Google Home) to the "vibecast (Android)"
device: it completes CastV2 device auth and a `LAUNCH` reaches the hub. There is
no renderer yet, so media does not play — that is a later phase.

## Ports (alternate, to coexist with a built-in Cast receiver)

| Service        | vibecast | system Cast |
|----------------|----------|-------------|
| CastV2 TLS     | 9009     | 8009        |
| eureka HTTP    | 9008     | 8008        |
| eureka HTTPS   | 9443     | 8443        |
| player bridge  | 8010 (loopback) | —    |

Change them in the status app's settings (SharedPreferences) if needed.

## Notes / limitations

- `targetSdk` is 34 for this phase (no extra SDK download); bump before any Play
  release (`OldTargetSdkVersion`/`ExpiredTargetSdkVersion` lint checks are disabled
  for now).
- `useLegacyPackaging = false` keeps `.so` files page-aligned for 16 KB devices;
  JNA (5.15) loads them from the APK without extraction.
