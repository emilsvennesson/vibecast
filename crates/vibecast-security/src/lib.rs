//! Cast device-authentication material and TLS certificate handling.
//!
//! - [`CertificateStore`] loads the manifest JSON and selects the active
//!   [`CertificateBundle`], rotating as certificates expire.
//! - [`build_auth_response`] / [`build_auth_error`] build the binary
//!   `DeviceAuthMessage` for the deviceauth namespace (static signatures).
//! - [`CertResolver`] / [`server_config`] provide a rustls TLS server config
//!   with lock-free, hot-swappable certificates.

#![forbid(unsafe_code)]

mod auth;
mod bundle;
mod error;
mod store;
mod tls;

#[cfg(test)]
mod tests;

pub use auth::{build_auth_error, build_auth_response};
pub use bundle::CertificateBundle;
pub use error::SecurityError;
pub use store::CertificateStore;
pub use tls::{server_config, CertResolver};

// Re-export the deviceauth enums so callers configure auth without depending on
// vibecast-proto directly.
pub use vibecast_proto::cast_channel::auth_error::ErrorType as AuthErrorType;
pub use vibecast_proto::{HashAlgorithm, SignatureAlgorithm};
