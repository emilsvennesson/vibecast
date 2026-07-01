//! Cast device discovery.
//!
//! - [`CastAdvertisement`] advertises the receiver over mDNS as
//!   `_googlecast._tcp` (plus best-effort per-app subtypes).
//! - [`EurekaServer`] serves the `/setup/eureka_info` endpoint probed by senders
//!   over HTTP and HTTPS.

#![forbid(unsafe_code)]

mod error;
mod eureka;
mod mdns;

pub use error::DiscoveryError;
pub use eureka::{DeviceCapabilities, EurekaIdentity, EurekaServer};
pub use mdns::{CastAdvertisement, CastServiceTxt};
