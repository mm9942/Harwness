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
) -> Result<Backend, MobileError> {
    let _ = (files_dir, keystore, features);
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
) -> Result<Backend, MobileError> {
    use crate::{home::AppHome, providers::{ProviderStore, build_model_provider}};

    let home = AppHome::open(&files_dir)?;
    home.install_process_home()?;
    let store = ProviderStore::from_parts(home.clone(), keystore);
    let resolver = store.resolver().clone();
    let model_home = home.clone();
    let model = tokio::task::spawn_blocking(move || build_model_provider(&model_home, &resolver))
        .await
        .map_err(|_| host_task_failed())??;
    open_local_with_model(home, model, features).await
}

/// Starts the host over `model`, connects as its owner and says hello.
#[cfg(feature = "local-host")]
pub(crate) async fn open_local_with_model(
    home: crate::home::AppHome,
    model: Arc<dyn harw_core::ModelProvider>,
    features: Vec<String>,
) -> Result<Backend, MobileError> {
    use harw_protocol::{SessionPort, session_wire::{HelloParams, SESSION_WIRE_MINOR}};

    let config = home.local_host_config();
    let host = tokio::task::spawn_blocking(move || harw_local_host::LocalHost::open(config, model))
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
    use harw_protocol::{SessionPort, session_wire::CreateParams};

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
        )
        .await;
        assert!(matches!(
            result.err(),
            Some(MobileError::Session { detail }) if detail == LOCAL_HOST_UNAVAILABLE
        ));
    }
}
