//! Native client for Harw's Android phone-test app.

mod backend;
mod chatgpt_local;
mod claude_local;
mod codex_local;
mod core;
mod device;
mod device_tools;
pub mod diagnostics;
mod home;
mod hybrid;
mod identity;
#[cfg(feature = "local-host")]
mod image_edit;
mod image_edit_mobile;
mod listener;
mod local;
mod local_vision;
mod manage;
mod mission;
mod models;
mod overview;
#[cfg(all(feature = "vault-uia-native", unix))]
#[allow(dead_code)] // Requires an enrolled Library owner and Android bridge.
mod private_library;
#[cfg(all(feature = "vault-uia-chat", unix))]
mod private_library_owner;
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
mod private_library_jni;
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
mod private_library_caption;
mod projection;
mod provider;
mod providers;
mod qr;
mod secrets;
mod session_title;
#[cfg(all(feature = "vault-pin-native", target_os = "android"))]
mod vault_pin_jni;
#[cfg(all(feature = "vault-pin-native", any(target_os = "android", test)))]
mod vault_pin_kdf;
mod types;
mod uia;
// Vault adapter and Android entry point are the next integration gate.
#[cfg(feature = "local-host")]
#[allow(dead_code)]
mod uia_room;
#[cfg(any(feature = "local-host", all(feature = "vault-uia-native", unix), test))]
#[allow(dead_code)]
mod uia_room_store;
#[cfg(all(feature = "vault-uia-native", unix))]
#[allow(dead_code)] // Never reachable without the future authenticated vault owner.
mod uia_room_vault;
#[cfg(all(feature = "vault-uia-native", unix))]
#[allow(dead_code)] // Native-only owner; not accessible from the APK or JNI.
mod uia_room_handoff;
#[cfg(all(feature = "vault-uia-native", unix))]
#[allow(dead_code)] // Explicit enrollment; no Android or JNI entry point.
mod uia_room_enrollment;
#[cfg(all(feature = "vault-uia-jni", unix))]
#[allow(dead_code)] // No interface to submit/read a chat from Kotlin yet.
mod uia_room_registry;
#[cfg(all(feature = "vault-uia-jni", target_os = "android"))]
mod uia_room_jni;
#[cfg(all(feature = "vault-uia-chat", unix))]
mod uia_room_operations;
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
mod uia_room_chat_jni;
#[cfg(test)]
mod vision_admission;
#[cfg(test)]
mod vision_batch;

pub use chatgpt_local::{ChatgptLocal, ChatgptLocalStatus};
pub use claude_local::{ClaudeLocal, ClaudeLocalStatus};
pub use codex_local::{CodexLocal, CodexLocalStatus, CodexSignInStart};
pub use core::cloud_assistant::{
    CloudAssistantAccess, MobileCloudAssistantSharedCommand, MobileCloudAssistantSharedDevice,
    MobileCloudAssistantSharedSnapshot, MobileCloudAssistantSharedStatus,
    MobileCloudAssistantSnapshot, MobileCloudAssistantStatus, MobileCloudAssistantTurn,
};
pub use core::auto_title::{AutoTitleAccess, AutoTitlePublication, AutoTitleSeedPair};
pub use core::MobileCore;
pub use core::manual_cloud_work::ManualCloudWorkAccess;
pub use core::local_assistant_sessions::LocalAssistantSessionAccess;
pub use core::media_preview::{MobileMediaPreview, MobileMediaPreviewMode, MobileMediaScope};
pub use core::cloud_library::{
    CloudLibraryCaptionAccess, CloudLibraryGeneratedCaption, MobileCloudLibraryAsset, MobileCloudLibraryCommand, MobileCloudLibraryReply,
    MobileCloudLibraryScope,
};
pub use core::archive::{
    MobileArchiveBackend, MobileArchiveCheckpoint, MobileArchivePage, MobileArchiveRole,
    MobileArchiveRetainedRevision, MobileArchiveRewriteCause, MobileArchiveRow,
    MobileArchiveRowLocator, MobileArchiveScope,
};
pub use device::{
    DeviceError, DeviceLimits, FileAccess, SafEntry, SafRoot, ShellExecutor, ShellOutput,
    ShellRequest, check_shell_request, device_limits, normalize_device_path,
};
pub use device_tools::{device_tools_enabled, set_device_tools_enabled};
pub use hybrid::{
    ApiVisionChoice, ApiVisionObservation, ApiVisionStatus, HybridVisionChoice,
    HybridVisionProgress,
};
pub use identity::{
    generate_identity, parse_device_enrollment, parse_server_profile, sign_admin_enrollment,
};
pub use session_title::{SessionTitleApiChoice, SessionTitleLocalChoice, SessionTitleTurn};
pub use image_edit_mobile::{ApiImageEditAsset, ApiImageEditChoice, ApiImageEditDraft};
pub use listener::CoreListener;
pub use local::set_local_image_concurrency;
pub use local_vision::{
    LocalVisionChoice, LocalVisionImage, LocalVisionOutcome, LocalVisionProgress,
};
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
