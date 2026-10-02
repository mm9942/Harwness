//! Self-cloud ingress: from a transport-authenticated peer to a
//! [`ClientIdentity`] (PL-68 §2.2, §6.2, W06).
//!
//! The node transport authenticates the *node* (PQ-TLS plus an ML-DSA
//! transcript signature) and inserts its `AuthenticatedPeer` into every
//! request. That proves a node id and nothing else. A [`RemoteLayer`] hands
//! the peer to a resolver, which maps it to the identity the host admits
//! against (device, tenant, tier). The resolver is the composition root's:
//! `harw-node-listener`'s `IdentityMapper` over its device registry is the
//! production one. This module holds no registry of its own.
//!
//! Rules the layer enforces regardless of the resolver:
//! - **remote approval is opt-in** (PL-68 §13, decision 4): `approve` is removed
//!   from every remote identity unless [`RemoteLayer::allow_approval_when`]
//!   says this identity may answer approvals; gateway and session caps stay as
//!   the resolver built them (tier-derived);
//! - an unresolved or refused peer is answered with the refusal's status
//!   (403 for [`ComRefusal::Denied`]) before anything is bound;
//! - the resolver must return the connection id it was given, so it cannot
//!   hand out an identity that belongs to another connection;
//! - a request without the transport's peer extension fails closed (500):
//!   the service was mounted outside the node transport;
//! - the resolved identity replaces any [`TrustedPeer`] already present.
//!
//! The layer is generic over the peer type so this crate does not depend on
//! the node transport and its TLS stack; the composition root names
//! `AuthenticatedPeer`.

use std::convert::Infallible;
use std::future::{Future, ready};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use harw_session_host::{ClientIdentity, ConnectionId};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Request, Response};
use tower::{Layer, Service};

use crate::layer::TrustedPeer;
use crate::refusal::{ComRefusal, plain};

type ResolveFuture = Pin<Box<dyn Future<Output = Result<ClientIdentity, ComRefusal>> + Send>>;
type Resolve<P> = dyn Fn(P, ConnectionId) -> ResolveFuture + Send + Sync;
type ApprovalGate = dyn Fn(&ClientIdentity) -> bool + Send + Sync;

/// Resolves the transport's peer extension `P` into the trusted identity.
pub struct RemoteLayer<P> {
    resolve: Arc<Resolve<P>>,
    approval: Option<Arc<ApprovalGate>>,
}

impl<P> Clone for RemoteLayer<P> {
    fn clone(&self) -> Self {
        Self {
            resolve: Arc::clone(&self.resolve),
            approval: self.approval.clone(),
        }
    }
}

impl<P: 'static> RemoteLayer<P> {
    /// A layer with an async resolver. `resolve` receives an owned copy of the
    /// peer and the id of the new connection, and must return an identity
    /// carrying that same connection id.
    #[must_use]
    pub fn new<F, Fut>(resolve: F) -> Self
    where
        F: Fn(P, ConnectionId) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ClientIdentity, ComRefusal>> + Send + 'static,
    {
        Self {
            resolve: Arc::new(move |peer, connection| Box::pin(resolve(peer, connection))),
            approval: None,
        }
    }

    /// Lets the identities for which `allow` returns `true` answer approvals.
    /// Without this, **no** remote identity keeps the `approve` cap, whatever
    /// its tier: high-risk approval stays local unless a policy opts a device
    /// in. The predicate sees the resolved identity (device, tenant, tier).
    #[must_use]
    pub fn allow_approval_when(
        mut self,
        allow: impl Fn(&ClientIdentity) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.approval = Some(Arc::new(allow));
        self
    }
}

impl<S, P> Layer<S> for RemoteLayer<P> {
    type Service = RemoteService<S, P>;

    fn layer(&self, inner: S) -> Self::Service {
        RemoteService {
            inner,
            resolve: Arc::clone(&self.resolve),
            approval: self.approval.clone(),
        }
    }
}

/// Service produced by [`RemoteLayer`].
pub struct RemoteService<S, P> {
    inner: S,
    resolve: Arc<Resolve<P>>,
    approval: Option<Arc<ApprovalGate>>,
}

impl<S: Clone, P> Clone for RemoteService<S, P> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            resolve: Arc::clone(&self.resolve),
            approval: self.approval.clone(),
        }
    }
}

type BoxedFuture = Pin<Box<dyn Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send>>;

impl<S, P, B> Service<Request<B>> for RemoteService<S, P>
where
    S: Service<Request<B>, Response = Response<Full<Bytes>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    P: Clone + Send + Sync + 'static,
    B: Send + 'static,
{
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = BoxedFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let Some(peer) = request.extensions().get::<P>().cloned() else {
            tracing::error!("session com: request without a transport peer");
            return Box::pin(ready(Ok(plain(
                hyper::StatusCode::INTERNAL_SERVER_ERROR,
                "connection has no transport identity",
            ))));
        };
        let connection = ConnectionId::next();
        let resolving = (self.resolve)(peer, connection);
        let approval = self.approval.clone();
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let identity = match resolving.await {
                Ok(mut identity) if identity.connection == connection => {
                    // Remote approval is opt-in: strip it unless a policy allows it.
                    if identity.is_remote()
                        && !approval.as_ref().is_some_and(|allow| allow(&identity))
                    {
                        identity.caps.approve = false;
                    }
                    identity
                }
                Ok(_) => {
                    tracing::error!("session com: resolver returned another connection's id");
                    return Ok(ComRefusal::Denied.response());
                }
                Err(refusal) => return Ok(refusal.response()),
            };
            // Replaces anything an earlier layer might have set.
            request.extensions_mut().insert(TrustedPeer(identity));
            inner.call(request).await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use hyper::StatusCode;
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::identity;

    #[derive(Clone, Debug)]
    struct Peer(&'static str);

    /// Inner service: records the label of the trusted peer it sees.
    #[derive(Clone)]
    struct Probe(Arc<Mutex<Vec<String>>>);

    impl Service<Request<()>> for Probe {
        type Response = Response<Full<Bytes>>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Infallible>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request<()>) -> Self::Future {
            let label = request
                .extensions()
                .get::<TrustedPeer>()
                .map_or_else(|| "none".to_owned(), |p| p.0.label.clone());
            if let Ok(mut seen) = self.0.lock() {
                seen.push(label);
            }
            ready(Ok(Response::new(Full::new(Bytes::new()))))
        }
    }

    fn request_with(peer: Option<Peer>) -> Request<()> {
        let mut request = Request::new(());
        if let Some(peer) = peer {
            request.extensions_mut().insert(peer);
        }
        request
    }

    #[tokio::test]
    async fn a_resolved_peer_reaches_the_inner_service_with_its_identity() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let layer = RemoteLayer::<Peer>::new(|peer, connection| async move {
            let mut id = identity(peer.0);
            id.connection = connection;
            Ok(id)
        });
        let response = layer
            .layer(Probe(Arc::clone(&seen)))
            .oneshot(request_with(Some(Peer("phone"))))
            .await;
        assert!(response.is_ok());
        assert_eq!(
            seen.lock().map(|s| s.clone()).unwrap_or_default(),
            ["phone"]
        );
    }

    #[tokio::test]
    async fn a_refused_peer_never_reaches_the_inner_service() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let layer = RemoteLayer::<Peer>::new(|_, _| async { Err(ComRefusal::Denied) });
        let response = layer
            .layer(Probe(Arc::clone(&seen)))
            .oneshot(request_with(Some(Peer("x"))))
            .await;
        assert_eq!(response.map(|r| r.status()), Ok(StatusCode::FORBIDDEN));
        assert!(seen.lock().map(|s| s.is_empty()).unwrap_or(false));
    }

    #[tokio::test]
    async fn a_resolver_cannot_hand_out_another_connections_identity() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        // `identity()` mints its own connection id, not the one it was given.
        let layer = RemoteLayer::<Peer>::new(|peer, _| async move { Ok(identity(peer.0)) });
        let response = layer
            .layer(Probe(Arc::clone(&seen)))
            .oneshot(request_with(Some(Peer("phone"))))
            .await;
        assert_eq!(response.map(|r| r.status()), Ok(StatusCode::FORBIDDEN));
        assert!(seen.lock().map(|s| s.is_empty()).unwrap_or(false));
    }

    /// An identity that looks remote, with the approve cap its tier gives.
    fn remote_identity(label: &str, connection: ConnectionId) -> ClientIdentity {
        let mut id = identity(label);
        id.connection = connection;
        id.zone = harw_types::TrustZone::Remote;
        id.tenant = harw_types::TenantId::try_from_str("tenant-a").ok();
        id.caps.approve = true;
        id
    }

    /// Inner service that records whether the trusted peer may approve.
    #[derive(Clone)]
    struct ApproveProbe(Arc<Mutex<Vec<bool>>>);

    impl Service<Request<()>> for ApproveProbe {
        type Response = Response<Full<Bytes>>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Infallible>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request<()>) -> Self::Future {
            let approve = request
                .extensions()
                .get::<TrustedPeer>()
                .is_some_and(|p| p.0.caps.approve);
            if let Ok(mut seen) = self.0.lock() {
                seen.push(approve);
            }
            ready(Ok(Response::new(Full::new(Bytes::new()))))
        }
    }

    #[tokio::test]
    async fn remote_identities_lose_approve_unless_a_policy_opts_them_in() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let layer = RemoteLayer::<Peer>::new(|peer, connection| async move {
            Ok(remote_identity(peer.0, connection))
        });
        let default = layer
            .layer(ApproveProbe(Arc::clone(&seen)))
            .oneshot(request_with(Some(Peer("phone"))))
            .await;
        assert!(default.is_ok());
        let opted = layer
            .clone()
            .allow_approval_when(|id| id.label == "trusted-phone")
            .layer(ApproveProbe(Arc::clone(&seen)));
        let _ = opted
            .clone()
            .oneshot(request_with(Some(Peer("trusted-phone"))))
            .await;
        let _ = opted.oneshot(request_with(Some(Peer("other-phone")))).await;
        assert_eq!(
            seen.lock().map(|s| s.clone()).unwrap_or_default(),
            [false, true, false],
            "default strips approve; the predicate opts a device in"
        );
    }

    #[tokio::test]
    async fn local_identities_keep_approve() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let layer = RemoteLayer::<Peer>::new(|peer, connection| async move {
            let mut id = identity(peer.0);
            id.connection = connection;
            id.caps.approve = true;
            Ok(id)
        });
        let _ = layer
            .layer(ApproveProbe(Arc::clone(&seen)))
            .oneshot(request_with(Some(Peer("local"))))
            .await;
        assert_eq!(seen.lock().map(|s| s.clone()).unwrap_or_default(), [true]);
    }

    #[tokio::test]
    async fn a_request_without_the_transport_peer_fails_closed() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let layer = RemoteLayer::<Peer>::new(|_, _| async { Err(ComRefusal::Denied) });
        let response = layer
            .layer(Probe(Arc::clone(&seen)))
            .oneshot(request_with(None))
            .await;
        assert_eq!(
            response.map(|r| r.status()),
            Ok(StatusCode::INTERNAL_SERVER_ERROR)
        );
        assert!(seen.lock().map(|s| s.is_empty()).unwrap_or(false));
    }
}
