//! CastV2 TLS transport.
//!
//! [`CastServer`] accepts TLS connections and drives each with
//! [`run_connection`], which answers device-auth and heartbeat locally and
//! forwards everything else as a [`ServerEvent`]. [`ConnectionHandle`] is the
//! cloneable send side used to reply to a sender.

#![forbid(unsafe_code)]

mod connection;
mod error;
pub mod message;
pub mod namespace;
mod server;

#[cfg(test)]
mod tests;

pub use connection::{run_connection, AuthMaterial, ConnectionHandle, ServerEvent};
pub use error::CastError;
pub use server::CastServer;
