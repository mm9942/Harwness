use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use std::time::Duration;

use harw_node_transport::{LocalNode, NodeBody, empty_body};
use harw_protocol::session_wire::{
    ApprovalRespondParams, ArchiveParams, AttachParams, ClientCaps, CreateInParams, CreateParams,
    Cursor, DEFAULT_TAIL_ITEMS, ForkFrom, HolderInfo, HostedState, InterruptParams, MediaGetParams,
    PageParams, ProjectsParams, RenameParams, RespondResult, SessionFrame, SetModelParams,
    SetProjectParams, StreamProfile, SubmitImage, SubmitParams, SubmitResult, features,
};
use harw_protocol::{PortError, PortFuture, SessionPort, TurnEvent};
use harw_session_remote::{BackoffPolicy, ConnectOptions, RemoteError};
use harw_types::{ApprovalId, SessionId, TurnId};
use http::Method;
use tokio::runtime::{Builder, Handle, Runtime};
use tokio::sync::{Semaphore, oneshot, watch};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::backend::{Backend, BackendKind};
use crate::diagnostics::{BaseLevel, DiagnosticLog, DiagnosticSnapshot, Domain, Level, OnionFailureStage, StreamSignal};
use crate::identity::{ValidatedProfile, restore_identity};
use crate::listener::{CoreListener, Notifier};
use crate::local::{LOCAL_HOST_UNAVAILABLE, open_local};
use crate::projection::{FrameAction, SessionState, SessionWorker};
use crate::provider::request_provider;
use crate::{
    ApprovalDecision, ApprovalOutcome, ApprovalReceipt, ChatgptProviderCredential,
    ChatgptProviderStatus, ConnectionPhase, ConnectionRoute, ConnectionStatus, DeviceEnrollment,
    HostKind, HostRef, ManageSupport, MobileError, MobileHolder, MobileProject, MobileProjectPage,
    MobileRowOrigin, MobileSession, MobileSessionPage, MobileSessionRow, MobileShareInfo,
    MobileShareMode, MobileTier, PlatformKeystore, PromptImage, RoutePreference, ServerProfile,
    SessionCursor, SessionSnapshot, SubmitOutcome, SubmitReceipt,
};

pub(crate) mod archive;
pub(crate) mod media_preview;
pub(crate) mod cloud_library;
pub(crate) mod manual_cloud_work;
pub(crate) mod route;
mod owner_status;
pub(crate) mod cloud_assistant;
pub(crate) mod auto_title;
pub(crate) mod local_assistant_sessions;

use route::{Dialer, available_routes, route_label, route_plan};

const MAX_ATTACHMENTS: usize = 32;
/// Upper bound for remembered lease holders (session ids only, no paths or titles).
const MAX_HOLDERS: usize = 4096;
/// Shown when the host predates the `projects` feature (wire minor 2).
const NO_PROJECTS_DETAIL: &str = "Server kennt keine Projekte";
/// Label of the pseudo project that collects sessions without a project.
const NO_PROJECT_LABEL: &str = "Ohne Projekt";
const MAX_SUBMISSION_BYTES: usize = 256 * 1024;
const MAX_PROCESS_SUBMISSIONS: usize = 8192;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_INTERVAL: Duration = Duration::from_secs(10);
const MAX_RECONNECT_ATTEMPTS: u32 = 8;
/// Two additional bounded attempts after a transient initial Onion failure.
const MAX_INITIAL_ONION_RETRIES: u32 = 2;
/// Polls of the previous on-device host while it stops (5 s in total).
const LOCAL_STOP_POLLS: u32 = 100;
const LOCAL_STOP_POLL: Duration = Duration::from_millis(50);

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
    diagnostics: DiagnosticLog,
    dialer: Dialer,
    /// Ends a running backoff pause early when the network returns.
    wake: tokio::sync::Notify,
    /// Pushes change signals to the platform listener.
    notifier: Notifier,
    /// Platform shell and file access for the on-device agent's device tools.
    device: Mutex<DeviceHandles>,
    #[cfg(feature = "onion")]
    owner_status_active: std::sync::OnceLock<Arc<Semaphore>>,
}

/// Platform implementations registered by Kotlin before the on-device host starts.
#[derive(Default)]
struct DeviceHandles {
    shell: Option<Arc<dyn crate::ShellExecutor>>,
    files: Option<Arc<dyn crate::FileAccess>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NetworkAction {
    Ignore,
    WakeBackoff,
    Restart,
}

fn network_action(
    phase: ConnectionPhase,
    run_active: bool,
    retryable_failure: bool,
) -> NetworkAction {
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
        .map_or(0, |elapsed| {
            elapsed.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

impl Shared {
    /// The registered device-tool handles, for the on-device host assembly.
    fn device_tools(&self) -> crate::device_tools::DeviceHandles {
        let handles = self.device.lock().unwrap_or_else(PoisonError::into_inner);
        (handles.shell.clone(), handles.files.clone())
    }

    fn state(&self) -> MutexGuard<'_, CoreState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Signals a connection change: status plus every open session.
    fn notify_link(&self, state: &CoreState) {
        self.notifier.status();
        for id in state.sessions.keys() {
            self.notifier.session(id.as_str());
        }
    }
}

struct CoreState {
    profile: Option<ValidatedProfile>,
    profile_epoch: u64,
    #[cfg(feature = "onion")]
    owner_status_scope: CancellationToken,
    phase: ConnectionPhase,
    detail: Option<String>,
    run: Option<CancellationToken>,
    link: Option<Arc<Link>>,
    sessions: HashMap<SessionId, SessionState>,
    /// Last known lease holder per session, from `holder` frames and pages.
    holders: HashMap<SessionId, Option<HolderInfo>>,
    detaching: HashSet<SessionId>,
    submissions: HashMap<(SessionId, String), Submission>,
    route_consent: Option<(ValidatedProfile, RoutePreference)>,
    route_policy_epoch: u64,
    route: Option<ConnectionRoute>,
    attempt: u32,
    next_retry_at_ms: Option<u64>,
    retryable_failure: bool,
    /// The last on-device link; it must be gone before a new one opens.
    last_local: Weak<Link>,
}

impl Default for CoreState {
    fn default() -> Self {
        Self {
            profile: None,
            profile_epoch: 0,
            #[cfg(feature = "onion")]
            owner_status_scope: CancellationToken::new(),
            phase: ConnectionPhase::Disconnected,
            detail: None,
            run: None,
            link: None,
            sessions: HashMap::new(),
            holders: HashMap::new(),
            detaching: HashSet::new(),
            submissions: HashMap::new(),
            route_consent: None,
            route_policy_epoch: 0,
            route: None,
            attempt: 0,
            next_retry_at_ms: None,
            retryable_failure: false,
            last_local: Weak::new(),
        }
    }
}

impl CoreState {
    fn preference_for(&self, profile: &ValidatedProfile) -> RoutePreference {
        #[cfg(all(feature = "android-vpn", unix))]
        if crate::android_vpn_jni::hub_is_held() {
            return RoutePreference::Onion;
        }

        self.route_consent.as_ref()
            .filter(|(approved, _)| approved.same_route_authority(profile))
            .map_or(RoutePreference::Onion, |(_, preference)| *preference)
    }

    fn active_preference(&self) -> RoutePreference {
        self.profile.as_ref().map_or(RoutePreference::Onion, |profile| self.preference_for(profile))
    }

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
        self.holders.clear();
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

    /// Applies a revocation the link reported; returns whether it did.
    fn apply_pending_revocation(&mut self) -> bool {
        let revocation = self.link.as_ref().and_then(|link| {
            link.failure
                .borrow()
                .as_ref()
                .filter(|issue| issue.disposition == LinkDisposition::Revoked)
                .map(|issue| issue.detail.clone())
        });
        if let Some(detail) = revocation {
            self.revoke(detail);
            true
        } else {
            false
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
    backend: Backend,
    stop: CancellationToken,
    failure: watch::Sender<Option<LinkIssue>>,
    attaching: Semaphore,
}

impl Link {
    async fn request<T>(
        &self,
        request: impl Future<Output = Result<T, PortError>>,
    ) -> Result<T, MobileError> {
        self.request_with_timeout(REQUEST_TIMEOUT, request).await
    }

    async fn request_with_timeout<T>(
        &self,
        timeout: Duration,
        request: impl Future<Output = Result<T, PortError>>,
    ) -> Result<T, MobileError> {
        self.request_with_revocation_policy(timeout, request, true).await
    }

    async fn request_with_revocation_policy<T>(
        &self,
        timeout: Duration,
        request: impl Future<Output = Result<T, PortError>>,
        revoke_link: bool,
    ) -> Result<T, MobileError> {
        let outcome = tokio::select! {
            biased;
            () = self.stop.cancelled() => return Err(MobileError::Disconnected),
            result = tokio::time::timeout(timeout, request) => result,
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
            // A room-scoped membership denial does not revoke gateway login.
            // Transport-confirmed revocation still takes the branch above.
            PortError::Revoked if revoke_link => self.fail(LinkIssue::revoked()),
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

    pub fn diagnostics_snapshot(&self) -> DiagnosticSnapshot {
        self.shared.diagnostics.snapshot()
    }

    pub fn diagnostics_clear(&self) {
        self.shared.diagnostics.clear();
    }

    pub fn diagnostics_level(&self, domain: Domain) -> Level {
        self.shared.diagnostics.level(domain)
    }

    pub fn diagnostics_set_level(&self, domain: Domain, level: BaseLevel) {
        self.shared.diagnostics.set_level(domain, level);
    }

    pub fn diagnostics_enable_trace(&self, domain: Domain) {
        self.shared.diagnostics.enable_trace(domain);
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
        self.start_connection(Some(ValidatedProfile::new(profile)?), None)
    }

    pub fn reconnect(&self) -> Result<(), MobileError> {
        self.start_connection(None, None)
    }

    pub fn disconnect(&self) {
        let mut state = self.shared.state();
        if state.apply_pending_revocation() {
            self.shared.notifier.status();
            self.shared.notifier.sessions();
        }
        if state.phase == ConnectionPhase::Revoked {
            return;
        }
        state.stop("Disconnected. This view is read-only");
        #[cfg(all(feature = "android-vpn", feature = "onion", unix))]
        if crate::android_vpn_jni::hub_is_held() {
            self.shared.dialer.retire_ordinary_tor();
        }

        state.phase = ConnectionPhase::Disconnected;
        state.detail = None;
        state.next_retry_at_ms = None;
        state.retryable_failure = false;
        self.shared.notify_link(&state);
    }

    /// Signals that the platform reports a usable network again.
    /// Never resends mutating calls; never lifts a revocation.
    pub fn network_available(&self) -> Result<(), MobileError> {
        let (action, route_epoch) = {
            let mut state = self.shared.state();
            let revoked = state.apply_pending_revocation();
            let action = network_action(
                state.phase,
                state.run.as_ref().is_some_and(|run| !run.is_cancelled()),
                state.retryable_failure,
            );
            if action == NetworkAction::WakeBackoff {
                state.attempt = 0;
                state.next_retry_at_ms = None;
            }
            if revoked || action != NetworkAction::Ignore {
                self.shared.notifier.status();
            }
            (action, state.route_policy_epoch)
        };
        match action {
            NetworkAction::Ignore => Ok(()),
            NetworkAction::WakeBackoff => {
                self.shared.wake.notify_one();
                Ok(())
            }
            NetworkAction::Restart => self.start_connection(None, Some(route_epoch)),
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

    /// Compatibility entry point. A caller with no exact profile can only
    /// remove route permission; it cannot authorize a direct connection.
    pub fn set_route_preference(&self, _preference: RoutePreference) {
        self.replace_route_consent(None);
    }

    /// Set policy for one exact profile (server key, node and locators). Direct
    /// modes require explicit caller consent; profiles cannot confer it.
    pub fn set_profile_route_preference(
        &self, profile: ServerProfile, preference: RoutePreference,
    ) -> Result<(), MobileError> {
        let profile = ValidatedProfile::new(profile)?;
        if preference != RoutePreference::Onion {
            route_plan(preference, &profile)?;
        }
        self.replace_route_consent(Some((profile, preference)));
        Ok(())
    }

    pub fn status(&self) -> ConnectionStatus {
        // Read Tor progress before the state lock: the Tor client lock can be
        // held while Tor starts, which must never stall state operations.
        let tor = self.shared.dialer.tor_status();
        let mut state = self.shared.state();
        if state.apply_pending_revocation() {
            self.shared.notifier.status();
            self.shared.notifier.sessions();
        }
        let profile = state.profile.as_ref().map(|profile| &profile.profile);
        let active_caps = state.active().ok().map(|link| link.backend.granted());
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
            endpoint: profile.and_then(|profile| (!profile.endpoint.is_empty()).then(|| profile.endpoint.clone())),
            label: profile.map(|profile| profile.label.clone()),
            detail: state.detail.clone(),
            can_create: active_caps.is_some_and(|caps| caps.steer),
            can_submit: active_caps.is_some_and(|caps| caps.steer),
            can_approve: active_caps.is_some_and(|caps| caps.approve),
            route,
            route_preference: state.active_preference(),
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
                .request(link.backend.port().list())
                .await?
                .into_iter()
                .map(Into::into)
                .collect())
        })
    }

    /// True when the connected host serves the `projects` feature (wire minor 3).
    pub fn existing_project_assignment_supported(&self) -> bool {
        self.feature_granted(features::EXISTING_PROJECT_ASSIGNMENT_V1)
    }

    pub fn projects_supported(&self) -> bool {
        self.shared
            .state()
            .active()
            .is_ok_and(|link| link_supports_projects(&link))
    }

    pub fn list_projects(
        &self,
        query: Option<String>,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Result<MobileProjectPage, MobileError> {
        let link = self.shared.state().active()?;
        self.run(async move {
            if !link_supports_projects(&link) {
                let sessions: Vec<MobileSession> = link
                    .request(link.backend.port().list())
                    .await?
                    .into_iter()
                    .map(Into::into)
                    .collect();
                return Ok(legacy_project_page(&sessions));
            }
            let result = link
                .request(link.backend.port().projects(ProjectsParams {
                    query,
                    limit,
                    cursor,
                }))
                .await?;
            Ok(MobileProjectPage {
                projects: result
                    .projects
                    .into_iter()
                    .map(MobileProject::from)
                    .collect(),
                next_cursor: result.next_cursor,
                share: result.share.into(),
                legacy: false,
            })
        })
    }

    /// Whether the current scope offers redacted Dream metadata.
    pub fn dream_session_rows_supported(&self) -> bool {
        self.feature_granted(features::SESSION_DREAM_ROWS_V1)
    }

    /// Whether the active profile offers the dedicated internal session page.
    pub fn system_session_rows_supported(&self) -> bool {
        self.feature_granted(features::SESSION_SYSTEM_ROWS_V1)
    }

    #[uniffi::method(default(include_dreams = false, include_system = false))]
    pub fn list_sessions_page(
        &self,
        project_key: Option<String>,
        query: Option<String>,
        include_archived: bool,
        cursor: Option<String>,
        limit: Option<u32>,
        include_dreams: bool,
        include_system: bool,
    ) -> Result<MobileSessionPage, MobileError> {
        if include_system && (project_key.is_some() || include_dreams) {
            return Err(MobileError::Denied { detail: "Invalid System session page scope".into() });
        }
        let link = self.shared.state().active()?;
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            if include_system && (!link_supports_projects(&link)
                || !link.backend.features().iter().any(|name| name == features::SESSION_SYSTEM_ROWS_V1))
            {
                return Err(MobileError::Denied { detail: "System session metadata is not supported".into() });
            }
            if include_dreams && !link.backend.features().iter().any(|name| name == features::SESSION_DREAM_ROWS_V1) {
                return Err(MobileError::Denied { detail: "Dream session metadata is not supported".into() });
            }
            if !link_supports_projects(&link) {
                let steer = link.backend.granted().steer;
                let sessions: Vec<MobileSession> = link
                    .request(link.backend.port().list())
                    .await?
                    .into_iter()
                    .map(Into::into)
                    .collect();
                return Ok(legacy_session_page(sessions, project_key.as_deref(), steer));
            }
            let result = link
                .request(link.backend.port().page(PageParams {
                    project_key,
                    query,
                    include_archived,
                    include_dreams,
                    include_system,
                    limit,
                    cursor,
                }))
                .await?;
            {
                let mut state = shared.state();
                if state.same_link(&link) {
                    if !include_system {
                        for row in &result.sessions {
                            remember_holder(&mut state, row.session_id.clone(), row.holder.clone());
                        }
                    }
                }
            }
            Ok(MobileSessionPage {
                sessions: result
                    .sessions
                    .into_iter()
                    .map(MobileSessionRow::from)
                    .collect(),
                next_cursor: result.next_cursor,
                legacy: false,
            })
        })
    }

    pub fn adopt_session(&self, session_id: String) -> Result<MobileSession, MobileError> {
        let id = parse_session_id(session_id)?;
        let link = self.projects_link()?;
        self.run(async move {
            link.request(link.backend.port().adopt(id))
                .await
                .map(Into::into)
        })
    }

    pub fn create_in_project(
        &self,
        project_key: String,
        title: Option<String>,
    ) -> Result<MobileSession, MobileError> {
        let link = self.projects_link()?;
        self.run(async move {
            link.request(
                link.backend
                    .port()
                    .create_in(CreateInParams { project_key, title }),
            )
            .await
            .map(Into::into)
        })
    }

    pub fn set_project(
        &self,
        session_id: String,
        project_key: String,
    ) -> Result<MobileSessionRow, MobileError> {
        let session_id = parse_session_id(session_id)?;
        let link = self.projects_link()?;
        self.run(async move {
            link.request(link.backend.port().set_project(SetProjectParams {
                session_id,
                project_key,
            }))
            .await
            .map(Into::into)
        })
    }

    pub fn rename_session(
        &self,
        session_id: String,
        title: String,
    ) -> Result<MobileSessionRow, MobileError> {
        let session_id = parse_session_id(session_id)?;
        let link = self.projects_link()?;
        self.run(async move {
            link.request(
                link.backend
                    .port()
                    .rename(RenameParams { session_id, title }),
            )
            .await
            .map(Into::into)
        })
    }

    /// `session.set_model`: `model` is `<provider>/<modell>`; applies from the next message of the session.
    pub fn set_session_model(&self, session_id: String, model: String) -> Result<(), MobileError> {
        let session_id = parse_session_id(session_id)?;
        let link = self.shared.state().active()?;
        self.run(async move {
            link.request(
                link.backend
                    .port()
                    .set_model(SetModelParams { session_id, model }),
            )
            .await
        })
    }

    pub fn archive_session(
        &self,
        session_id: String,
        archived: bool,
    ) -> Result<MobileSessionRow, MobileError> {
        let session_id = parse_session_id(session_id)?;
        let link = self.projects_link()?;
        self.run(async move {
            link.request(link.backend.port().archive(ArchiveParams {
                session_id,
                archived,
            }))
            .await
            .map(Into::into)
        })
    }

    /// Last known lease holder from `holder` frames or the last page. No network call.
    pub fn session_holder(&self, session_id: String) -> Result<Option<MobileHolder>, MobileError> {
        let id = parse_session_id(session_id)?;
        Ok(self
            .shared
            .state()
            .holders
            .get(&id)
            .cloned()
            .flatten()
            .map(MobileHolder::from))
    }

    pub fn create_session(&self, title: Option<String>) -> Result<MobileSession, MobileError> {
        let link = self.shared.state().active()?;
        if !link.backend.granted().steer {
            return Err(MobileError::Denied {
                detail: "This device cannot create sessions".into(),
            });
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let params = CreateParams {
                workspace: None,
                title,
                fork_from: None,
            };
            let created = link.request(link.backend.port().create(params)).await?;
            shared.notifier.sessions();
            Ok(created.into())
        })
    }

    /// Legt eine neue Session an, die den Verlauf der Quelle bis vor der User-Nachricht `before_item` übernimmt (Fork).
    pub fn fork_session(
        &self,
        source_session_id: String,
        before_item: String,
        title: Option<String>,
    ) -> Result<MobileSession, MobileError> {
        let source = parse_session_id(source_session_id)?;
        let link = self.shared.state().active()?;
        if !link.backend.granted().steer {
            return Err(MobileError::Denied {
                detail: "This device cannot create sessions".into(),
            });
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let params = CreateParams {
                workspace: None,
                title,
                fork_from: Some(ForkFrom {
                    session_id: source,
                    before_item,
                    displayed: None,
                }),
            };
            let created = link.request(link.backend.port().create(params)).await?;
            shared.notifier.sessions();
            Ok(created.into())
        })
    }

    /// Creates a new session from the exact conversational prefix the attached UI drew.
    /// Over the wire only send reference IDs and hashes, never client-supplied import content.
    pub fn fork_displayed_session(
        &self,
        source_session_id: String,
        expected: SessionCursor,
        displayed: Vec<crate::TranscriptEntry>,
    ) -> Result<MobileSession, MobileError> {
        use harw_protocol::session_wire::{DISPLAYED_FORK_MAX_ROWS, DisplayedForkV1};
        let source = parse_session_id(source_session_id)?;
        let head: Cursor = expected.into();
        let stale = || MobileError::Denied {
            detail: "Displayed chat changed; refresh it and try Fork again".into(),
        };
        if !(1..=DISPLAYED_FORK_MAX_ROWS).contains(&displayed.len()) {
            return Err(MobileError::Denied {
                detail: "The entire displayed prefix exceeds the fork limit; nothing was copied".into(),
            });
        }
        let link = {
            let state = self.shared.state();
            let link = state.active()?;
            if !link.backend.granted().steer {
                return Err(MobileError::Denied { detail: "This device cannot create sessions".into() });
            }
            let attachment = state.session(&source)?;
            attachment.require_live()?;
            let actual = attachment.snapshot(&source, true);
            if actual.running_turn_id.is_some()
                || actual.head.is_none_or(|at| (at.generation, at.durable) != (head.generation, head.durable))
            { return Err(stale()); }
            let mut current = actual.transcript.iter();
            for row in &displayed {
                let exact = current.find(|candidate| candidate.id == row.id).ok_or_else(stale)?;
                if exact.kind != row.kind || exact.text != row.text || exact.images != row.images
                    || row.id.starts_with("mobile-")
                { return Err(stale()); }
            }
            link
        };
        let rows = displayed_fork_rows(displayed)?;
        let end = rows.last().ok_or_else(stale)?.item_id.clone();
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            if !shared.state().same_link(&link) { return Err(MobileError::Disconnected); }
            let created = link.request(link.backend.port().create(CreateParams {
                workspace: None, title: None,
                fork_from: Some(ForkFrom {
                    session_id: source.clone(), before_item: end,
                    displayed: Some(DisplayedForkV1 { head, rows, durable_receipt: true }),
                }),
            })).await?;
            if created.session_id == source || !shared.state().same_link(&link) {
                return Err(MobileError::Disconnected);
            }
            shared.notifier.sessions();
            Ok(created.into())
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
            let exact_stop = link.backend.features().iter().any(|f| f == features::EXACT_TURN_INTERRUPT_V1);
            let session = state.sessions.entry(id.clone()).or_default();
            if session.worker.is_none() {
                start_attachment(&shared, session, id.clone(), link);
            }
            let mut snapshot = session.snapshot(&id, true);
            snapshot.can_interrupt &= exact_stop;
            shared.notifier.session(id.as_str());
            Ok(snapshot)
        })
    }

    pub fn snapshot(&self, session_id: String) -> Result<SessionSnapshot, MobileError> {
        let id = parse_session_id(session_id)?;
        let state = self.shared.state();
        let link = state.active().ok();
        let mut snapshot = state.session(&id)?.snapshot(&id, link.is_some());
        snapshot.can_interrupt &= link.is_some_and(|link| {
            link.backend.features().iter().any(|f| f == features::EXACT_TURN_INTERRUPT_V1)
        });
        Ok(snapshot)
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
                shared.notifier.session(id.as_str());
                (session, state.active().ok(), state.profile_epoch)
            };
            if let Some(worker) = session.worker {
                // Let a pending attach finish before detaching it. Aborting a
                // local future does not cancel an RPC accepted by the server.
                worker.stop.cancel();
                let _ = worker.task.await;
            }
            let result = match link {
                Some(link) => link.request(link.backend.port().detach(id.clone())).await,
                None => Ok(()),
            };
            let mut state = shared.state();
            if state.profile_epoch == profile_epoch {
                state.detaching.remove(&id);
                shared.notifier.session(id.as_str());
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
        self.submit_with_images(
            session_id,
            text,
            expected_head,
            client_message_id,
            Vec::new(),
        )
    }

    /// Returns only a loaded local GGUF candidate with an exact revision.
    pub fn session_title_local_choices(&self) -> Result<Vec<crate::SessionTitleLocalChoice>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            if let Some(model) = &link.backend.hybrid_model {
                return model.session_title_local_choices();
            }
        }
        Ok(Vec::new())
    }

    /// One text-only operation on the exact loaded model. Cannot activate a route.
    pub async fn session_title_generate_local(
        &self,
        choice: crate::SessionTitleLocalChoice,
        turns: Vec<crate::SessionTitleTurn>,
    ) -> Result<String, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            let model = link.backend.hybrid_model.clone().ok_or_else(|| {
                session_error("Kein lokales Titelmodell geladen.")
            })?;
            crate::session_title::generate_local(model, link.stop.clone(), choice, turns).await
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (choice, turns);
            Err(session_error("Lokale Titelerstellung ist in diesem Build nicht verfügbar."))
        }
    }

    /// The already loaded, vision-capable GGUF, if its local install is still current.
    pub async fn local_vision_choices(&self) -> Result<Vec<crate::LocalVisionChoice>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            if let Some(model) = link.backend.hybrid_model.clone() {
                let runtime = self
                    .runtime
                    .as_ref()
                    .ok_or_else(|| internal("Runtime stopped"))?;
                return runtime
                    .spawn_blocking(move || model.local_vision_choices())
                    .await
                    .map_err(|_| internal("Local vision lookup stopped"))?;
            }
        }
        Ok(Vec::new())
    }

    /// Describe only the user's explicit images with the named already-loaded local model.
    pub async fn local_vision_describe(
        &self,
        choice: crate::LocalVisionChoice,
        images: Vec<crate::LocalVisionImage>,
        progress: Arc<dyn crate::LocalVisionProgress>,
    ) -> Result<(), MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            let model = link.backend.hybrid_model.clone().ok_or_else(|| {
                session_error("Lokale Bildanalyse ist in diesem Host nicht verfügbar.")
            })?;
            crate::local_vision::describe(model, link.stop.clone(), choice, images, progress).await
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (choice, images, progress);
            Err(session_error(
                "Lokale Bildanalyse ist in diesem Build nicht verfügbar.",
            ))
        }
    }

    /// Device-configured API keys only; never uses a gateway or chat account.
    pub fn api_image_edit_choices(&self) -> Result<Vec<crate::ApiImageEditChoice>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            if let Some(model) = &link.backend.hybrid_model {
                return crate::image_edit_mobile::binding::choices(model);
            }
        }
        Ok(Vec::new())
    }

    /// Sanitizes one selected source and pins its preview to this exact local host and route.
    /// Preparing an edit never sends anything to a provider.
    pub fn prepare_api_image_edit(
        &self,
        choice: crate::ApiImageEditChoice,
        source_png: Vec<u8>,
        instruction: String,
    ) -> Result<Arc<crate::ApiImageEditDraft>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            let model = link
                .backend
                .hybrid_model
                .as_ref()
                .ok_or_else(crate::image_edit_mobile::unavailable)?;
            crate::image_edit_mobile::binding::prepare(model, choice, source_png, instruction)
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (choice, source_png, instruction);
            Err(crate::image_edit_mobile::unavailable())
        }
    }

    /// Send once, after explicit approval of this immutable preview and synchronous mode.
    pub async fn confirm_api_image_edit(
        &self,
        draft: Arc<crate::ApiImageEditDraft>,
        explicitly_synchronous: bool,
    ) -> Result<crate::ApiImageEditAsset, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            let model = link
                .backend
                .hybrid_model
                .clone()
                .ok_or_else(crate::image_edit_mobile::unavailable)?;
            let stop = link.stop.clone();
            let runtime = self.runtime.as_ref().ok_or_else(|| internal("Runtime stopped"))?;
            let work = runtime.spawn(async move {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => Err(MobileError::Disconnected),
                    result = crate::image_edit_mobile::binding::confirm(model, draft, explicitly_synchronous) => result,
                }
            });
            tokio_util::task::AbortOnDropHandle::new(work)
                .await
                .map_err(|_| internal("Image edit task stopped"))?
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (draft, explicitly_synchronous);
            Err(crate::image_edit_mobile::unavailable())
        }
    }

    /// Explicit on-device API route selection, independent of the chat and GGUF model.
    pub fn api_vision_choices(&self) -> Result<Vec<crate::ApiVisionChoice>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            if let Some(model) = &link.backend.hybrid_model {
                return crate::hybrid::api_choices(model);
            }
        }
        Ok(Vec::new())
    }

    /// Describe exactly one approved image without the chat prompt or transcript.
    pub async fn api_vision_describe(
        &self,
        choice: crate::ApiVisionChoice,
        image: PromptImage,
    ) -> Result<String, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            let model = link.backend.hybrid_model.clone().ok_or_else(|| {
                session_error("Die API-Bildanalyse ist in diesem Host nicht konfiguriert")
            })?;
            let stop = link.stop.clone();
            let runtime = self
                .runtime
                .as_ref()
                .ok_or_else(|| internal("Runtime stopped"))?;
            let work = runtime.spawn(async move {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => Err(MobileError::Disconnected),
                    result = crate::hybrid::api_describe(model, choice, image) => result,
                }
            });
            tokio_util::task::AbortOnDropHandle::new(work)
                .await
                .map_err(|_| internal("Vision task stopped"))?
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (choice, image);
            Err(session_error(
                "API-Bildanalyse ist in diesem Build nicht verfügbar",
            ))
        }
    }

    /// Describe exactly one approved image without the chat prompt or transcript.
    pub async fn api_vision_observe(
        &self,
        choice: crate::ApiVisionChoice,
        image: PromptImage,
    ) -> Result<crate::ApiVisionObservation, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            let model = link.backend.hybrid_model.clone().ok_or_else(|| {
                session_error("Die API-Bildanalyse ist in diesem Host nicht konfiguriert")
            })?;
            let stop = link.stop.clone();
            let runtime = self
                .runtime
                .as_ref()
                .ok_or_else(|| internal("Runtime stopped"))?;
            let work = runtime.spawn(async move {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => Err(MobileError::Disconnected),
                    result = crate::hybrid::api_observe(model, choice, image) => result,
                }
            });
            tokio_util::task::AbortOnDropHandle::new(work)
                .await
                .map_err(|_| internal("Vision task stopped"))?
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (choice, image);
            Err(session_error(
                "API-Bildanalyse ist in diesem Build nicht verfügbar",
            ))
        }
    }

    /// Sichtbare exakte Cloud-Modellrouten für ein einmaliges Opt-in, nur lokal.
    pub fn hybrid_vision_choices(&self) -> Result<Vec<crate::HybridVisionChoice>, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            if let Some(model) = &link.backend.hybrid_model {
                return crate::hybrid::choices(model);
            }
        }
        Ok(Vec::new())
    }

    /// One-shot: the named cloud route only sees the approved images. The
    /// question and final generation remain on the pinned local Qwen.
    pub async fn hybrid_vision_reply(
        &self,
        choice: crate::HybridVisionChoice,
        images: Vec<PromptImage>,
        question: String,
        progress: Arc<dyn crate::HybridVisionProgress>,
    ) -> Result<String, MobileError> {
        #[cfg(feature = "local-host")]
        {
            let link = self.shared.state().active()?;
            let model = link.backend.hybrid_model.clone().ok_or_else(|| {
                session_error("Cloud-Bildanalyse ist nur im lokalen Chat verfügbar")
            })?;
            let stop = link.stop.clone();
            let runtime = self
                .runtime
                .as_ref()
                .ok_or_else(|| internal("Runtime stopped"))?;
            let work = runtime.spawn(async move {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => Err(MobileError::Disconnected),
                    result = crate::hybrid::reply(model, choice, images, question, progress) => result,
                }
            });
            tokio_util::task::AbortOnDropHandle::new(work)
                .await
                .map_err(|_| internal("Vision task stopped"))?
        }
        #[cfg(not(feature = "local-host"))]
        {
            let _ = (choice, images, question, progress);
            Err(session_error(
                "Cloud-Bildanalyse ist in diesem Build nicht verfügbar",
            ))
        }
    }

    /// Wie `submit`, mit Bildern am Prompt. Ohne Host-Feature `images` wird das ohne Anfrage abgelehnt.
    pub fn submit_with_images(
        &self,
        session_id: String,
        text: String,
        expected_head: SessionCursor,
        client_message_id: String,
        images: Vec<PromptImage>,
    ) -> Result<SubmitReceipt, MobileError> {
        self.submit_edit(
            session_id,
            text,
            expected_head,
            client_message_id,
            images,
            None,
        )
    }

    /// Sendet `text` neu und ersetzt dabei die frühere User-Nachricht `rewind_before` samt allem danach
    /// (Verlauf wird vor ihr abgeschnitten), statt eine zweite Nachricht anzuhängen. Ohne `rewind_before` wie `submit_with_images`.
    pub fn submit_edit(
        &self,
        session_id: String,
        text: String,
        expected_head: SessionCursor,
        client_message_id: String,
        images: Vec<PromptImage>,
        rewind_before: Option<String>,
    ) -> Result<SubmitReceipt, MobileError> {
        if let Some(target) = &rewind_before {
            if target.is_empty()
                || target.len() > 128
                || !target.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(session_error("Ungültige Nachrichten-Id"));
            }
        }
        let id = parse_session_id(session_id)?;
        if rewind_before.is_some() && !images.is_empty() {
            return Err(session_error("Exact edit reuses the original stored images"));
        }
        validate_submission(if rewind_before.is_some() && text.is_empty() { "retry" } else { &text }, &client_message_id)?;
        let images = validate_images(images)?;
        if !images.is_empty() && !self.supports_images() {
            return Err(MobileError::Denied {
                detail: NO_IMAGES_DETAIL.into(),
            });
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            submit(
                shared,
                id,
                text,
                images,
                expected_head.into(),
                client_message_id,
                SubmitRoute::Normal { rewind_before },
            )
            .await
        })
    }

    /// Ob der aktive Host Bilder am Prompt annimmt (Feature `images`). Ohne Verbindung `false`.
    pub fn supports_images(&self) -> bool {
        self.shared.state().active().is_ok_and(|link| {
            link.backend
                .features()
                .iter()
                .any(|f| f == features::IMAGES)
        })
    }

    /// Whether the current authority negotiated exact durable prompt rewrite.
    pub fn supports_exact_rewind(&self) -> bool {
        self.shared.state().active().is_ok_and(|link| {
            link.backend.features().iter().any(|f| f == features::EXACT_REWIND_V1)
        })
    }

    /// Whether this exact link can return a matching historical rewrite receipt.
    pub fn supports_exact_rewrite_receipts(&self) -> bool {
        self.shared.state().active().is_ok_and(|link| {
            link.backend.features().iter().any(|f| f == features::EXACT_REWIND_RECEIPT_V1)
        })
    }

    /// Setzt eine durch einen Host-Neustart unterbrochene Session fort (`session.resume`) und laedt den Verlauf neu.
    pub fn resume_session(&self, session_id: String) -> Result<(), MobileError> {
        let id = parse_session_id(session_id)?;
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let link = shared.state().active()?;
            link.request(link.backend.port().resume(id.clone())).await?;
            let mut state = shared.state();
            if state.same_link(&link)
                && let Ok(session) = state.session_mut(&id)
            {
                session.mark_resumed();
                shared.notifier.session(id.as_str());
            }
            Ok(())
        })
    }

    /// Ob der aktive Host gespeicherte Prompt-Bilder ausliefert (Feature `media`). Ohne Verbindung `false`.
    pub fn supports_media(&self) -> bool {
        self.shared
            .state()
            .active()
            .is_ok_and(|link| link.backend.features().iter().any(|f| f == features::MEDIA))
    }

    /// Holt ein gespeichertes Prompt-Bild (`image_ref` aus `TranscriptEntry.images`) vom Host.
    /// Die Bytes sind das gespeicherte Bild; die App verkleinert beim Dekodieren.
    pub fn media_thumbnail(
        &self,
        session_id: String,
        image_ref: String,
    ) -> Result<PromptImage, MobileError> {
        let id = parse_session_id(session_id)?;
        let media: harw_protocol::MediaRef =
            serde_json::from_str(&image_ref).map_err(|_| MobileError::Denied {
                detail: "Ungueltige Bildreferenz".into(),
            })?;
        if !self.supports_media() {
            return Err(MobileError::Denied {
                detail: "Dieser Host liefert keine Bilder aus".into(),
            });
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let link = shared.state().active()?;
            let result = link
                .request(link.backend.port().media_get(MediaGetParams {
                    session_id: id,
                    media,
                }))
                .await?;
            Ok(PromptImage {
                mime: result.mime,
                data_b64: result.data_b64,
            })
        })
    }

    pub fn interrupt(&self, session_id: String, turn_id: String) -> Result<(), MobileError> {
        let id = parse_session_id(session_id)?;
        let turn_id =
            TurnId::try_from_str(turn_id).map_err(|_| session_error("Invalid turn ID"))?;
        let link = {
            let state = self.shared.state();
            let link = state.active()?;
            if !link.backend.features().iter().any(|f| f == features::EXACT_TURN_INTERRUPT_V1) {
                return Err(MobileError::Denied {
                    detail: "This host does not support stopping an exact turn".into(),
                });
            }
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
            link.request(link.backend.port().interrupt(params)).await
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
                shared.notifier.session(id.as_str());
                link
            };
            let result = link
                .request(link.backend.port().respond(ApprovalRespondParams {
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
                    shared.notifier.session(id.as_str());
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

    /// Registers the platform shell and file access used by the on-device agent's device
    /// tools. Call it before `connect_local`; `None` removes a handle.
    pub fn set_device_tools(
        &self,
        shell: Option<Arc<dyn crate::ShellExecutor>>,
        files: Option<Arc<dyn crate::FileAccess>>,
    ) {
        let mut handles = self
            .shared
            .device
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        handles.shell = shell;
        handles.files = files;
    }

    /// Registers the listener that hears pushed change signals.
    pub fn set_listener(&self, listener: Arc<dyn CoreListener>) {
        self.shared.notifier.set(listener);
    }

    /// Removes the listener; it is never called again once this returns.
    pub fn clear_listener(&self) {
        self.shared.notifier.clear();
    }

    /// Switches from any server to the on-device host kept under `files_dir`.
    pub fn connect_local(
        &self,
        files_dir: String,
        keystore: Arc<dyn PlatformKeystore>,
    ) -> Result<(), MobileError> {
        if !cfg!(feature = "local-host") {
            return Err(MobileError::Session {
                detail: LOCAL_HOST_UNAVAILABLE.into(),
            });
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let run = {
                let mut state = shared.state();
                let _ = state.apply_pending_revocation();
                state.stop("Switching to the on-device host");
                state.profile = None;
                state.profile_epoch = state.profile_epoch.wrapping_add(1);
                state.sessions.clear();
                state.detaching.clear();
                state.submissions.clear();
                let run = CancellationToken::new();
                state.run = Some(run.clone());
                state.phase = ConnectionPhase::Connecting;
                state.detail = Some("Starting the on-device host".into());
                state.attempt = 0;
                state.next_retry_at_ms = None;
                state.retryable_failure = false;
                state.route = None;
                shared.notify_link(&state);
                shared.notifier.sessions();
                run
            };
            let mut polls = 0_u32;
            loop {
                let stopping = shared.state().last_local.strong_count() > 0;
                if run.is_cancelled() {
                    return Err(MobileError::Disconnected);
                }
                if !stopping {
                    break;
                }
                if polls >= LOCAL_STOP_POLLS {
                    let detail = "the previous on-device host is still stopping";
                    fail_local(&shared, &run, detail.into());
                    return Err(session_error(detail));
                }
                polls += 1;
                tokio::time::sleep(LOCAL_STOP_POLL).await;
            }
            let backend = match open_local(
                PathBuf::from(files_dir),
                keystore,
                requested_features(),
                shared.device_tools(),
            )
            .await
            {
                Ok(backend) => backend,
                Err(error) => {
                    fail_local(&shared, &run, error.to_string());
                    return Err(error);
                }
            };
            let (link, failure_rx) = {
                let mut state = shared.state();
                if run.is_cancelled() {
                    return Err(MobileError::Disconnected);
                }
                let (failure, failure_rx) = watch::channel(None);
                let link = Arc::new(Link {
                    backend,
                    stop: run.child_token(),
                    failure,
                    attaching: Semaphore::new(4),
                });
                state.link = Some(Arc::clone(&link));
                state.last_local = Arc::downgrade(&link);
                state.phase = ConnectionPhase::Connected;
                state.route = None;
                state.detail = Some("On-device host".into());
                state.attempt = 0;
                state.next_retry_at_ms = None;
                state.retryable_failure = false;
                shared.notifier.status();
                shared.notifier.sessions();
                (link, failure_rx)
            };
            tokio::spawn(supervise_local(Arc::clone(&shared), link, failure_rx, run));
            Ok(())
        })
    }

    /// The host the phone currently talks to, if any.
    pub fn active_host(&self) -> Option<HostRef> {
        let state = self.shared.state();
        match state.active() {
            Ok(link) if link.backend.kind() == BackendKind::Local => Some(HostRef {
                id: "local".into(),
                kind: HostKind::Local,
                label: "Phone".into(),
                reachable: true,
            }),
            active => state.profile.as_ref().map(|profile| HostRef {
                id: "remote".into(),
                kind: HostKind::Server,
                label: profile.profile.label.clone(),
                reachable: active.is_ok(),
            }),
        }
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
    fn replace_route_consent(&self, consent: Option<(ValidatedProfile, RoutePreference)>) {
        let mut state = self.shared.state();
        let prior = state.active_preference();
        let replacing_profile = consent.as_ref().is_some_and(|(selection, _)| {
            state.profile.as_ref().is_some_and(|active| !active.same_route_authority(selection))
        });
        state.route_consent = consent;
        state.route_policy_epoch = state.route_policy_epoch.wrapping_add(1);
        if state.profile.is_some() && (state.run.is_some() || state.retryable_failure)
            && (replacing_profile || state.active_preference() != prior)
        {
            // Invalidate cached fallback plans and independent control requests
            // before a new policy is reported to the UI.
            state.stop("Gateway route policy changed; reconnect explicitly");
            state.phase = ConnectionPhase::Disconnected;
            state.retryable_failure = false;
            state.next_retry_at_ms = None;
            state.detail = Some("Gateway route policy changed; reconnect explicitly".into());
            self.shared.notify_link(&state);
            self.shared.notifier.sessions();
        }
        self.shared.notifier.status();
    }

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
                .is_some_and(|selected| selected.same_route_authority(&consent))
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
            let route = state.route.ok_or(MobileError::Disconnected)?;
            if !route_plan(state.preference_for(&selected), &selected)?.contains(&route) {
                return Err(MobileError::Denied { detail: "Gateway route is no longer authorized".into() });
            }
            (link.stop.clone(), route, selected)
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

    fn start_connection(
        &self, profile: Option<ValidatedProfile>, automatic_route_epoch: Option<u64>,
    ) -> Result<(), MobileError> {
        let shared = Arc::clone(&self.shared);
        let local = self.local.clone();
        let label = self.enrollment.label.clone();
        self.run(async move {
            let explicit_reconnect = profile.is_none();
            let (run, profile) = {
                let mut state = shared.state();
                if state.apply_pending_revocation() {
                    shared.notifier.status();
                    shared.notifier.sessions();
                }
                if automatic_route_epoch.is_some_and(|epoch| {
                    epoch != state.route_policy_epoch || state.phase != ConnectionPhase::Failed
                        || !state.retryable_failure
                }) {
                    return Ok(());
                }
                let profile = profile.or_else(|| state.profile.clone()).ok_or_else(|| {
                    MobileError::InvalidProfile {
                        detail: "Select a server profile before connecting".into(),
                    }
                })?;
                if local.node_id() == &profile.expected_node {
                    return Err(MobileError::InvalidProfile {
                        detail: "Server and phone must have different node identities".into(),
                    });
                }
                let routes = route_plan(state.preference_for(&profile), &profile)?;
                let same_profile = state
                    .profile
                    .as_ref()
                    .is_some_and(|old| old.same_authority(&profile));
                if same_profile && !explicit_reconnect && state.active().is_ok()
                    && state.profile.as_ref().is_some_and(|previous| previous.same_route_authority(&profile))
                    && state.route.is_some_and(|route| routes.contains(&route))
                {
                    #[cfg(feature = "onion")]
                    if state.profile.as_ref().is_none_or(|old| old.profile != profile.profile) {
                        state.owner_status_scope.cancel();
                        state.owner_status_scope = CancellationToken::new();
                    }
                    state.profile = Some(profile);
                    shared.notifier.status();
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
                shared.notify_link(&state);
                if !same_profile {
                    shared.notifier.sessions();
                }
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

    /// Active link for a project mutation; a minor-2 host is refused without a request.
    fn projects_link(&self) -> Result<Arc<Link>, MobileError> {
        let link = self.shared.state().active()?;
        if link_supports_projects(&link) {
            Ok(link)
        } else {
            Err(MobileError::Denied {
                detail: NO_PROJECTS_DETAIL.into(),
            })
        }
    }

    /// Bruecke fuer crate::manage: aktive Verbindung, Feature-Pruefung ohne Anfrage, Anfrage mit Timeout/Revocation.
    pub(crate) fn manage_call<T, F>(&self, feature: &'static str, call: F) -> Result<T, MobileError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a dyn SessionPort) -> PortFuture<'a, T> + Send + 'static,
    {
        let link = self.shared.state().active()?;
        if !link.backend.features().iter().any(|f| f == feature) {
            let detail = if feature == features::STEPUP {
                crate::manage::NO_STEPUP_DETAIL
            } else {
                crate::manage::NO_MANAGE_DETAIL
            };
            return Err(MobileError::Denied {
                detail: detail.into(),
            });
        }
        self.run(async move { link.request(call(link.backend.port())).await })
    }

    /// Hat der Host `feature` bei `session.hello` gewaehrt? Ohne Verbindung `false`. Nie ein Fehler.
    pub(crate) fn feature_granted(&self, feature: &str) -> bool {
        self.shared
            .state()
            .active()
            .is_ok_and(|link| link.backend.features().iter().any(|f| f == feature))
    }

    /// Features/Caps der aktiven Verbindung, ohne Verbindung Default. Nie ein Fehler.
    pub(crate) fn manage_support_now(&self) -> ManageSupport {
        let Ok(link) = self.shared.state().active() else {
            return ManageSupport::default();
        };
        let has = |f: &str| link.backend.features().iter().any(|x| x == f);
        let caps = link.backend.granted();
        ManageSupport {
            manage: has(features::MANAGE),
            stepup: has(features::MANAGE) && has(features::STEPUP),
            observe: caps.observe,
            steer: caps.steer,
            approve: caps.approve,
            control: caps.control,
        }
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
    let mut initial_retries = 0_u32;
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
                state.next_retry_at_ms = Some(
                    now_ms().saturating_add(delay.as_millis().min(u128::from(u64::MAX)) as u64),
                );
                shared.notifier.status();
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
            shared.notifier.status();
        } else if run.is_cancelled() {
            break;
        }
        let mut options = ConnectOptions::new(&label);
        options.requested_caps = Some(ClientCaps::ALL);
        options.features = requested_features();
        options.features.push(features::CLOUD_ASSISTANT_V1.to_owned());
        options.features.push(features::CLOUD_ASSISTANT_RECALL_V1.to_owned());
        options.features.push(features::CLOUD_ASSISTANT_MODELS_V1.to_owned());
        options.features.push(features::CLOUD_ASSISTANT_SHARED_V1.to_owned());
        let plan = {
            let preference = shared.state().preference_for(&profile);
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
                shared.notifier.status();
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
            {
                let mut state = shared.state();
                if run.is_cancelled() {
                    cancelled = true;
                    break;
                }
                state.route = Some(route);
                shared.notifier.status();
            }
            let mut onion = if route == ConnectionRoute::Onion {
                shared.diagnostics.begin_onion()
            } else {
                None
            };
            let attempt = tokio::select! {
                biased;
                () = run.cancelled() => None,
                result = shared.dialer.connect_session(route, &profile, &local, options.clone()) => Some(result),
            };
            match attempt {
                Some(Ok(connection)) => {
                    if let Some(diagnostic) = &mut onion {
                        diagnostic.connected();
                    }
                    connected = Some((connection, onion));
                    break;
                }
                Some(Err(error)) => {
                    let disposition = dial_failure(&error);
                    if let Some(diagnostic) = &mut onion {
                        diagnostic.failed(onion_failure_stage(&error), disposition == DialFailure::Retry);
                    }
                    let stop = disposition == DialFailure::Final;
                    failures.push((route, error));
                    if stop {
                        break;
                    }
                }
                None => {
                    if let Some(diagnostic) = &mut onion {
                        diagnostic.cancelled();
                    }
                    cancelled = true;
                    break;
                }
            }
        }
        if cancelled {
            break;
        }
        let (connection, _onion_lifetime) = match connected {
            Some(connection) => connection,
            None => {
                let detail = failure_detail(&failures);
                let kind = attempt_failure(&failures);
                let mut state = shared.state();
                if run.is_cancelled() {
                    break;
                }
                state.detail = Some(detail.clone());
                shared.notifier.status();
                if !should_retry_dial(first.is_some(), initial_retries, retry, &failures, &kind) {
                    state.phase = ConnectionPhase::Failed;
                    state.retryable_failure = kind == DialFailure::Retry;
                    state.next_retry_at_ms = None;
                    if let Some(first) = first.take() {
                        let _ = first.send(Err(MobileError::Connect { detail }));
                    }
                    return;
                }
                if first.is_some() {
                    initial_retries = initial_retries.saturating_add(1);
                }
                delay = retry_delay(retry, entropy);
                retry = retry.saturating_add(1);
                continue;
            }
        };
        let (failure, failure_rx) = watch::channel(None);
        let link = Arc::new(Link {
            backend: Backend::remote(connection),
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
            shared.notifier.status();
            shared.notifier.sessions();
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
                shared.notifier.status();
                shared.notifier.sessions();
                return;
            }
            state.clear_link(&issue.detail);
            state.detail = Some(issue.detail);
            if issue.disposition == LinkDisposition::Final {
                state.phase = ConnectionPhase::Failed;
                state.retryable_failure = false;
                state.next_retry_at_ms = None;
                shared.notify_link(&state);
                return;
            }
            state.phase = ConnectionPhase::Reconnecting;
            shared.notify_link(&state);
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

/// Initial Onion recovery is bounded independently of network wakeups, which
/// may reset the ordinary reconnect backoff counter. Direct-only initial
/// connections retain their previous fail-fast behavior.
fn should_retry_dial(
    initial: bool,
    initial_retries: u32,
    retry: u32,
    failures: &[(ConnectionRoute, RemoteError)],
    disposition: &DialFailure,
) -> bool {
    if *disposition != DialFailure::Retry {
        return false;
    }
    if initial {
        return initial_retries < MAX_INITIAL_ONION_RETRIES
            && failures.iter().any(|(route, error)| {
                *route == ConnectionRoute::Onion && dial_failure(error) == DialFailure::Retry
            });
    }
    retry < MAX_RECONNECT_ATTEMPTS
}

/// Until the remote transport exposes typed stages, report only known
/// prefixes and never export the error string itself.
fn onion_failure_stage(error: &RemoteError) -> OnionFailureStage {
    let RemoteError::Connect(detail) = error else {
        return OnionFailureStage::Unknown;
    };
    if detail.starts_with("tor: tor bootstrap") {
        OnionFailureStage::TorBootstrap
    } else if detail.starts_with("tor: onion connect") {
        OnionFailureStage::OnionStream
    } else if detail.starts_with("node transport:") {
        OnionFailureStage::NodeTransport
    } else if detail.starts_with("upgrade:") {
        OnionFailureStage::Upgrade
    } else if detail.starts_with("session.hello") {
        OnionFailureStage::SessionHello
    } else {
        OnionFailureStage::Unknown
    }
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
                // The remote port owns the socket, so a read-only
                // probe also detects disconnects with no session attached.
                if let Err(MobileError::Session { detail }) =
                    link.request(link.backend.port().list()).await
                {
                    link.fail(LinkIssue::protocol(detail));
                }
            }
        }
    }
}

/// Marks a failed on-device start, unless the run was replaced meanwhile.
fn fail_local(shared: &Shared, run: &CancellationToken, detail: String) {
    let mut state = shared.state();
    if run.is_cancelled() {
        return;
    }
    run.cancel();
    state.run = None;
    state.phase = ConnectionPhase::Failed;
    state.detail = Some(detail);
    state.retryable_failure = false;
    state.next_retry_at_ms = None;
    shared.notifier.status();
}

/// Watches the on-device link; a failure is final and never reconnects.
async fn supervise_local(
    shared: Arc<Shared>,
    link: Arc<Link>,
    failure: watch::Receiver<Option<LinkIssue>>,
    run: CancellationToken,
) {
    let issue = monitor_connection(&link, failure, &run).await;
    drop(link);
    let Some(issue) = issue else {
        return;
    };
    let mut state = shared.state();
    if run.is_cancelled() {
        return;
    }
    state.clear_link(&issue.detail);
    if let Some(active) = state.run.take() {
        active.cancel();
    }
    state.route = None;
    state.phase = ConnectionPhase::Failed;
    state.detail = Some(issue.detail);
    state.retryable_failure = false;
    state.next_retry_at_ms = None;
    shared.notify_link(&state);
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
        features::PROJECTS,
        features::AUTO_TITLE_CAS_V1,
        features::AUTO_TITLE_SEED_V1,
        features::SESSION_DREAM_ROWS_V1,
        features::SESSION_SYSTEM_ROWS_V1,
        features::EXISTING_PROJECT_ASSIGNMENT_V1,
        features::MANAGE,
        features::STEPUP,
        features::IMAGES,
        features::MEDIA,
        features::SESSION_MEDIA_PREVIEW_V1,
        features::OVERVIEW,
        features::ARCHIVE_CHECKPOINT_V1,
        features::ARCHIVE_RETAINED_REWRITE_V1,
        features::EXACT_REWIND_V1,
        features::EXACT_REWIND_RECEIPT_V1,
        features::EXACT_TURN_INTERRUPT_V1,
        features::CLOUD_LIBRARY_V1,
        features::MANUAL_CLOUD_WORK_V1,
    ]
    .iter()
    .map(|feature| (*feature).to_owned())
    .collect()
}

fn validate_existing_project_key(key: &str) -> Result<(), MobileError> {
    if key.is_empty() || key.len() > 128
        || !key.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(MobileError::Denied { detail: "Invalid existing project identifier".into() });
    }
    Ok(())
}

fn link_supports_projects(link: &Link) -> bool {
    link.backend
        .ack()
        .features
        .iter()
        .any(|feature| feature == features::PROJECTS)
}

fn remember_holder(state: &mut CoreState, id: SessionId, holder: Option<HolderInfo>) {
    if state.holders.len() >= MAX_HOLDERS && !state.holders.contains_key(&id) {
        state.holders.clear();
    }
    state.holders.insert(id, holder);
}

/// Minor-2 fallback: one pseudo project built from `session.list`.
fn legacy_project_page(sessions: &[MobileSession]) -> MobileProjectPage {
    let latest = sessions
        .iter()
        .filter_map(|session| session.updated_at.parse::<jiff::Timestamp>().ok())
        .max()
        .map(|timestamp| timestamp.to_string())
        .or_else(|| sessions.first().map(|session| session.updated_at.clone()));
    let projects = latest
        .map(|last_activity| {
            vec![MobileProject {
                project_key: String::new(),
                label: NO_PROJECT_LABEL.into(),
                path_hint: None,
                session_count: u32::try_from(sessions.len()).unwrap_or(u32::MAX),
                active_count: u32::try_from(
                    sessions
                        .iter()
                        .filter(|session| session.attached > 0)
                        .count(),
                )
                .unwrap_or(u32::MAX),
                last_activity,
            }]
        })
        .unwrap_or_default();
    MobileProjectPage {
        projects,
        next_cursor: None,
        share: MobileShareInfo {
            mode: MobileShareMode::Off,
            tier: MobileTier::Observer,
        },
        legacy: true,
    }
}

/// Minor-2 fallback: rows from `session.list`, only for "all" and "no project".
fn legacy_session_page(
    sessions: Vec<MobileSession>,
    project_key: Option<&str>,
    steer: bool,
) -> MobileSessionPage {
    let rows = if matches!(project_key, None | Some("")) {
        sessions
            .into_iter()
            .map(|session| MobileSessionRow {
                session_id: session.session_id,
                title: session.title,
                project_key: None,
                updated_at: session.updated_at,
                turns: None,
                state: session.state,
                attached: session.attached,
                model: session.model,
                origin: MobileRowOrigin::Hosted,
                purpose: crate::MobileSessionPurpose::Unknown,
                continuable: steer,
                reason: None,
                holder: None,
                last_origin: None,
                first_message: None,
            })
            .collect()
    } else {
        Vec::new()
    };
    MobileSessionPage {
        sessions: rows,
        next_cursor: None,
        legacy: true,
    }
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
    shared.notifier.session(id.as_str());
    let from = session.resume(link.backend.host_epoch());
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

#[derive(Default)]
struct AttachmentRecovery {
    attempts: usize,
}

impl AttachmentRecovery {
    fn after_eof(&mut self) -> Option<Duration> {
        let ms = [250, 500, 1_000].get(self.attempts).copied()?;
        self.attempts += 1;
        Some(Duration::from_millis(ms))
    }

    fn made_progress(&mut self) {
        self.attempts = 0;
    }
}

async fn drive_attachment(
    shared: Arc<Shared>,
    id: SessionId,
    link: Arc<Link>,
    stop: CancellationToken,
    mut from: Option<Cursor>,
) {
    let mut stalled = 0_u8;
    let mut recovery = AttachmentRecovery::default();
    'attach: while !stop.is_cancelled() {
        let Some(Ok(permit)) = stop.run_until_cancelled(link.attaching.acquire()).await else {
            return;
        };
        let params = AttachParams {
            session_id: id.clone(),
            // Replay the observed durable suffix. Rebuild the partial answer
            // from the live ring or its snapshot: compact replay can merge an
            // already seen delta and a new delta into the same envelope.
            from: from.map(|cursor| Cursor { live: 0, ..cursor }),
            profile: attach_profile(link.backend.features()),
            tail_items: DEFAULT_TAIL_ITEMS,
        };
        let (mut ack, mut source) = match link.request(link.backend.port().attach(params)).await {
            Ok(attachment) => attachment,
            Err(error) => {
                end_attachment(&shared, &id, &link, &error.to_string());
                return;
            }
        };
        drop(permit);
        ack.granted = ack.granted.intersect(link.backend.granted());
        // A host restart leaves sessions `Interrupted`; the app is not read-only
        // for that, so resume them right away (needs steer) and show them idle.
        if ack.session.state == HostedState::Interrupted
            && ack.granted.steer
            && link
                .request(link.backend.port().resume(id.clone()))
                .await
                .is_ok()
        {
            ack.session.state = HostedState::Idle;
        }
        if with_session(&shared, &id, &link, |session| {
            session.attached(&ack, from.is_none())
        })
        .is_none()
        {
            return;
        }
        let mut attachment_lifetime = shared.diagnostics.begin_attachment();
        let mut diagnostic_cursor = ack.head;
        let (next_from, refreshed) = loop {
            let envelope = match stop.run_until_cancelled(source.next()).await {
                Some(Ok(Some(envelope))) => envelope,
                Some(Err(PortError::Transport(detail))) if revoked_close(&detail) => {
                    if let Some(log) = &attachment_lifetime { log.signal(StreamSignal::Revoked); }
                    link.fail(LinkIssue::revoked());
                    return;
                }
                Some(Err(PortError::Transport(detail))) => {
                    if let Some(log) = &attachment_lifetime { log.signal(StreamSignal::ConnectionError); }
                    link.fail(LinkIssue::transport(detail));
                    return;
                }
                Some(Err(PortError::Revoked)) => {
                    if let Some(log) = &attachment_lifetime { log.signal(StreamSignal::Revoked); }
                    link.fail(LinkIssue::revoked());
                    return;
                }
                Some(Err(PortError::Protocol(detail))) => {
                    if let Some(log) = &attachment_lifetime { log.signal(StreamSignal::ProtocolError); }
                    link.fail(LinkIssue::protocol(detail));
                    return;
                }
                Some(Err(error)) => {
                    end_attachment(&shared, &id, &link, &error.to_string());
                    return;
                }
                Some(Ok(None)) => {
                    if let Some(log) = &attachment_lifetime { log.signal(StreamSignal::EndOfStream); }
                    drop(attachment_lifetime.take());
                    // A frame source can end while the base connection is healthy.
                    // Kotlin still considers the window open, so its snapshot polls
                    // cannot restart a dead worker. Recover here, where the cursor
                    // and attachment cancellation are both available.
                    let Some(delay) = recovery
                        .after_eof()
                        .filter(|_| ack.session.state != HostedState::Closed)
                    else {
                        end_attachment(&shared, &id, &link, "The server ended this attachment");
                        return;
                    };
                    let Some(resume) =
                        with_session(&shared, &id, &link, SessionState::recover_stream)
                    else {
                        return;
                    };
                    drop(source);
                    match stop
                        .run_until_cancelled(link.request(link.backend.port().detach(id.clone())))
                        .await
                    {
                        Some(Ok(())) => {}
                        Some(Err(error)) => {
                            end_attachment(&shared, &id, &link, &error.to_string());
                            return;
                        }
                        None => return,
                    }
                    if stop
                        .run_until_cancelled(tokio::time::sleep(delay))
                        .await
                        .is_none()
                    {
                        return;
                    }
                    from = resume;
                    continue 'attach;
                }
                None => return,
            };
            // Merely seeing replay or handshake frames is not evidence that a
            // broken stream recovered; let three rapid replay-only EOFs stop.
            if envelope.cursor > ack.head
                && !matches!(&envelope.frame, SessionFrame::Presence { .. })
            {
                recovery.made_progress();
            }
            if let SessionFrame::Holder { session_id, holder } = &envelope.frame {
                let mut state = shared.state();
                if state.same_link(&link) {
                    remember_holder(&mut state, session_id.clone(), holder.clone());
                }
            }
            let Some(action) =
                with_session(&shared, &id, &link, |session| session.observe(&envelope))
            else {
                return;
            };
            if envelope.cursor > diagnostic_cursor {
                diagnostic_cursor = envelope.cursor;
                let signal = match &envelope.frame {
                    SessionFrame::Turn(TurnEvent::TurnStarted { .. }) => Some(StreamSignal::TurnStarted),
                    SessionFrame::Turn(TurnEvent::TurnCompleted { .. }) => Some(StreamSignal::TurnCompleted),
                    SessionFrame::Turn(TurnEvent::TurnAborted { .. }) => Some(StreamSignal::TurnAborted),
                    SessionFrame::Turn(TurnEvent::TurnFailed { .. }) => Some(StreamSignal::TurnFailed),
                    SessionFrame::Turn(TurnEvent::AssistantDelta { .. }) => Some(StreamSignal::AnswerDelta),
                    SessionFrame::Turn(TurnEvent::ReasoningDelta { .. }) => Some(StreamSignal::ReasoningDelta),
                    _ => None,
                };
                if let (Some(log), Some(signal)) = (&attachment_lifetime, signal) {
                    log.signal(signal);
                }
                if matches!(&action, FrameAction::Continue)
                    && let (Some(log), SessionFrame::Turn(event)) =
                        (&mut attachment_lifetime, &envelope.frame)
                {
                    log.observe_turn(event);
                }
            }
            match action {
                FrameAction::Continue => {}
                FrameAction::Reattach(cursor) => {
                    if let Some(log) = &attachment_lifetime { log.signal(StreamSignal::Reattach); }
                    break (cursor, false)
                }
                FrameAction::RefreshTranscript => {
                    if let Some(log) = &attachment_lifetime { log.signal(StreamSignal::Refresh); }
                    break (None, true)
                }
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
        drop(attachment_lifetime);
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
        if let Err(error) = link.request(link.backend.port().detach(id.clone())).await {
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
    let result = state.sessions.get_mut(id).map(operation);
    if result.is_some() {
        shared.notifier.session(id.as_str());
    }
    result
}

fn end_attachment(shared: &Shared, id: &SessionId, link: &Arc<Link>, detail: &str) {
    with_session(shared, id, link, |session| session.worker_ended(detail));
}

enum SubmitRoute {
    Normal { rewind_before: Option<String> },
    CloudLibrary { link: Arc<Link> },
}

async fn submit(
    shared: Arc<Shared>,
    id: SessionId,
    text: String,
    images: Vec<SubmitImage>,
    displayed: Cursor,
    client_message_id: String,
    route: SubmitRoute,
) -> Result<SubmitReceipt, MobileError> {
    let (rewind_before, pinned_cloud_library_link) = match route {
        SubmitRoute::Normal { rewind_before } => (rewind_before, None),
        SubmitRoute::CloudLibrary { link } => (None, Some(link)),
    };
    let submission_id = (id.clone(), client_message_id.clone());
    let fingerprint = {
        let mut hasher = blake3::Hasher::new_derive_key("harw mobile submit receipt v2");
        fn field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
            hasher.update(&(bytes.len() as u64).to_be_bytes());
            hasher.update(bytes);
        }
        if pinned_cloud_library_link.is_some() {
            field(&mut hasher, b"cloud-library");
        } else {
            field(&mut hasher, b"conversation");
        }
        if let Some(target) = &rewind_before {
            field(&mut hasher, b"exact-rewind");
            field(&mut hasher, target.as_bytes());
            hasher.update(&displayed.generation.to_be_bytes());
            hasher.update(&displayed.durable.to_be_bytes());
        } else {
            field(&mut hasher, b"append");
        }
        field(&mut hasher, text.as_bytes());
        for image in &images {
            field(&mut hasher, image.mime.as_bytes());
            field(&mut hasher, image.data_b64.as_bytes());
        }
        hasher.finalize()
    };
    let (link, head, profile_epoch, may_resume) = {
        let mut state = shared.state();
        let link = state.active()?;
        if pinned_cloud_library_link.as_ref().is_some_and(|expected| !Arc::ptr_eq(expected, &link)) {
            return Err(MobileError::Denied { detail: "Cloud Library conversation changed".into() });
        }
        if rewind_before.is_some() && !link.backend.ack().features.iter().any(|f| f == features::EXACT_REWIND_V1) {
            return Err(MobileError::Denied { detail: "This host cannot safely edit or retry earlier prompts".into() });
        }
        if let Some(previous) = state.submissions.get(&submission_id) {
            if fingerprint != previous.fingerprint {
                return Err(session_error(
                    "The submission ID is already associated with a different draft",
                ));
            }
            match &previous.state {
                SubmissionState::InFlight => return Ok(SubmitReceipt::new(SubmitOutcome::InFlight)),
                SubmissionState::Accepted(receipt) => return Ok(receipt.clone()),
                SubmissionState::Uncertain if rewind_before.is_none() => return Err(ambiguous()),
                // Exact rewrite only: an explicit retry can use the original ID and
                // fingerprint. Where a durable receipt is negotiated, host
                // arbitration may also resolve an already-committed old fence.
                SubmissionState::Uncertain => {}
            }
        }
        let (head, may_resume) = match state.session(&id)?.current_submission_head(displayed)? {
            Ok(head) => (head, true),
            // Only a receipt-aware host can safely resolve a retransmission
            // against a stale view. Preserve the original fence on the wire.
            Err(_) if rewind_before.is_some() && link.backend.ack().features.iter()
                .any(|feature| feature == features::EXACT_REWIND_RECEIPT_V1) => (displayed, false),
            Err(head) => return Ok(stale(head)),
        };
        if !state.submissions.contains_key(&submission_id)
            && state.submissions.len() >= MAX_PROCESS_SUBMISSIONS
        {
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
        (link, head, state.profile_epoch, may_resume)
    };
    let replacement_item_id = rewind_before.as_deref()
        .filter(|_| link.backend.ack().features.iter()
            .any(|feature| feature == features::EXACT_REWIND_RECEIPT_V1))
        .map(|target| harw_protocol::session_wire::exact_rewrite_item_id(
            &id, target, head, &client_message_id,
        ).to_string());
    let params = SubmitParams {
        session_id: id.clone(),
        text: text.clone(),
        expect_head: head,
        client_msg_id: client_message_id.clone(),
        force: false,
        images,
        rewind_before: rewind_before.clone(),
    };
    let mut result = link
        .request(link.backend.port().submit(params.clone()))
        .await;
    // A host restart left the session interrupted: resume it and send once more.
    if may_resume && pinned_cloud_library_link.is_none()
        && matches!(&result, Ok(SubmitResult::Denied { reason }) if reason.contains("session.resume"))
        && link
            .request(link.backend.port().resume(id.clone()))
            .await
            .is_ok()
    {
        result = link.request(link.backend.port().submit(params)).await;
    }
    let result = match result {
        Ok(SubmitResult::Accepted { position }) => Ok(SubmitReceipt {
            position: Some(position),
            replacement_item_id,
            ..SubmitReceipt::new(SubmitOutcome::Accepted)
        }),
        Ok(SubmitResult::Stale { head }) => Ok(stale(head)),
        Ok(SubmitResult::QueueFull) => Ok(SubmitReceipt::new(SubmitOutcome::QueueFull)),
        Ok(SubmitResult::Denied { reason }) => Ok(SubmitReceipt {
            detail: Some(reason),
            ..SubmitReceipt::new(SubmitOutcome::Denied)
        }),
        Err(error @ (MobileError::Denied { .. } | MobileError::Revoked)) => Err(error),
        // The lease is held elsewhere: nothing was written, so this is a plain refusal.
        Err(MobileError::Busy { detail }) => Ok(SubmitReceipt {
            detail: Some(detail),
            ..SubmitReceipt::new(SubmitOutcome::Busy)
        }),
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
                if rewind_before.is_none() {
                    session.echo_prompt(&id, displayed, &client_message_id, text);
                }
                shared.notifier.session(id.as_str());
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

const NO_IMAGES_DETAIL: &str = "Dieser Host nimmt keine Bilder am Prompt an";

/// Grenzen wie auf dem Draht (`MAX_SUBMIT_IMAGES`, `MAX_SUBMIT_IMAGE_B64_BYTES`), ohne Anfrage geprueft.
fn validate_images(images: Vec<PromptImage>) -> Result<Vec<SubmitImage>, MobileError> {
    use harw_protocol::session_wire::{MAX_SUBMIT_IMAGE_B64_BYTES, MAX_SUBMIT_IMAGES};
    if images.len() > MAX_SUBMIT_IMAGES {
        return Err(session_error("Zu viele Bilder an einer Nachricht"));
    }
    let mut total = 0usize;
    let mut out = Vec::with_capacity(images.len());
    for image in images {
        let valid_mime = matches!(
            image.mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        );
        let valid_data = !image.data_b64.is_empty()
            && image
                .data_b64
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
        if !valid_mime || !valid_data {
            return Err(session_error("Ungueltiges Bild"));
        }
        total = total.saturating_add(image.data_b64.len());
        out.push(SubmitImage {
            mime: image.mime,
            data_b64: image.data_b64,
        });
    }
    if total > MAX_SUBMIT_IMAGE_B64_BYTES {
        return Err(session_error("Bilder sind zusammen zu gross"));
    }
    Ok(out)
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
    fn clean_session_stream_eof_retries_with_a_bounded_backoff() {
        let mut recovery = AttachmentRecovery::default();
        assert_eq!(recovery.after_eof(), Some(Duration::from_millis(250)));
        assert_eq!(recovery.after_eof(), Some(Duration::from_millis(500)));
        assert_eq!(recovery.after_eof(), Some(Duration::from_secs(1)));
        assert_eq!(recovery.after_eof(), None);
        recovery.made_progress();
        assert_eq!(recovery.after_eof(), Some(Duration::from_millis(250)));
    }

    #[cfg(feature = "local-host")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_retained_session_reattaches_after_its_source_ends()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let backend = crate::local::open_local_with_model(
            crate::home::AppHome::open(dir.path())?,
            Arc::new(harw_core::EchoModelProvider::new("fixture")),
            requested_features(),
        )
        .await?;
        let shared = Arc::new(Shared::default());
        let link = Arc::new(Link {
            backend,
            stop: CancellationToken::new(),
            failure: watch::channel(None).0,
            attaching: Semaphore::new(1),
        });
        let id = link
            .backend
            .port()
            .create(CreateParams {
                workspace: None,
                title: Some("attachment fixture".into()),
                fork_from: None,
            })
            .await?
            .session_id;
        {
            let mut state = shared.state();
            state.link = Some(Arc::clone(&link));
            let session = state.sessions.entry(id.clone()).or_default();
            start_attachment(&shared, session, id.clone(), Arc::clone(&link));
        }
        let wait_until_online = |online| {
            let shared = Arc::clone(&shared);
            let id = id.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if shared
                            .state()
                            .session(&id)
                            .is_ok_and(|state| state.snapshot(&id, true).online == online)
                        {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
            }
        };
        wait_until_online(true).await?;

        // Inject source EOF without stopping the session, the connection, or
        // the retained Rust worker. Hold reattach until we observe unready.
        let permit = link.attaching.acquire().await?;
        link.backend.port().detach(id.clone()).await?;
        wait_until_online(false).await?;
        drop(permit);

        // The same subscription recovers; no second open_session call.
        let recovered = wait_until_online(true).await;
        link.stop.cancel();
        recovered?;
        Ok(())
    }

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
mod projects_tests {
    use super::*;
    use crate::MobileSessionState;

    fn session(id: &str, updated_at: &str, attached: u32) -> MobileSession {
        MobileSession {
            session_id: id.into(),
            title: None,
            state: MobileSessionState::Idle,
            attached,
            updated_at: updated_at.into(),
            model: None,
        }
    }

    #[test]
    fn requested_features_include_projects() {
        assert!(requested_features().iter().any(|f| f == features::PROJECTS));
    }

    #[test]
    fn requested_features_include_manage_and_stepup() {
        let requested = requested_features();
        assert!(requested.iter().any(|f| f == features::MANAGE));
        assert!(requested.iter().any(|f| f == features::STEPUP));
    }

    #[test]
    fn clear_link_forgets_holders() {
        let mut state = CoreState::default();
        remember_holder(
            &mut state,
            SessionId::try_from_str("sess-a1").expect("id"),
            None,
        );
        state.clear_link("offline");
        assert!(state.holders.is_empty());
    }

    #[test]
    fn legacy_project_page_from_list() {
        let page = legacy_project_page(&[
            session("sess-a1", "2026-01-02T00:00:00Z", 1),
            session("sess-a2", "2026-03-04T00:00:00Z", 0),
        ]);
        assert!(page.legacy);
        assert_eq!(page.projects.len(), 1);
        let project = &page.projects[0];
        assert_eq!(project.project_key, "");
        assert_eq!(project.label, "Ohne Projekt");
        assert_eq!(project.session_count, 2);
        assert_eq!(project.active_count, 1);
        assert_eq!(project.last_activity, "2026-03-04T00:00:00Z");
        assert_eq!(page.share.mode, MobileShareMode::Off);
        assert_eq!(page.share.tier, MobileTier::Observer);
        assert!(legacy_project_page(&[]).projects.is_empty());
    }

    #[test]
    fn legacy_session_page_filters_by_project() {
        let list = || vec![session("sess-a1", "2026-01-02T00:00:00Z", 0)];
        let all = legacy_session_page(list(), None, true);
        assert!(all.legacy && all.next_cursor.is_none());
        assert_eq!(all.sessions.len(), 1);
        assert!(all.sessions[0].continuable);
        assert!(matches!(all.sessions[0].origin, MobileRowOrigin::Hosted));
        assert_eq!(
            legacy_session_page(list(), Some(""), false).sessions.len(),
            1
        );
        assert!(
            legacy_session_page(list(), Some("proj-x"), true)
                .sessions
                .is_empty()
        );
    }

    #[test]
    fn holder_map_is_bounded() {
        let mut state = CoreState::default();
        for index in 0..=MAX_HOLDERS {
            let id = SessionId::try_from_str(format!("sess-{index}")).expect("id");
            remember_holder(&mut state, id, None);
        }
        assert!(state.holders.len() <= MAX_HOLDERS);
    }
}

#[cfg(test)]
mod route_failure_tests {
    use super::*;

    fn connect(detail: &str) -> RemoteError {
        RemoteError::Connect(detail.into())
    }

    #[test]
    fn initial_onion_retry_is_bounded_even_if_network_wakes_reset_backoff() {
        let failures = [(ConnectionRoute::Onion, connect("tor: tor bootstrap did not finish within 120s"))];
        let kind = attempt_failure(&failures);
        assert!(should_retry_dial(true, 0, 0, &failures, &kind));
        assert!(should_retry_dial(true, 1, 0, &failures, &kind));
        assert!(!should_retry_dial(true, 2, 0, &failures, &kind));
        assert!(should_retry_dial(false, 0, 7, &failures, &kind));
        assert!(!should_retry_dial(false, 0, 8, &failures, &kind));
    }

    #[test]
    fn initial_retry_never_overrides_refusal_or_direct_only_policy() {
        let direct = [(ConnectionRoute::Direct, connect("tcp connect: refused"))];
        assert!(!should_retry_dial(true, 0, 0, &direct, &attempt_failure(&direct)));
        let refused = [
            (ConnectionRoute::Onion, connect("tor: tor bootstrap failed: x")),
            (ConnectionRoute::Direct, connect("upgrade: peer did not switch protocols (status 403)")),
        ];
        assert!(!should_retry_dial(true, 0, 0, &refused, &attempt_failure(&refused)));
    }

    #[test]
    fn diagnostic_stage_uses_fixed_labels_and_unknown_for_unclassified_errors() {
        assert_eq!(onion_failure_stage(&connect("tor: tor bootstrap failed: private")), OnionFailureStage::TorBootstrap);
        assert_eq!(onion_failure_stage(&connect("tor: onion connect did not finish within 120s")), OnionFailureStage::OnionStream);
        assert_eq!(onion_failure_stage(&connect("node transport: node handshake: private")), OnionFailureStage::NodeTransport);
        assert_eq!(onion_failure_stage(&connect("upgrade: private")), OnionFailureStage::Upgrade);
        assert_eq!(onion_failure_stage(&connect("session.hello failed: private")), OnionFailureStage::SessionHello);
        assert_eq!(onion_failure_stage(&connect("unclassified")), OnionFailureStage::Unknown);
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
                assert_eq!(
                    network_action(phase, true, retryable),
                    NetworkAction::WakeBackoff
                );
            }
        }
    }

    #[test]
    fn network_action_ignores_dialing_without_run() {
        for phase in [ConnectionPhase::Connecting, ConnectionPhase::Reconnecting] {
            for retryable in [false, true] {
                assert_eq!(
                    network_action(phase, false, retryable),
                    NetworkAction::Ignore
                );
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

fn displayed_fork_rows(
    displayed: Vec<crate::TranscriptEntry>,
) -> Result<Vec<harw_protocol::session_wire::DisplayedForkRowV1>, MobileError> {
    use crate::TranscriptKind;
    use harw_protocol::session_wire::{
        DISPLAYED_FORK_MAX_ROWS, DisplayedForkRoleV1, DisplayedForkRowV1, displayed_fork_body_v1,
    };
    let stale = || MobileError::Denied {
        detail: "The displayed rows cannot be verified; nothing was copied".into(),
    };
    if !(1..=DISPLAYED_FORK_MAX_ROWS).contains(&displayed.len()) {
        return Err(stale());
    }
    let mut ids = std::collections::HashSet::with_capacity(displayed.len());
    let total = displayed.iter().try_fold(0usize, |bytes, entry| {
        let bytes = bytes.checked_add(entry.text.len())?;
        entry.images.iter().try_fold(bytes, |bytes, image| bytes.checked_add(image.len()))
    });
    if total.is_none_or(|bytes| bytes > 2 * 1024 * 1024) {
        return Err(MobileError::Denied { detail: "The displayed chat exceeds the fork limit; nothing was copied".into() });
    }
    let mut rows = Vec::with_capacity(displayed.len());
    for entry in displayed {
        if entry.id.is_empty() || entry.id.starts_with("mobile-") || !ids.insert(entry.id.clone()) {
            return Err(stale());
        }
        let role = match entry.kind {
            TranscriptKind::User => DisplayedForkRoleV1::User,
            TranscriptKind::Assistant => DisplayedForkRoleV1::Assistant,
            _ => return Err(stale()),
        };
        let media = entry.images.iter().map(|reference| serde_json::from_str::<harw_protocol::MediaRef>(reference))
            .collect::<Result<Vec<_>, _>>().map_err(|_| MobileError::Denied {
                detail: "A displayed attachment could not be verified; nothing was copied".into(),
            })?;
        let wire = displayed_fork_body_v1(&entry.text, &media).map_err(|_| stale())?;
        rows.push(DisplayedForkRowV1 { item_id: entry.id, role, body_digest: *blake3::hash(&wire).as_bytes() });
    }
    Ok(rows)
}
