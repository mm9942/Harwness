//! [`NetworkControlClient`]: skeleton client of the NetSec daemon's local
//! control API (`network.sock`, masterplan §6.2).
//!
//! Common health queries plus a separately pinned, read-only lifecycle client.

use core::fmt;
#[cfg(target_os = "linux")]
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use http::Method;
#[cfg(target_os = "linux")]
use serde::Deserialize;

use crate::error::InfraClientError;
use crate::info::{self, Capabilities, Health, VersionInfo};
#[cfg(target_os = "linux")]
use crate::transport::RequestBody;
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


/// Read-only client for the opt-in NetSec owner epoch operation on Linux.
/// This is not a general NetSec operations client and never opens another
/// socket or reads the persisted state as a fallback.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct OwnerEpochClient {
    transport: Arc<UdsTransport>,
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum OwnerEpochResponse {
    Active { node: String, epoch: NonZeroU64 },
    Unavailable { node: String },
}

/// An actual activation observed over the configured, UID-pinned NetSec
/// socket. The incarnation changes on daemon restart and cannot be supplied
/// by the caller. Neither value is an assignment or permission.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OwnerActivationObservation {
    epoch: NonZeroU64,
    authority_instance: [u8; 32],
}

#[cfg(target_os = "linux")]
impl OwnerActivationObservation {
    /// The current active epoch of the selected node.
    #[must_use]
    pub fn epoch(self) -> NonZeroU64 {
        self.epoch
    }

    /// The ephemeral incarnation of the particular daemon that observed it.
    #[must_use]
    pub fn authority_instance(self) -> [u8; 32] {
        self.authority_instance
    }
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum OwnerAuthorityResponse {
    Active {
        node: String,
        epoch: NonZeroU64,
        authority_instance: [u8; 32],
    },
    Unavailable { node: String },
}

#[cfg(target_os = "linux")]
impl OwnerEpochClient {
    /// Selects the one operator-configured local listener.
    ///
    /// The path must be absolute and its directory must be managed by the
    /// operator, not writable by the Hub UID or untrusted users. The UID
    /// pins the kernel-reported peer, not a daemon binary or HTTP field.
    /// Nothing enables this operation without the daemon's own allowlist.
    ///
    /// # Errors
    /// Returns a protocol error for a relative path.
    pub fn new(socket: impl Into<PathBuf>, expected_peer_uid: u32) -> Result<Self, InfraClientError> {
        let socket = socket.into();
        if !socket.is_absolute() {
            return Err(InfraClientError::Protocol("owner epoch socket must be absolute"));
        }
        let options = ClientOptions {
            timeout: Duration::from_millis(750),
            max_response_bytes: 512,
        };
        Ok(Self {
            transport: Arc::new(
                UdsTransport::new(socket, None, options).pin_local_peer_uid(expected_peer_uid),
            ),
        })
    }

    /// Reads the current epoch of exactly the selected node. Callers at a
    /// remote boundary must derive it independently from authenticated
    /// identity and their authorization policy, not from payload or headers.
    ///
    /// # Errors
    /// Invalid identifiers, auth, deadline, I/O and wire failures all fail
    /// closed. `Ok(None)` is the confirmed writer's negative answer.
    pub async fn active_epoch(&self, node: &str) -> Result<Option<NonZeroU64>, InfraClientError> {
        validate_owner_node(node)?;
        let path = format!("/v1/owner-epochs/{node}");
        let response = self.transport.call(Method::GET, &path, RequestBody::Empty, None).await?;
        response.expect_content_type("application/json")?;
        let answer = serde_json::from_slice::<OwnerEpochResponse>(&response.body)
            .map_err(|_| InfraClientError::Protocol("invalid owner epoch response"))?;
        match answer {
            OwnerEpochResponse::Active { node: echoed, epoch } if echoed == node => Ok(Some(epoch)),
            OwnerEpochResponse::Unavailable { node: echoed } if echoed == node => Ok(None),
            _ => Err(InfraClientError::Protocol("owner epoch response names another node")),
        }
    }

    /// Reads an activation and its daemon incarnation from the pinned Unix
    /// peer. The selected node must be authorized by the caller separately.
    ///
    /// # Errors
    /// Invalid identifiers, peer auth, I/O, old peers without v2, and
    /// malformed responses fail closed; this operation never falls back to v1.
    pub async fn active_authority(
        &self,
        node: &str,
    ) -> Result<Option<OwnerActivationObservation>, InfraClientError> {
        validate_owner_node(node)?;
        let path = format!("/v2/owner-epochs/{node}");
        let response = self.transport.call(Method::GET, &path, RequestBody::Empty, None).await?;
        response.expect_content_type("application/json")?;
        let answer = serde_json::from_slice::<OwnerAuthorityResponse>(&response.body)
            .map_err(|_| InfraClientError::Protocol("invalid owner authority response"))?;
        match answer {
            OwnerAuthorityResponse::Active { node: echoed, epoch, authority_instance }
                if echoed == node && authority_instance != [0; 32] =>
            {
                Ok(Some(OwnerActivationObservation { epoch, authority_instance }))
            }
            OwnerAuthorityResponse::Unavailable { node: echoed } if echoed == node => Ok(None),
            _ => Err(InfraClientError::Protocol("invalid owner authority identity")),
        }
    }
}

#[cfg(target_os = "linux")]
fn validate_owner_node(node: &str) -> Result<(), InfraClientError> {
    if node.is_empty()
        || node.len() > 128
        || node.starts_with('.')
        || !node.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(InfraClientError::Protocol("invalid owner epoch node token"));
    }
    Ok(())
}
