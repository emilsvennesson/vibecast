//! Cast device discovery.
//!
//! - [`CastAdvertisement`] computes the portable advertisement identity (service
//!   instance label + TXT record) that identifies the receiver as a
//!   `_googlecast._tcp` device. It is always available and carries no network
//!   dependency, so frontends with native discovery consume it directly.
//! - `MdnsResponder` (behind the `mdns` feature) announces that advertisement
//!   over multicast DNS via `mdns-sd`. Platforms that advertise through a native
//!   API (Android `NsdManager`, iOS `NWListener`) omit the feature and never
//!   link `mdns-sd`.
//! - [`EurekaServer`] serves the `/setup/eureka_info` endpoint probed by senders
//!   over HTTP and HTTPS.

#![forbid(unsafe_code)]

mod error;
mod eureka;
mod mdns;

pub use error::DiscoveryError;
pub use eureka::{DeviceCapabilities, EurekaIdentity, EurekaServer};
pub use mdns::{CastAdvertisement, CastServiceTxt};

#[cfg(feature = "mdns")]
pub use mdns::MdnsResponder;
