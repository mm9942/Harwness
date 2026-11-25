//! Explicit opt-in, non-upgrading NodeTransport listener composition for
//! owner placement. No listener is installed by default.

use std::convert::Infallible;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use harw_node_transport::{
    NodeBody, NodeTransportServer, OwnerProcessChannelBinding, TransportError, empty_body,
};
use http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tower_service::Service;

use crate::owner_direct_link::DirectOwnerStatusPlan;
use crate::owner_ingress::{HubIngressError, HubSessionGrants, HubTransportIngress};
use crate::owner_liveness::ActiveOwnerLifecycle;
use crate::owner_placement::OWNER_PLACEMENT_PATH;
use crate::owner_report::OWNER_REPORT_PATH;

/// Fixed original-device delivery path. No address, node or arbitrary
/// operation can be selected by the request body.
pub const DIRECT_OWNER_PAIR_PATH: &str = "/v1/hub/direct-owner-placement-pair";
pub(crate) const PAIR_REQUEST_LIMIT: usize = 512;
pub(crate) const PAIR_RESPONSE_LIMIT: usize = 2 * 1024;

/// Only trusted startup composition may supply the exact owner exchange.
/// Implementations do not accept caller-supplied endpoints.
pub trait OwnerPlacementProofExchange: Send + Sync {
    fn open<'a>(
        &'a self,
        plan: &'a DirectOwnerStatusPlan,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn OwnerPlacementProofLink>, HubIngressError>> + Send + 'a>>;
}

/// Owns the actual narrow proof connection. Its sealed witness must match
/// the direct-plan pin and cannot be fabricated by this interface. The Hub
/// holds this object through the two lookups to keep the proved channel live.
pub trait OwnerPlacementProofLink: Send {
    fn channel(&self) -> &OwnerProcessChannelBinding;

    /// Request a signature for precisely this canonical proof, not a
    /// generic signer or any routing/remote-execution operation.
    fn sign_observe<'a>(
        &'a mut self,
        transcript: &'a [u8],
        reservation: [u8; 32],
        presign: [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, HubIngressError>> + Send + 'a>>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
    schema: String,
    version: u8,
    session: String,
    reservation: [u8; 32],
}

pub(crate) fn decode_request(bytes: &[u8]) -> Result<(String, [u8; 32]), HubIngressError> {
    let request: PairRequest = serde_json::from_slice(bytes).map_err(|_| HubIngressError::InvalidReport)?;
    if request.schema != "harw.owner-placement-pair-request" || request.version != 2
        || request.reservation == [0; 32]
        || !crate::owner_directory::valid_id(&request.session)
    {
        return Err(HubIngressError::WrongProtocol);
    }
    Ok((request.session, request.reservation))
}

#[derive(Serialize)]
pub(crate) struct PairResponse<'a> {
    pub schema: &'static str,
    pub version: u8,
    pub reservation: &'a [u8; 32],
    pub session: &'a str,
    pub node: &'a str,
    pub instance: &'a str,
    pub generation: NonZeroU64,
    pub owner_epoch: NonZeroU64,
    pub before: &'a [u8; 32],
    pub after: &'a [u8; 32],
}

/// Serve exactly pinned owner liveness reports, owner placement, the one-use
/// pre-sign lookup, and, if an exchange is supplied, the original-device
/// pair route on a caller-selected existing listener.
/// The supplied NodeTransport server is responsible for actual pinned peer
/// verification. No upgrade, fallback service, default server, or deployment.
pub async fn serve_owner_placement_listener<L, A, F>(
    transport: &NodeTransportServer,
    listener: TcpListener,
    ingress: Arc<HubTransportIngress<L, A>>,
    exchange: Option<Arc<dyn OwnerPlacementProofExchange>>,
    shutdown: F,
) -> Result<(), TransportError>
where
    L: ActiveOwnerLifecycle + 'static,
    A: HubSessionGrants + 'static,
    F: Future<Output = ()>,
{
    let live = Arc::new(AtomicBool::new(true));
    let _lifetime = ListenerLifetime(Arc::clone(&live));
    let service = OwnerPlacementService { ingress, exchange, live };
    transport.serve_with_shutdown(listener, service, shutdown).await
}

/// Serve the identical fixed read-only routes on an independently pinned
/// Onion service. No TCP socket is opened. Even an authenticated stream
/// receives no service until Arti reports the expected local locator.
///
/// # Errors
/// Tor startup failure or mismatch with the configured service locator.
#[cfg(feature = "hub-owner-onion")]
pub async fn serve_owner_placement_onion<L, A, F>(
    transport: NodeTransportServer,
    config: harw_onion::OnionServerConfig,
    expected: &harw_onion::OnionLocator,
    ingress: Arc<HubTransportIngress<L, A>>,
    exchange: Arc<dyn OwnerPlacementProofExchange>,
    shutdown: F,
) -> Result<(), harw_onion::OnionError>
where
    L: ActiveOwnerLifecycle + 'static,
    A: HubSessionGrants + 'static,
    F: Future<Output = ()>,
{
    use std::time::Duration;
    use harw_onion::{OnionError, OnionServer, OnionStream};

    if config.virtual_port != expected.port() {
        return Err(OnionError::Service("owner Hub Onion port does not match the pin".to_owned()));
    }
    let live = Arc::new(AtomicBool::new(false));
    let lifetime = ListenerLifetime(Arc::clone(&live));
    let service = OwnerPlacementService { ingress, exchange: Some(exchange), live: Arc::clone(&live) };
    let mut shutdown = std::pin::pin!(shutdown);
    let server = tokio::select! {
        biased;
        () = shutdown.as_mut() => return Ok(()),
        started = OnionServer::launch(config, |_| {}, move |io: OnionStream| {
            let service = service.clone();
            let transport = transport.clone();
            async move {
                if !service.live.load(Ordering::Acquire) {
                    return;
                }
                // Both Arti's stream limits and NodeTransport's connection
                // limits remain in force. Expire an idle accepted connection
                // and never drive HTTP Upgrade on this listener.
                let _ = tokio::time::timeout(
                    Duration::from_secs(30),
                    transport.serve_io(io, service, Duration::from_secs(5)),
                ).await;
            }
        }) => started?,
    };
    if server.locator() != expected {
        return Err(OnionError::Service("owner Hub Onion identity does not match the pin".to_owned()));
    }
    live.store(true, Ordering::Release);
    shutdown.as_mut().await;
    drop(lifetime);
    server.shutdown();
    Ok(())
}

struct ListenerLifetime(Arc<AtomicBool>);

impl Drop for ListenerLifetime {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct OwnerPlacementService<L, A> {
    ingress: Arc<HubTransportIngress<L, A>>,
    exchange: Option<Arc<dyn OwnerPlacementProofExchange>>,
    live: Arc<AtomicBool>,
}

impl<L, A> Clone for OwnerPlacementService<L, A> {
    fn clone(&self) -> Self {
        Self { ingress: Arc::clone(&self.ingress), exchange: self.exchange.clone(), live: Arc::clone(&self.live) }
    }
}

impl<L, A, B> Service<Request<B>> for OwnerPlacementService<L, A>
where
    L: ActiveOwnerLifecycle + 'static,
    A: HubSessionGrants + 'static,
    B: BodyExt + Unpin + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
{
    type Response = Response<NodeBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        let ingress = Arc::clone(&self.ingress);
        let exchange = self.exchange.clone();
        let live = Arc::clone(&self.live);
        Box::pin(async move {
            if !live.load(Ordering::Acquire) {
                return Ok(refusal());
            }
            let response = match request.uri().path() {
                OWNER_REPORT_PATH => ingress.pinned_owner_report_response(request, Arc::clone(&live)).await,
                OWNER_PLACEMENT_PATH => ingress.owner_placement_response(request).await,
                crate::owner_proof_exchange::OWNER_PRESIGN_PATH => ingress.owner_presign_response(request).await,
                DIRECT_OWNER_PAIR_PATH => match exchange {
                    Some(exchange) => ingress.owner_placement_pair_response(request, exchange.as_ref()).await,
                    None => refusal(),
                },
                _ => refusal(),
            };
            if !live.load(Ordering::Acquire) {
                return Ok(refusal());
            }
            Ok(response)
        })
    }
}

pub(crate) fn refusal() -> Response<NodeBody> {
    let mut response = Response::new(empty_body());
    *response.status_mut() = StatusCode::FORBIDDEN;
    response
}
