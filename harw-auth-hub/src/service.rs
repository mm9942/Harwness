//! The hub's HTTP service: meta routes in front of the CryptGuard adapter.
//!
//! ```text
//! AF_UNIX accept ── SO_PEERCRED ──┐
//!                                 ▼
//! ConnectionService (per connection, carries PeerCred)
//!   ├─ GET /v1/health | /v1/version | /v1/capabilities   → meta::respond
//!   └─ everything else, with PeerCred as request extension →
//!        TowerToHyperService<CryptoHttpService<NetworkHandle, PeerCredAuthenticator>>
//!          └─ Buffer ─ ConcurrencyLimit ─ CryptoService
//!               └─ AuditProvider ─ PolicyProvider<_, NamespacePolicy> ─ InMemoryProvider
//! ```
//!
//! Only the `NetworkHandle` (a channel sender) is cloned per connection; all
//! key state stays owned by the single buffer worker.

use core::convert::Infallible;
use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use crypt_guard_hyper::{
    BearerTokens, CryptoHttpService, HttpConfig, TowerToHyperService, into_hyper,
};
use crypt_guard_service::{
    CryptoService, InMemoryProvider, NamespacePolicy, NetworkHandle, PolicyProvider, StackConfig,
    network_handle,
};
use http::{Request, Response};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::Service as HyperService;

use crate::audit::{self, AuditProvider};
use crate::auth::{PeerCred, PeerCredAuthenticator};
use crate::config::HubConfig;
use crate::meta::{self, MetaInfo};

/// The provider stack behind the buffer: audit → policy → in-memory keys.
pub type HubProvider = AuditProvider<PolicyProvider<InMemoryProvider, NamespacePolicy>>;

/// The CryptGuard HTTP adapter as configured by the hub.
pub type HubCryptoHttp = CryptoHttpService<NetworkHandle, PeerCredAuthenticator>;

/// Response type of every hub route.
pub type HubResponse = Response<Full<Bytes>>;

/// Boxed response future of [`ConnectionService`].
pub type HubFuture = Pin<Box<dyn Future<Output = Result<HubResponse, Infallible>> + Send>>;

/// Shared, cloneable hub service (one per process).
#[derive(Clone, Debug)]
pub struct HubService {
    crypto: TowerToHyperService<HubCryptoHttp>,
    meta: Arc<MetaInfo>,
}

impl HubService {
    /// Build the full stack from a validated config and the (optional)
    /// bearer-token table.
    ///
    /// # Panics
    ///
    /// Must be called inside a Tokio runtime: `network_handle` spawns the
    /// buffer worker with `tokio::spawn`.
    #[must_use]
    pub fn new(config: &HubConfig, bearer: Option<BearerTokens>) -> Self {
        Self::with_stack_config(config, bearer, StackConfig::default())
    }

    /// As [`HubService::new`] with explicit queue/concurrency bounds.
    ///
    /// # Panics
    ///
    /// As [`HubService::new`]; also if `stack.queue_bound` is zero.
    #[must_use]
    pub fn with_stack_config(
        config: &HubConfig,
        bearer: Option<BearerTokens>,
        stack: StackConfig,
    ) -> Self {
        let meta = MetaInfo {
            bearer_enabled: bearer.is_some(),
        };
        let authenticator = PeerCredAuthenticator::new(config.peer_principals(), bearer);
        let provider = AuditProvider::new(PolicyProvider::new(
            InMemoryProvider::new(),
            config.policy(),
        ));
        let handle = network_handle(CryptoService::new(provider), stack);
        // `HttpConfig::default()` keeps `hide_forbidden_keys = true`: a
        // principal without a grant sees 404, never "exists but forbidden".
        let crypto = into_hyper(
            CryptoHttpService::new(handle, HttpConfig::default()).with_authenticator(authenticator),
        );
        Self {
            crypto,
            meta: Arc::new(meta),
        }
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

        if let Some(response) = meta::respond(&self.hub.meta, &method, &path) {
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
