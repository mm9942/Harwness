//! Recovery of an embedded Onion service under the same persisted identity.

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::{OnionServer, OnionServerConfig, OnionServiceStatus};
use crate::{BootstrapProgress, OnionStage, OnionStream};

const PUBLICATION_BUDGET: Duration = Duration::from_secs(600);
const ACTIVE_STREAM_GRACE: Duration = Duration::from_secs(60);
const INITIAL_BACKOFF: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Owns a single embedded Onion publisher and its recovery task.
pub struct OnionSupervisor {
    task: JoinHandle<()>,
}

impl OnionSupervisor {
    /// Supervises an already-started service. A replacement must load the
    /// started service's identity and the old tasks are joined first.
    #[must_use]
    pub fn start<P, H, F>(
        mut server: OnionServer,
        mut config: OnionServerConfig,
        progress: P,
        handler: H,
    ) -> Self
    where
        P: FnMut(&BootstrapProgress) + Clone + Send + 'static,
        H: Fn(OnionStream) -> F + Clone + Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        config.expected_locator = Some(server.locator().clone());
        let task = tokio::spawn(async move {
            loop {
                wait_until_unhealthy(&server).await;
                tracing::warn!("restarting embedded onion service");
                server.stop_and_join().await;
                let mut delay = INITIAL_BACKOFF;
                server = loop {
                    tokio::time::sleep(delay).await;
                    match OnionServer::launch(config.clone(), progress.clone(), handler.clone())
                        .await
                    {
                        Ok(replacement) => break replacement,
                        Err(error) => {
                            let stage = error.stage();
                            tracing::warn!(?stage, "embedded onion recovery failed");
                            if matches!(
                                stage,
                                OnionStage::Identity
                                    | OnionStage::Configuration
                                    | OnionStage::Compatibility
                            ) {
                                return;
                            }
                            delay = (delay * 2).min(MAX_BACKOFF);
                        }
                    }
                };
            }
        });
        Self { task }
    }
}

impl Drop for OnionSupervisor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn wait_until_unhealthy(server: &OnionServer) {
    let mut status = server.status();
    let mut publication_deadline = tokio::time::Instant::now() + PUBLICATION_BUDGET;
    loop {
        let current = *status.borrow_and_update();
        match current {
            OnionServiceStatus::AcceptLoopEnded | OnionServiceStatus::StatusStreamEnded => return,
            OnionServiceStatus::Publishing => {
                match tokio::time::timeout_at(publication_deadline, status.changed()).await {
                    Ok(Ok(())) => {}
                    Err(_) if server.has_active_streams() => {
                        publication_deadline = tokio::time::Instant::now() + ACTIVE_STREAM_GRACE;
                    }
                    Ok(Err(_)) | Err(_) => return,
                }
            }
            OnionServiceStatus::Published => {
                if status.changed().await.is_err() {
                    return;
                }
                publication_deadline = tokio::time::Instant::now() + PUBLICATION_BUDGET;
            }
        }
    }
}
