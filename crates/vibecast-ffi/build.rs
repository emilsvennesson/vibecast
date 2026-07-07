//! Build script: force 16 KB max page alignment for Android shared objects.
//!
//! Android 15+ devices ship 16 KB pages, and Play requires 16 KB-aligned
//! native libraries (enforced since Nov 2025). NDK r28+ defaults to this, so
//! these link args are belt-and-suspenders for older/other linkers.
//!
//! No UniFFI scaffolding is generated here: `vibecast-ffi` is a pure
//! proc-macro crate (`uniffi::setup_scaffolding!()`), which needs no build.rs
//! scaffolding step and no UDL file.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg=-Wl,-z,max-page-size=16384");
        println!("cargo:rustc-link-arg=-Wl,-z,common-page-size=16384");
    }
}
