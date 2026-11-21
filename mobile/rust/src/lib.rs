//! Native client for Harw's Android phone-test app.

mod core;
mod identity;
mod projection;
mod provider;
mod types;

pub use core::MobileCore;
pub use identity::{generate_identity, parse_device_enrollment, parse_server_profile};
pub use provider::{ChatgptProviderCredential, ChatgptProviderStatus};
pub use types::*;

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests;
