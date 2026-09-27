//! [`NetworkControlClient`]: skeleton client of the NetSec daemon's local
//! control API (`network.sock`, masterplan §6.2).
//!
//! Only the common surface exists yet (health, version, capabilities).
//! Node, route, listener and drain operations come with the daemon (H7).

use core::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::InfraClientError;
use crate::info::{self, Capabilities, Health, VersionInfo};
use crate::transport::{ClientOptions, UdsTransport};

/// Default NetSec control socket (drift report D1).
pub const DEFAULT_NETWORK_SOCKET: &str = "/run/harw/infra/network.sock";

/// Client of the NetSec local control API. `Clone` shares only immutable
/// connection parameters.
#[derive(Clone)]
pub struct NetworkControlClient {
    transport: Arc<UdsTransport>,
}

impl NetworkControlClient {
    /// A client for the daemon at `socket`.
    pub fn new(socket: impl Into<PathBuf>, options: ClientOptions) -> Self {
        Self {
            transport: Arc::new(UdsTransport::new(socket.into(), None, options)),
        }
    }

    /// The configured socket path.
    pub fn socket_path(&self) -> &Path {
        self.transport.socket()
    }

    /// `GET /v1/health`.
    pub async fn health(&self) -> Result<Health, InfraClientError> {
        info::health(&self.transport).await
    }

    /// `GET /v1/version`.
    pub async fn version(&self) -> Result<VersionInfo, InfraClientError> {
        info::version(&self.transport).await
    }

    /// `GET /v1/capabilities` (descriptive, not authority).
    pub async fn capabilities(&self) -> Result<Capabilities, InfraClientError> {
        info::capabilities(&self.transport).await
    }
}

impl fmt::Debug for NetworkControlClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkControlClient")
            .field("socket", &self.transport.socket())
            .field("options", &self.transport.options())
            .finish()
    }
}
