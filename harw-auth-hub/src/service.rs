//! The hub's HTTP service: meta routes in front of the CryptGuard adapter.
//!
//! ```text
//! AF_UNIX accept ── SO_PEERCRED ──┐
//!                                 ▼
//! ConnectionService (per connection, carries PeerCred)
//!   ├─ GET /v1/health | /v1/version | /v1/capabilities
//!   │     → meta::respond_with(meta, store identity of this hub)
//!   └─ everything else, with PeerCred as request extension →
//!        TowerToHyperService<CryptoHttpService<CryptoWorkerHandle, PeerCredAuthenticator>>
//!          │   (bounded bodies; sign/verify limit = SIGN_BODY_LIMIT)
//!          └─ CryptoWorkerHandle ── bounded sync_channel ──▶ thread harw-kms-crypto-0
//!               └─ AuditProvider
//!                    └─ PolicyProvider<_, HarwUsageAuthorizer<NamespacePolicy>>
//!                         └─ KmsGuardProvider
//!                              └─ InMemoryProvider | SealedProvider   (KeyStore)
//! ```
//!
//! Only the [`CryptoWorkerHandle`] (a channel sender and an `Arc`) is cloned
//! per connection; all key state is owned by the single crypto worker thread,
//! which executes requests strictly one at a time (as [`KmsGuardProvider`]
//! requires). No crypto runs on a Tokio executor thread.
//!
//! The key store is chosen by the caller ([`KeyStore`]); the concrete
//! provider type is erased by the worker handle, so [`HubService`] has one
//! type for both stores. Each hub reports its own [`StoreIdentity`] on the
//! meta routes: a fresh epoch for an in-memory store, the persisted epoch
//! for a sealed store.
//!
//! [`KmsGuardProvider`]: crate::guard::KmsGuardProvider

use core::convert::Infallible;
use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use crypt_guard_hyper::{
    BearerTokens, BodyLimits, CryptoHttpService, HttpConfig, TowerToHyperService, into_hyper,
};
use crypt_guard_service::{CryptoProvider, InMemoryProvider, NamespacePolicy, PolicyProvider};
use harw_dod_encrypt::cg::HarwUsageAuthorizer;
use harw_dod_encrypt::{CGK1_VERIFY_OVERHEAD, MAX_SIGNABLE_TRANSCRIPT_LEN};
use http::{Request, Response};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::Service as HyperService;

use crate::audit::{self, AuditProvider};
use crate::auth::{PeerCred, PeerCredAuthenticator};
use crate::config::HubConfig;
use crate::crypto_worker::CryptoWorkerHandle;
use crate::error::HubError;
use crate::guard::KmsGuardProvider;
use crate::meta::{self, MetaInfo, StoreIdentity};
use crate::sealed::{SealedProvider, SealedStoreError};

/// The provider stack the crypto worker runs over key store `P`:
/// audit → policy (namespace grants + Harw key usage) → version guard → keys.
pub type HubProvider<P> =
    AuditProvider<PolicyProvider<KmsGuardProvider<P>, HarwUsageAuthorizer<NamespacePolicy>>>;

/// The CryptGuard HTTP adapter as configured by the hub.
pub type HubCryptoHttp = CryptoHttpService<CryptoWorkerHandle, PeerCredAuthenticator>;

/// Response type of every hub route.
pub type HubResponse = Response<Full<Bytes>>;

/// Boxed response future of [`ConnectionService`].
pub type HubFuture = Pin<Box<dyn Future<Output = Result<HubResponse, Infallible>> + Send>>;

/// Body limit of sign and verify requests (they share one limit in
/// `crypt_guard_hyper`).
///
/// A verify request is the larger of the two: `CGK1` framing plus the
/// transcript plus the largest signature, i.e. exactly
/// [`CGK1_VERIFY_OVERHEAD`] on top of the transcript. A sign request of a
/// [`MAX_SIGNABLE_TRANSCRIPT_LEN`]-byte transcript (9 bytes of framing)
/// therefore always fits, and a transcript the KMS signed can always be
/// verified over the same adapter. Not larger than that: the sign route
/// buffers its body before decoding.
pub const SIGN_BODY_LIMIT: usize = MAX_SIGNABLE_TRANSCRIPT_LEN + CGK1_VERIFY_OVERHEAD;

// A sign request: magic (4) + version (1) + `u32` length + message.
const _: () = assert!(SIGN_BODY_LIMIT >= MAX_SIGNABLE_TRANSCRIPT_LEN + 4 + 1 + 4);

/// The key store the hub serves.
#[derive(Debug)]
pub enum KeyStore {
    /// CryptGuard's `InMemoryProvider`: **all keys are lost when the process
    /// exits.** Reported as `persistence = "in-memory"` with a fresh epoch.
    InMemory,
    /// A sealed store file ([`SealedProvider`]), opened by the caller.
    /// Reported as `persistence = "sealed-file"` with the store's persisted
    /// epoch.
    Sealed(SealedProvider),
}

/// Shared, cloneable hub service (one per process).
#[derive(Clone, Debug)]
pub struct HubService {
    crypto: TowerToHyperService<HubCryptoHttp>,
    meta: Arc<MetaInfo>,
    store: Arc<StoreIdentity>,
}

impl HubService {
    /// Build the full stack over `store` from a validated config and the
    /// (optional) bearer-token table, and start the crypto worker thread
    /// (bounded by `config.worker`).
    ///
    /// Does not need a Tokio runtime to build; the returned service must be
    /// driven inside one (per-request timeouts use `tokio::time`).
    ///
    /// # Errors
    ///
    /// - [`HubError::KeyStore`] (`Corrupt`) if a sealed store reports an
    ///   epoch that is not 32 lowercase hex digits; the store is dropped.
    /// - [`HubError::Worker`] if the OS refuses to create the worker thread;
    ///   the provider (and its key material) is dropped.
    pub fn build(
        config: &HubConfig,
        bearer: Option<BearerTokens>,
        store: KeyStore,
    ) -> Result<Self, HubError> {
        let (handle, identity) = match store {
            KeyStore::InMemory => (
                spawn_stack(config, InMemoryProvider::new())?,
                StoreIdentity::in_memory(),
            ),
            KeyStore::Sealed(provider) => {
                let identity =
                    StoreIdentity::sealed_file(provider.store_epoch()).map_err(|_| {
                        HubError::KeyStore(SealedStoreError::Corrupt(
                            "store epoch is not 32 lowercase hex digits",
                        ))
                    })?;
                (spawn_stack(config, provider)?, identity)
            }
        };
        let meta = MetaInfo {
            bearer_enabled: bearer.is_some(),
        };
        let authenticator = PeerCredAuthenticator::new(config.peer_principals(), bearer);
        let crypto = into_hyper(
            CryptoHttpService::new(handle, http_config()).with_authenticator(authenticator),
        );
        Ok(Self {
            crypto,
            meta: Arc::new(meta),
            store: Arc::new(identity),
        })
    }

    /// [`HubService::build`] over a fresh in-memory key store.
    ///
    /// # Errors
    ///
    /// [`HubError::Worker`] if the crypto worker thread cannot be started.
    pub fn in_memory(config: &HubConfig, bearer: Option<BearerTokens>) -> Result<Self, HubError> {
        Self::build(config, bearer, KeyStore::InMemory)
    }

    /// Identity (epoch and persistence) of the key store this hub serves,
    /// as reported by `/v1/version` and `/v1/capabilities`.
    #[must_use]
    pub fn store_identity(&self) -> &StoreIdentity {
        &self.store
    }

    /// The per-connection service for a peer.
    #[must_use]
    pub fn for_connection(&self, peer: Option<PeerCred>) -> ConnectionService {
        ConnectionService {
            hub: self.clone(),
            peer,
        }
    }
}

/// Wrap `keys` in the hub's provider stack and move it onto the crypto
/// worker thread. Generic over the key store; the handle erases `P`.
fn spawn_stack<P: CryptoProvider>(
    config: &HubConfig,
    keys: P,
) -> Result<CryptoWorkerHandle, HubError> {
    let provider: HubProvider<P> = AuditProvider::new(PolicyProvider::new(
        KmsGuardProvider::new(keys),
        HarwUsageAuthorizer::new(config.policy()),
    ));
    CryptoWorkerHandle::spawn(provider, config.worker).map_err(HubError::Worker)
}

/// The adapter configuration: CryptGuard's defaults (including
/// `hide_forbidden_keys = true`: a principal without a grant sees 404, never
/// "exists but forbidden"), with the sign/verify body limit set to
/// [`SIGN_BODY_LIMIT`]. The other per-route limits stay at their defaults.
fn http_config() -> HttpConfig {
    let defaults = HttpConfig::default();
    HttpConfig {
        max_body: BodyLimits {
            sign: SIGN_BODY_LIMIT,
            ..defaults.max_body
        },
        ..defaults
    }
}

/// Hyper service for one accepted connection.
#[derive(Clone, Debug)]
pub struct ConnectionService {
    hub: HubService,
    peer: Option<PeerCred>,
}

impl HyperService<Request<Incoming>> for ConnectionService {
    type Response = HubResponse;
    type Error = Infallible;
    type Future = HubFuture;

    fn call(&self, mut request: Request<Incoming>) -> Self::Future {
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let peer = self.peer;

        if let Some(response) = meta::respond_with(&self.hub.meta, &self.hub.store, &method, &path)
        {
            audit::http_event(peer, &method, &path, response.status().as_u16());
            return Box::pin(core::future::ready(Ok(response)));
        }

        // Never trust a pre-existing extension; the only source of a
        // `PeerCred` is the accept loop.
        request.extensions_mut().remove::<PeerCred>();
        if let Some(peer) = peer {
            request.extensions_mut().insert(peer);
        }
        let future = self.hub.crypto.call(request);
        Box::pin(async move {
            let response = future.await?;
            audit::http_event(peer, &method, &path, response.status().as_u16());
            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use crypt_guard_hyper::{BodyLimits, HttpConfig};
    use harw_dod_encrypt::{CG_SIGN_BODY_LIMIT, MAX_SIGNABLE_TRANSCRIPT_LEN};
    use http::Method;
    use http_body_util::BodyExt;
    use zeroize::Zeroizing;

    use super::{HubService, KeyStore, SIGN_BODY_LIMIT, http_config};
    use crate::config::HubConfig;
    use crate::meta::{self, MetaInfo, StorePersistence};
    use crate::sealed::{KEK_LEN, Kek, SealedProvider};
    use crate::test_support::{TestError, TestResult, ctx};

    const MINIMAL: &str = r#"
        [[peers]]
        uid = 1000
        principal = "harw-web"
        grants = [ { namespace = "app", ops = ["read-public", "encrypt"] } ]
    "#;

    fn config() -> TestResult<HubConfig> {
        HubConfig::from_toml_str(MINIMAL).map_err(ctx("parse config"))
    }

    async fn meta_json(hub: &HubService, path: &str) -> TestResult<serde_json::Value> {
        let info = MetaInfo {
            bearer_enabled: false,
        };
        let response = meta::respond_with(&info, hub.store_identity(), &Method::GET, path)
            .ok_or(TestError::Missing("meta response"))?;
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(ctx("collect body"))?
            .to_bytes();
        serde_json::from_slice(&body).map_err(ctx("meta json"))
    }

    #[test]
    fn sign_limit_fits_max_transcript_and_other_limits_stay_default() {
        let config = http_config();
        let defaults = HttpConfig::default();
        // Sign request: CGK1 magic + version + u32 length + transcript.
        assert!(config.max_body.sign >= MAX_SIGNABLE_TRANSCRIPT_LEN + 4 + 1 + 4);
        // Verify of the same transcript with the largest signature fits too.
        assert_eq!(config.max_body.sign, SIGN_BODY_LIMIT);
        assert_eq!(SIGN_BODY_LIMIT, CG_SIGN_BODY_LIMIT);
        assert_eq!(
            config.max_body,
            BodyLimits {
                sign: SIGN_BODY_LIMIT,
                ..defaults.max_body
            }
        );
        assert!(config.hide_forbidden_keys);
    }

    #[tokio::test]
    async fn in_memory_build_reports_in_memory_persistence() -> TestResult {
        let config = config()?;
        let hub = HubService::in_memory(&config, None).map_err(ctx("build hub"))?;
        assert_eq!(
            hub.store_identity().persistence(),
            StorePersistence::InMemory
        );
        let caps = meta_json(&hub, "/v1/capabilities").await?;
        assert_eq!(caps["persistence"], "in-memory");
        assert_eq!(caps["store_epoch"], hub.store_identity().epoch());
        let version = meta_json(&hub, "/v1/version").await?;
        assert_eq!(version["store_epoch"], hub.store_identity().epoch());

        // Each in-memory hub is its own store with its own epoch.
        let other = HubService::in_memory(&config, None).map_err(ctx("build second hub"))?;
        assert_ne!(other.store_identity().epoch(), hub.store_identity().epoch());
        Ok(())
    }

    #[tokio::test]
    async fn sealed_build_reports_the_persisted_epoch() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create tempdir"))?;
        let path = dir.path().join("keys.sealed");
        let kek = Kek::from_bytes(Zeroizing::new([0x42; KEK_LEN]));
        let provider = SealedProvider::create(&path, &kek).map_err(ctx("create store"))?;
        let epoch = provider.store_epoch().to_owned();

        let hub = HubService::build(&config()?, None, KeyStore::Sealed(provider))
            .map_err(ctx("build hub"))?;
        assert_eq!(
            hub.store_identity().persistence(),
            StorePersistence::SealedFile
        );
        assert_eq!(hub.store_identity().epoch(), epoch);
        let caps = meta_json(&hub, "/v1/capabilities").await?;
        assert_eq!(caps["persistence"], "sealed-file");
        assert_eq!(caps["store_epoch"], epoch.as_str());
        Ok(())
    }
}
