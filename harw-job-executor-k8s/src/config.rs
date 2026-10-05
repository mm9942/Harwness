//! Executor configuration.

use std::fmt;
use std::time::Duration;

use harw_container_model::ImageDigest;
use harw_job_core::SandboxProfileName;

use crate::error::K8sError;

/// Configuration of a [`crate::K8sExecutor`].
#[derive(Clone)]
pub struct K8sConfig {
    /// API server base URL. `https://` for a cluster; `http://` only for a
    /// loopback `kubectl proxy`.
    pub api_base: String,
    /// Bearer token (never printed).
    pub token: Option<String>,
    /// PEM bundle of the cluster CA.
    pub ca_pem: Option<Vec<u8>>,
    /// Namespace (one per tenant).
    pub namespace: String,
    /// Runner id for the `harw.owner` label.
    pub owner: String,
    /// Tenant for the `harw.tenant` label.
    pub tenant: String,
    /// Image per sandbox profile, always by digest.
    pub profile_images: Vec<(SandboxProfileName, ImageDigest)>,
    /// The operator attests that the namespace's NetworkPolicy denies egress.
    /// Without it the network dimension is at most `Partial`.
    pub network_attested: bool,
    /// Numeric uid/gid the container runs as.
    pub run_as: i64,
    /// Grace period of a pod deletion, in seconds.
    pub stop_grace_secs: u32,
    /// Pause between pod status polls.
    pub poll_interval: Duration,
    /// Largest accepted API response body.
    pub max_body_bytes: usize,
    /// Request timeout of control calls.
    pub request_timeout: Duration,
    /// Scratch `emptyDir` size limit in bytes (mounted at `/work`).
    pub work_bytes: u64,
}

impl fmt::Debug for K8sConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("K8sConfig")
            .field("api_base", &self.api_base)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("namespace", &self.namespace)
            .field("owner", &self.owner)
            .field("tenant", &self.tenant)
            .field("network_attested", &self.network_attested)
            .finish_non_exhaustive()
    }
}

impl K8sConfig {
    /// A configuration with conservative defaults.
    #[must_use]
    pub fn new(api_base: &str, namespace: &str, owner: &str, tenant: &str) -> Self {
        Self {
            api_base: api_base.trim_end_matches('/').to_owned(),
            token: None,
            ca_pem: None,
            namespace: namespace.to_owned(),
            owner: owner.to_owned(),
            tenant: tenant.to_owned(),
            profile_images: Vec::new(),
            network_attested: false,
            run_as: 65532,
            stop_grace_secs: 10,
            poll_interval: Duration::from_millis(500),
            max_body_bytes: 4 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            work_bytes: 256 * 1024 * 1024,
        }
    }

    /// Maps `profile` to `image`.
    #[must_use]
    pub fn with_image(mut self, profile: SandboxProfileName, image: ImageDigest) -> Self {
        self.profile_images.retain(|(name, _)| *name != profile);
        self.profile_images.push((profile, image));
        self
    }

    /// Validates the configuration.
    ///
    /// # Errors
    /// [`K8sError::Config`].
    pub fn validate(&self) -> Result<(), K8sError> {
        let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|p| {
                self.api_base
                    .strip_prefix(p)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
            });
        if !(self.api_base.starts_with("https://") || loopback) {
            return Err(K8sError::Config(
                "api_base must be https:// (http:// only for loopback)".into(),
            ));
        }
        if self.token.is_some() && !self.api_base.starts_with("https://") && !loopback {
            return Err(K8sError::Config("token over cleartext".into()));
        }
        if !is_dns_label(&self.namespace) {
            return Err(K8sError::Config("namespace must be a DNS label".into()));
        }
        harw_container_model::OwnerLabels::new(&self.owner, "x", 0, 0, &self.tenant, "x")
            .map_err(|e| K8sError::Config(e.to_string()))?;
        if self.run_as <= 0 {
            return Err(K8sError::Config("run_as must be a non-root uid".into()));
        }
        if self.max_body_bytes == 0 || self.work_bytes == 0 {
            return Err(K8sError::Config("limits must be positive".into()));
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

pub(crate) fn is_dns_label(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 63
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !text.starts_with('-')
        && !text.ends_with('-')
}
