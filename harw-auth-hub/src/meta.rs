//! Infrastructure health/capabilities contract (Masterplan v2 §38).
//!
//! ```text
//! GET /v1/health        200 application/json {"status": "ok", "service": "harw-auth-hub"}
//! GET /v1/version       200 application/json {service, version, protocol, cryptguard,
//!                                             store_epoch, boot_id}
//! GET /v1/capabilities  200 application/json {service, protocol, crypto_profiles, operations,
//!                                             algorithms, persistence, transport, authentication,
//!                                             store_epoch, boot_id}
//! ```
//!
//! This is the contract `harw-infra-client` (`src/info.rs`) parses: the
//! `service`/`version`/`protocol` and `service`/`protocol`/`crypto_profiles`/
//! `operations` fields are required by it; every other field is extra and
//! ignored by the client. `crypto_profiles` are the CryptGuard wire names the
//! client's `KeyProfile::wire_name` uses. `algorithms` is kept as an alias of
//! `crypto_profiles` for existing readers.
//!
//! # Store epoch and boot id
//!
//! `store_epoch` is a random 128-bit value (32 lowercase hex digits) that
//! names one key store: it is generated when a store is **created** and a
//! persistent store keeps it across restarts ([`StoreIdentity`]). An
//! in-memory store is created anew by every process, so its epoch changes on
//! every hub restart. `boot_id` is the same kind of value, fresh for every
//! hub process ([`boot_id`]). A client that sees `store_epoch` change knows
//! the keys it used earlier are gone (store replaced), which it can tell
//! apart from a genuine authentication failure; a changed `boot_id` with an
//! unchanged epoch is a plain restart of a persistent store. Both are
//! descriptive, carry no secret, and are additive fields (older clients
//! ignore them). `persistence` in `/v1/capabilities` is the store's
//! [`StorePersistence::wire_name`].
//!
//! These routes are answered before any request reaches CryptGuard and do
//! not require an authenticated principal: anyone allowed to `connect(2)` to
//! the socket (socket mode `0660`) may read them. They are descriptive, not
//! authority, and are never used to negotiate crypto downward.

use core::fmt;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Persistence mode of the store the hub currently builds
/// (`InMemoryProvider`), i.e. [`StorePersistence::InMemory`]'s wire name.
/// `/v1/capabilities` reports the persistence of the [`StoreIdentity`] it is
/// given.
pub const PERSISTENCE: &str = StorePersistence::InMemory.wire_name();

/// Length of a `store_epoch` / `boot_id` in hex digits (128 bits).
pub const EPOCH_HEX_LEN: usize = 32;

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

/// How a key store keeps its keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StorePersistence {
    /// Process memory only: lost on exit, a new store (and epoch) per
    /// process.
    InMemory,
    /// A sealed store file: survives restarts and keeps its epoch.
    SealedFile,
}

impl StorePersistence {
    /// The wire name reported as `persistence` (`in-memory` / `sealed-file`).
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::InMemory => "in-memory",
            Self::SealedFile => "sealed-file",
        }
    }
}

/// Identity of one key store: its epoch and how it persists.
///
/// The hub's service layer passes the identity of the store it serves to
/// [`respond_with`]. An in-memory store uses [`StoreIdentity::in_memory`]
/// (a fresh epoch); a persistent store generates its epoch once at creation
/// ([`generate_store_epoch`]), persists it with the store and restores it
/// with [`StoreIdentity::sealed_file`] on every load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreIdentity {
    epoch: String,
    persistence: StorePersistence,
}

impl StoreIdentity {
    /// A new in-memory store: fresh random epoch.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            epoch: generate_store_epoch(),
            persistence: StorePersistence::InMemory,
        }
    }

    /// A newly created sealed-file store: fresh random epoch, which the
    /// caller must persist with the store.
    #[must_use]
    pub fn new_sealed_file() -> Self {
        Self {
            epoch: generate_store_epoch(),
            persistence: StorePersistence::SealedFile,
        }
    }

    /// An existing sealed-file store with its persisted `epoch`.
    ///
    /// # Errors
    /// [`InvalidStoreEpoch`] unless `epoch` is exactly
    /// [`EPOCH_HEX_LEN`] lowercase hex digits.
    pub fn sealed_file(epoch: impl Into<String>) -> Result<Self, InvalidStoreEpoch> {
        let epoch = epoch.into();
        if !is_epoch(&epoch) {
            return Err(InvalidStoreEpoch);
        }
        Ok(Self {
            epoch,
            persistence: StorePersistence::SealedFile,
        })
    }

    /// The store epoch (32 lowercase hex digits).
    #[must_use]
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// How the store persists.
    #[must_use]
    pub fn persistence(&self) -> StorePersistence {
        self.persistence
    }
}

impl Default for StoreIdentity {
    /// Same as [`StoreIdentity::in_memory`]: a fresh epoch.
    fn default() -> Self {
        Self::in_memory()
    }
}

/// A persisted store epoch was not 32 lowercase hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidStoreEpoch;

impl fmt::Display for InvalidStoreEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("store epoch must be 32 lowercase hex digits")
    }
}

impl std::error::Error for InvalidStoreEpoch {}

/// A fresh random store epoch (128 bits, 32 lowercase hex digits).
#[must_use]
pub fn generate_store_epoch() -> String {
    random_hex128()
}

/// This process's boot id (128 random bits as hex), generated on first use
/// and constant for the life of the process.
#[must_use]
pub fn boot_id() -> &'static str {
    static BOOT_ID: OnceLock<String> = OnceLock::new();
    BOOT_ID.get_or_init(random_hex128)
}

/// The in-memory store identity [`respond`] reports: generated on first use,
/// one per process.
///
/// Correct while the process builds exactly one in-memory store; a service
/// layer that builds several stores per process (tests) or a persistent one
/// passes each store's own [`StoreIdentity`] to [`respond_with`] instead.
#[must_use]
pub fn process_store_identity() -> &'static StoreIdentity {
    static STORE: OnceLock<StoreIdentity> = OnceLock::new();
    STORE.get_or_init(StoreIdentity::in_memory)
}

fn is_epoch(value: &str) -> bool {
    value.len() == EPOCH_HEX_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 128 random bits as 32 lowercase hex digits. Uniqueness, not secrecy, is
/// what the ids need: the OS RNG (`/dev/urandom`) first; if that cannot be
/// read, `RandomState`-keyed SipHash (OS-seeded keys) over the clock, the
/// pid and a process counter, as elsewhere in the workspace, so id
/// generation never fails.
fn random_hex128() -> String {
    let mut bytes = [0_u8; 16];
    let from_os =
        std::fs::File::open("/dev/urandom").and_then(|mut rng| rng.read_exact(&mut bytes));
    if from_os.is_err() {
        bytes = fallback_entropy();
    }
    let mut out = String::with_capacity(EPOCH_HEX_LEN);
    for byte in bytes {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

fn fallback_entropy() -> [u8; 16] {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let mut out = [0_u8; 16];
    for (lane, chunk) in out.chunks_exact_mut(8).enumerate() {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_usize(lane);
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    out
}

fn hex_digit(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16).unwrap_or('0')
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
    store_epoch: String,
    boot_id: String,
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
    store_epoch: String,
    boot_id: String,
}

/// Answer `method path` if it is a meta route, otherwise `None` (the request
/// goes on to the CryptGuard adapter). Reports the process-wide in-memory
/// store identity ([`process_store_identity`]); see [`respond_with`].
#[must_use]
pub fn respond(info: &MetaInfo, method: &Method, path: &str) -> Option<Response<Full<Bytes>>> {
    respond_with(info, process_store_identity(), method, path)
}

/// As [`respond`], reporting `store` as the served key store's identity.
#[must_use]
pub fn respond_with(
    info: &MetaInfo,
    store: &StoreIdentity,
    method: &Method,
    path: &str,
) -> Option<Response<Full<Bytes>>> {
    let body = match path {
        HEALTH => json(&health()),
        VERSION => json(&version(store)),
        CAPABILITIES => json(&capabilities(info, store)),
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

fn version(store: &StoreIdentity) -> VersionBody {
    VersionBody {
        service: SERVICE_NAME,
        version: env!("CARGO_PKG_VERSION"),
        protocol: PROTOCOL_VERSION,
        cryptguard: CRYPTGUARD_VERSION,
        store_epoch: store.epoch().to_owned(),
        boot_id: boot_id().to_owned(),
    }
}

fn capabilities(info: &MetaInfo, store: &StoreIdentity) -> CapabilitiesBody {
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
        persistence: store.persistence().wire_name(),
        transport: "unix-stream+http1",
        authentication,
        store_epoch: store.epoch().to_owned(),
        boot_id: boot_id().to_owned(),
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

    use http_body_util::BodyExt;

    use super::{
        CRYPTO_PROFILES, EPOCH_HEX_LEN, InvalidStoreEpoch, MetaInfo, PROTOCOL_VERSION,
        SERVICE_NAME, StoreIdentity, StorePersistence, boot_id, capabilities, fallback_entropy,
        generate_store_epoch, health, is_epoch, process_store_identity, respond, respond_with,
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

        let store = StoreIdentity::in_memory();
        let version = serde_json::to_value(version(&store)).map_err(ctx("version json"))?;
        assert_eq!(version["service"], SERVICE_NAME);
        assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(version["protocol"], PROTOCOL_VERSION);

        let info = MetaInfo {
            bearer_enabled: false,
        };
        let caps =
            serde_json::to_value(capabilities(&info, &store)).map_err(ctx("capabilities json"))?;
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

    /// `store_epoch` and `boot_id` are additive fields of both documents.
    #[test]
    fn version_and_capabilities_carry_store_epoch_and_boot_id() -> TestResult {
        let info = MetaInfo {
            bearer_enabled: false,
        };
        let store = StoreIdentity::in_memory();
        let version = serde_json::to_value(version(&store)).map_err(ctx("version json"))?;
        let caps =
            serde_json::to_value(capabilities(&info, &store)).map_err(ctx("capabilities json"))?;
        for doc in [&version, &caps] {
            assert_eq!(doc["store_epoch"], store.epoch());
            assert_eq!(doc["boot_id"], boot_id());
        }
        assert_eq!(caps["persistence"], "in-memory");

        let sealed = StoreIdentity::sealed_file("0123456789abcdef0123456789abcdef")
            .map_err(|err| TestError::Unexpected(err.to_string()))?;
        let caps =
            serde_json::to_value(capabilities(&info, &sealed)).map_err(ctx("capabilities json"))?;
        assert_eq!(caps["persistence"], "sealed-file");
        assert_eq!(caps["store_epoch"], "0123456789abcdef0123456789abcdef");
        Ok(())
    }

    #[test]
    fn store_epochs_are_random_128_bit_hex() {
        let first = generate_store_epoch();
        let second = generate_store_epoch();
        assert!(is_epoch(&first), "{first}");
        assert!(is_epoch(&second), "{second}");
        assert_eq!(first.len(), EPOCH_HEX_LEN);
        assert_ne!(first, second);
        assert!(is_epoch(boot_id()));
        assert_ne!(
            fallback_entropy(),
            fallback_entropy(),
            "fallback must not repeat"
        );

        // Each in-memory store is a new store; persistence is recorded.
        let a = StoreIdentity::in_memory();
        let b = StoreIdentity::default();
        assert_ne!(a.epoch(), b.epoch());
        assert_eq!(a.persistence(), StorePersistence::InMemory);
        let created = StoreIdentity::new_sealed_file();
        assert!(is_epoch(created.epoch()));
        assert_eq!(created.persistence(), StorePersistence::SealedFile);

        // The process defaults are stable for the life of the process.
        assert_eq!(boot_id(), boot_id());
        assert_eq!(process_store_identity(), process_store_identity());
    }

    #[test]
    fn persisted_epoch_is_kept_and_validated() {
        let kept = StoreIdentity::sealed_file("ffffffffffffffffffffffffffffffff");
        assert_eq!(
            kept.map(|store| store.epoch().to_owned()),
            Ok("ffffffffffffffffffffffffffffffff".to_owned())
        );
        for bad in [
            "",
            "abc",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef0123456789abcdef0",
        ] {
            assert_eq!(
                StoreIdentity::sealed_file(bad),
                Err(InvalidStoreEpoch),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn respond_with_reports_the_given_store() -> TestResult {
        let info = MetaInfo {
            bearer_enabled: false,
        };
        let store = StoreIdentity::in_memory();
        for path in ["/v1/version", "/v1/capabilities"] {
            let response = respond_with(&info, &store, &Method::GET, path)
                .ok_or(TestError::Missing("response"))?;
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(ctx("collect body"))?
                .to_bytes();
            let doc: serde_json::Value = serde_json::from_slice(&body).map_err(ctx("meta json"))?;
            assert_eq!(doc["store_epoch"], store.epoch(), "{path}");
            assert_eq!(doc["boot_id"], boot_id(), "{path}");
        }
        // `respond` reports the process-wide in-memory store.
        let response =
            respond(&info, &Method::GET, "/v1/version").ok_or(TestError::Missing("response"))?;
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(ctx("collect body"))?
            .to_bytes();
        let doc: serde_json::Value = serde_json::from_slice(&body).map_err(ctx("version json"))?;
        assert_eq!(doc["store_epoch"], process_store_identity().epoch());
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
