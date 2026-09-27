//! Infrastructure health/capabilities contract (Masterplan v2 §38).
//!
//! ```text
//! GET /v1/health        200 application/json {"status": "ok", "service": "harw-auth-hub"}
//! GET /v1/version       200 application/json {service, version, protocol, cryptguard}
//! GET /v1/capabilities  200 application/json {service, protocol, crypto_profiles, operations,
//!                                             algorithms, persistence, transport, authentication}
//! ```
//!
//! This is the contract `harw-infra-client` (`src/info.rs`) parses: the
//! `service`/`version`/`protocol` and `service`/`protocol`/`crypto_profiles`/
//! `operations` fields are required by it; every other field is extra and
//! ignored by the client. `crypto_profiles` are the CryptGuard wire names the
//! client's `KeyProfile::wire_name` uses. `algorithms` is kept as an alias of
//! `crypto_profiles` for existing readers.
//!
//! These routes are answered before any request reaches CryptGuard and do
//! not require an authenticated principal: anyone allowed to `connect(2)` to
//! the socket (socket mode `0660`) may read them. They are descriptive, not
//! authority, and are never used to negotiate crypto downward.

use bytes::Bytes;
use crypt_guard_service::OpKind;
use http::{HeaderValue, Method, Response, StatusCode, header};
use http_body_util::Full;
use serde::Serialize;

/// The service name reported by `/v1/version` and `/v1/capabilities`.
pub const SERVICE_NAME: &str = "harw-auth-hub";

/// Protocol generation of the hub's HTTP surface. Must equal the
/// `protocol` value `harw-infra-client` documents for the §38 surface (`1`).
pub const PROTOCOL_VERSION: u32 = 1;

/// The CryptGuard release this crate is built against (git `main` of
/// `mm9942/crypt_guard`, workspace version 3.1.0). Cargo exposes no
/// dependency versions at compile time, so this is maintained by hand and
/// must be bumped together with the workspace dependency.
pub const CRYPTGUARD_VERSION: &str = "3.1.0";

/// Persistence mode reported by `/v1/capabilities`.
pub const PERSISTENCE: &str = "in-memory";

/// Health state reported by `/v1/health` (the hub has no degraded mode).
pub const HEALTH_STATUS: &str = "ok";

/// Wire names of the crypto profiles `InMemoryProvider` can generate
/// (`crypt_guard_hyper::codec::profile`), identical to
/// `harw_infra_client::KeyProfile::wire_name`; ML-DSA requires the
/// `crypt_guard_service/ml-dsa` feature, which the workspace enables.
pub const CRYPTO_PROFILES: [&str; 4] = ["pq-hpke-default", "ml-dsa-44", "ml-dsa-65", "ml-dsa-87"];

/// Every operation of the CryptGuard KMS surface, in route-table order.
pub const OPERATIONS: [OpKind; 14] = [
    OpKind::Generate,
    OpKind::Describe,
    OpKind::PublicKey,
    OpKind::Encrypt,
    OpKind::Decrypt,
    OpKind::Sign,
    OpKind::Verify,
    OpKind::Rotate,
    OpKind::Disable,
    OpKind::Enable,
    OpKind::Destroy,
    OpKind::WrapKey,
    OpKind::UnwrapKey,
    OpKind::RewrapKey,
];

/// Paths answered by this module.
const HEALTH: &str = "/v1/health";
const VERSION: &str = "/v1/version";
const CAPABILITIES: &str = "/v1/capabilities";

/// Static facts reported by the meta routes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaInfo {
    /// Whether the bearer-token fallback is configured.
    pub bearer_enabled: bool,
}

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
    service: &'static str,
}

#[derive(Serialize)]
struct VersionBody {
    service: &'static str,
    version: &'static str,
    protocol: u32,
    cryptguard: &'static str,
}

#[derive(Serialize)]
struct CapabilitiesBody {
    service: &'static str,
    protocol: u32,
    crypto_profiles: [&'static str; 4],
    operations: Vec<&'static str>,
    algorithms: [&'static str; 4],
    persistence: &'static str,
    transport: &'static str,
    authentication: Vec<&'static str>,
}

/// Answer `method path` if it is a meta route, otherwise `None` (the request
/// goes on to the CryptGuard adapter).
#[must_use]
pub fn respond(info: &MetaInfo, method: &Method, path: &str) -> Option<Response<Full<Bytes>>> {
    let body = match path {
        HEALTH => json(&health()),
        VERSION => json(&version()),
        CAPABILITIES => json(&capabilities(info)),
        _ => return None,
    };
    if method != Method::GET && method != Method::HEAD {
        return Some(status_only(StatusCode::METHOD_NOT_ALLOWED));
    }
    Some(match body {
        Some(bytes) => build(StatusCode::OK, "application/json", Bytes::from(bytes)),
        None => status_only(StatusCode::INTERNAL_SERVER_ERROR),
    })
}

fn health() -> HealthBody {
    HealthBody {
        status: HEALTH_STATUS,
        service: SERVICE_NAME,
    }
}

fn version() -> VersionBody {
    VersionBody {
        service: SERVICE_NAME,
        version: env!("CARGO_PKG_VERSION"),
        protocol: PROTOCOL_VERSION,
        cryptguard: CRYPTGUARD_VERSION,
    }
}

fn capabilities(info: &MetaInfo) -> CapabilitiesBody {
    let mut authentication = vec!["so-peercred"];
    if info.bearer_enabled {
        authentication.push("bearer");
    }
    CapabilitiesBody {
        service: SERVICE_NAME,
        protocol: PROTOCOL_VERSION,
        crypto_profiles: CRYPTO_PROFILES,
        operations: OPERATIONS.iter().map(|op| op.name()).collect(),
        algorithms: CRYPTO_PROFILES,
        persistence: PERSISTENCE,
        transport: "unix-stream+http1",
        authentication,
    }
}

fn json<T: Serialize>(value: &T) -> Option<Vec<u8>> {
    serde_json::to_vec(value).ok()
}

fn status_only(status: StatusCode) -> Response<Full<Bytes>> {
    let reason = status.canonical_reason().unwrap_or("error");
    build(
        status,
        "text/plain; charset=utf-8",
        Bytes::from_static(reason.as_bytes()),
    )
}

fn build(status: StatusCode, content_type: &'static str, body: Bytes) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use crypt_guard_hyper::codec::profile;
    use http::{Method, StatusCode};

    use super::{
        CRYPTO_PROFILES, MetaInfo, PROTOCOL_VERSION, SERVICE_NAME, capabilities, health, respond,
        version,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn crypto_profile_names_are_cryptguard_wire_names() {
        for name in CRYPTO_PROFILES {
            assert!(profile::parse(name).is_some(), "unknown profile {name}");
        }
    }

    /// The field set `harw-infra-client` (`src/info.rs`) deserializes.
    #[test]
    fn bodies_match_infra_client_contract() -> TestResult {
        let health = serde_json::to_value(health()).map_err(ctx("health json"))?;
        assert_eq!(health["status"], "ok");
        assert_eq!(health["service"], SERVICE_NAME);

        let version = serde_json::to_value(version()).map_err(ctx("version json"))?;
        assert_eq!(version["service"], SERVICE_NAME);
        assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(version["protocol"], PROTOCOL_VERSION);

        let info = MetaInfo {
            bearer_enabled: false,
        };
        let caps = serde_json::to_value(capabilities(&info)).map_err(ctx("capabilities json"))?;
        assert_eq!(caps["service"], SERVICE_NAME);
        assert_eq!(caps["protocol"], PROTOCOL_VERSION);
        let profiles = caps["crypto_profiles"]
            .as_array()
            .ok_or(TestError::Missing("crypto_profiles"))?;
        assert_eq!(profiles.len(), CRYPTO_PROFILES.len());
        for (listed, expected) in profiles.iter().zip(CRYPTO_PROFILES) {
            assert_eq!(listed, expected);
        }
        assert!(caps["operations"].is_array(), "operations must be an array");
        Ok(())
    }

    #[test]
    fn meta_routes_are_application_json() -> TestResult {
        let info = MetaInfo {
            bearer_enabled: true,
        };
        for path in ["/v1/health", "/v1/version", "/v1/capabilities"] {
            let response =
                respond(&info, &Method::GET, path).ok_or(TestError::Missing("response"))?;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let content_type = response
                .headers()
                .get(http::header::CONTENT_TYPE)
                .ok_or(TestError::Missing("content-type"))?;
            assert_eq!(content_type, "application/json", "{path}");
        }
        Ok(())
    }

    #[test]
    fn non_meta_paths_fall_through() {
        let info = MetaInfo {
            bearer_enabled: false,
        };
        assert!(respond(&info, &Method::POST, "/v1/keys").is_none());
        assert!(respond(&info, &Method::GET, "/v1/healthz").is_none());
    }

    #[test]
    fn wrong_method_is_405() -> TestResult {
        let info = MetaInfo {
            bearer_enabled: false,
        };
        let response =
            respond(&info, &Method::POST, "/v1/health").ok_or(TestError::Missing("response"))?;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        Ok(())
    }
}
