//! Composition entry: production ports are injected as trait objects.
//!
//! The daemon depends only on the host's port traits. The production
//! `CoreTurnDriver` (S03) and `Durable*` adapters (S04) are built by the
//! binary that owns those dependencies and handed in here, so this crate
//! compiles and tests without them.

use std::sync::Arc;

use harw_session_host::approvals::{ApprovalBackend, MemoryApprovals};
use harw_session_host::driver::TurnDriver;
use harw_session_host::replay::{MemoryTranscripts, TranscriptSource};
use harw_session_host::{HostConfig, SessionHost};
use tokio::sync::watch;

use crate::server::{DaemonError, UdsConfig, UdsServer};

/// The ports a [`SessionHost`] needs, as trait objects.
#[derive(Clone)]
pub struct DaemonServices {
    /// Turn execution (production: `CoreTurnDriver`).
    pub driver: Arc<dyn TurnDriver>,
    /// Transcript replay (production: `DurableTranscripts`).
    pub transcripts: Arc<dyn TranscriptSource>,
    /// Approval store (production: `DurableApprovals`).
    pub approvals: Arc<dyn ApprovalBackend>,
}

impl std::fmt::Debug for DaemonServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonServices").finish_non_exhaustive()
    }
}

impl DaemonServices {
    /// Explicit services.
    #[must_use]
    pub fn new(
        driver: Arc<dyn TurnDriver>,
        transcripts: Arc<dyn TranscriptSource>,
        approvals: Arc<dyn ApprovalBackend>,
    ) -> Self {
        Self {
            driver,
            transcripts,
            approvals,
        }
    }

    /// In-memory transcripts and approvals around the given driver. Nothing
    /// survives a restart; for tests and development only.
    #[must_use]
    pub fn in_memory(driver: Arc<dyn TurnDriver>) -> Self {
        Self::new(
            driver,
            Arc::new(MemoryTranscripts::new()),
            Arc::new(MemoryApprovals::new()),
        )
    }
}

/// A bound daemon: the listener plus the host it serves.
#[derive(Debug)]
pub struct Daemon {
    server: UdsServer,
    host: SessionHost,
}

impl Daemon {
    /// Open the host over `services`, then bind the socket. A host failure
    /// never touches the socket path.
    pub async fn start(
        host_config: HostConfig,
        services: DaemonServices,
        uds: UdsConfig,
    ) -> Result<Self, DaemonError> {
        let host = SessionHost::open(
            host_config,
            services.driver,
            services.transcripts,
            services.approvals,
        )
        .map_err(|e| DaemonError::Host(e.to_string()))?;
        let server = UdsServer::bind(uds).await?;
        Ok(Self { server, host })
    }

    /// The host being served (for in-process callers such as tests).
    #[must_use]
    pub fn host(&self) -> &SessionHost {
        &self.host
    }

    /// Serve until `shutdown` flips, then drain (see [`UdsServer::run`]).
    pub async fn run(self, shutdown: watch::Receiver<bool>) -> Result<(), DaemonError> {
        self.server.run(self.host, shutdown).await
    }
}
