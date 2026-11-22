//! Native client for Harw's Android phone-test app.

mod core;
mod home;
mod identity;
mod mission;
mod projection;
mod provider;
mod providers;
mod secrets;
mod types;
mod backend;
mod device;
mod listener;
mod local;
mod models;

pub use core::MobileCore;
pub use identity::{generate_identity, parse_device_enrollment, parse_server_profile};
pub use provider::{ChatgptProviderCredential, ChatgptProviderStatus};
pub use providers::ProviderStore;
pub use secrets::{KeystoreError, KeystoreSecretResolver, PlatformKeystore};
pub use types::*;
pub use device::{
    DeviceError, DeviceLimits, FileAccess, SafEntry, SafRoot, ShellExecutor, ShellOutput,
    ShellRequest, check_shell_request, device_limits, normalize_device_path,
};
pub use listener::CoreListener;
pub use models::{
    CatalogModel, LocalModel, ModelFileState, delete_model, finalize_model, local_models,
    model_catalog,
};

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests;
