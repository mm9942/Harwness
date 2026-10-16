//! Executor configuration.

use std::path::PathBuf;
use std::time::Duration;

use harw_container_model::{EngineKind, ImageDigest};
use harw_job_core::SandboxProfileName;

use crate::error::OciError;

/// Who must own the engine socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerPolicy {
    /// The socket's peer must run as the same user as this process
    /// (rootless engines; the default).
    SameUser,
    /// The peer must run as exactly this uid (`Uid(0)` opts into a rootful
    /// engine, which is root-equivalent for the caller).
    Uid(u32),
}

/// Configuration of an [`crate::OciExecutor`].
#[derive(Debug, Clone)]
pub struct OciConfig {
    /// Path of the engine's Unix socket.
    pub socket: PathBuf,
    /// Which engine answers (recorded in the identity).
    pub engine: EngineKind,
    /// Required socket owner.
    pub peer: PeerPolicy,
    /// Runner id written to the `harw.owner` label (matches the lease holder).
    pub owner: String,
    /// Tenant written to the `harw.tenant` label.
    pub tenant: String,
    /// Image per sandbox profile, always by digest.
    pub profile_images: Vec<(SandboxProfileName, ImageDigest)>,
    /// Grace between the stop signal and the kill, in seconds.
    pub stop_grace_secs: u32,
    /// Largest accepted engine response body.
    pub max_body_bytes: usize,
    /// Socket connect/read timeout for control calls.
    pub control_timeout: Duration,
    /// Size of the writable scratch tmpfs at `/work`, in bytes.
    pub work_tmpfs_bytes: u64,
}

impl OciConfig {
    /// A configuration with conservative defaults.
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>, engine: EngineKind, owner: &str, tenant: &str) -> Self {
        Self {
            socket: socket.into(),
            engine,
            peer: PeerPolicy::SameUser,
            owner: owner.to_owned(),
            tenant: tenant.to_owned(),
            profile_images: Vec::new(),
            stop_grace_secs: 10,
            max_body_bytes: 4 * 1024 * 1024,
            control_timeout: Duration::from_secs(30),
            work_tmpfs_bytes: 256 * 1024 * 1024,
        }
    }

    /// Maps `profile` to `image` (digest-pinned by construction).
    #[must_use]
    pub fn with_image(mut self, profile: SandboxProfileName, image: ImageDigest) -> Self {
        self.profile_images.retain(|(name, _)| *name != profile);
        self.profile_images.push((profile, image));
        self
    }

    /// Validates the configuration.
    ///
    /// # Errors
    /// [`OciError::Config`] for a label-unsafe owner/tenant or a
    /// non-positive limit.
    pub fn validate(&self) -> Result<(), OciError> {
        if self.engine == EngineKind::Kubernetes {
            return Err(OciError::Config(
                "the OCI executor talks to a container engine, not Kubernetes".into(),
            ));
        }
        harw_container_model::OwnerLabels::new(&self.owner, "x", 0, 0, &self.tenant, "x")
            .map_err(|error| OciError::Config(error.to_string()))?;
        if self.max_body_bytes == 0 || self.stop_grace_secs == 0 || self.work_tmpfs_bytes == 0 {
            return Err(OciError::Config("limits must be positive".into()));
        }
        Ok(())
    }

    pub(crate) fn image_for(&self, profile: SandboxProfileName) -> Option<&ImageDigest> {
        self.profile_images
            .iter()
            .find(|(name, _)| *name == profile)
            .map(|(_, image)| image)
    }
}
