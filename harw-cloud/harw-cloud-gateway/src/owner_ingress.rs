//! Compose the Hub directory and owner observation policy at the existing
//! authenticated node-transport request boundary.
//!
//! Mount only behind NodeTransportServer. It overwrites the authenticated
//! peer extension before the service sees each request. No body field or
//! header supplies an identity or endpoint. This adapter is not a handler,
//! route, owner publisher, or relay and does not itself listen on a socket.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(feature = "hub-owner-process-proof")]
use std::{collections::BTreeMap, sync::Mutex};
#[cfg(feature = "hub-owner-process-proof")]
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
#[cfg(feature = "hub-owner-placement-listener")]
use std::sync::atomic::{AtomicBool, Ordering};

use harw_node_transport::{AuthenticatedPeer, peer_of};
#[cfg(feature = "hub-owner-process-proof")]
use harw_node_transport::{OwnerProcessChannelBinding, node_key_fingerprint, owner_process_channel_binding_of};
use http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[cfg(feature = "hub-owner-process-proof")]
use crate::owner_direct_link::{DirectOwnerStatusPlan, OwnerDirectLinks};
use crate::owner_directory::valid_id;
use crate::owner_liveness::{
    ActiveOwnerLifecycle, FRESH_FOR, HubOwnerRoutes, LiveRouteAdmission, OwnerReportError,
};
#[cfg(feature = "hub-owner-process-proof")]
use crate::owner_process_proof::{OwnerProcessTrustSet, ProcessChallenge, ProcessProofError};
#[cfg(feature = "hub-owner-process-proof")]
use crate::owner_placement::{self, OwnerPlacementLookup, OwnerPlacementLookupId, OwnerPlacementLookupPair, OwnerPlacementLookupPhase};
#[cfg(feature = "hub-owner-placement-listener")]
use crate::owner_placement_listener::{self, OwnerPlacementProofExchange, OwnerPlacementProofLink};
#[cfg(feature = "hub-owner-placement-listener")]
use crate::owner_proof_exchange::{self, OwnerPresignPolicy, PresignRequest};
use crate::owner_report;
use crate::route_admission::{AttachKind, CallerGrant, OwnerFence, RouteRefusal};

pub use crate::owner_report::OWNER_REPORT_PATH;

const MAX_IN_FLIGHT: usize = 32;
const OBSERVATION_PREFLIGHT: Duration = Duration::from_secs(5);
#[cfg(feature = "hub-owner-process-proof")]
const MAX_PENDING_PROCESS_PROOFS: usize = 16;
#[cfg(feature = "hub-owner-process-proof")]
const MAX_PLACEMENT_LOOKUP_IDS: usize = 64;

/// Result of re-reading trusted enrollment and exact session policy.
pub type GrantFuture<'a> = Pin<Box<dyn Future<Output = Option<CallerGrant>> + Send + 'a>>;

/// The application must use its authenticated device policy and exact session
/// authorization. A node key, derived device tier, or tenant string alone
/// never grants a Hub route. The resolver must fail closed if state is absent,
/// revoked or unavailable, and cannot consult request headers or body fields.
pub trait HubSessionGrants: Send + Sync {
    fn resolve<'a>(
        &'a self,
        caller: &'a AuthenticatedPeer,
        session_id: &'a str,
        kind: AttachKind,
    ) -> GrantFuture<'a>;

    /// Require current exact-session policy from the original device key
    /// proven on this accepted transport channel, never a current-key query
    /// by NodeId alone. Without a real enrollment resolver the direct route
    /// stays unavailable.
    #[cfg(feature = "hub-owner-process-proof")]
    fn resolve_direct_observe<'a>(
        &'a self,
        _caller: &'a AuthenticatedPeer,
        _verified_key_fingerprint: [u8; 32],
        _session_id: &'a str,
    ) -> GrantFuture<'a> {
        Box::pin(async { None })
    }

    /// Cryptographically verified fingerprint of the key used in this
    /// caller's handshake, in the destination's fingerprint domain. This
    /// is not a lookup of the currently enrolled key by NodeId. Unless the
    /// trusted composition can prove the per-connection binding (e.g. the
    /// same immutable verifier pin used on this listener), return nothing.
    #[cfg(feature = "hub-owner-local-host")]
    fn observation_caller_key(&self, _caller: &AuthenticatedPeer) -> Option<[u8; 32]> {
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HubIngressError {
    Unauthenticated,
    Forbidden,
    Expired,
    Busy,
    WrongProtocol,
    InvalidReport,
    BodyLimit,
    Unavailable,
    OwnerReport(OwnerReportError),
    Route(RouteRefusal),
}

impl fmt::Display for HubIngressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthenticated => f.write_str("no authenticated Hub transport peer"),
            Self::Forbidden => f.write_str("Hub request is not permitted"),
            Self::Expired => f.write_str("Hub request expired before admission"),
            Self::Busy => f.write_str("Hub ingress is busy"),
            Self::WrongProtocol => f.write_str("unsupported owner report protocol"),
            Self::InvalidReport => f.write_str("invalid owner report"),
            Self::BodyLimit => f.write_str("owner report exceeds its bound"),
            Self::Unavailable => f.write_str("Hub request could not be evaluated"),
            Self::OwnerReport(error) => fmt::Display::fmt(error, f),
            Self::Route(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for HubIngressError {}

/// In-process binding of one transport request to this ingress. Not
/// serializable, not reusable across gateways, and consumed by one operation.
struct ScopedHubRequest {
    peer: AuthenticatedPeer,
    issuer: Arc<()>,
    received_at: Instant,
    #[cfg(feature = "hub-owner-process-proof")]
    caller_channel: Option<OwnerProcessChannelBinding>,
}

/// Transport-bound owner report scope, consumed only by the dispatcher.
struct OwnerReportRequest(ScopedHubRequest);

/// Transport-bound client attach scope, consumed only by route admission.
pub struct SessionAttachRequest(ScopedHubRequest);

/// One observe-only preflight bound to the original authenticated peer and
/// this ingress. Not serializable or clonable. Holding it cannot attach a
/// session or authorize opening a transport.
pub struct PendingOwnerObservation {
    request: ScopedHubRequest,
    decision: LiveRouteAdmission,
    expires_at: Instant,
}

#[cfg(feature = "hub-owner-process-proof")]
pub(crate) struct DirectStatusAuthority {
    issuer: Arc<()>,
    caller: AuthenticatedPeer,
    caller_channel: OwnerProcessChannelBinding,
    caller_key: [u8; 32],
    grant: CallerGrant,
    decision: LiveRouteAdmission,
}

#[cfg(feature = "hub-owner-process-proof")]
impl DirectStatusAuthority {
    pub(crate) fn session(&self) -> &str {
        self.decision.session_id()
    }
}

/// One Hub-local before/after fence comparison. Never sent to the owner or
/// treated as a session grant; possession cannot authenticate a device.
#[cfg(feature = "hub-owner-process-proof")]
pub struct LiveOwnerFenceRead {
    plan: DirectOwnerStatusPlan,
    owner_channel: OwnerProcessChannelBinding,
}

/// One offline process-proof challenge bound to a pending observation.
/// Holding it cannot attach to a session. It is not serializable or clonable.
#[cfg(feature = "hub-owner-process-proof")]
pub struct PendingDestinationProcessProof {
    pending: PendingOwnerObservation,
    challenge: ProcessChallenge,
    channel: OwnerProcessChannelBinding,
    _capacity: OwnedSemaphorePermit,
}

#[cfg(feature = "hub-owner-process-proof")]
impl PendingDestinationProcessProof {
    /// Exact versioned public transcript to verify. This does not provide a
    /// signature service, an endpoint, or any private material.
    pub fn transcript(&self) -> &[u8] {
        self.challenge.transcript()
    }

    /// Local monotonic deadline; the holder must drop unfinished work when
    /// it expires rather than retaining a capacity slot indefinitely.
    pub fn expires_at(&self) -> Instant {
        self.pending.expires_at
    }
}

/// One administrator-selected plan awaiting independent process proof.
/// Neither it nor the proof witness is transferable over the wire.
#[cfg(feature = "hub-owner-process-proof")]
pub struct PendingOwnerPlacementProof {
    plan: DirectOwnerStatusPlan,
    challenge: ProcessChallenge,
    channel: OwnerProcessChannelBinding,
    _capacity: OwnedSemaphorePermit,
    reservation: Option<[u8; 32]>,
    #[cfg(feature = "hub-owner-placement-listener")]
    presign_checked: Arc<AtomicBool>,
}

#[cfg(feature = "hub-owner-placement-listener")]
struct PresignEntry {
    policy: OwnerPresignPolicy,
    fence: OwnerFence,
    grant: CallerGrant,
    caller: AuthenticatedPeer,
    caller_channel: OwnerProcessChannelBinding,
    proof_channel: OwnerProcessChannelBinding,
    expires_at: Instant,
    checked: Arc<AtomicBool>,
}

#[cfg(feature = "hub-owner-process-proof")]
impl PendingOwnerPlacementProof {
    /// Exact canonical transcript for the selected owner process.
    pub fn transcript(&self) -> &[u8] {
        self.challenge.transcript()
    }

    /// Original admission deadline; never extended by a lookup.
    pub fn expires_at(&self) -> Instant {
        self.plan.expires_at()
    }
}

#[cfg(feature = "hub-owner-process-proof")]
struct PlacementEntry {
    plan: DirectOwnerStatusPlan,
    proof_channel: OwnerProcessChannelBinding,
    correlation: [u8; 32],
    reservation: Option<[u8; 32]>,
    #[cfg(feature = "hub-owner-placement-listener")]
    proof_link: Option<Box<dyn OwnerPlacementProofLink>>,
}

#[cfg(feature = "hub-owner-process-proof")]
enum PlacementLookupState {
    Before(Box<PlacementEntry>, [u8; 32]),
    WaitingAfter { expiry: Instant, correlation: [u8; 32] },
    After(Box<PlacementEntry>),
}

#[cfg(feature = "hub-owner-process-proof")]
impl PlacementLookupState {
    fn expires_at(&self) -> Instant {
        match self {
            Self::Before(entry, _) | Self::After(entry) => entry.plan.expires_at(),
            Self::WaitingAfter { expiry, .. } => *expiry,
        }
    }

    fn matches_issue(&self, expiry: Instant, correlation: [u8; 32]) -> bool {
        if self.expires_at() != expiry {
            return false;
        }
        match self {
            Self::Before(entry, _) | Self::After(entry) => entry.correlation == correlation,
            Self::WaitingAfter { correlation: current, .. } => *current == correlation,
        }
    }
}

/// Exact owner coordinates freshly evaluated at the Hub. This is private
/// topology metadata for a trusted consumer, not proof that a destination
/// connection, owner process, or originating device authenticated there.
pub struct OwnerObservationTarget {
    decision: LiveRouteAdmission,
}

impl OwnerObservationTarget {
    /// Node selected by the durable directory, never by the caller.
    pub fn node_id(&self) -> &str {
        self.decision.destination_node()
    }

    /// Exact host instance selected by that same directory record.
    pub fn host_instance(&self) -> &str {
        self.decision.destination_instance()
    }

    /// Session authorized for the original caller's observe-only request.
    pub fn session_id(&self) -> &str {
        self.decision.session_id()
    }
}


/// Integration adapter for a single selected Hub. No arbitrary transport,
/// location, liveness input or raw caller grant enters the admission API.
pub struct HubTransportIngress<L, A> {
    routes: Arc<HubOwnerRoutes<L>>,
    authorization: A,
    issuer: Arc<()>,
    active: Arc<Semaphore>,
    #[cfg(feature = "hub-owner-process-proof")]
    process_trust: OwnerProcessTrustSet,
    #[cfg(feature = "hub-owner-process-proof")]
    direct_links: OwnerDirectLinks,
    #[cfg(feature = "hub-owner-process-proof")]
    pending_proofs: Arc<Semaphore>,
    #[cfg(feature = "hub-owner-process-proof")]
    placement_lookups: Arc<Mutex<BTreeMap<[u8; 32], PlacementLookupState>>>,
    #[cfg(feature = "hub-owner-process-proof")]
    placement_expiries: Arc<Semaphore>,
    #[cfg(feature = "hub-owner-placement-listener")]
    presign: Mutex<BTreeMap<[u8; 32], PresignEntry>>,
}

impl<L: ActiveOwnerLifecycle + 'static, A: HubSessionGrants> HubTransportIngress<L, A> {
    pub fn new(routes: HubOwnerRoutes<L>, authorization: A) -> Self {
        Self {
            routes: Arc::new(routes),
            authorization,
            issuer: Arc::new(()),
            active: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            #[cfg(feature = "hub-owner-process-proof")]
            process_trust: OwnerProcessTrustSet::default(),
            #[cfg(feature = "hub-owner-process-proof")]
            direct_links: OwnerDirectLinks::default(),
            #[cfg(feature = "hub-owner-process-proof")]
            pending_proofs: Arc::new(Semaphore::new(MAX_PENDING_PROCESS_PROOFS)),
            #[cfg(feature = "hub-owner-process-proof")]
            placement_lookups: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(feature = "hub-owner-process-proof")]
            placement_expiries: Arc::new(Semaphore::new(MAX_PLACEMENT_LOOKUP_IDS / 2)),
            #[cfg(feature = "hub-owner-placement-listener")]
            presign: Mutex::new(BTreeMap::new()),
        }
    }

    /// Select independently provisioned, exact process pins for this Hub
    /// before any requests are bound. A new issuer invalidates pending
    /// admission; the default empty registry refuses all destination proofs.
    #[cfg(feature = "hub-owner-process-proof")]
    pub fn with_owner_process_trust(mut self, trust: OwnerProcessTrustSet) -> Self {
        self.process_trust = trust;
        self.issuer = Arc::new(());
        self
    }

    /// Select one literal address/key per enrolled node in trusted startup
    /// composition. A new issuer refuses already-pending work.
    #[cfg(feature = "hub-owner-process-proof")]
    pub fn with_owner_direct_links(mut self, links: OwnerDirectLinks) -> Self {
        self.direct_links = links;
        self.issuer = Arc::new(());
        self
    }

    /// Local control-plane withdrawal of the exact configured process.
    /// A withdrawn or unavailable pin never substitutes another owner.
    #[cfg(feature = "hub-owner-process-proof")]
    pub fn withdraw_owner_process(
        &self,
        node: &str,
        instance: &str,
        owner_epoch: std::num::NonZeroU64,
    ) -> bool {
        self.process_trust.withdraw(node, instance, owner_epoch)
    }

    /// Bind one request selected by the dedicated authenticated owner-report
    /// dispatcher, before buffering its bounded protocol payload.
    fn bind_owner_report<B>(
        &self,
        request: &Request<B>,
    ) -> Result<OwnerReportRequest, HubIngressError> {
        self.bind_transport_request(request).map(OwnerReportRequest)
    }

    /// Bind one request selected by the authenticated session-attach dispatcher.
    pub fn bind_session_attach<B>(
        &self,
        request: &Request<B>,
    ) -> Result<SessionAttachRequest, HubIngressError> {
        self.bind_transport_request(request).map(SessionAttachRequest)
    }

    /// Use only in a service owned by NodeTransportServer. That server, not
    /// this adapter, authenticates the transport. An arbitrary in-process
    /// request with a locally injected extension is not a trusted request.
    ///
    /// Both future ingress operations require a POST. This function does not
    /// identify a route or parse a body. Call it after selecting the exact
    /// protocol operation, before buffering the bounded protocol payload.
    fn bind_transport_request<B>(
        &self,
        request: &Request<B>,
    ) -> Result<ScopedHubRequest, HubIngressError> {
        let peer = peer_of(request).map_err(|_| HubIngressError::Unauthenticated)?;
        if request.method() != Method::POST {
            return Err(HubIngressError::Forbidden);
        }
        Ok(ScopedHubRequest {
            peer,
            issuer: Arc::clone(&self.issuer),
            received_at: Instant::now(),
            #[cfg(feature = "hub-owner-process-proof")]
            caller_channel: owner_process_channel_binding_of(request),
        })
    }

    /// Process a single owner-report request received from the existing
    /// authenticated NodeTransportServer boundary. Requires the exact
    /// protocol route, content type and declared byte length; reads within a
    /// fixed deadline, frame and byte cap. This does not install a listener.
    ///
    /// The request cannot supply a node ID, lifecycle epoch, liveness time,
    /// routing endpoint or unscoped grant. The report never changes the
    /// durable owner directory.
    pub async fn dispatch_owner_report<B>(
        &self,
        request: Request<B>,
    ) -> Result<(), HubIngressError>
    where
        B: BodyExt + Unpin + Send,
        B::Data: AsRef<[u8]> + Send,
    {
        let bound = self.bind_owner_report(&request)?;
        self.dispatch_bound_owner_report(request, bound, || true).await
    }

    async fn dispatch_bound_owner_report<B>(
        &self,
        mut request: Request<B>,
        bound: OwnerReportRequest,
        current_transport: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), HubIngressError>
    where
        B: BodyExt + Unpin + Send,
        B::Data: AsRef<[u8]> + Send,
    {
        self.check_scope(&bound.0)?;
        if !current_transport() {
            return Err(HubIngressError::Unauthenticated);
        }
        let length = owner_report::body_len(&request)?;
        let permit = self.try_permit()?;
        let received_at = bound.0.received_at;
        let absolute_deadline = received_at
            .checked_add(owner_report::BODY_TIMEOUT)
            .ok_or(HubIngressError::Expired)?;
        let deadline = tokio::time::Instant::from_std(absolute_deadline);

        // The same permit covers both blocking steps as well as the async
        // read. A timed-out/cancelled async waiter cannot release a permit
        // while its blocking task is still reading local authority state.
        let routes = Arc::clone(&self.routes);
        let peer = bound.0.peer.clone();
        let (activation, permit) = tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || {
                let activation = routes.bind_owner_report_activation(
                    &peer, received_at, absolute_deadline,
                )?;
                Ok::<_, OwnerReportError>((activation, permit))
            }),
        )
        .await
        .map_err(|_| HubIngressError::Expired)?
        .map_err(|_| HubIngressError::Unavailable)?
        .map_err(HubIngressError::OwnerReport)?;
        let reported = tokio::time::timeout_at(
            deadline,
            owner_report::read(request.body_mut(), length, bound.0.peer.node_id.as_str()),
        )
        .await
        .map_err(|_| HubIngressError::Expired)??;
        self.check_scope(&bound.0)?;
        if !current_transport() {
            return Err(HubIngressError::Unauthenticated);
        }
        let routes = Arc::clone(&self.routes);
        tokio::time::timeout_at(
            deadline,
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                routes.record_from_owner_transport(activation, &reported, current_transport)
            }),
        )
        .await
        .map_err(|_| HubIngressError::Expired)?
        .map_err(|_| HubIngressError::Unavailable)?
        .map_err(HubIngressError::OwnerReport)
    }

    /// Empty authenticated report acknowledgment for a future dedicated
    /// transport mount. Keep local refusal details out of remote responses;
    /// the reporter only learns whether this exact observation was accepted.
    /// This does not mount a route or provide session attachment.
    pub async fn owner_report_response<B>(&self, request: Request<B>) -> Response<()>
    where
        B: BodyExt + Unpin + Send,
        B::Data: AsRef<[u8]> + Send,
    {
        let status = if self.dispatch_owner_report(request).await.is_ok() {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::FORBIDDEN
        };
        let mut response = Response::new(());
        *response.status_mut() = status;
        response
    }

    /// Wire boundary for the opted-in fixed listener only. An enrolled
    /// device, or even an otherwise verified peer with the same node label
    /// but a different proof key, cannot refresh owner observations.
    #[cfg(feature = "hub-owner-placement-listener")]
    pub(crate) async fn pinned_owner_report_response<B>(
        &self,
        request: Request<B>,
        listener_live: Arc<AtomicBool>,
    ) -> Response<harw_node_transport::NodeBody>
    where
        B: BodyExt + Unpin + Send,
        B::Data: AsRef<[u8]> + Send,
    {
        let result = async {
            let bound = self.bind_owner_report(&request)?;
            let channel = bound.0.caller_channel.clone()
                .filter(|channel| !channel.is_dialer() && channel.peer() == &bound.0.peer)
                .ok_or(HubIngressError::Unauthenticated)?;
            let owner = self.direct_links.select(bound.0.peer.node_id.as_str())
                .ok_or(HubIngressError::Forbidden)?;
            let expected = node_key_fingerprint(owner.node_key())
                .ok_or(HubIngressError::Forbidden)?;
            let current_transport = move || {
                listener_live.load(Ordering::Acquire)
                    && channel.verified_peer_key_fingerprint() == Some(expected)
            };
            self.dispatch_bound_owner_report(request, bound, current_transport).await
        }.await;
        if result.is_err() {
            return owner_placement_listener::refusal();
        }
        let mut response = Response::new(harw_node_transport::empty_body());
        *response.status_mut() = StatusCode::NO_CONTENT;
        response
    }

    /// Derive and check the exact session grant using the transport peer,
    /// then consult the bound durable directory and live observation cache.
    /// Asking to attach never records a heartbeat, grants a new permission,
    /// dials a target, or forwards a connection.
    pub async fn admit_session(
        &self,
        request: SessionAttachRequest,
        session_id: String,
        kind: AttachKind,
    ) -> Result<LiveRouteAdmission, HubIngressError> {
        let expires_at = request.0.received_at.checked_add(FRESH_FOR)
            .ok_or(HubIngressError::Expired)?;
        self.admit_bound(&request.0, &session_id, kind, expires_at).await
    }

    /// Begin a bounded local preflight for observation only. Requires an
    /// exact current session grant from the authenticated transport peer
    /// before owner coordinates are read. Does not return an endpoint.
    pub async fn prepare_owner_observation(
        &self,
        request: SessionAttachRequest,
        session_id: &str,
    ) -> Result<PendingOwnerObservation, HubIngressError> {
        let request = request.0;
        let expires_at = request.received_at.checked_add(OBSERVATION_PREFLIGHT)
            .ok_or(HubIngressError::Expired)?;
        let decision = self.admit_bound(&request, session_id, AttachKind::Observe, expires_at)
            .await?;
        Ok(PendingOwnerObservation { request, decision, expires_at })
    }

    /// Admit the original device through its sealed accepting witness and
    /// direct exact-session policy. Never falls back to the broad resolver.
    #[cfg(feature = "hub-owner-placement-listener")]
    async fn prepare_reserved_direct(
        &self,
        request: SessionAttachRequest,
        session: &str,
    ) -> Result<PendingOwnerObservation, HubIngressError> {
        let request = request.0;
        self.check_scope(&request)?;
        let expires_at = request.received_at.checked_add(OBSERVATION_PREFLIGHT).ok_or(HubIngressError::Expired)?;
        let channel = request.caller_channel.as_ref()
            .filter(|channel| !channel.is_dialer() && channel.peer() == &request.peer)
            .ok_or(HubIngressError::Unauthenticated)?;
        let key = channel.verified_peer_key_fingerprint().filter(|key| *key != [0; 32])
            .ok_or(HubIngressError::Unauthenticated)?;
        let permit = self.try_permit()?;
        let deadline = tokio::time::Instant::from_std(expires_at);
        let grant = tokio::time::timeout_at(
            deadline, self.authorization.resolve_direct_observe(&request.peer, key, session),
        ).await.map_err(|_| HubIngressError::Expired)?
            .filter(|grant| grant.authenticated_node == request.peer.node_id.as_str()
                && grant.session_id == session && valid_id(&grant.tenant) && valid_id(&grant.device_id))
            .ok_or(HubIngressError::Forbidden)?;
        let routes = Arc::clone(&self.routes);
        let selected = session.to_owned();
        let decision = tokio::time::timeout_at(deadline, tokio::task::spawn_blocking(move || {
            let _permit = permit;
            routes.admit(&selected, AttachKind::Observe, Some(&grant)).map_err(HubIngressError::Route)
        })).await.map_err(|_| HubIngressError::Expired)?
            .map_err(|_| HubIngressError::Unavailable)??;
        if channel.verified_peer_key_fingerprint() != Some(key) {
            return Err(HubIngressError::Unauthenticated);
        }
        ensure_before(expires_at)?;
        Ok(PendingOwnerObservation { request, decision, expires_at })
    }

    /// Consume a preflight once. Re-read the original peer's exact session
    /// grant (including policy revision), durable generation and live epoch.
    /// No endpoint, raw grant or destination instance is accepted from
    /// the caller. Keep the result inside trusted composition; it is not
    /// a lease or a replacement for the destination's end-to-end policy.
    pub async fn finish_owner_observation(
        &self,
        pending: PendingOwnerObservation,
    ) -> Result<OwnerObservationTarget, HubIngressError> {
        let PendingOwnerObservation { request, decision, expires_at } = pending;
        self.check_scope(&request)?;
        ensure_before(expires_at)?;
        let permit = self.try_permit()?;
        let grant = self.current_grant(
            &request, decision.session_id(), AttachKind::Observe, expires_at,
        ).await?;
        let routes = Arc::clone(&self.routes);
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(expires_at),
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                ensure_before(expires_at)?;
                routes.recheck_before_attach(
                    &decision,
                    decision.destination_node(),
                    decision.destination_instance(),
                    Some(&grant),
                ).map_err(HubIngressError::Route)?;
                ensure_before(expires_at)?;
                Ok(OwnerObservationTarget { decision })
            }),
        )
        .await
        .map_err(|_| HubIngressError::Expired)?
        .map_err(|_| HubIngressError::Unavailable)?
    }

    /// Same-process observe preflight using a previously pinned independent
    /// destination host and the actual end-to-end caller's connection to it.
    /// Opens no channel and does not use the Hub node as a stand-in for the
    /// caller. The result records a point-in-time authorization only.
    #[cfg(feature = "hub-owner-local-host")]
    pub async fn confirm_local_owner_observation(
        &self,
        pending: PendingOwnerObservation,
        pin: &harw_session_host::OwnerProcessPin,
        caller_connection: &harw_session_host::HostConnection,
    ) -> Result<(), HubIngressError> {
        self.observe_local_owner_status(pending, pin, caller_connection)
            .await.map(|_| ())
    }

    /// The only data returned is the destination's finite status enum.
    /// It is computed after the separately authenticated owner's exact
    /// membership and hosted-record checks. It is a point-in-time read,
    /// not a stream, a session capability, or permission to do anything.
    #[cfg(feature = "hub-owner-local-host")]
    pub async fn observe_local_owner_status(
        &self,
        pending: PendingOwnerObservation,
        pin: &harw_session_host::OwnerProcessPin,
        caller_connection: &harw_session_host::HostConnection,
    ) -> Result<harw_session_host::OwnerObservationStatus, HubIngressError> {
        self.check_scope(&pending.request)?;
        ensure_before(pending.expires_at)?;
        let caller_key_fingerprint = self.authorization
            .observation_caller_key(&pending.request.peer)
            .filter(|key| *key != [0; 32])
            .ok_or(HubIngressError::Forbidden)?;
        let (fence, grant) = pending.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let epoch = std::num::NonZeroU64::new(fence.owner_epoch)
            .ok_or(HubIngressError::Forbidden)?;
        let scope = harw_session_host::OwnerObservationScope {
            owner_node: &fence.node_id,
            owner_instance: &fence.host_instance,
            owner_epoch: epoch,
            session: &fence.session_id,
            tenant: &fence.tenant,
            caller_node: &grant.authenticated_node,
            caller_device: &grant.device_id,
            caller_key_fingerprint,
        };
        let status = tokio::time::timeout_at(
            tokio::time::Instant::from_std(pending.expires_at),
            pin.observe_status(caller_connection, scope),
        ).await
            .map_err(|_| HubIngressError::Expired)?
            .map_err(|_| HubIngressError::Forbidden)?;
        // Refresh original authorization and current directory/liveness
        // after the destination's independent read; no reusable token.
        self.finish_owner_observation(pending).await?;
        Ok(status)
    }

    /// Select the explicitly configured direct-owner status candidate. The
    /// original device must have been authenticated on this accepting
    /// listener and still holds the exact, key-bound observe grant.
    /// The result is internal initial-dial metadata only, not authority
    /// to forward credentials, prove the owner process or call the RPC.
    #[cfg(feature = "hub-owner-process-proof")]
    pub async fn choose_direct_owner_status(
        &self,
        pending: PendingOwnerObservation,
    ) -> Result<DirectOwnerStatusPlan, HubIngressError> {
        self.check_scope(&pending.request)?;
        let expires_at = pending.expires_at;
        let caller_peer = pending.request.peer.clone();
        ensure_before(expires_at)?;
        let inflight = self.try_permit()?;
        let caller_channel = pending.request.caller_channel.as_ref()
            .filter(|channel| !channel.is_dialer() && channel.peer() == &pending.request.peer)
            .ok_or(HubIngressError::Unauthenticated)?.clone();
        let caller_key = caller_channel.verified_peer_key_fingerprint()
            .filter(|value| *value != [0; 32])
            .ok_or(HubIngressError::Unauthenticated)?;
        let (fence, original_grant) = pending.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let exact_grant = tokio::time::timeout_at(
            tokio::time::Instant::from_std(expires_at),
            self.authorization.resolve_direct_observe(
                &pending.request.peer, caller_key, &fence.session_id,
            ),
        ).await.map_err(|_| HubIngressError::Expired)?
            .ok_or(HubIngressError::Forbidden)?;
        if &exact_grant != original_grant {
            return Err(HubIngressError::Forbidden);
        }

        let pin = self.process_trust.select(fence).map_err(process_proof_refusal)?;
        let link = self.direct_links.select(&fence.node_id).ok_or(HubIngressError::Forbidden)?;
        if !pin.matches_enrolled_node_key(link.node_key()) {
            return Err(HubIngressError::Forbidden);
        }
        let epoch = std::num::NonZeroU64::new(fence.owner_epoch)
            .ok_or(HubIngressError::Forbidden)?;
        let instance = fence.host_instance.clone();
        let generation = fence.generation;
        drop(inflight);
        let plan = self.revalidate_direct_authority(DirectOwnerStatusPlan::new(
            link, instance, generation, epoch, expires_at,
            DirectStatusAuthority {
                issuer: Arc::clone(&self.issuer),
                caller: caller_peer,
                caller_channel,
                caller_key,
                grant: exact_grant,
                decision: pending.decision,
            },
        )).await?;
        pin.ensure_live().map_err(process_proof_refusal)?;
        Ok(plan)
    }

    /// Compare the exact selected incarnation before a separately authorized
    /// direct owner read. Invoked by the local trusted composition on the
    /// owner's distinct accepted Hub control connection. An arbitrary
    /// expected epoch, a claimed caller or a status request cannot mint it.
    #[cfg(feature = "hub-owner-process-proof")]
    pub async fn begin_live_owner_fence_read(
        &self,
        plan: DirectOwnerStatusPlan,
        owner_channel: &OwnerProcessChannelBinding,
    ) -> Result<LiveOwnerFenceRead, HubIngressError> {
        self.check_owner_fence_channel(&plan, owner_channel)?;
        let plan = self.revalidate_direct_authority(plan).await?;
        self.check_owner_fence_channel(&plan, owner_channel)?;
        Ok(LiveOwnerFenceRead { plan, owner_channel: owner_channel.clone() })
    }

    /// Consume a before-read check once after the destination's independent
    /// original-device read. Any new generation, activation, grant, owner
    /// pin, channel or timeout refuses publication. This does not perform
    /// or authenticate that read itself, and no result leaves this module.
    #[cfg(feature = "hub-owner-process-proof")]
    pub async fn complete_live_owner_fence_read(
        &self,
        pending: LiveOwnerFenceRead,
        owner_channel: &OwnerProcessChannelBinding,
    ) -> Result<(), HubIngressError> {
        if !pending.owner_channel.same_live_channel(owner_channel) {
            return Err(HubIngressError::Unauthenticated);
        }
        self.check_owner_fence_channel(&pending.plan, owner_channel)?;
        let plan = self.revalidate_direct_authority(pending.plan).await?;
        self.check_owner_fence_channel(&plan, owner_channel)
    }

    /// Co-located authority bridge: independently authenticate the actual
    /// local process and the original device's negotiated destination
    /// connection between the two Hub fence checks. The separate owner
    /// control channel cannot act for the device. No RPC is mounted here.
    #[cfg(all(feature = "hub-owner-process-proof", feature = "hub-owner-local-host"))]
    pub async fn observe_fenced_owner_status(
        &self,
        plan: DirectOwnerStatusPlan,
        owner_control_channel: &OwnerProcessChannelBinding,
        target_pin: &harw_session_host::OwnerProcessPin,
        original_device_connection: &harw_session_host::HostConnection,
    ) -> Result<harw_session_host::OwnerObservationStatus, HubIngressError> {
        let read = self.begin_live_owner_fence_read(plan, owner_control_channel).await?;
        let (fence, grant) = read.plan.authority.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let epoch = std::num::NonZeroU64::new(fence.owner_epoch)
            .ok_or(HubIngressError::Forbidden)?;
        let scope = harw_session_host::OwnerObservationScope {
            owner_node: &fence.node_id,
            owner_instance: &fence.host_instance,
            owner_epoch: epoch,
            session: &fence.session_id,
            tenant: &fence.tenant,
            caller_node: &grant.authenticated_node,
            caller_device: &grant.device_id,
            caller_key_fingerprint: read.plan.authority.caller_key,
        };
        let status = tokio::time::timeout_at(
            tokio::time::Instant::from_std(read.plan.expires_at()),
            target_pin.observe_status(original_device_connection, scope),
        ).await.map_err(|_| HubIngressError::Expired)?
            .map_err(|_| HubIngressError::Forbidden)?;
        self.complete_live_owner_fence_read(read, owner_control_channel).await?;
        Ok(status)
    }

    #[cfg(feature = "hub-owner-process-proof")]
    fn check_owner_fence_channel(
        &self,
        plan: &DirectOwnerStatusPlan,
        channel: &OwnerProcessChannelBinding,
    ) -> Result<(), HubIngressError> {
        ensure_before(plan.expires_at())?;
        if !Arc::ptr_eq(&self.issuer, &plan.authority.issuer) {
            return Err(HubIngressError::Forbidden);
        }
        let expected_key = node_key_fingerprint(plan.public_node_key())
            .ok_or(HubIngressError::Forbidden)?;
        if channel.is_dialer()
            || channel.peer().node_id.as_str() != plan.owner_node()
            || channel.verified_peer_key_fingerprint() != Some(expected_key)
        {
            return Err(HubIngressError::Unauthenticated);
        }
        let (fence, _) = plan.authority.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let pin = self.process_trust.select(fence).map_err(process_proof_refusal)?;
        if !pin.matches_enrolled_node_key(plan.public_node_key()) {
            return Err(HubIngressError::Forbidden);
        }
        Ok(())
    }

    #[cfg(feature = "hub-owner-process-proof")]
    async fn revalidate_direct_authority(
        &self,
        plan: DirectOwnerStatusPlan,
    ) -> Result<DirectOwnerStatusPlan, HubIngressError> {
        ensure_before(plan.expires_at())?;
        let permit = self.try_permit()?;
        if plan.authority.caller_channel.verified_peer_key_fingerprint() != Some(plan.authority.caller_key) {
            return Err(HubIngressError::Unauthenticated);
        }
        let deadline = tokio::time::Instant::from_std(plan.expires_at());
        let current = tokio::time::timeout_at(
            deadline,
            self.authorization.resolve_direct_observe(
                &plan.authority.caller, plan.authority.caller_key, plan.session(),
            ),
        ).await.map_err(|_| HubIngressError::Expired)?
            .ok_or(HubIngressError::Forbidden)?;
        if current != plan.authority.grant {
            return Err(HubIngressError::Forbidden);
        }
        let routes = Arc::clone(&self.routes);
        let plan = tokio::time::timeout_at(deadline, tokio::task::spawn_blocking(move || {
            let _permit = permit;
            ensure_before(plan.expires_at())?;
            let (instance, _, _) = plan.expected_process();
            routes.recheck_before_attach(
                &plan.authority.decision, plan.owner_node(), instance, Some(&current),
            ).map_err(HubIngressError::Route)?;
            if plan.authority.caller_channel.verified_peer_key_fingerprint() != Some(plan.authority.caller_key) {
                return Err(HubIngressError::Unauthenticated);
            }
            ensure_before(plan.expires_at())?;
            Ok::<_, HubIngressError>(plan)
        })).await.map_err(|_| HubIngressError::Expired)?
            .map_err(|_| HubIngressError::Unavailable)??;
        let _permit = self.try_permit()?;
        let latest = tokio::time::timeout_at(
            deadline,
            self.authorization.resolve_direct_observe(
                &plan.authority.caller, plan.authority.caller_key, plan.session(),
            ),
        ).await.map_err(|_| HubIngressError::Expired)?;
        if latest.as_ref() != Some(&plan.authority.grant) {
            return Err(HubIngressError::Forbidden);
        }
        if plan.authority.caller_channel.verified_peer_key_fingerprint() != Some(plan.authority.caller_key) {
            return Err(HubIngressError::Unauthenticated);
        }
        ensure_before(plan.expires_at())?;
        Ok(plan)
    }

    /// Bind a future one-use placement pair to an exact previously
    /// authorized direct plan and an actual outbound owner process challenge.
    /// No IDs exist yet and nothing is sent automatically.
    #[cfg(feature = "hub-owner-process-proof")]
    pub fn begin_owner_placement_proof(
        &self,
        plan: DirectOwnerStatusPlan,
        channel: &OwnerProcessChannelBinding,
    ) -> Result<PendingOwnerPlacementProof, HubIngressError> {
        self.begin_bound_owner_placement_proof(plan, channel, None)
    }

    #[cfg(feature = "hub-owner-process-proof")]
    fn begin_bound_owner_placement_proof(
        &self,
        plan: DirectOwnerStatusPlan,
        channel: &OwnerProcessChannelBinding,
        reservation: Option<[u8; 32]>,
    ) -> Result<PendingOwnerPlacementProof, HubIngressError> {
        if !Arc::ptr_eq(&self.issuer, &plan.authority.issuer) {
            return Err(HubIngressError::Forbidden);
        }
        let (fence, grant) = plan.authority.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let pin = self.process_trust.select(fence).map_err(process_proof_refusal)?;
        let exporter = selected_owner_channel(channel, &fence.node_id, pin.enrolled_key_fingerprint())?;
        let capacity = Arc::clone(&self.pending_proofs).try_acquire_owned()
            .map_err(|_| HubIngressError::Busy)?;
        let challenge = ProcessChallenge::issue_bound(
            pin, fence, grant, plan.authority.decision.active_epoch(), exporter, plan.expires_at(), reservation,
        ).map_err(process_proof_refusal)?;
        Ok(PendingOwnerPlacementProof {
            plan, challenge, channel: channel.clone(), _capacity: capacity, reservation,
            #[cfg(feature = "hub-owner-placement-listener")]
            presign_checked: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Verify the dedicated process key on the same actual proof link,
    /// then refresh exact Hub authority before minting the paired lookup IDs.
    /// IDs are for trusted delivery only, never bearer authentication.
    #[cfg(feature = "hub-owner-process-proof")]
    pub async fn issue_owner_placement_pair(
        &self,
        proof: PendingOwnerPlacementProof,
        channel: &OwnerProcessChannelBinding,
        signature: &[u8],
    ) -> Result<OwnerPlacementLookupPair, HubIngressError> {
        self.issue_placement_pair_inner(
            proof, channel, signature,
            #[cfg(feature = "hub-owner-placement-listener")]
            None,
        ).await
    }

    #[cfg(feature = "hub-owner-process-proof")]
    async fn issue_placement_pair_inner(
        &self,
        proof: PendingOwnerPlacementProof,
        channel: &OwnerProcessChannelBinding,
        signature: &[u8],
        #[cfg(feature = "hub-owner-placement-listener")]
        proof_link: Option<Box<dyn OwnerPlacementProofLink>>,
    ) -> Result<OwnerPlacementLookupPair, HubIngressError> {
        #[cfg(feature = "hub-owner-placement-listener")]
        if proof_link.as_ref().is_some_and(|link| !link.channel().same_live_channel(channel)) {
            return Err(HubIngressError::Unauthenticated);
        }
        #[cfg(feature = "hub-owner-placement-listener")]
        if proof.reservation.is_some() && !proof.presign_checked.load(Ordering::Acquire) {
            return Err(HubIngressError::Forbidden);
        }
        let PendingOwnerPlacementProof { plan, challenge, channel: issued, _capacity, reservation, .. } = proof;
        if !Arc::ptr_eq(&self.issuer, &plan.authority.issuer) || !issued.same_live_channel(channel) {
            return Err(HubIngressError::Unauthenticated);
        }
        let (fence, _) = plan.authority.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let pin = self.process_trust.select(fence).map_err(process_proof_refusal)?;
        let exporter = selected_owner_channel(channel, &fence.node_id, pin.enrolled_key_fingerprint())?;
        {
            let _permit = self.try_permit()?;
            challenge.verify(pin, exporter, signature).map_err(process_proof_refusal)?;
        }
        let plan = self.revalidate_direct_authority(plan).await?;
        pin.ensure_live().map_err(process_proof_refusal)?;
        if !issued.same_live_channel(channel) {
            return Err(HubIngressError::Unauthenticated);
        }

        let expiry = plan.expires_at();
        let before = placement_nonce()?;
        let after = placement_nonce()?;
        let correlation = placement_nonce()?;
        let pair = OwnerPlacementLookupPair::new(
            OwnerPlacementLookupId::from_bytes(before).ok_or(HubIngressError::Unavailable)?,
            OwnerPlacementLookupId::from_bytes(after).ok_or(HubIngressError::Unavailable)?,
        ).ok_or(HubIngressError::Unavailable)?;
        // Even immediately completed lookups still hold one timer slot
        // until expiry. Churn cannot create an unbounded set of sleepers.
        let expiry_slot = Arc::clone(&self.placement_expiries).try_acquire_owned()
            .map_err(|_| HubIngressError::Busy)?;
        let mut entries = self.placement_lookups.try_lock().map_err(|_| HubIngressError::Busy)?;
        let now = Instant::now();
        if now >= expiry {
            return Err(HubIngressError::Expired);
        }
        if entries.len() > MAX_PLACEMENT_LOOKUP_IDS - 2
            || entries.contains_key(&before) || entries.contains_key(&after)
            || correlation == before || correlation == after
        {
            return Err(HubIngressError::Busy);
        }
        entries.insert(before, PlacementLookupState::Before(Box::new(PlacementEntry {
            plan, proof_channel: issued, correlation, reservation,
            #[cfg(feature = "hub-owner-placement-listener")]
            proof_link,
        }), after));
        entries.insert(after, PlacementLookupState::WaitingAfter { expiry, correlation });
        drop(entries);
        // Release the proof connection at the original deadline even if
        // the device goes away without ever attempting a lookup.
        let lookups = Arc::clone(&self.placement_lookups);
        let _reap = tokio::spawn(async move {
            let _expiry_slot = expiry_slot;
            tokio::time::sleep_until(tokio::time::Instant::from_std(expiry)).await;
            let removed = {
                let Ok(mut entries) = lookups.lock() else { return };
                [before, after].map(|key| {
                    if entries.get(&key).is_some_and(|entry| entry.matches_issue(expiry, correlation)) {
                        entries.remove(&key)
                    } else {
                        None
                    }
                })
            };
            // Dropping the owned connection must not happen under the lock.
            drop(removed);
        });
        Ok(pair)
    }

    #[cfg(feature = "hub-owner-placement-listener")]
    fn register_owner_presign(
        &self,
        proof: &PendingOwnerPlacementProof,
        reservation: [u8; 32],
    ) -> Result<[u8; 32], HubIngressError> {
        ensure_before(proof.expires_at())?;
        if proof.reservation != Some(reservation) {
            return Err(HubIngressError::Forbidden);
        }
        let (fence, grant) = proof.plan.authority.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let selector = placement_nonce()?;
        let entry = PresignEntry {
            policy: OwnerPresignPolicy {
                version: 1, node: fence.node_id.clone(), instance: fence.host_instance.clone(),
                tenant: fence.tenant.clone(), session: fence.session_id.clone(),
                generation: fence.generation,
                epoch: std::num::NonZeroU64::new(fence.owner_epoch).ok_or(HubIngressError::Forbidden)?,
                activation: proof.plan.authority.decision.active_epoch(),
                caller_node: grant.authenticated_node.clone(), caller_device: grant.device_id.clone(),
                caller_key: proof.plan.authority.caller_key, policy_revision: grant.policy_revision,
                reservation, transcript_digest: owner_proof_exchange::transcript_digest(proof.transcript()),
            },
            fence: fence.clone(), grant: grant.clone(), caller: proof.plan.authority.caller.clone(),
            caller_channel: proof.plan.authority.caller_channel.clone(),
            proof_channel: proof.channel.clone(), expires_at: proof.expires_at(),
            checked: Arc::clone(&proof.presign_checked),
        };
        let mut entries = self.presign.try_lock().map_err(|_| HubIngressError::Busy)?;
        entries.retain(|_, entry| Instant::now() < entry.expires_at);
        if entries.len() >= MAX_PENDING_PROCESS_PROOFS || entries.contains_key(&selector) {
            return Err(HubIngressError::Busy);
        }
        entries.insert(selector, entry);
        Ok(selector)
    }

    #[cfg(feature = "hub-owner-placement-listener")]
    pub(crate) async fn owner_presign_response<B>(&self, mut request: Request<B>)
        -> Response<harw_node_transport::NodeBody>
    where B: BodyExt + Unpin + Send, B::Data: AsRef<[u8]> + Send,
    {
        let work = async {
            let channel = owner_process_channel_binding_of(&request)
                .filter(|channel| !channel.is_dialer()).ok_or(HubIngressError::Unauthenticated)?;
            let expected = self.direct_links.select(channel.peer().node_id.as_str())
                .and_then(|link| node_key_fingerprint(link.node_key()))
                .ok_or(HubIngressError::Forbidden)?;
            if channel.verified_peer_key_fingerprint() != Some(expected) {
                return Err(HubIngressError::Unauthenticated);
            }
            let len = owner_report::request_len(&request, owner_proof_exchange::OWNER_PRESIGN_PATH,
                owner_proof_exchange::PRESIGN_LIMIT)?;
            let permit = self.try_permit()?;
            let payload = owner_report::read_json(request.body_mut(), len, owner_proof_exchange::PRESIGN_LIMIT).await?;
            let query: PresignRequest = serde_json::from_slice(&payload).map_err(|_| HubIngressError::InvalidReport)?;
            if query.version != 1 || query.presign == [0; 32] { return Err(HubIngressError::Forbidden); }
            let entry = {
                let mut lookup = self.presign.try_lock().map_err(|_| HubIngressError::Busy)?;
                let entry = lookup.get(&query.presign).ok_or(HubIngressError::Forbidden)?;
                ensure_before(entry.expires_at)?;
                if channel.peer().node_id.as_str() != entry.policy.node {
                    return Err(HubIngressError::Forbidden);
                }
                lookup.remove(&query.presign).ok_or(HubIngressError::Forbidden)?
            };
            drop(permit);
            let deadline = tokio::time::Instant::from_std(entry.expires_at);
            let recheck_grant = || async {
                if entry.caller_channel.verified_peer_key_fingerprint() != Some(entry.policy.caller_key)
                    || channel.verified_peer_key_fingerprint() != Some(expected)
                { return Err(HubIngressError::Unauthenticated); }
                let current = self.authorization.resolve_direct_observe(
                    &entry.caller, entry.policy.caller_key, &entry.policy.session,
                ).await;
                if current.as_ref() != Some(&entry.grant) {
                    return Err(HubIngressError::Forbidden);
                }
                Ok(())
            };
            recheck_grant().await?;
            let pin = self.process_trust.select(&entry.fence).map_err(process_proof_refusal)?;
            if selected_owner_channel(&entry.proof_channel, &entry.fence.node_id, pin.enrolled_key_fingerprint()).is_err() {
                return Err(HubIngressError::Unauthenticated);
            }
            let permit = self.try_permit()?;
            let routes = Arc::clone(&self.routes);
            let fence = entry.fence.clone();
            let epoch = entry.policy.activation;
            tokio::time::timeout_at(deadline, tokio::task::spawn_blocking(move || {
                let _permit = permit;
                routes.confirm_presign_fence(&fence, epoch).map_err(HubIngressError::Route)
            })).await.map_err(|_| HubIngressError::Expired)?
                .map_err(|_| HubIngressError::Unavailable)??;
            recheck_grant().await?;
            ensure_before(entry.expires_at)?;
            if channel.verified_peer_key_fingerprint() != Some(expected)
                || entry.caller_channel.verified_peer_key_fingerprint() != Some(entry.policy.caller_key)
                || selected_owner_channel(&entry.proof_channel, &entry.fence.node_id, pin.enrolled_key_fingerprint()).is_err()
            {
                return Err(HubIngressError::Unauthenticated);
            }
            pin.ensure_live().map_err(process_proof_refusal)?;
            let bytes = serde_json::to_vec(&entry.policy).map_err(|_| HubIngressError::Unavailable)?;
            if bytes.len() > owner_proof_exchange::POLICY_LIMIT { return Err(HubIngressError::BodyLimit); }
            entry.checked.store(true, Ordering::Release);
            Ok(bytes)
        };
        match tokio::time::timeout(owner_placement::BODY_DEADLINE, work).await {
            Ok(Ok(bytes)) => placement_json_response(bytes),
            _ => owner_placement_listener::refusal(),
        }
    }

    /// Deliver the pair only as the response on the original accepted
    /// device link participating in the exact enrollment and proof checks.
    /// This is not a polling or recovery path; a lost response requires a
    /// fresh explicitly authorized operation.
    #[cfg(feature = "hub-owner-placement-listener")]
    pub(crate) async fn owner_placement_pair_response<B>(
        &self,
        request: Request<B>,
        exchange: &dyn OwnerPlacementProofExchange,
    ) -> Response<harw_node_transport::NodeBody>
    where
        B: BodyExt + Unpin + Send,
        B::Data: AsRef<[u8]> + Send,
    {
        match self.dispatch_placement_pair(request, exchange).await {
            Ok(bytes) => placement_json_response(bytes),
            Err(_) => owner_placement_listener::refusal(),
        }
    }

    #[cfg(feature = "hub-owner-placement-listener")]
    async fn dispatch_placement_pair<B>(
        &self,
        mut request: Request<B>,
        exchange: &dyn OwnerPlacementProofExchange,
    ) -> Result<Vec<u8>, HubIngressError>
    where
        B: BodyExt + Unpin + Send,
        B::Data: AsRef<[u8]> + Send,
    {
        let source = self.bind_session_attach(&request)?;
        let original = source.0.caller_channel.as_ref().cloned()
            .filter(|channel| !channel.is_dialer() && channel.peer() == &source.0.peer)
            .ok_or(HubIngressError::Unauthenticated)?;
        let source_key = original.verified_peer_key_fingerprint()
            .filter(|key| *key != [0; 32])
            .ok_or(HubIngressError::Unauthenticated)?;
        let len = owner_report::request_len(
            &request, owner_placement_listener::DIRECT_OWNER_PAIR_PATH,
            owner_placement_listener::PAIR_REQUEST_LIMIT,
        )?;
        let permit = self.try_permit()?;
        let bytes = tokio::time::timeout(
            owner_placement::BODY_DEADLINE,
            owner_report::read_json(request.body_mut(), len, owner_placement_listener::PAIR_REQUEST_LIMIT),
        ).await.map_err(|_| HubIngressError::Expired)??;
        let (session, reservation) = owner_placement_listener::decode_request(&bytes)?;
        drop(permit);

        let plan = self.choose_direct_owner_status(self.prepare_reserved_direct(source, &session).await?).await?;
        let (instance, generation, owner_epoch) = plan.expected_process();
        let instance = instance.to_owned();
        let node = plan.owner_node().to_owned();
        let deadline = plan.expires_at();
        // Limit asynchronous exchange slots before starting a dial supplied
        // by trusted composition. Busy never starts a second exchange.
        let _exchange_capacity = Arc::clone(&self.pending_proofs).try_acquire_owned()
            .map_err(|_| HubIngressError::Busy)?;
        let mut link = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), exchange.open(&plan))
            .await.map_err(|_| HubIngressError::Expired)??;
        let channel = link.channel().clone();
        // The proof lifecycle takes its own slot; avoid holding the same
        // semaphore slot twice through sign and registry insertion.
        drop(_exchange_capacity);
        let proof = self.begin_bound_owner_placement_proof(plan, &channel, Some(reservation))?;
        let presign = self.register_owner_presign(&proof, reservation)?;
        let signature = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline), link.sign_observe(proof.transcript(), reservation, presign),
        ).await.map_err(|_| HubIngressError::Expired)??;
        if original.verified_peer_key_fingerprint() != Some(source_key) {
            return Err(HubIngressError::Unauthenticated);
        }
        let pair = self.issue_placement_pair_inner(proof, &channel, &signature, Some(link)).await?;
        let (before, after) = pair.into_ids();
        let encode = || serde_json::to_vec(&owner_placement_listener::PairResponse {
            schema: "harw.owner-placement-pair",
            version: 2,
            reservation: &reservation,
            session: &session,
            node: &node,
            instance: &instance,
            generation,
            owner_epoch,
            before: before.as_bytes(),
            after: after.as_bytes(),
        }).map_err(|_| HubIngressError::Unavailable);
        if let Ok(bytes) = encode() {
            if bytes.len() <= owner_placement_listener::PAIR_RESPONSE_LIMIT
                && Instant::now() < deadline
                && original.verified_peer_key_fingerprint() == Some(source_key)
            {
                return Ok(bytes);
            }
        }
        // The response was not delivered; best-effort removal is safe even
        // if a concurrent lookup happens. Expiry always bound the entries.
        let removed = self.placement_lookups.try_lock().ok().map(|mut entries| {
            [entries.remove(before.as_bytes()), entries.remove(after.as_bytes())]
        });
        drop(removed);
        Err(HubIngressError::Forbidden)
    }

    /// Fixed internal read-only endpoint. Mount only if separately opting
    /// into the authenticator-owned NodeTransport service. No topology or
    /// exact denial explanation is sent for malformed/unauthorized calls.
    #[cfg(feature = "hub-owner-process-proof")]
    pub async fn owner_placement_response<B>(
        &self,
        request: Request<B>,
    ) -> Response<harw_node_transport::NodeBody>
    where
        B: BodyExt + Unpin + Send,
        B::Data: AsRef<[u8]> + Send,
    {
        let result = self.dispatch_owner_placement(request).await
            .and_then(|result| serde_json::to_vec(&result).map_err(|_| HubIngressError::Unavailable));
        let mut response = Response::new(harw_node_transport::empty_body());
        *response.status_mut() = StatusCode::FORBIDDEN;
        if let Ok(body) = result {
            if body.len() <= owner_placement::RESPONSE_LIMIT {
                let length = http::HeaderValue::from_str(&body.len().to_string());
                if let Ok(length) = length {
                    *response.status_mut() = StatusCode::OK;
                    response.headers_mut().insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
                    response.headers_mut().insert(http::header::CONTENT_LENGTH, length);
                    response.headers_mut().insert(http::header::CACHE_CONTROL, http::HeaderValue::from_static("no-store"));
                    *response.body_mut() = harw_node_transport::full_body(body);
                }
            }
        }
        response
    }

    #[cfg(feature = "hub-owner-process-proof")]
    async fn dispatch_owner_placement<B>(&self, mut request: Request<B>) -> Result<OwnerPlacementLookup, HubIngressError>
    where
        B: BodyExt + Unpin + Send,
        B::Data: AsRef<[u8]> + Send,
    {
        let channel = owner_process_channel_binding_of(&request)
            .filter(|channel| !channel.is_dialer())
            .ok_or(HubIngressError::Unauthenticated)?;
        let configured = self.direct_links.select(channel.peer().node_id.as_str())
            .ok_or(HubIngressError::Forbidden)?;
        let expected = node_key_fingerprint(configured.node_key()).ok_or(HubIngressError::Forbidden)?;
        if channel.verified_peer_key_fingerprint() != Some(expected) {
            return Err(HubIngressError::Unauthenticated);
        }
        let len = owner_report::request_len(&request, owner_placement::OWNER_PLACEMENT_PATH, owner_placement::REQUEST_LIMIT)?;
        let permit = self.try_permit()?;
        let bytes = tokio::time::timeout(
            owner_placement::BODY_DEADLINE,
            owner_report::read_json(request.body_mut(), len, owner_placement::REQUEST_LIMIT),
        ).await.map_err(|_| HubIngressError::Expired)??;
        let id = owner_placement::decode_request(&bytes)?;
        let key = id.as_bytes();

        // Check owner identity before removing anything. Wrong-node requests
        // never consume another authorized operation's selector.
        let (entry, next, phase) = {
            let mut entries = self.placement_lookups.try_lock().map_err(|_| HubIngressError::Busy)?;
            let entry = match entries.get(key) {
                Some(PlacementLookupState::Before(entry, _)) | Some(PlacementLookupState::After(entry)) => entry,
                _ => return Err(HubIngressError::Forbidden),
            };
            self.check_owner_fence_channel(&entry.plan, &channel)?;
            match entries.remove(key) {
                Some(PlacementLookupState::Before(entry, next)) => (entry, Some(next), OwnerPlacementLookupPhase::Before),
                Some(PlacementLookupState::After(entry)) => (entry, None, OwnerPlacementLookupPhase::After),
                _ => return Err(HubIngressError::Forbidden),
            }
        };
        drop(permit);
        let PlacementEntry {
            plan, proof_channel, correlation, reservation,
            #[cfg(feature = "hub-owner-placement-listener")]
            proof_link,
        } = *entry;
        self.check_owner_fence_channel(&plan, &channel)?;
        if !proof_channel.is_live() {
            return Err(HubIngressError::Unauthenticated);
        }
        let plan = self.revalidate_direct_authority(plan).await?;
        self.check_owner_fence_channel(&plan, &channel)?;
        if !proof_channel.is_live() {
            return Err(HubIngressError::Unauthenticated);
        }
        let (fence, grant) = plan.authority.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let response = OwnerPlacementLookup::from_fence(
            fence, plan.authority.decision.active_epoch(), phase, correlation, reservation,
            &grant.authenticated_node, &grant.device_id, plan.authority.caller_key,
        )?;
        if let Some(next) = next {
            let mut entries = self.placement_lookups.try_lock().map_err(|_| HubIngressError::Busy)?;
            ensure_before(plan.expires_at())?;
            if !matches!(entries.get(&next),
                Some(PlacementLookupState::WaitingAfter { expiry, correlation: bound }) if *expiry == plan.expires_at() && *bound == correlation
            ) {
                return Err(HubIngressError::Forbidden);
            }
            entries.insert(next, PlacementLookupState::After(Box::new(PlacementEntry {
                plan, proof_channel, correlation, reservation,
                #[cfg(feature = "hub-owner-placement-listener")]
                proof_link,
            })));
        }
        Ok(response)
    }

    /// Create a short-lived, offline proof challenge on an actual
    /// authenticated candidate channel to the exact selected owner. No
    /// external string, pin, raw exporter or received header determines
    /// trust. The challenge contains no routing destination or authority.
    #[cfg(feature = "hub-owner-process-proof")]
    pub fn begin_destination_process_proof(
        &self,
        pending: PendingOwnerObservation,
        channel: &OwnerProcessChannelBinding,
    ) -> Result<PendingDestinationProcessProof, HubIngressError> {
        self.check_scope(&pending.request)?;
        let (fence, grant) = pending.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let pin = self.process_trust.select(fence).map_err(process_proof_refusal)?;
        let binding = selected_owner_channel(channel, &fence.node_id, pin.enrolled_key_fingerprint())?;
        let capacity = Arc::clone(&self.pending_proofs)
            .try_acquire_owned().map_err(|_| HubIngressError::Busy)?;
        let challenge = ProcessChallenge::issue(
            pin,
            fence,
            grant,
            pending.decision.active_epoch(),
            binding,
            pending.expires_at,
        ).map_err(process_proof_refusal)?;
        Ok(PendingDestinationProcessProof {
            pending,
            challenge,
            channel: channel.clone(),
            _capacity: capacity,
        })
    }

    /// Consume an exact proof on the same still-live authenticated channel
    /// and under the operator-selected pin. Then refresh Hub policy and
    /// the current owner fence/activation within the original deadline.
    /// This never grants destination caller authorization or a stream.
    #[cfg(feature = "hub-owner-process-proof")]
    pub async fn finish_destination_process_proof(
        &self,
        proof: PendingDestinationProcessProof,
        channel: &OwnerProcessChannelBinding,
        signature: &[u8],
    ) -> Result<(), HubIngressError> {
        let PendingDestinationProcessProof { pending, challenge, channel: issued, _capacity } = proof;
        self.check_scope(&pending.request)?;
        if !issued.same_live_channel(channel) {
            return Err(HubIngressError::Unauthenticated);
        }
        let (fence, _) = pending.decision.observation_context()
            .ok_or(HubIngressError::Forbidden)?;
        let pin = self.process_trust.select(fence).map_err(process_proof_refusal)?;
        let binding = selected_owner_channel(channel, &fence.node_id, pin.enrolled_key_fingerprint())?;
        {
            let _permit = self.try_permit()?;
            challenge.verify(pin, binding, signature).map_err(process_proof_refusal)?;
        }
        self.finish_owner_observation(pending).await?;
        if !issued.same_live_channel(channel) {
            return Err(HubIngressError::Unauthenticated);
        }
        pin.ensure_live().map_err(process_proof_refusal)
    }

    async fn admit_bound(
        &self,
        request: &ScopedHubRequest,
        session_id: &str,
        kind: AttachKind,
        expires_at: Instant,
    ) -> Result<LiveRouteAdmission, HubIngressError> {
        self.check_scope(request)?;
        ensure_before(expires_at)?;
        let permit = self.try_permit()?;
        let grant = self.current_grant(request, session_id, kind, expires_at).await?;
        let routes = Arc::clone(&self.routes);
        let session_id = session_id.to_owned();
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(expires_at),
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                ensure_before(expires_at)?;
                let route = routes.admit(&session_id, kind, Some(&grant))
                    .map_err(HubIngressError::Route)?;
                ensure_before(expires_at)?;
                Ok(route)
            }),
        )
        .await
        .map_err(|_| HubIngressError::Expired)?
        .map_err(|_| HubIngressError::Unavailable)?
    }

    async fn current_grant(
        &self,
        request: &ScopedHubRequest,
        session_id: &str,
        kind: AttachKind,
        expires_at: Instant,
    ) -> Result<CallerGrant, HubIngressError> {
        if !valid_id(session_id) {
            return Err(HubIngressError::Forbidden);
        }
        let grant = tokio::time::timeout_at(
            tokio::time::Instant::from_std(expires_at),
            self.authorization.resolve(&request.peer, session_id, kind),
        )
        .await
        .map_err(|_| HubIngressError::Expired)?
        .filter(|grant| {
            grant.authenticated_node.as_str() == request.peer.node_id.as_str()
                && grant.session_id == session_id
                && valid_id(&grant.device_id)
                && valid_id(&grant.tenant)
        })
        .ok_or(HubIngressError::Forbidden)?;
        self.check_scope(request)?;
        ensure_before(expires_at)?;
        Ok(grant)
    }

    fn try_permit(&self) -> Result<OwnedSemaphorePermit, HubIngressError> {
        Arc::clone(&self.active)
            .try_acquire_owned()
            .map_err(|_| HubIngressError::Busy)
    }

    fn check_scope(&self, request: &ScopedHubRequest) -> Result<(), HubIngressError> {
        if !Arc::ptr_eq(&self.issuer, &request.issuer) {
            return Err(HubIngressError::Forbidden);
        }
        ensure_fresh(request.received_at)
    }
}

fn ensure_fresh(received_at: Instant) -> Result<(), HubIngressError> {
    if Instant::now()
        .checked_duration_since(received_at)
        .is_some_and(|age| age < FRESH_FOR)
    {
        Ok(())
    } else {
        Err(HubIngressError::Expired)
    }
}


fn ensure_before(deadline: Instant) -> Result<(), HubIngressError> {
    if Instant::now() < deadline {
        Ok(())
    } else {
        Err(HubIngressError::Expired)
    }
}


#[cfg(feature = "hub-owner-process-proof")]
fn process_proof_refusal(error: ProcessProofError) -> HubIngressError {
    match error {
        ProcessProofError::Expired => HubIngressError::Expired,
        ProcessProofError::Unavailable => HubIngressError::Unavailable,
        _ => HubIngressError::Forbidden,
    }
}


#[cfg(feature = "hub-owner-process-proof")]
fn selected_owner_channel(
    channel: &OwnerProcessChannelBinding,
    expected_node: &str,
    expected_key: Option<[u8; 32]>,
) -> Result<[u8; 32], HubIngressError> {
    let peer = channel.peer();
    if !channel.is_dialer()
        || peer.node_id.as_str() != expected_node
        || expected_key.is_none()
        || channel.verified_peer_key_fingerprint() != expected_key
    {
        return Err(HubIngressError::Forbidden);
    }
    channel.export_for_peer(&peer.node_id).ok_or(HubIngressError::Unauthenticated)
}


#[cfg(feature = "hub-owner-process-proof")]
fn placement_nonce() -> Result<[u8; 32], HubIngressError> {
    let mut bytes = [0; 32];
    SystemRandom::new().fill(&mut bytes).map_err(|_| HubIngressError::Unavailable)?;
    if bytes == [0; 32] {
        return Err(HubIngressError::Unavailable);
    }
    Ok(bytes)
}


#[cfg(feature = "hub-owner-placement-listener")]
pub(crate) fn placement_json_response(body: Vec<u8>) -> Response<harw_node_transport::NodeBody> {
    let Ok(length) = http::HeaderValue::from_str(&body.len().to_string()) else {
        return owner_placement_listener::refusal();
    };
    let mut response = Response::new(harw_node_transport::full_body(body));
    response.headers_mut().insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("application/json"));
    response.headers_mut().insert(http::header::CONTENT_LENGTH, length);
    response.headers_mut().insert(http::header::CACHE_CONTROL, http::HeaderValue::from_static("no-store"));
    response
}
