//! Wire contract of `network.sock`: endpoints, request and response bodies.
//!
//! # Endpoints (protocol 1)
//! | Method | Path                                   | Result                      |
//! |--------|----------------------------------------|-----------------------------|
//! | GET    | `/v1/health`                           | `{"status":"ok"}`           |
//! | GET    | `/v1/version`                          | service, version, protocol  |
//! | GET    | `/v1/capabilities`                     | service, protocol, ops      |
//! | GET    | `/v1/nodes`                            | `{"nodes":[NodeRecord…]}`   |
//! | POST   | `/v1/nodes`                            | 201 + pending `NodeRecord`  |
//! | GET    | `/v1/nodes/{id}`                       | `NodeRecord`                |
//! | POST   | `/v1/nodes/{id}:activate`              | `NodeRecord`                |
//! | POST   | `/v1/nodes/{id}:drain`                 | `NodeRecord`                |
//! | POST   | `/v1/nodes/{id}:complete-drain`        | `NodeRecord`                |
//! | POST   | `/v1/nodes/{id}:undrain`               | `NodeRecord`                |
//! | POST   | `/v1/nodes/{id}:revoke`                | `NodeRecord`                |
//! | GET    | `/v1/zones`                            | `{"zones":[Zone…]}`         |
//! | GET    | `/v1/routes`                           | `{"routes":[Route…]}`       |
//!
//! Errors are `{"error":{"code":"…","message":"…"}}`. Capabilities are
//! descriptive, never authority (masterplan §38).

use hyper::Method;
use serde::{Deserialize, Serialize};

use crate::model::{NodeRecord, Route, Zone};
use crate::state_machine::NodeEvent;

/// Service name reported by `/v1/version` and `/v1/capabilities`.
pub const SERVICE_NAME: &str = "harw-netsec";
/// Wire protocol version.
pub const PROTOCOL_VERSION: u32 = 1;
/// Operations listed by `/v1/capabilities`.
pub const OPERATIONS: &[&str] = &[
    "node.list",
    "node.get",
    "node.register",
    "node.activate",
    "node.drain",
    "node.complete_drain",
    "node.undrain",
    "node.revoke",
    "zone.list",
    "route.list",
];

const NODES_PREFIX: &str = "/v1/nodes/";

/// A resolved endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// `GET /v1/health`.
    Health,
    /// `GET /v1/version`.
    Version,
    /// `GET /v1/capabilities`.
    Capabilities,
    /// `GET /v1/nodes`.
    ListNodes,
    /// `POST /v1/nodes`.
    RegisterNode,
    /// `GET /v1/nodes/{id}` (raw, not yet validated id).
    GetNode(String),
    /// `POST /v1/nodes/{id}:{action}`.
    NodeAction(String, NodeEvent),
    /// `GET /v1/zones`.
    ListZones,
    /// `GET /v1/routes`.
    ListRoutes,
}

/// Why a request did not resolve to an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// No such path.
    NotFound,
    /// Path exists, method does not.
    MethodNotAllowed,
}

/// Maps method and path onto an [`Endpoint`].
///
/// # Errors
/// [`ResolveError::NotFound`] or [`ResolveError::MethodNotAllowed`].
pub fn resolve(method: &Method, path: &str) -> Result<Endpoint, ResolveError> {
    let only = |allowed: Method, endpoint: Endpoint| {
        if *method == allowed {
            Ok(endpoint)
        } else {
            Err(ResolveError::MethodNotAllowed)
        }
    };
    match path {
        "/v1/health" => return only(Method::GET, Endpoint::Health),
        "/v1/version" => return only(Method::GET, Endpoint::Version),
        "/v1/capabilities" => return only(Method::GET, Endpoint::Capabilities),
        "/v1/zones" => return only(Method::GET, Endpoint::ListZones),
        "/v1/routes" => return only(Method::GET, Endpoint::ListRoutes),
        "/v1/nodes" => {
            return if *method == Method::GET {
                Ok(Endpoint::ListNodes)
            } else if *method == Method::POST {
                Ok(Endpoint::RegisterNode)
            } else {
                Err(ResolveError::MethodNotAllowed)
            };
        }
        _ => {}
    }
    let Some(rest) = path.strip_prefix(NODES_PREFIX) else {
        return Err(ResolveError::NotFound);
    };
    if rest.is_empty() || rest.contains('/') {
        return Err(ResolveError::NotFound);
    }
    match rest.split_once(':') {
        None => only(Method::GET, Endpoint::GetNode(rest.to_owned())),
        Some((id, action)) => {
            let Some(event) = NodeEvent::from_action(action) else {
                return Err(ResolveError::NotFound);
            };
            if id.is_empty() {
                return Err(ResolveError::NotFound);
            }
            only(Method::POST, Endpoint::NodeAction(id.to_owned(), event))
        }
    }
}

/// Optional body of `POST /v1/nodes/{id}:{action}`. An empty body is the
/// same as `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeActionRequest {
    /// Operator-supplied reason, recorded in `last_transition`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// `GET /v1/health` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Always `"ok"` when the daemon answers.
    pub status: String,
}

/// `GET /v1/version` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionResponse {
    /// [`SERVICE_NAME`].
    pub service: String,
    /// Crate version.
    pub version: String,
    /// [`PROTOCOL_VERSION`].
    pub protocol: u32,
}

/// `GET /v1/capabilities` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    /// [`SERVICE_NAME`].
    pub service: String,
    /// [`PROTOCOL_VERSION`].
    pub protocol: u32,
    /// [`OPERATIONS`].
    pub operations: Vec<String>,
}

impl CapabilitiesResponse {
    /// The capabilities of this build.
    #[must_use]
    pub fn current() -> Self {
        Self {
            service: SERVICE_NAME.to_owned(),
            protocol: PROTOCOL_VERSION,
            operations: OPERATIONS.iter().map(|op| (*op).to_owned()).collect(),
        }
    }
}

/// `GET /v1/nodes` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeList {
    /// Nodes ordered by id.
    pub nodes: Vec<NodeRecord>,
}

/// `GET /v1/zones` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneList {
    /// Zones ordered by id.
    pub zones: Vec<Zone>,
}

/// `GET /v1/routes` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteList {
    /// Routes ordered by id.
    pub routes: Vec<Route>,
}

/// Error body: `{"error":{"code":…,"message":…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The error.
    pub error: ErrorDetail,
}

/// Machine-readable code plus human-readable message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// Stable, machine-readable code (e.g. `invalid_transition`).
    pub code: String,
    /// Human-readable description; never contains filesystem paths.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_paths_resolve() {
        assert_eq!(resolve(&Method::GET, "/v1/health"), Ok(Endpoint::Health));
        assert_eq!(resolve(&Method::GET, "/v1/version"), Ok(Endpoint::Version));
        assert_eq!(
            resolve(&Method::GET, "/v1/capabilities"),
            Ok(Endpoint::Capabilities)
        );
        assert_eq!(resolve(&Method::GET, "/v1/nodes"), Ok(Endpoint::ListNodes));
        assert_eq!(
            resolve(&Method::POST, "/v1/nodes"),
            Ok(Endpoint::RegisterNode)
        );
        assert_eq!(resolve(&Method::GET, "/v1/zones"), Ok(Endpoint::ListZones));
        assert_eq!(
            resolve(&Method::GET, "/v1/routes"),
            Ok(Endpoint::ListRoutes)
        );
    }

    #[test]
    fn test_node_paths_resolve() {
        assert_eq!(
            resolve(&Method::GET, "/v1/nodes/n-1"),
            Ok(Endpoint::GetNode("n-1".to_owned()))
        );
        assert_eq!(
            resolve(&Method::POST, "/v1/nodes/n-1:drain"),
            Ok(Endpoint::NodeAction("n-1".to_owned(), NodeEvent::Drain))
        );
        assert_eq!(
            resolve(&Method::POST, "/v1/nodes/n-1:complete-drain"),
            Ok(Endpoint::NodeAction(
                "n-1".to_owned(),
                NodeEvent::CompleteDrain
            ))
        );
        assert_eq!(
            resolve(&Method::POST, "/v1/nodes/n-1:revoke"),
            Ok(Endpoint::NodeAction("n-1".to_owned(), NodeEvent::Revoke))
        );
    }

    #[test]
    fn test_wrong_methods_are_405() {
        assert_eq!(
            resolve(&Method::POST, "/v1/health"),
            Err(ResolveError::MethodNotAllowed)
        );
        assert_eq!(
            resolve(&Method::DELETE, "/v1/nodes"),
            Err(ResolveError::MethodNotAllowed)
        );
        assert_eq!(
            resolve(&Method::GET, "/v1/nodes/n-1:drain"),
            Err(ResolveError::MethodNotAllowed)
        );
        assert_eq!(
            resolve(&Method::POST, "/v1/nodes/n-1"),
            Err(ResolveError::MethodNotAllowed)
        );
    }

    #[test]
    fn test_unknown_paths_are_404() {
        for path in [
            "/",
            "/v1",
            "/v2/health",
            "/v1/nodes/",
            "/v1/nodes/a/b",
            "/v1/nodes/a:explode",
            "/v1/nodes/:drain",
            "/v1/health/",
        ] {
            assert_eq!(
                resolve(&Method::POST, path),
                Err(ResolveError::NotFound),
                "{path}"
            );
        }
    }

    #[test]
    fn test_action_body_rejects_unknown_fields() {
        assert!(serde_json::from_str::<NodeActionRequest>(r#"{"reason":"x"}"#).is_ok());
        assert!(serde_json::from_str::<NodeActionRequest>(r#"{"force":true}"#).is_err());
    }
}
