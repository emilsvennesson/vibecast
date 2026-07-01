//! Receiver runtime: the device hub and platform-namespace handling.
//!
//! [`DeviceHub`] is a single-task actor driven by the transport's
//! `ServerEvent` stream. It answers the platform namespaces addressed to
//! `receiver-0` (connection, receiver, discovery, multizone, setup). App
//! sessions and media routing are added in a later phase.

#![forbid(unsafe_code)]

mod hub;
mod identity;
mod status;

#[cfg(test)]
mod tests;

pub use hub::DeviceHub;
pub use identity::DeviceIdentity;
pub use status::build_receiver_status;
