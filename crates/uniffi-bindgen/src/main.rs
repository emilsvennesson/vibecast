//! Version-locked `uniffi-bindgen` entry point.
//!
//! Generates foreign bindings from the compiled `vibecast-ffi` library, e.g.:
//!
//! ```sh
//! cargo run -p uniffi-bindgen -- generate \
//!     --library target/<abi>/release/libvibecast_ffi.so \
//!     --language kotlin --out-dir android/app/build/generated/uniffi
//! ```

fn main() {
    uniffi::uniffi_bindgen_main()
}
