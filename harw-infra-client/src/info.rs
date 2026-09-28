//! The common daemon surface: `/v1/health`, `/v1/version`,
//! `/v1/capabilities` (masterplan §38).
//!
//! # Wire contract (JSON, `application/json`)
//!
//! ```text
//! GET /v1/health        200 {"status": "ok" | "degraded", "service"?: "<name>"}
//!                       (an empty 2xx body also counts as "ok")
//!                       503 → InfraClientError::Unavailable
//! GET /v1/version       200 {"service": "<name>", "version": "<semver>", "protocol": <u32>,
//!                            "store_epoch"?: "<hex>", "boot_id"?: "<hex>"}
//! GET /v1/capabilities  200 {"service": "<name>", "protocol": <u32>,
//!                            "crypto_profiles"?: ["…"], "operations"?: ["…"],
//!                            "persistence"?: "<mode>",
//!                            "store_epoch"?: "<hex>", "boot_id"?: "<hex>"}
//! ```
//!
//! `store_epoch` names the daemon's key store (it changes when the store is
//! replaced, e.g. an in-memory AuthHub restarts); `boot_id` changes with
//! every daemon process. Both are optional: daemons that predate them omit
//! them and they parse as `None`.
//!
//! Unknown JSON fields are ignored (the documents are descriptive and may
//! grow). Capabilities are descriptive, **not** authority: nothing in this
//! crate enables or downgrades behaviour based on them.

use http::Method;
use serde::Deserialize;

use crate::error::InfraClientError;
use crate::transport::{RequestBody, UdsTransport};

/// Media type of the info documents.
pub(crate) const JSON: &str = "application/json";

/// Health state reported by a daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HealthState {
    /// Fully operational.
    Ok,
    /// Serving, with reduced function.
    Degraded,
    /// A state string this client does not know.
    Unknown,
}

/// `GET /v1/health`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    /// Reported state.
    pub state: HealthState,
    /// Service name, if reported.
    pub service: Option<String>,
}

#[derive(Deserialize)]
struct HealthWire {
    status: String,
    #[serde(default)]
    service: Option<String>,
}

/// `GET /v1/version`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct VersionInfo {
    /// Service name (`harw-auth-hub`, …).
    pub service: String,
    /// Daemon version.
    pub version: String,
    /// Wire protocol generation.
    pub protocol: u32,
    /// Identity of the daemon's key store; changes when the store is
    /// replaced. `None` from daemons that do not report it.
    #[serde(default)]
    pub store_epoch: Option<String>,
    /// Identity of the daemon process; changes on every restart. `None`
    /// from daemons that do not report it.
    #[serde(default)]
    pub boot_id: Option<String>,
}

/// `GET /v1/capabilities`. Descriptive only.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Capabilities {
    /// Service name.
    pub service: String,
    /// Wire protocol generation.
    pub protocol: u32,
    /// Crypto profiles offered (e.g. `harw-strong-v1`).
    #[serde(default)]
    pub crypto_profiles: Vec<String>,
    /// Operation names offered (e.g. `key.wrap`).
    #[serde(default)]
    pub operations: Vec<String>,
    /// Key store persistence (`in-memory`, `sealed-file`), if reported.
    #[serde(default)]
    pub persistence: Option<String>,
    /// See [`VersionInfo::store_epoch`].
    #[serde(default)]
    pub store_epoch: Option<String>,
    /// See [`VersionInfo::boot_id`].
    #[serde(default)]
    pub boot_id: Option<String>,
}

impl Capabilities {
    /// Whether `operation` is listed.
    pub fn lists_operation(&self, operation: &str) -> bool {
        self.operations.iter().any(|listed| listed == operation)
    }
}

pub(crate) async fn health(transport: &UdsTransport) -> Result<Health, InfraClientError> {
    let response = transport
        .call(Method::GET, "/v1/health", RequestBody::Empty, None)
        .await?;
    if response.body.is_empty() {
        return Ok(Health {
            state: HealthState::Ok,
            service: None,
        });
    }
    response.expect_content_type(JSON)?;
    let wire: HealthWire = serde_json::from_slice(&response.body)
        .map_err(|_| InfraClientError::Protocol("malformed health document"))?;
    let state = match wire.status.as_str() {
        "ok" => HealthState::Ok,
        "degraded" => HealthState::Degraded,
        _ => HealthState::Unknown,
    };
    Ok(Health {
        state,
        service: wire.service,
    })
}

pub(crate) async fn version(transport: &UdsTransport) -> Result<VersionInfo, InfraClientError> {
    let response = transport
        .call(Method::GET, "/v1/version", RequestBody::Empty, None)
        .await?;
    response.expect_content_type(JSON)?;
    serde_json::from_slice(&response.body)
        .map_err(|_| InfraClientError::Protocol("malformed version document"))
}

pub(crate) async fn capabilities(
    transport: &UdsTransport,
) -> Result<Capabilities, InfraClientError> {
    let response = transport
        .call(Method::GET, "/v1/capabilities", RequestBody::Empty, None)
        .await?;
    response.expect_content_type(JSON)?;
    serde_json::from_slice(&response.body)
        .map_err(|_| InfraClientError::Protocol("malformed capabilities document"))
}

#[cfg(test)]
mod tests {
    use super::{Capabilities, VersionInfo};
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_version_and_capabilities_parse_store_epoch_and_boot_id() -> TestResult {
        let version: VersionInfo = serde_json::from_str(
            r#"{"service":"harw-auth-hub","version":"0.3.0","protocol":1,"cryptguard":"3.1.0",
                "store_epoch":"00112233445566778899aabbccddeeff",
                "boot_id":"ffeeddccbbaa99887766554433221100"}"#,
        )
        .map_err(ctx("version json"))?;
        assert_eq!(
            version.store_epoch.as_deref(),
            Some("00112233445566778899aabbccddeeff")
        );
        assert_eq!(
            version.boot_id.as_deref(),
            Some("ffeeddccbbaa99887766554433221100")
        );

        let caps: Capabilities = serde_json::from_str(
            r#"{"service":"harw-auth-hub","protocol":1,"crypto_profiles":[],"operations":[],
                "persistence":"in-memory","store_epoch":"00112233445566778899aabbccddeeff",
                "boot_id":"ffeeddccbbaa99887766554433221100"}"#,
        )
        .map_err(ctx("capabilities json"))?;
        assert_eq!(caps.persistence.as_deref(), Some("in-memory"));
        assert_eq!(
            caps.store_epoch.as_deref(),
            Some("00112233445566778899aabbccddeeff")
        );
        assert_eq!(
            caps.boot_id.as_deref(),
            Some("ffeeddccbbaa99887766554433221100")
        );
        Ok(())
    }

    #[test]
    fn test_documents_without_epoch_fields_still_parse() -> TestResult {
        let version: VersionInfo =
            serde_json::from_str(r#"{"service":"old-hub","version":"0.1.0","protocol":1}"#)
                .map_err(ctx("version json"))?;
        assert_eq!(version.store_epoch, None);
        assert_eq!(version.boot_id, None);

        let caps: Capabilities = serde_json::from_str(r#"{"service":"old-hub","protocol":1}"#)
            .map_err(ctx("capabilities json"))?;
        assert_eq!(caps.persistence, None);
        assert_eq!(caps.store_epoch, None);
        assert_eq!(caps.boot_id, None);
        Ok(())
    }
}
