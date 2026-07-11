#!/usr/bin/env bash
# Build the `vibecast-primitives-ffi` cdylib and (re)generate the UniFFI Python
# bindings + native library into ./generated (git-ignored). Re-run whenever the
# FFI surface changes.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

case "$(uname -s)" in
  Darwin) lib="libvibecast_primitives_ffi.dylib" ;;
  *)      lib="libvibecast_primitives_ffi.so" ;;
esac

cd "$repo"
cargo build -p vibecast-primitives-ffi --release
cargo run -p uniffi-bindgen -- generate \
  --library "target/release/$lib" \
  --language python \
  --out-dir "$here/generated"

# The generated module loads the native library from its own directory.
cp "target/release/$lib" "$here/generated/"
echo "Wrote bindings + $lib to $here/generated/"
