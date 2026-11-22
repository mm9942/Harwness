//! Native client for Harw's Android phone-test app.

mod backend;
mod core;
mod device;
mod home;
mod identity;
mod listener;
mod local;
mod manage;
mod mission;
mod models;
mod projection;
mod provider;
mod providers;
mod qr;
mod secrets;
mod types;

pub use core::MobileCore;
pub use device::{
    DeviceError, DeviceLimits, FileAccess, SafEntry, SafRoot, ShellExecutor, ShellOutput,
    ShellRequest, check_shell_request, device_limits, normalize_device_path,
};
pub use identity::{generate_identity, parse_device_enrollment, parse_server_profile};
pub use listener::CoreListener;
pub use models::{
    CatalogModel, ImportCheck, ImportVerdict, LocalModel, ModelFileState, ModelOrigin,
    ModelProgress, ModelVerification, commit_import, delete_model, discard_import, finalize_model,
    import_model, local_models, model_catalog, model_path,
};
pub use provider::{ChatgptLinkAvailability, ChatgptProviderCredential, ChatgptProviderStatus};
pub use providers::{ProviderStore, active_local_model, activate_local_model};
pub use qr::{ScannedProfile, decode_pairing_text, scan_pairing_frame};
pub use secrets::{KeystoreError, KeystoreSecretResolver, PlatformKeystore};
pub use types::*;

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests;
