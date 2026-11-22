//! Native client for Harw's Android phone-test app.

mod backend;
mod chatgpt_local;
mod claude_local;
mod codex_local;
mod core;
mod device;
mod device_tools;
mod home;
mod identity;
mod listener;
mod local;
mod manage;
mod mission;
mod models;
mod overview;
mod projection;
mod provider;
mod providers;
mod qr;
mod secrets;
mod types;
mod uia;

pub use chatgpt_local::{ChatgptLocal, ChatgptLocalStatus};
pub use claude_local::{ClaudeLocal, ClaudeLocalStatus};
pub use codex_local::{CodexLocal, CodexLocalStatus, CodexSignInStart};
pub use core::MobileCore;
pub use device::{
    DeviceError, DeviceLimits, FileAccess, SafEntry, SafRoot, ShellExecutor, ShellOutput,
    ShellRequest, check_shell_request, device_limits, normalize_device_path,
};
pub use device_tools::{device_tools_enabled, set_device_tools_enabled};
pub use identity::{
    generate_identity, parse_device_enrollment, parse_server_profile, sign_admin_enrollment,
};
pub use listener::CoreListener;
pub use models::{
    CatalogModel, ImportCheck, ImportVerdict, LocalModel, ModelFileState, ModelOrigin,
    ModelProgress, ModelVerification, commit_import, delete_model, discard_import, finalize_model,
    import_model, local_models, model_catalog, model_path,
};
pub use overview::{
    OverviewAgent, OverviewAgentDef, OverviewAgents, OverviewJob, OverviewJobDetail,
    OverviewJobEvent, OverviewJobs,
};
pub use provider::{ChatgptLinkAvailability, ChatgptProviderCredential, ChatgptProviderStatus};
pub use providers::{ProviderStore, activate_local_model, active_local_model};
pub use qr::{ScannedProfile, decode_pairing_text, scan_pairing_frame};
pub use secrets::{KeystoreError, KeystoreSecretResolver, PlatformKeystore};
pub use types::*;
pub use uia::{UiaProfile, UiaSetup, UiaStore};

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests;
