//! Receiver runtime: the device hub, per-session coordinator, and app wiring.
//!
//! [`DeviceHub`] is a single-task actor fed through a [`DeviceHubHandle`] (Cast
//! transport events and player reports). It answers the `receiver-0` platform
//! namespaces, launches app sessions, routes media messages to their per-session
//! coordinator state, drives the player, and registers the DRM-license /
//! manifest proxy handlers. Slow app callbacks run on per-session tasks so one
//! app can never stall routing.

#![forbid(unsafe_code)]

mod coordinator;
mod hub;
mod identity;
mod proxy;
mod registry;

#[cfg(test)]
mod tests;

pub use hub::{DeviceHub, DeviceHubHandle, HubClosed, HubConfig};
pub use identity::DeviceIdentity;
pub use registry::{AppRegistry, RegisteredApp, RegistryError};
pub use vibecast_player_api::ProxyRegistrar;
