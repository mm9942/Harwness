//! Publication and listener health of the embedded Onion service.

use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::watch;
use tor_hsservice::RunningOnionService;

/// Arti's local view of the service. An independent probe checks actual reachability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnionServiceStatus {
    /// Descriptor publication is pending or the service is degraded.
    Publishing,
    /// Arti reports the service as fully reachable.
    Published,
    /// Arti stopped producing inbound rendezvous requests.
    AcceptLoopEnded,
    /// Arti stopped producing service status events.
    StatusStreamEnded,
}

pub(super) async fn observe(
    service: Arc<RunningOnionService>,
    health: watch::Sender<OnionServiceStatus>,
) {
    let mut events = std::pin::pin!(service.status_events());
    loop {
        let status = if service.status().state().is_fully_reachable() {
            OnionServiceStatus::Published
        } else {
            OnionServiceStatus::Publishing
        };
        report(&health, status);
        if events.next().await.is_none() {
            report(&health, OnionServiceStatus::StatusStreamEnded);
            break;
        }
    }
}

pub(super) fn report(health: &watch::Sender<OnionServiceStatus>, status: OnionServiceStatus) {
    let changed = health.send_if_modified(|current| {
        if *current == status
            || matches!(
                *current,
                OnionServiceStatus::AcceptLoopEnded | OnionServiceStatus::StatusStreamEnded
            )
        {
            return false;
        }
        *current = status;
        true
    });
    if changed {
        match status {
            OnionServiceStatus::Published => tracing::info!("onion descriptor published"),
            OnionServiceStatus::Publishing => tracing::warn!("onion service publication degraded"),
            OnionServiceStatus::AcceptLoopEnded => tracing::warn!("onion accept loop ended"),
            OnionServiceStatus::StatusStreamEnded => {
                tracing::warn!("onion service status stream ended")
            }
        }
    }
}
