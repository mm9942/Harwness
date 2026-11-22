use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::Duration;

use harw_node_transport::{LocalNode, NodeBody, empty_body};
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, ClientCaps, CreateParams, Cursor, DEFAULT_TAIL_ITEMS,
    InterruptParams, RespondResult, StreamProfile, SubmitParams, SubmitResult, features,
};
use harw_protocol::{PortError, SessionPort};
use harw_session_remote::{BackoffPolicy, ConnectOptions, RemoteError, RemotePort};
use harw_types::{ApprovalId, SessionId, TurnId};
use http::Method;
use tokio::runtime::{Builder, Handle, Runtime};
use tokio::sync::{Semaphore, oneshot, watch};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::identity::{ValidatedProfile, restore_identity};
use crate::projection::{FrameAction, SessionState, SessionWorker};
use crate::provider::request_provider;
use crate::{
    ApprovalDecision, ApprovalOutcome, ApprovalReceipt, ChatgptProviderCredential,
    ChatgptProviderStatus, ConnectionPhase, ConnectionRoute, ConnectionStatus, DeviceEnrollment,
    MobileError, MobileSession, RoutePreference, ServerProfile, SessionCursor, SessionSnapshot,
    SubmitOutcome, SubmitReceipt,
};

pub(crate) mod route;

use route::{Dialer, available_routes, route_label, route_plan};

const MAX_ATTACHMENTS: usize = 32;
const MAX_SUBMISSION_BYTES: usize = 256 * 1024;
const MAX_PROCESS_SUBMISSIONS: usize = 8192;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_INTERVAL: Duration = Duration::from_secs(10);
const MAX_RECONNECT_ATTEMPTS: u32 = 8;

#[derive(uniffi::Object)]
pub struct MobileCore {
    runtime: Option<Runtime>,
    shared: Arc<Shared>,
    local: LocalNode,
    enrollment: DeviceEnrollment,
}

#[derive(Default)]
struct Shared {
    state: Mutex<CoreState>,
    dialer: Dialer,
    /// Ends a running backoff pause early when the network returns.
    wake: tokio::sync::Notify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NetworkAction {
    Ignore,
    WakeBackoff,
    Restart,
}

fn network_action(phase: ConnectionPhase, run_active: bool, retryable_failure: bool) -> NetworkAction {
    match phase {
        ConnectionPhase::Connecting | ConnectionPhase::Reconnecting if run_active => {
            NetworkAction::WakeBackoff
        }
        ConnectionPhase::Failed if retryable_failure => NetworkAction::Restart,
        _ => NetworkAction::Ignore,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, CoreState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct CoreState {
    profile: Option<ValidatedProfile>,
    profile_epoch: u64,
    phase: ConnectionPhase,
    detail: Option<String>,
    run: Option<CancellationToken>,
    link: Option<Arc<Link>>,
    sessions: HashMap<SessionId, SessionState>,
    detaching: HashSet<SessionId>,
    submissions: HashMap<(SessionId, String), Submission>,
    route_preference: RoutePreference,
    route: Option<ConnectionRoute>,
    attempt: u32,
    next_retry_at_ms: Option<u64>,
    retryable_failure: bool,
}

impl Default for CoreState {
    fn default() -> Self {
        Self {
            profile: None,
            profile_epoch: 0,
            phase: ConnectionPhase::Disconnected,
            detail: None,
            run: None,
            link: None,
            sessions: HashMap::new(),
            detaching: HashSet::new(),
            submissions: HashMap::new(),
            route_preference: RoutePreference::Auto,
            route: None,
            attempt: 0,
            next_retry_at_ms: None,
            retryable_failure: false,
        }
    }
}

impl CoreState {
    fn active(&self) -> Result<Arc<Link>, MobileError> {
        if self.phase == ConnectionPhase::Revoked {
            return Err(MobileError::Revoked);
        }
        self.link
            .as_ref()
            .filter(|link| !link.stop.is_cancelled())
            .cloned()
            .ok_or(MobileError::Disconnected)
    }

    fn same_link(&self, link: &Arc<Link>) -> bool {
        !link.stop.is_cancelled()
            && self
                .link
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, link))
    }

    fn session(&self, id: &SessionId) -> Result<&SessionState, MobileError> {
        self.sessions.get(id).ok_or_else(unopened)
    }

    fn session_mut(&mut self, id: &SessionId) -> Result<&mut SessionState, MobileError> {
        self.sessions.get_mut(id).ok_or_else(unopened)
    }

    fn clear_link(&mut self, detail: &str) {
        if let Some(link) = self.link.take() {
            link.stop.cancel();
        }
        for session in self.sessions.values_mut() {
            session.offline(detail);
        }
    }

    fn stop(&mut self, detail: &str) {
        if let Some(run) = self.run.take() {
            run.cancel();
        }
        self.route = None;
        self.clear_link(detail);
    }

    fn revoke(&mut self, detail: String) {
        self.stop(&detail);
        self.phase = ConnectionPhase::Revoked;
        self.next_retry_at_ms = None;
        self.retryable_failure = false;
        self.detail = Some(detail);
        self.sessions.clear();
    }

    fn apply_pending_revocation(&mut self) {
        let revocation = self.link.as_ref().and_then(|link| {
            link.failure
                .borrow()
                .as_ref()
                .filter(|issue| issue.disposition == LinkDisposition::Revoked)
                .map(|issue| issue.detail.clone())
        });
        if let Some(detail) = revocation {
            self.revoke(detail);
        }
    }
}

struct Submission {
    fingerprint: blake3::Hash,
    state: SubmissionState,
}

enum SubmissionState {
    InFlight,
    Accepted(SubmitReceipt),
    Uncertain,
}

struct Link {
    port: RemotePort,
    stop: CancellationToken,
    failure: watch::Sender<Option<LinkIssue>>,
    attaching: Semaphore,
}

impl Link {
    async fn request<T>(
        &self,
        request: impl Future<Output = Result<T, PortError>>,
    ) -> Result<T, MobileError> {
        let outcome = tokio::select! {
            biased;
            () = self.stop.cancelled() => return Err(MobileError::Disconnected),
            result = tokio::time::timeout(REQUEST_TIMEOUT, request) => result,
        };
        let error = match outcome {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(error)) => error,
            Err(_) => PortError::Transport("Server did not answer in time".into()),
        };
        match &error {
            PortError::Transport(detail) if revoked_close(detail) => {
                self.fail(LinkIssue::revoked());
                return Err(MobileError::Revoked);
            }
            PortError::Transport(detail) => self.fail(LinkIssue::transport(detail.clone())),
            PortError::Revoked => self.fail(LinkIssue::revoked()),
            _ => {}
        }
        Err(error.into())
    }

    fn fail(&self, issue: LinkIssue) {
        self.failure.send_if_modified(|current| {
            if current.as_ref().is_none_or(|prior| {
                issue.disposition == LinkDisposition::Revoked
                    && prior.disposition != LinkDisposition::Revoked
            }) {
                *current = Some(issue);
                true
            } else {
                false
            }
        });
        self.stop.cancel();
    }
}

#[derive(Clone)]
struct LinkIssue {
    detail: String,
    disposition: LinkDisposition,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkDisposition {
    Retry(Duration),
    Final,
    Revoked,
}

impl LinkIssue {
    fn transport(detail: String) -> Self {
        Self {
            detail,
            disposition: LinkDisposition::Retry(Duration::ZERO),
        }
    }

    fn revoked() -> Self {
        Self {
            detail: "The server withdrew this connection's authority. Reconnect after access is restored.".into(),
            disposition: LinkDisposition::Revoked,
        }
    }

    fn protocol(detail: String) -> Self {
        Self {
            detail,
            disposition: LinkDisposition::Final,
        }
    }
}

#[uniffi::export]
impl MobileCore {
    #[uniffi::constructor]
    pub fn new(private_key: Vec<u8>, label: String) -> Result<Arc<Self>, MobileError> {
        let (enrollment, local) = restore_identity(&Zeroizing::new(private_key), label)?;
        let runtime = Builder::new_multi_thread()
            .worker_threads(4)
            .max_blocking_threads(16)
            .thread_name("harw-mobile")
            .enable_all()
            .build()
            .map_err(|error| internal(format!("Could not create background runtime: {error}")))?;
        Ok(Arc::new(Self {
            runtime: Some(runtime),
            shared: Arc::new(Shared::default()),
            local,
            enrollment,
        }))
    }

    pub fn enrollment(&self) -> DeviceEnrollment {
        self.enrollment.clone()
    }

    pub fn export_enrollment_json(&self) -> Result<String, MobileError> {
        serde_json::to_string_pretty(&self.enrollment)
            .map_err(|_| internal("Could not encode public enrollment"))
    }

    pub fn chatgpt_provider_status(
        &self,
        profile: ServerProfile,
    ) -> Result<ChatgptProviderStatus, MobileError> {
        self.provider_call(profile, Method::GET, empty_body())
    }

    pub fn link_chatgpt_provider(
        &self,
        profile: ServerProfile,
        credential: ChatgptProviderCredential,
    ) -> Result<ChatgptProviderStatus, MobileError> {
        let body = Zeroizing::new(credential).request_body()?;
        self.provider_call(profile, Method::POST, body)
    }

    pub fn unlink_chatgpt_provider(
        &self,
        profile: ServerProfile,
    ) -> Result<ChatgptProviderStatus, MobileError> {
        self.provider_call(profile, Method::DELETE, empty_body())
    }

    pub fn connect(&self, profile: ServerProfile) -> Result<(), MobileError> {
        self.start_connection(Some(ValidatedProfile::new(profile)?))
    }

    pub fn reconnect(&self) -> Result<(), MobileError> {
        self.start_connection(None)
    }

    pub fn disconnect(&self) {
        let mut state = self.shared.state();
        state.apply_pending_revocation();
        if state.phase == ConnectionPhase::Revoked {
            return;
        }
        state.stop("Disconnected. This view is read-only");
        state.phase = ConnectionPhase::Disconnected;
        state.detail = None;
        state.next_retry_at_ms = None;
        state.retryable_failure = false;
    }

    /// Signals that the platform reports a usable network again.
    /// Never resends mutating calls; never lifts a revocation.
    pub fn network_available(&self) -> Result<(), MobileError> {
        let action = {
            let mut state = self.shared.state();
            state.apply_pending_revocation();
            let action = network_action(
                state.phase,
                state.run.as_ref().is_some_and(|run| !run.is_cancelled()),
                state.retryable_failure,
            );
            if action == NetworkAction::WakeBackoff {
                state.attempt = 0;
                state.next_retry_at_ms = None;
            }
            action
        };
        match action {
            NetworkAction::Ignore => Ok(()),
            NetworkAction::WakeBackoff => {
                self.shared.wake.notify_one();
                Ok(())
            }
            NetworkAction::Restart => self.start_connection(None),
        }
    }

    /// Sets the absolute Tor state and cache directories (before the first onion dial).
    pub fn set_tor_directories(
        &self,
        state_dir: String,
        cache_dir: String,
    ) -> Result<(), MobileError> {
        self.shared
            .dialer
            .set_tor_directories(PathBuf::from(state_dir), PathBuf::from(cache_dir))
    }

    /// Selects the route preference used by the next connect or reconnect.
    pub fn set_route_preference(&self, preference: RoutePreference) {
        self.shared.state().route_preference = preference;
    }

    pub fn status(&self) -> ConnectionStatus {
        // Read Tor progress before the state lock: the Tor client lock can be
        // held while Tor starts, which must never stall state operations.
        let tor = self.shared.dialer.tor_status();
        let mut state = self.shared.state();
        state.apply_pending_revocation();
        let profile = state.profile.as_ref().map(|profile| &profile.profile);
        let active_caps = state
            .active()
            .ok()
            .map(|link| link.port.connection().hello_ack().granted);
        let phase = if state.phase == ConnectionPhase::Connected && active_caps.is_none() {
            ConnectionPhase::Reconnecting
        } else {
            state.phase
        };
        let route = matches!(
            phase,
            ConnectionPhase::Connecting
                | ConnectionPhase::Connected
                | ConnectionPhase::Reconnecting
        )
        .then_some(state.route)
        .flatten();
        ConnectionStatus {
            phase,
            node_id: profile.map(|profile| profile.server_node_id.clone()),
            endpoint: profile.map(|profile| profile.endpoint.clone()),
            label: profile.map(|profile| profile.label.clone()),
            detail: state.detail.clone(),
            can_create: active_caps.is_some_and(|caps| caps.steer),
            can_submit: active_caps.is_some_and(|caps| caps.steer),
            can_approve: active_caps.is_some_and(|caps| caps.approve),
            route,
            route_preference: state.route_preference,
            available_routes: state
                .profile
                .as_ref()
                .map(available_routes)
                .unwrap_or_default(),
            tor,
            attempt: state.attempt,
            next_retry_at_ms: state.next_retry_at_ms,
        }
    }

    pub fn list_sessions(&self) -> Result<Vec<MobileSession>, MobileError> {
        let link = self.shared.state().active()?;
        self.run(async move {
            Ok(link
                .request(link.port.list())
                .await?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }

    pub fn create_session(&self, title: Option<String>) -> Result<MobileSession, MobileError> {
        let link = self.shared.state().active()?;
        if !link.port.connection().hello_ack().granted.steer {
            return Err(MobileError::Denied {
                detail: "This device cannot create sessions".into(),
            });
        }
        self.run(async move {
            let params = CreateParams {
                workspace: None,
                title,
            };
            link.request(link.port.create(params)).await.map(Into::into)
        })
    }

    pub fn open_session(&self, session_id: String) -> Result<SessionSnapshot, MobileError> {
        let id = parse_session_id(session_id)?;
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let mut state = shared.state();
            let link = match state.active() {
                Ok(link) => link,
                Err(error) => {
                    return state
                        .sessions
                        .get(&id)
                        .map(|session| session.snapshot(&id, false))
                        .ok_or(error);
                }
            };
            if state.detaching.contains(&id) {
                return Err(session_error("Session is being detached"));
            }
            if !state.sessions.contains_key(&id)
                && state.sessions.len() + state.detaching.len() >= MAX_ATTACHMENTS
            {
                return Err(session_error(
                    "This connection already has 32 open sessions",
                ));
            }
            let session = state.sessions.entry(id.clone()).or_default();
            if session.worker.is_none() {
                start_attachment(&shared, session, id.clone(), link);
            }
            Ok(session.snapshot(&id, true))
        })
    }

    pub fn snapshot(&self, session_id: String) -> Result<SessionSnapshot, MobileError> {
        let id = parse_session_id(session_id)?;
        let state = self.shared.state();
        Ok(state.session(&id)?.snapshot(&id, state.active().is_ok()))
    }

    pub fn detach_session(&self, session_id: String) -> Result<(), MobileError> {
        let id = parse_session_id(session_id)?;
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let (session, link, profile_epoch) = {
                let mut state = shared.state();
                let Some(session) = state.sessions.remove(&id) else {
                    return Ok(());
                };
                state.detaching.insert(id.clone());
                (session, state.active().ok(), state.profile_epoch)
            };
            if let Some(worker) = session.worker {
                // Let a pending attach finish before detaching it. Aborting a
                // local future does not cancel an RPC accepted by the server.
                worker.stop.cancel();
                let _ = worker.task.await;
            }
            let result = match link {
                Some(link) => link.request(link.port.detach(id.clone())).await,
                None => Ok(()),
            };
            let mut state = shared.state();
            if state.profile_epoch == profile_epoch {
                state.detaching.remove(&id);
            }
            result
        })
    }

    pub fn submit(
        &self,
        session_id: String,
        text: String,
        expected_head: SessionCursor,
        client_message_id: String,
    ) -> Result<SubmitReceipt, MobileError> {
        let id = parse_session_id(session_id)?;
        validate_submission(&text, &client_message_id)?;
        let shared = Arc::clone(&self.shared);
        self.run(
            async move { submit(shared, id, text, expected_head.into(), client_message_id).await },
        )
    }

    pub fn interrupt(&self, session_id: String, turn_id: String) -> Result<(), MobileError> {
        let id = parse_session_id(session_id)?;
        let turn_id =
            TurnId::try_from_str(turn_id).map_err(|_| session_error("Invalid turn ID"))?;
        let link = {
            let state = self.shared.state();
            let link = state.active()?;
            let session = state.session(&id)?;
            session.require_live()?;
            if !session.can_submit() || session.running_turn_id() != Some(&turn_id) {
                return Err(MobileError::Denied {
                    detail: "The displayed turn is no longer interruptible".into(),
                });
            }
            link
        };
        self.run(async move {
            let params = InterruptParams {
                session_id: id,
                turn_id: Some(turn_id),
            };
            link.request(link.port.interrupt(params)).await
        })
    }

    pub fn decide(
        &self,
        session_id: String,
        approval_id: String,
        decision: ApprovalDecision,
    ) -> Result<ApprovalReceipt, MobileError> {
        let id = parse_session_id(session_id)?;
        let approval = ApprovalId::try_from_str(approval_id)
            .map_err(|_| session_error("Invalid approval ID"))?;
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let decision = decision.into();
            let link = {
                let mut state = shared.state();
                let link = state.active()?;
                state
                    .session_mut(&id)?
                    .begin_decision(&approval, decision)?;
                link
            };
            let result = link
                .request(link.port.respond(ApprovalRespondParams {
                    request_id: approval.clone(),
                    decision,
                    reason: None,
                }))
                .await;
            {
                let mut state = shared.state();
                if state.same_link(&link)
                    && let Ok(session) = state.session_mut(&id)
                {
                    let remove = matches!(
                        result,
                        Ok(RespondResult::Resolved
                            | RespondResult::AlreadyResolved { .. }
                            | RespondResult::Expired)
                    );
                    session.finish_decision(&approval, remove);
                }
            }
            let (outcome, detail) = match result? {
                RespondResult::Resolved => (ApprovalOutcome::Resolved, None),
                RespondResult::AlreadyResolved { by } => {
                    (ApprovalOutcome::AlreadyResolved, Some(by))
                }
                RespondResult::Expired => (ApprovalOutcome::Expired, None),
                RespondResult::Denied { reason } => (ApprovalOutcome::Denied, Some(reason)),
            };
            Ok(ApprovalReceipt { outcome, detail })
        })
    }
}

impl Drop for MobileCore {
    fn drop(&mut self) {
        self.shared.state().stop("Mobile core stopped");
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl MobileCore {
    fn provider_call(
        &self,
        profile: ServerProfile,
        method: Method,
        body: NodeBody,
    ) -> Result<ChatgptProviderStatus, MobileError> {
        let consent = ValidatedProfile::new(profile)?;
        let (stop, route, selected) = {
            let state = self.shared.state();
            let link = state.active()?;
            if !state
                .profile
                .as_ref()
                .is_some_and(|selected| selected.same_authority(&consent))
            {
                return Err(MobileError::InvalidProfile {
                    detail: "The selected server changed after ChatGPT authorization".into(),
                });
            }
            let selected = state
                .profile
                .clone()
                .ok_or_else(|| MobileError::InvalidProfile {
                    detail: "The selected server changed after ChatGPT authorization".into(),
                })?;
            (
                link.stop.clone(),
                state.route.unwrap_or(ConnectionRoute::Direct),
                selected,
            )
        };
        self.run(request_provider(
            self.shared.dialer.clone(),
            route,
            self.local.clone(),
            selected,
            stop,
            method,
            body,
        ))
    }

    fn start_connection(&self, profile: Option<ValidatedProfile>) -> Result<(), MobileError> {
        let shared = Arc::clone(&self.shared);
        let local = self.local.clone();
        let label = self.enrollment.label.clone();
        self.run(async move {
            let explicit_reconnect = profile.is_none();
            let (run, profile) = {
                let mut state = shared.state();
                state.apply_pending_revocation();
                let profile = profile.or_else(|| state.profile.clone()).ok_or_else(|| {
                    MobileError::InvalidProfile {
                        detail: "Select a server profile before connecting".into(),
                    }
                })?;
                if local.node_id() == &profile.endpoint.expected_node {
                    return Err(MobileError::InvalidProfile {
                        detail: "Server and phone must have different node identities".into(),
                    });
                }
                route_plan(state.route_preference, &profile)?;
                let same_profile = state
                    .profile
                    .as_ref()
                    .is_some_and(|old| old.same_authority(&profile));
                if same_profile && !explicit_reconnect && state.active().is_ok() {
                    state.profile = Some(profile);
                    return Ok(());
                }
                state.stop("Connection is being restarted");
                if !same_profile {
                    state.sessions.clear();
                    state.detaching.clear();
                    state.submissions.clear();
                    state.profile_epoch = state.profile_epoch.wrapping_add(1);
                }
                let run = CancellationToken::new();
                state.run = Some(run.clone());
                state.profile = Some(profile.clone());
                state.phase = if explicit_reconnect {
                    ConnectionPhase::Reconnecting
                } else {
                    ConnectionPhase::Connecting
                };
                state.detail = None;
                state.attempt = 0;
                state.next_retry_at_ms = None;
                state.retryable_failure = false;
                (run, profile)
            };
            let (first_tx, first_rx) = oneshot::channel();
            tokio::spawn(drive_connection(
                shared, local, label, profile, run, first_tx,
            ));
            first_rx
                .await
                .map_err(|_| internal("Connection task stopped"))?
        })
    }

    fn run<T, F>(&self, operation: F) -> Result<T, MobileError>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, MobileError>> + Send + 'static,
    {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| internal("Runtime stopped"))?;
        if Handle::try_current().is_ok_and(|handle| handle.id() == runtime.handle().id()) {
            return Err(internal(
                "Call synchronous mobile methods from a platform thread",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        runtime.spawn(async move {
            let _ = sender.send(operation.await);
        });
        receiver
            .recv()
            .map_err(|_| internal("Background task stopped"))?
    }
}

async fn drive_connection(
    shared: Arc<Shared>,
    local: LocalNode,
    label: String,
    profile: ValidatedProfile,
    run: CancellationToken,
    first: oneshot::Sender<Result<(), MobileError>>,
) {
    let mut first = Some(first);
    let mut retry = 0_u32;
    let mut delay = Duration::ZERO;
    let mut entropy = [0_u8; 8];
    let _ = aws_lc_rs::rand::fill(&mut entropy);
    let entropy = u64::from_ne_bytes(entropy);
    loop {
        if !delay.is_zero() {
            {
                let mut state = shared.state();
                if run.is_cancelled() {
                    break;
                }
                state.attempt = retry;
                state.next_retry_at_ms =
                    Some(now_ms().saturating_add(delay.as_millis().min(u128::from(u64::MAX)) as u64));
            }
            let woken = tokio::select! {
                () = run.cancelled() => break,
                () = tokio::time::sleep(delay) => false,
                () = shared.wake.notified() => true,
            };
            if woken {
                retry = 0;
            }
            delay = Duration::ZERO;
            let mut state = shared.state();
            if run.is_cancelled() {
                break;
            }
            state.attempt = retry;
            state.next_retry_at_ms = None;
        } else if run.is_cancelled() {
            break;
        }
        let mut options = ConnectOptions::new(&label);
        options.requested_caps = Some(ClientCaps::OPERATE);
        options.features = requested_features();
        let plan = {
            let preference = shared.state().route_preference;
            route_plan(preference, &profile)
        };
        let routes = match plan {
            Ok(routes) => routes,
            Err(error) => {
                let detail = match error {
                    MobileError::InvalidProfile { detail } => detail,
                    other => other.to_string(),
                };
                let mut state = shared.state();
                if run.is_cancelled() {
                    break;
                }
                state.detail = Some(detail.clone());
                state.phase = ConnectionPhase::Failed;
                state.retryable_failure = false;
                state.next_retry_at_ms = None;
                if let Some(first) = first.take() {
                    let _ = first.send(Err(MobileError::Connect { detail }));
                }
                return;
            }
        };
        let mut failures: Vec<(ConnectionRoute, RemoteError)> = Vec::new();
        let mut connected = None;
        let mut cancelled = false;
        for route in routes {
            shared.state().route = Some(route);
            let attempt = run
                .run_until_cancelled(shared.dialer.connect_session(
                    route,
                    &profile,
                    &local,
                    options.clone(),
                ))
                .await;
            match attempt {
                Some(Ok(connection)) => {
                    connected = Some(connection);
                    break;
                }
                Some(Err(error)) => {
                    let stop = dial_failure(&error) == DialFailure::Final;
                    failures.push((route, error));
                    if stop {
                        break;
                    }
                }
                None => {
                    cancelled = true;
                    break;
                }
            }
        }
        if cancelled {
            break;
        }
        let connection = match connected {
            Some(connection) => connection,
            None => {
                let detail = failure_detail(&failures);
                let kind = attempt_failure(&failures);
                let mut state = shared.state();
                if run.is_cancelled() {
                    break;
                }
                state.detail = Some(detail.clone());
                if first.is_some() || kind != DialFailure::Retry || retry >= MAX_RECONNECT_ATTEMPTS
                {
                    state.phase = ConnectionPhase::Failed;
                    state.retryable_failure = kind == DialFailure::Retry;
                    state.next_retry_at_ms = None;
                    if let Some(first) = first.take() {
                        let _ = first.send(Err(MobileError::Connect { detail }));
                    }
                    return;
                }
                delay = retry_delay(retry, entropy);
                retry = retry.saturating_add(1);
                continue;
            }
        };
        let (failure, failure_rx) = watch::channel(None);
        let link = Arc::new(Link {
            port: RemotePort::new(connection),
            stop: run.child_token(),
            failure,
            // Bulk reattachment leaves request capacity for user actions.
            attaching: Semaphore::new(4),
        });
        {
            let mut state = shared.state();
            if run.is_cancelled() {
                break;
            }
            state.phase = ConnectionPhase::Connected;
            state.attempt = 0;
            state.next_retry_at_ms = None;
            state.retryable_failure = false;
            state.detail = fallback_note(&failures, state.route);
            state.link = Some(Arc::clone(&link));
            for (id, session) in &mut state.sessions {
                start_attachment(&shared, session, id.clone(), Arc::clone(&link));
            }
        }
        if let Some(first) = first.take() {
            let _ = first.send(Ok(()));
        }
        let Some(issue) = monitor_connection(&link, failure_rx, &run).await else {
            break;
        };
        drop(link);
        {
            let mut state = shared.state();
            if run.is_cancelled() {
                break;
            }
            if issue.disposition == LinkDisposition::Revoked {
                state.revoke(issue.detail);
                return;
            }
            state.clear_link(&issue.detail);
            state.detail = Some(issue.detail);
            if issue.disposition == LinkDisposition::Final {
                state.phase = ConnectionPhase::Failed;
                state.retryable_failure = false;
                state.next_retry_at_ms = None;
                return;
            }
            state.phase = ConnectionPhase::Reconnecting;
        }
        if let LinkDisposition::Retry(retry_after) = issue.disposition {
            delay = retry_delay(0, entropy).max(retry_after);
        }
        retry = 1;
    }
    if let Some(first) = first.take() {
        let _ = first.send(Err(MobileError::Disconnected));
    }
}

fn dial_detail(error: &RemoteError) -> String {
    match error {
        RemoteError::Connect(detail)
            if detail == "upgrade: peer did not switch protocols (status 403)" =>
        {
            "The server refused this device (status 403). Check the enrollment and server device registry.".into()
        }
        _ => error.to_string(),
    }
}

/// Joins every route failure of one dial attempt into a user-facing detail.
fn failure_detail(failures: &[(ConnectionRoute, RemoteError)]) -> String {
    failures
        .iter()
        .map(|(route, error)| format!("{}: {}", route_label(*route), dial_detail(error)))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Visible note for a link that only came up after earlier routes failed.
fn fallback_note(
    failures: &[(ConnectionRoute, RemoteError)],
    route: Option<ConnectionRoute>,
) -> Option<String> {
    let route = route?;
    if failures.is_empty() {
        return None;
    }
    let via = match route {
        ConnectionRoute::Direct => "directly".to_owned(),
        ConnectionRoute::Onion => format!("via {}", route_label(route)),
    };
    Some(format!(
        "{} unavailable, connected {via} ({})",
        failures
            .iter()
            .map(|(failed, _)| route_label(*failed))
            .collect::<Vec<_>>()
            .join(", "),
        failure_detail(failures)
    ))
}

/// Whether a dial attempt whose every route failed is worth retrying later.
fn attempt_failure(failures: &[(ConnectionRoute, RemoteError)]) -> DialFailure {
    let mut retry = false;
    for (_, error) in failures {
        match dial_failure(error) {
            DialFailure::Final => return DialFailure::Final,
            DialFailure::Retry => retry = true,
            DialFailure::Unavailable => {}
        }
    }
    if retry {
        DialFailure::Retry
    } else {
        DialFailure::Final
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DialFailure {
    /// Transient; a later attempt over this route may succeed.
    Retry,
    /// This route cannot work in the current setup; try the next route but
    /// do not schedule a retry for it.
    Unavailable,
    /// The server refused this device (node handshake, TLS configuration,
    /// hello denied, HTTP 4xx); stop trying further routes. Tor setup errors
    /// are never `Final` for a route: they are `Unavailable`, so Auto still
    /// falls back to the direct route.
    Final,
}

/// Tor setup failures that no retry can fix without user or build changes.
const TOR_SETUP_FAILURES: [&str; 5] = [
    "tor: Tor storage is not configured",
    "tor: tor configuration:",
    "tor: no rustls crypto provider",
    "tor: invalid onion locator:",
    "tor: this build's Tor",
];

fn dial_failure(error: &RemoteError) -> DialFailure {
    // The transport authenticates the host before upgrade. Its current
    // RemoteError flattens auth and HTTP failures into stable messages.
    let RemoteError::Connect(detail) = error else {
        return DialFailure::Final;
    };
    if TOR_SETUP_FAILURES
        .iter()
        .any(|prefix| detail.starts_with(prefix))
    {
        return DialFailure::Unavailable;
    }
    // An upgrade refusal is terminal for this dial. HTTP 403 can also
    // represent a missing/invalid registry; it is not a revocation signal.
    if detail.starts_with("node transport: node handshake:")
        || detail.starts_with("node transport: tls configuration:")
        || detail == "session.hello failed: session access revoked"
        || detail.starts_with("session.hello failed: session call denied:")
        || detail.starts_with("upgrade: peer did not switch protocols (status 4")
            && !detail.ends_with("429)")
    {
        DialFailure::Final
    } else {
        DialFailure::Retry
    }
}

fn revoked_close(detail: &str) -> bool {
    // RemoteConnection currently formats the authenticated close frame as
    // Debug. Inspect the code field only, never the peer-supplied reason.
    detail.starts_with("host closed the connection: Close(Some(CloseFrame { code: Library(4004),")
}

async fn monitor_connection(
    link: &Link,
    mut failure: watch::Receiver<Option<LinkIssue>>,
    run: &CancellationToken,
) -> Option<LinkIssue> {
    loop {
        tokio::select! {
            biased;
            () = run.cancelled() => return None,
            changed = failure.changed() => {
                changed.ok()?;
                if let Some(issue) = failure.borrow_and_update().clone() {
                    return Some(issue);
                }
            }
            () = tokio::time::sleep(PROBE_INTERVAL) => {
                // The existing RemotePort owns the socket, so a read-only
                // probe also detects disconnects with no session attached.
                if let Err(MobileError::Session { detail }) = link.request(link.port.list()).await {
                    link.fail(LinkIssue::protocol(detail));
                }
            }
        }
    }
}

fn retry_delay(attempt: u32, entropy: u64) -> Duration {
    BackoffPolicy::default()
        .delay(attempt, entropy.rotate_left(attempt % 64))
        .unwrap_or(Duration::from_secs(30))
}

/// Features the phone asks the host for in `session.hello`.
fn requested_features() -> Vec<String> {
    [
        features::COMPACT,
        features::CHILD_FRAMES,
        features::COMPACT_REASONING,
        features::JOB_FRAMES,
    ]
    .iter()
    .map(|feature| (*feature).to_owned())
    .collect()
}

/// Profile for `session.attach`: reasoning-aware only when the host granted it.
/// An older host cannot decode the new profile, so fall back to `Compact`.
fn attach_profile(granted: &[String]) -> StreamProfile {
    if granted
        .iter()
        .any(|feature| feature == features::COMPACT_REASONING)
    {
        StreamProfile::CompactReasoning
    } else {
        StreamProfile::Compact
    }
}

fn start_attachment(
    shared: &Arc<Shared>,
    session: &mut SessionState,
    id: SessionId,
    link: Arc<Link>,
) {
    session.begin_attach("Attaching to session");
    let from = session.resume(link.port.connection().hello_ack().host_epoch);
    let stop = link.stop.child_token();
    let task = tokio::spawn(drive_attachment(
        Arc::clone(shared),
        id,
        link,
        stop.clone(),
        from,
    ));
    session.worker = Some(SessionWorker { stop, task });
}

async fn drive_attachment(
    shared: Arc<Shared>,
    id: SessionId,
    link: Arc<Link>,
    stop: CancellationToken,
    mut from: Option<Cursor>,
) {
    let mut stalled = 0_u8;
    while !stop.is_cancelled() {
        let Some(Ok(permit)) = stop.run_until_cancelled(link.attaching.acquire()).await else {
            return;
        };
        let params = AttachParams {
            session_id: id.clone(),
            // Replay the observed durable suffix. Rebuild the partial answer
            // from the live ring or its snapshot: compact replay can merge an
            // already seen delta and a new delta into the same envelope.
            from: from.map(|cursor| Cursor { live: 0, ..cursor }),
            profile: attach_profile(&link.port.connection().hello_ack().features),
            tail_items: DEFAULT_TAIL_ITEMS,
        };
        let (mut ack, mut source) = match link.request(link.port.attach(params)).await {
            Ok(attachment) => attachment,
            Err(error) => {
                end_attachment(&shared, &id, &link, &error.to_string());
                return;
            }
        };
        drop(permit);
        ack.granted = ack
            .granted
            .intersect(link.port.connection().hello_ack().granted);
        if with_session(&shared, &id, &link, |session| {
            session.attached(&ack, from.is_none())
        })
        .is_none()
        {
            return;
        }
        let (next_from, refreshed) = loop {
            let envelope = match stop.run_until_cancelled(source.next()).await {
                Some(Ok(Some(envelope))) => envelope,
                Some(Err(PortError::Transport(detail))) if revoked_close(&detail) => {
                    link.fail(LinkIssue::revoked());
                    return;
                }
                Some(Err(PortError::Transport(detail))) => {
                    link.fail(LinkIssue::transport(detail));
                    return;
                }
                Some(Err(PortError::Revoked)) => {
                    link.fail(LinkIssue::revoked());
                    return;
                }
                Some(Err(PortError::Protocol(detail))) => {
                    link.fail(LinkIssue::protocol(detail));
                    return;
                }
                Some(Err(error)) => {
                    end_attachment(&shared, &id, &link, &error.to_string());
                    return;
                }
                Some(Ok(None)) => {
                    end_attachment(&shared, &id, &link, "The server ended this attachment");
                    return;
                }
                None => return,
            };
            let Some(action) =
                with_session(&shared, &id, &link, |session| session.observe(&envelope))
            else {
                return;
            };
            match action {
                FrameAction::Continue => {}
                FrameAction::Reattach(cursor) => break (cursor, false),
                FrameAction::RefreshTranscript => break (None, true),
                FrameAction::Revoked => {
                    link.fail(LinkIssue::revoked());
                    return;
                }
                FrameAction::Draining(retry_after_ms) => {
                    link.fail(LinkIssue {
                        detail: "Server is draining connections".into(),
                        disposition: LinkDisposition::Retry(Duration::from_millis(
                            retry_after_ms.min(300_000),
                        )),
                    });
                    return;
                }
                FrameAction::Closed => {
                    end_attachment(&shared, &id, &link, "Session closed");
                    return;
                }
            }
        };
        stalled = if !refreshed && from == next_from {
            stalled.saturating_add(1)
        } else {
            0
        };
        if stalled >= 3 {
            end_attachment(
                &shared,
                &id,
                &link,
                "The server could not complete session replay",
            );
            return;
        }
        if let Err(error) = link.request(link.port.detach(id.clone())).await {
            end_attachment(&shared, &id, &link, &error.to_string());
            return;
        }
        from = next_from;
    }
}

fn with_session<T>(
    shared: &Shared,
    id: &SessionId,
    link: &Arc<Link>,
    operation: impl FnOnce(&mut SessionState) -> T,
) -> Option<T> {
    let mut state = shared.state();
    if !state.same_link(link) {
        return None;
    }
    state.sessions.get_mut(id).map(operation)
}

fn end_attachment(shared: &Shared, id: &SessionId, link: &Arc<Link>, detail: &str) {
    with_session(shared, id, link, |session| session.worker_ended(detail));
}

async fn submit(
    shared: Arc<Shared>,
    id: SessionId,
    text: String,
    displayed: Cursor,
    client_message_id: String,
) -> Result<SubmitReceipt, MobileError> {
    let submission_id = (id.clone(), client_message_id.clone());
    let fingerprint = blake3::hash(text.as_bytes());
    let (link, head, profile_epoch) = {
        let mut state = shared.state();
        let link = state.active()?;
        if let Some(previous) = state.submissions.get(&submission_id) {
            if fingerprint != previous.fingerprint {
                return Err(session_error(
                    "The submission ID is already associated with a different draft",
                ));
            }
            return match &previous.state {
                SubmissionState::InFlight => Ok(SubmitReceipt::new(SubmitOutcome::InFlight)),
                SubmissionState::Accepted(receipt) => Ok(receipt.clone()),
                SubmissionState::Uncertain => Err(ambiguous()),
            };
        }
        let head = match state.session(&id)?.current_submission_head(displayed)? {
            Ok(head) => head,
            Err(head) => return Ok(stale(head)),
        };
        if state.submissions.len() >= MAX_PROCESS_SUBMISSIONS {
            return Err(session_error(
                "This process has reached its submission limit",
            ));
        }
        state.submissions.insert(
            submission_id.clone(),
            Submission {
                fingerprint,
                state: SubmissionState::InFlight,
            },
        );
        (link, head, state.profile_epoch)
    };
    let result = link
        .request(link.port.submit(SubmitParams {
            session_id: id.clone(),
            text: text.clone(),
            expect_head: head,
            client_msg_id: client_message_id.clone(),
            force: false,
        }))
        .await;
    let result = match result {
        Ok(SubmitResult::Accepted { position }) => Ok(SubmitReceipt {
            position: Some(position),
            ..SubmitReceipt::new(SubmitOutcome::Accepted)
        }),
        Ok(SubmitResult::Stale { head }) => Ok(stale(head)),
        Ok(SubmitResult::QueueFull) => Ok(SubmitReceipt::new(SubmitOutcome::QueueFull)),
        Ok(SubmitResult::Denied { reason }) => Ok(SubmitReceipt {
            detail: Some(reason),
            ..SubmitReceipt::new(SubmitOutcome::Denied)
        }),
        Err(error @ (MobileError::Denied { .. } | MobileError::Revoked)) => Err(error),
        Err(_) => Err(ambiguous()),
    };
    {
        let mut state = shared.state();
        if state.profile_epoch == profile_epoch {
            if result
                .as_ref()
                .is_ok_and(|receipt| receipt.outcome == SubmitOutcome::Accepted)
                && state.same_link(&link)
                && let Ok(session) = state.session_mut(&id)
            {
                session.echo_prompt(&id, displayed, &client_message_id, text);
            }
            let keep = match &result {
                Ok(receipt) if receipt.outcome == SubmitOutcome::Accepted => {
                    Some(SubmissionState::Accepted(receipt.clone()))
                }
                Err(MobileError::Ambiguous { .. }) => Some(SubmissionState::Uncertain),
                _ => None,
            };
            if let Some(keep) = keep {
                if let Some(submission) = state.submissions.get_mut(&submission_id) {
                    submission.state = keep;
                }
            } else {
                state.submissions.remove(&submission_id);
            }
        }
    }
    result
}

fn stale(head: Cursor) -> SubmitReceipt {
    SubmitReceipt {
        head: Some(head.into()),
        ..SubmitReceipt::new(SubmitOutcome::Stale)
    }
}

fn validate_submission(text: &str, id: &str) -> Result<(), MobileError> {
    if text.trim().is_empty() || text.len() > MAX_SUBMISSION_BYTES {
        return Err(session_error(
            "Prompt must contain text and fit within 256 KiB",
        ));
    }
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(session_error("Invalid client message ID"));
    }
    Ok(())
}

fn parse_session_id(id: String) -> Result<SessionId, MobileError> {
    SessionId::try_from_str(id).map_err(|_| session_error("Invalid session ID"))
}

fn internal(detail: impl Into<String>) -> MobileError {
    MobileError::Internal {
        detail: detail.into(),
    }
}

fn session_error(detail: impl Into<String>) -> MobileError {
    MobileError::Session {
        detail: detail.into(),
    }
}

fn unopened() -> MobileError {
    session_error("Open this session before requesting its state")
}

fn ambiguous() -> MobileError {
    MobileError::Ambiguous {
        detail: "Check the session transcript before sending the draft again".into(),
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn attach_profile_follows_the_granted_features() {
        assert!(matches!(attach_profile(&[]), StreamProfile::Compact));
        assert!(matches!(
            attach_profile(&["compact".into(), "child_frames".into()]),
            StreamProfile::Compact
        ));
        assert!(matches!(
            attach_profile(&["compact_reasoning".into()]),
            StreamProfile::CompactReasoning
        ));
        let requested = requested_features();
        assert!(
            requested
                .iter()
                .any(|feature| feature == "compact_reasoning")
        );
        assert!(requested.iter().any(|feature| feature == "job_frames"));
    }
}

#[cfg(test)]
mod route_failure_tests {
    use super::*;

    fn connect(detail: &str) -> RemoteError {
        RemoteError::Connect(detail.into())
    }

    #[test]
    fn tor_setup_errors_skip_to_the_next_route_without_retrying() {
        for detail in [
            "tor: Tor storage is not configured",
            "tor: tor configuration: bad state dir",
            "tor: this build's Tor (arti 0.48) is too old for the current Tor network; update harw",
        ] {
            assert_eq!(dial_failure(&connect(detail)), DialFailure::Unavailable);
        }
        assert_eq!(
            dial_failure(&connect("tor: onion connect did not finish within 120s")),
            DialFailure::Retry
        );
        let unconfigured = || connect("tor: Tor storage is not configured");
        let onion_only = [(ConnectionRoute::Onion, unconfigured())];
        assert_eq!(attempt_failure(&onion_only), DialFailure::Final);
        let auto = [
            (ConnectionRoute::Onion, unconfigured()),
            (ConnectionRoute::Direct, connect("tcp connect: refused")),
        ];
        assert_eq!(attempt_failure(&auto), DialFailure::Retry);
        let refused = [
            (
                ConnectionRoute::Onion,
                connect("tor: tor bootstrap failed: x"),
            ),
            (
                ConnectionRoute::Direct,
                connect("upgrade: peer did not switch protocols (status 403)"),
            ),
        ];
        assert_eq!(attempt_failure(&refused), DialFailure::Final);
        assert_eq!(attempt_failure(&[]), DialFailure::Final);
    }

    #[test]
    fn direct_fallback_keeps_a_visible_note() {
        let failures = [(
            ConnectionRoute::Onion,
            connect("tor: Tor storage is not configured"),
        )];
        let text = fallback_note(&failures, Some(ConnectionRoute::Direct)).unwrap_or_default();
        assert!(text.starts_with("Tor (.onion) unavailable, connected directly (Tor (.onion): "));
        assert!(text.contains("Tor storage is not configured"));
        assert_eq!(fallback_note(&[], Some(ConnectionRoute::Direct)), None);
        assert_eq!(fallback_note(&failures, None), None);
    }

    #[test]
    fn network_action_ignores_revoked() {
        for run in [false, true] {
            for retryable in [false, true] {
                assert_eq!(
                    network_action(ConnectionPhase::Revoked, run, retryable),
                    NetworkAction::Ignore
                );
            }
        }
    }

    #[test]
    fn network_action_ignores_disconnected_and_connected() {
        for phase in [ConnectionPhase::Disconnected, ConnectionPhase::Connected] {
            for run in [false, true] {
                for retryable in [false, true] {
                    assert_eq!(network_action(phase, run, retryable), NetworkAction::Ignore);
                }
            }
        }
    }

    #[test]
    fn network_action_wakes_backoff_while_dialing() {
        for phase in [ConnectionPhase::Connecting, ConnectionPhase::Reconnecting] {
            for retryable in [false, true] {
                assert_eq!(network_action(phase, true, retryable), NetworkAction::WakeBackoff);
            }
        }
    }

    #[test]
    fn network_action_ignores_dialing_without_run() {
        for phase in [ConnectionPhase::Connecting, ConnectionPhase::Reconnecting] {
            for retryable in [false, true] {
                assert_eq!(network_action(phase, false, retryable), NetworkAction::Ignore);
            }
        }
    }

    #[test]
    fn network_action_restarts_retryable_failure() {
        for run in [false, true] {
            assert_eq!(
                network_action(ConnectionPhase::Failed, run, true),
                NetworkAction::Restart
            );
        }
    }

    #[test]
    fn network_action_ignores_final_failure() {
        for run in [false, true] {
            assert_eq!(
                network_action(ConnectionPhase::Failed, run, false),
                NetworkAction::Ignore
            );
        }
    }

    #[test]
    fn max_reconnect_attempts_unchanged() {
        assert_eq!(MAX_RECONNECT_ATTEMPTS, 8);
    }
}
