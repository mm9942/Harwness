//! Typed, backend-neutral contracts for installing bounded page bridges.

use std::time::Duration;

use crate::error::{Error, Result};
use crate::ids::BrowserContextId;

/// Trust assigned to every value emitted by page-owned bridge code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PageBridgeContentTrust {
    Untrusted,
}

/// Payload, rate, and time bounds enforced for one installed bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageBridgePolicy {
    max_payload_bytes: usize,
    max_messages: u32,
    window: Duration,
}

impl PageBridgePolicy {
    pub fn new(max_payload_bytes: usize, max_messages: u32, window: Duration) -> Result<Self> {
        if max_payload_bytes == 0 {
            return Err(Error::InvalidArgument {
                detail: "page bridge payload limit must be greater than zero".to_owned(),
            });
        }
        if max_messages == 0 {
            return Err(Error::InvalidArgument {
                detail: "page bridge message limit must be greater than zero".to_owned(),
            });
        }
        if window.is_zero() {
            return Err(Error::InvalidArgument {
                detail: "page bridge rate window must be greater than zero".to_owned(),
            });
        }
        Ok(Self {
            max_payload_bytes,
            max_messages,
            window,
        })
    }

    pub fn max_payload_bytes(&self) -> usize {
        self.max_payload_bytes
    }

    pub fn max_messages(&self) -> u32 {
        self.max_messages
    }

    pub fn window(&self) -> Duration {
        self.window
    }
}

/// Owned, validated request to install versioned script source in one context.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageBridgeInstallRequest {
    context_id: BrowserContextId,
    bridge_id: String,
    version: String,
    script: String,
    policy: PageBridgePolicy,
    emitted_content_trust: PageBridgeContentTrust,
}

impl PageBridgeInstallRequest {
    pub fn new(
        context_id: BrowserContextId,
        bridge_id: impl Into<String>,
        version: impl Into<String>,
        script: impl Into<String>,
        policy: PageBridgePolicy,
    ) -> Result<Self> {
        let bridge_id = nonempty("page bridge id", bridge_id.into())?;
        let version = nonempty("page bridge version", version.into())?;
        let script = nonempty("page bridge script", script.into())?;
        Ok(Self {
            context_id,
            bridge_id,
            version,
            script,
            policy,
            emitted_content_trust: PageBridgeContentTrust::Untrusted,
        })
    }

    pub fn context_id(&self) -> BrowserContextId {
        self.context_id
    }

    pub fn bridge_id(&self) -> &str {
        &self.bridge_id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn script(&self) -> &str {
        &self.script
    }

    pub fn policy(&self) -> &PageBridgePolicy {
        &self.policy
    }

    pub fn emitted_content_trust(&self) -> PageBridgeContentTrust {
        self.emitted_content_trust
    }
}

/// Opaque identity assigned to one successful bridge installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PageBridgeInstallationId(uuid::Uuid);

impl PageBridgeInstallationId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for PageBridgeInstallationId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PageBridgeInstallationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

/// Stable receipt returned after the backend installs the exact script version.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageBridgeInstallationReceipt {
    installation_id: PageBridgeInstallationId,
    context_id: BrowserContextId,
    bridge_id: String,
    version: String,
    script_sha256: String,
}

impl PageBridgeInstallationReceipt {
    pub fn new(
        installation_id: PageBridgeInstallationId,
        context_id: BrowserContextId,
        bridge_id: impl Into<String>,
        version: impl Into<String>,
        script_sha256: impl Into<String>,
    ) -> Self {
        Self {
            installation_id,
            context_id,
            bridge_id: bridge_id.into(),
            version: version.into(),
            script_sha256: script_sha256.into(),
        }
    }

    pub fn installation_id(&self) -> PageBridgeInstallationId {
        self.installation_id
    }

    pub fn context_id(&self) -> BrowserContextId {
        self.context_id
    }

    pub fn bridge_id(&self) -> &str {
        &self.bridge_id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn script_sha256(&self) -> &str {
        &self.script_sha256
    }
}

fn nonempty(kind: &str, value: String) -> Result<String> {
    if value.trim().is_empty() {
        return Err(Error::InvalidArgument {
            detail: format!("{kind} must not be empty"),
        });
    }
    Ok(value)
}
