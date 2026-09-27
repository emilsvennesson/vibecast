# vibecast for Android TV

An Android TV app that runs the vibecast receiver on the device. The Rust core
is loaded through UniFFI-generated Kotlin bindings and runs in a foreground
service, advertising each connected player over `NsdManager`. It runs alongside
the device's built-in Cast receiver.

There is no native player yet: playback goes through the Kodi add-on or the
browser player, which connect to the player bridge on port `8010`.

## Requirements

- Android 8.0 (API 26) or later; `arm64-v8a` or `x86_64`
- Device-auth certificates (`certs.json`, see [Certificates](../README.md#certificates))

## Build

Prerequisites:

- Android SDK with platform 36 and build-tools 36
- Android NDK r28+ (`ANDROID_NDK_HOME`, or the newest under `$ANDROID_HOME/ndk/`)
- JDK 17
- Rust with the Android targets and `cargo-ndk`:
  ```sh
  rustup target add aarch64-linux-android x86_64-linux-android
  cargo install cargo-ndk
  ```

```sh
cd android
./gradlew :app:assembleDebug                                # APK
./gradlew :app:assembleDebug lintDebug ktlintCheck detekt   # APK + lint
```

## Install and provision certificates

`certs.json` is never bundled in the APK. Copy it into the app's private
storage over adb (requires a debug build):

```sh
adb install -r app/build/outputs/apk/debug/app-debug.apk

PKG=com.vibecast.receiver
adb shell run-as "$PKG" mkdir -p files
adb push ~/.vibecast/certs.json /data/local/tmp/certs.json
adb shell run-as "$PKG" cp /data/local/tmp/certs.json files/certs.json
adb shell rm /data/local/tmp/certs.json
```

## Run

Open vibecast on the TV and press **Start receiver**, or:

```sh
adb shell am start -n com.vibecast.receiver/.MainActivity
```

Then point a player at `<tv-ip>:8010`. Once it connects, it appears as a Cast
device on the network. Logs:

```sh
adb logcat -s vibecast
```
