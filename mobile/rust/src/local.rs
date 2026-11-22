//! Opens the in-process (on-device) harw session host as a [`Backend`].

use std::{path::PathBuf, sync::Arc};

use crate::{MobileError, PlatformKeystore, backend::Backend};

/// Client label announced to the on-device host in the hello.
#[cfg_attr(not(feature = "local-host"), allow(dead_code))]
pub(crate) const LOCAL_CLIENT_LABEL: &str = "harw-android-local";

/// Error detail of builds without the on-device host.
pub(crate) const LOCAL_HOST_UNAVAILABLE: &str = "the on-device host is not available in this build";

/// Builds without the `local-host` feature have no on-device host.
#[cfg(not(feature = "local-host"))]
pub(crate) async fn open_local(
    files_dir: PathBuf,
    keystore: Arc<dyn PlatformKeystore>,
    features: Vec<String>,
    device: crate::device_tools::DeviceHandles,
) -> Result<Backend, MobileError> {
    let _ = (files_dir, keystore, features, device);
    Err(MobileError::Session {
        detail: LOCAL_HOST_UNAVAILABLE.into(),
    })
}

#[cfg(feature = "local-host")]
fn host_task_failed() -> MobileError {
    MobileError::Internal {
        detail: "on-device host task failed".into(),
    }
}

/// Opens the app home, builds the model of the active route and starts the host.
#[cfg(feature = "local-host")]
pub(crate) async fn open_local(
    files_dir: PathBuf,
    keystore: Arc<dyn PlatformKeystore>,
    features: Vec<String>,
    device: crate::device_tools::DeviceHandles,
) -> Result<Backend, MobileError> {
    use crate::{home::AppHome, providers::ProviderStore};

    let home = AppHome::open(&files_dir)?;
    home.install_process_home()?;
    let store = ProviderStore::from_parts(home.clone(), keystore);
    let resolver = store.resolver().clone();
    let model_home = home.clone();
    let model = tokio::task::spawn_blocking(move || build_active_model(&model_home, &resolver))
        .await
        .map_err(|_| host_task_failed())??;
    // The device tools follow the profile switch (off by default for the on-device model).
    let (shell, files) = device;
    let mut contributors: Vec<Arc<dyn harw_local_host::ext::AssemblyContributor>> = Vec::new();
    if (shell.is_some() || files.is_some()) && home.device_tools_enabled()? {
        contributors.push(Arc::new(crate::device_tools::DeviceToolsContributor::new(
            shell, files,
        )));
    }
    // A phone model has a small context window: the on-device GGUF is
    // text-only ("nur Text, keine Werkzeuge"), a cloud route sees a lean set.
    let surface = if home.active_local_model()?.is_some() {
        harw_local_host::ToolSurface::default()
    } else {
        harw_local_host::ToolSurface {
            allow: LEAN_TOOLS.iter().map(|name| (*name).to_owned()).collect(),
        }
    };
    open_local_with_tools(home, model, features, contributors, Some(surface)).await
}

/// Tools a cloud route on the phone keeps visible: the phone UIA's set and the
/// device tools (the latter only exist when device tools are mounted).
#[cfg(feature = "local-host")]
const LEAN_TOOLS: &[&str] = &[
    "fs.read",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
    "fs.write",
    "fs.edit",
    "plan.current",
    "skills.search",
    "skills.load",
    "device_roots",
    "device_list",
    "device_read",
    "device_write",
    "device_search",
    "termux_run",
];

/// The model of the active selection: the GGUF on this device when that is the
/// active provider, else the HTTP route.
#[cfg(feature = "local-host")]
fn build_active_model(
    home: &crate::home::AppHome,
    resolver: &crate::secrets::KeystoreSecretResolver,
) -> Result<Arc<dyn harw_core::ModelProvider>, MobileError> {
    match home.active_local_model()? {
        Some(model_id) => build_local_gguf(home, &model_id),
        None => crate::providers::build_model_provider(home, resolver),
    }
}

/// Settings of the on-device model: `HARW_LOCAL_N_CTX` sets the context
/// window (default 16384), `HARW_LOCAL_LEAN=0` brings back the full host
/// prompt and tool specs (text-only by default).
#[cfg(feature = "local-host")]
fn local_gguf_config() -> harw_llama::LocalGgufConfig {
    let mut config = harw_llama::LocalGgufConfig::default();
    if let Some(n) = std::env::var("HARW_LOCAL_N_CTX")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|n| *n >= 1024)
    {
        config.n_ctx = n;
    }
    if std::env::var("HARW_LOCAL_LEAN").is_ok_and(|v| v.trim() == "0") {
        config.lean = false;
    }
    config
}

/// Loads the selected GGUF from the model store (memory-mapped, blocking).
#[cfg(feature = "local-host")]
fn build_local_gguf(
    home: &crate::home::AppHome,
    model_id: &str,
) -> Result<Arc<dyn harw_core::ModelProvider>, MobileError> {
    let session = |detail: String| MobileError::Session { detail };
    if !harw_llama::engine_available() {
        return Err(session(
            "on-device models are not available in this build".to_owned(),
        ));
    }
    let models_dir = home.local_models_dir().ok_or_else(|| {
        session("the on-device model folder is unknown; choose the model again".to_owned())
    })?;
    // Fails unless the file is complete and readable (e.g. deleted since selecting).
    let path = crate::models::model_path(models_dir, model_id.to_owned())?;
    let provider = harw_llama::LocalGgufProvider::open(
        std::path::Path::new(&path),
        model_id,
        local_gguf_config(),
    )
    .map_err(|e| session(e.to_string()))?;
    Ok(Arc::new(provider))
}

/// Starts the host over `model` without device tools.
#[cfg(feature = "local-host")]
pub(crate) async fn open_local_with_model(
    home: crate::home::AppHome,
    model: Arc<dyn harw_core::ModelProvider>,
    features: Vec<String>,
) -> Result<Backend, MobileError> {
    open_local_with_tools(home, model, features, Vec::new(), None).await
}

/// Starts the host over `model` with extra tool contributors, connects as its
/// owner and says hello.
#[cfg(feature = "local-host")]
async fn open_local_with_tools(
    home: crate::home::AppHome,
    model: Arc<dyn harw_core::ModelProvider>,
    features: Vec<String>,
    contributors: Vec<Arc<dyn harw_local_host::ext::AssemblyContributor>>,
    surface: Option<harw_local_host::ToolSurface>,
) -> Result<Backend, MobileError> {
    use harw_protocol::{
        SessionPort,
        session_wire::{HelloParams, SESSION_WIRE_MINOR},
    };

    let config = home.local_host_config();
    let host = tokio::task::spawn_blocking(move || {
        harw_local_host::LocalHost::open_with_surface(config, model, contributors, surface)
    })
    .await
    .map_err(|_| host_task_failed())?
    .map_err(|e| MobileError::Internal {
        detail: e.to_string(),
    })?;
    let connection = match host.connect_owner() {
        Ok(connection) => connection,
        Err(e) => {
            host.shutdown();
            return Err(MobileError::Internal {
                detail: e.to_string(),
            });
        }
    };
    let hello = SessionPort::hello(
        &connection,
        HelloParams {
            client_label: LOCAL_CLIENT_LABEL.into(),
            wire_minor: SESSION_WIRE_MINOR,
            features,
            requested_caps: None,
        },
    )
    .await;
    let ack = match hello {
        Ok(ack) => ack,
        Err(e) => {
            host.shutdown();
            return Err(MobileError::from(e));
        }
    };
    Ok(Backend::local(
        Arc::new(connection),
        ack,
        Box::new(LocalGuard {
            host: std::sync::Mutex::new(host),
        }),
    ))
}

/// Keeps the host alive for the backend's lifetime and drains it on drop.
#[cfg(feature = "local-host")]
struct LocalGuard {
    host: std::sync::Mutex<harw_local_host::LocalHost>,
}

#[cfg(feature = "local-host")]
impl Drop for LocalGuard {
    fn drop(&mut self) {
        let host = self
            .host
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        host.shutdown();
    }
}

#[cfg(all(test, feature = "local-host"))]
mod tests {
    use harw_protocol::session_wire::CreateParams;

    use super::*;
    use crate::{backend::BackendKind, home::AppHome};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn model() -> Arc<dyn harw_core::ModelProvider> {
        Arc::new(harw_core::EchoModelProvider::new("hi"))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_backend_creates_and_lists_a_session() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        let backend = open_local_with_model(home, model(), Vec::new()).await?;
        assert_eq!(backend.kind(), BackendKind::Local);
        let granted = backend.granted();
        assert!(granted.steer && granted.approve);
        let summary = backend
            .port()
            .create(CreateParams {
                workspace: None,
                title: Some("t".into()),
            })
            .await
            .map_err(MobileError::from)?;
        let listed = backend.port().list().await.map_err(MobileError::from)?;
        assert!(listed.iter().any(|s| s.session_id == summary.session_id));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_backend_reopens_with_existing_sessions() -> TestResult {
        let dir = tempfile::tempdir()?;
        let home = AppHome::open(dir.path())?;
        let backend = open_local_with_model(home.clone(), model(), Vec::new()).await?;
        let summary = backend
            .port()
            .create(CreateParams {
                workspace: None,
                title: Some("t".into()),
            })
            .await
            .map_err(MobileError::from)?;
        drop(backend);
        let reopened = open_local_with_model(home, model(), Vec::new()).await?;
        let listed = reopened.port().list().await.map_err(MobileError::from)?;
        assert!(listed.iter().any(|s| s.session_id == summary.session_id));
        Ok(())
    }
}

#[cfg(all(test, not(feature = "local-host")))]
mod tests {
    use super::*;
    use crate::secrets::test_support::MemoryKeystore;

    #[tokio::test]
    async fn open_local_is_unavailable_without_the_feature() {
        let result = open_local(
            PathBuf::from("/nonexistent"),
            MemoryKeystore::new(),
            Vec::new(),
            (None, None),
        )
        .await;
        assert!(matches!(
            result.err(),
            Some(MobileError::Session { detail }) if detail == LOCAL_HOST_UNAVAILABLE
        ));
    }
}
