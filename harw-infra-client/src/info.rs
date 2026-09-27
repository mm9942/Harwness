//! The common daemon surface: `/v1/health`, `/v1/version`,
//! `/v1/capabilities` (masterplan §38).
//!
//! # Wire contract (JSON, `application/json`)
//!
//! ```text
//! GET /v1/health        200 {"status": "ok" | "degraded", "service"?: "<name>"}
//!                       (an empty 2xx body also counts as "ok")
//!                       503 → InfraClientError::Unavailable
//! GET /v1/version       200 {"service": "<name>", "version": "<semver>", "protocol": <u32>}
//! GET /v1/capabilities  200 {"service": "<name>", "protocol": <u32>,
//!                            "crypto_profiles"?: ["…"], "operations"?: ["…"]}
//! ```
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
