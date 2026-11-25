//! Deadline and retry policy for establishing an Onion stream.

use std::future::Future;
use std::time::Duration;

use arti_client::ErrorKind;
use tokio::time::Instant;

use crate::OnionError;

pub(super) async fn dial_before_deadline<T>(
    deadline: Instant,
    total: Duration,
    bootstrap: impl Future<Output = Result<(), OnionError>>,
    dial: impl Future<Output = Result<T, OnionError>>,
) -> Result<T, OnionError> {
    tokio::time::timeout_at(deadline, bootstrap)
        .await
        .map_err(|_| OnionError::BootstrapTimeout(total))??;
    if Instant::now() >= deadline {
        return Err(OnionError::ConnectTimeout(total));
    }
    tokio::time::timeout_at(deadline, dial)
        .await
        .map_err(|_| OnionError::ConnectTimeout(total))?
}

pub(super) fn circuit_retry_delay(kind: ErrorKind) -> Option<Duration> {
    match kind {
        // Arti's Onion directory can lag behind its general bootstrap.
        ErrorKind::BootstrapRequired => Some(Duration::from_secs(2)),
        ErrorKind::TorNetworkTimeout
        | ErrorKind::RemoteNetworkTimeout
        | ErrorKind::OnionServiceConnectionFailed => Some(Duration::ZERO),
        _ => None,
    }
}

pub(super) async fn retry_circuit<T, E, F: Future<Output = Result<T, E>>>(
    initial_budget: Duration,
    mut connect: impl FnMut(bool) -> F,
    retry_delay: impl FnOnce(&E) -> Option<Duration>,
) -> Result<T, E> {
    match tokio::time::timeout(initial_budget, connect(false)).await {
        Ok(Ok(stream)) => return Ok(stream),
        Ok(Err(error)) => {
            let Some(delay) = retry_delay(&error) else {
                return Err(error);
            };
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        Err(_) => {}
    }
    connect(true).await
}
