//! Receiver runtime: the device hub, per-session coordinator, and app wiring.
//!
//! [`DeviceHub`] is a single-task actor driven by [`HubEvent`]s (Cast transport
//! events, renderer reports, and internal media-resolution results). It answers
//! the `receiver-0` platform namespaces, launches app sessions, routes media
//! messages to their per-session coordinator state, drives the renderer, and
//! registers the DRM-license / manifest proxy handlers.

#![forbid(unsafe_code)]

mod coordinator;
mod hub;
mod identity;
mod proxy;
mod registry;

#[cfg(test)]
mod tests;

pub use hub::{DeviceHub, HubConfig, HubEvent, MediaResolved};
pub use identity::DeviceIdentity;
pub use registry::{AppRegistry, ProxyRegistrar};
