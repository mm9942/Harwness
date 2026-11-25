//! Optional Linux composition adapter for the live NetSec writer.
//!
//! It is not mounted by any listener. A Hub must independently supply the
//! pinned local client and pass only exact authenticated/authorized node IDs
//! using the existing owner-report and session-grant boundaries.

use std::num::NonZeroU64;

use harw_infra_client::OwnerEpochClient;
use tokio::sync::Semaphore;

use crate::owner_directory::OwnerDirectory;
use crate::owner_ingress::{HubSessionGrants, HubTransportIngress};
use crate::owner_liveness::{ActiveOwnerLifecycle, HubOwnerRoutes};

// Process-wide, so constructing another adapter never increases capacity.
// Permit acquisition is non-blocking. There is no pending queue or cache.
static READS: Semaphore = Semaphore::const_new(16);

/// Sources activation epochs from the current NetSec writer, not from its
/// state file, the reporting owner, a route, or an external network endpoint.
pub struct NetsecOwnerLifecycle {
    client: OwnerEpochClient,
}

impl NetsecOwnerLifecycle {
    /// Uses the explicitly selected and kernel-peer-pinned local client.
    pub fn new(client: OwnerEpochClient) -> Self {
        Self { client }
    }
}

impl ActiveOwnerLifecycle for NetsecOwnerLifecycle {
    fn active_epoch(&self, node_id: &str) -> Option<NonZeroU64> {
        let _permit = READS.try_acquire().ok()?;
        // The liveness contract is synchronous and Gateway calls it on its
        // bounded blocking workers. A fresh scoped thread, like the Infra
        // DEK bridge, avoids a nested Tokio runtime and cannot outlive this
        // call. The client applies a single connect-through-response deadline.
        std::thread::scope(|scope| {
            let worker = std::thread::Builder::new()
                .name("harw-owner-epoch".to_owned())
                .spawn_scoped(scope, || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .ok()?;
                    runtime.block_on(self.client.active_epoch(node_id)).ok().flatten()
                })
                .ok()?;
            worker.join().ok().flatten()
        })
    }
}


impl<A: HubSessionGrants> HubTransportIngress<NetsecOwnerLifecycle, A> {
    /// Compose the already-selected directory, explicitly pinned live NetSec
    /// source, and independent exact-session resolver. Does not dial or
    /// attach, install a listener, or discover a target on the network.
    pub fn with_pinned_netsec(
        directory: OwnerDirectory,
        client: OwnerEpochClient,
        authorization: A,
    ) -> Self {
        Self::new(
            HubOwnerRoutes::new(directory, NetsecOwnerLifecycle::new(client)),
            authorization,
        )
    }
}
