//! Compile `cast_channel.proto` into Rust at build time.
//!
//! Uses `protox` (a pure-Rust protobuf compiler) so no external `protoc` binary
//! is required on build machines, CI, or the future Android target.

use std::io::Result;

fn main() -> Result<()> {
    let proto = "proto/cast_channel.proto";
    println!("cargo:rerun-if-changed={proto}");

    let file_descriptors = protox::compile([proto], ["proto"])
        .expect("failed to compile cast_channel.proto with protox");

    prost_build::Config::new()
        .compile_fds(file_descriptors)
        .expect("failed to generate Rust from protobuf descriptors");

    Ok(())
}
