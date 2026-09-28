//! HTTP/1 over an `AF_UNIX` socket: routing, peer authentication, graceful
//! shutdown.
//!
//! # Transport
//! Same substrate as `harw-web` and `harw-auth-hub`: Hyper HTTP/1 on a
//! `tokio::net::UnixListener`. The caller is identified solely by the
//! kernel-attested `SO_PEERCRED` of the accepted stream
//! (`UnixStream::peer_cred`), read once directly after `accept()`. A
//! connection whose credentials cannot be read is dropped before any HTTP is
//! parsed. There is no bearer token and no identity in headers or bodies.
//!
//! # Socket
//! Bound at the configured path (default `/run/harw/infra/security.sock`,
//! drift report D2) and chmod'ed to [`SOCKET_MODE`] (`0660`) right after
//! `bind()`. The parent directory must belong to the service user and not be
//! world-writable (tmpfiles/deploy concern); that closes the window between
//! `bind()` and `chmod()`. A stale socket file is only removed after a
//! connect probe proves nobody listens; anything that is not a socket is
//! never touched.
//!
//! With `--systemd-socket` ([`systemd_listener`], [`run_systemd`]) the hub
//! instead adopts the one `ListenStream=` socket systemd passed
//! (`deploy/systemd/harw-security-hub.socket`); systemd owns path, mode
//! (`0660`) and group (`harw-security`), and the hub neither binds, chmods
//! nor removes a socket file. There is no fallback to binding the path.
//!
//! # API (protocol 1)
//! | Method | Path | Who | Result |
//! |---|---|---|---|
//! | `GET` | `/v1/health` | any peer | `{"status":"ok"}` |
//! | `GET` | `/v1/version` | any peer | service, version, protocol |
//! | `GET` | `/v1/capabilities` | any peer | descriptive only, never authority (§38) |
//! | `POST` | `/v1/contexts` | policy peers | `201` + context id + summary |
//! | `GET` | `/v1/contexts/{id}` | verifiers, requester | `200` active / `410` expired or revoked / `404` |
//! | `DELETE` | `/v1/contexts/{id}` | verifiers, requester | `200` revoked / `410` expired / `404` |
//! | `GET` | `/v1/posture` | policy peers, verifiers | identity + recent DoD findings for this host |
//!
//! A peer that is neither a verifier nor the requester of a context gets
//! `404` for it, so context ids cannot be probed.
//!
//! # Shutdown
//! [`serve`] stops accepting when its shutdown future resolves, asks every
//! open connection to finish its in-flight request (`graceful_shutdown`),
//! waits up to [`SHUTDOWN_GRACE`], then aborts the rest. [`run`] and
//! [`run_systemd`] wire the shutdown future to `SIGTERM`/`SIGINT`; only
//! [`run`] removes the socket file (it bound it).

use std::convert::Infallible;
use std::future::Future;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

use harw_types::{
    Clock, HostId, ImpactSeverity, SecurityContextId, SecurityContextIssuer, SystemClock,
};

use crate::config::HubConfig;
use crate::error::HubError;
use crate::findings::{DodFindingSource, FindingFilter, JsonlFindingSource};
use crate::policy::{ContextRequest, PeerIdentity, PolicyDenied, PolicyEngine};
use crate::systemd;
use crate::table::{ContextTable, Lookup, RevokeOutcome, TableFull};

/// Service name reported by `/v1/version` and `/v1/capabilities`.
pub const SERVICE_NAME: &str = "harw-security-hub";
/// Wire protocol version.
pub const PROTOCOL_VERSION: u32 = 1;
/// Mode of the bound socket file.
pub const SOCKET_MODE: u32 = 0o660;
/// Largest accepted request body.
pub const MAX_BODY_BYTES: usize = 16 * 1024;
/// Longest accepted context id in a path.
pub const MAX_CONTEXT_ID_LEN: usize = 128;
/// How long open connections get to finish after shutdown starts.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
const STALE_SOCKET_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
const PURGE_INTERVAL: Duration = Duration::from_secs(30);
const CONTEXTS_PATH: &str = "/v1/contexts";

/// The read-only DoD wiring of a hub.
struct FindingsWiring {
    source: Arc<dyn DodFindingSource>,
    min_severity: ImpactSeverity,
    window: SignedDuration,
}

/// Shared state behind every connection.
pub struct HubState {
    engine: PolicyEngine,
    table: Mutex<ContextTable>,
    clock: Arc<dyn Clock>,
    host: Option<HostId>,
    findings: Option<FindingsWiring>,
}

impl std::fmt::Debug for HubState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubState")
            .field("issuer", &self.engine.issuer_name())
            .field("host", &self.host)
            .field("findings", &self.findings.is_some())
            .finish_non_exhaustive()
    }
}

impl HubState {
    /// State without DoD correlation.
    #[must_use]
    pub fn new(engine: PolicyEngine, table: ContextTable, clock: Arc<dyn Clock>) -> Self {
        Self {
            engine,
            table: Mutex::new(table),
            clock,
            host: None,
            findings: None,
        }
    }

    /// Sets this host's id (the correlation key for DoD findings).
    #[must_use]
    pub fn with_host(mut self, host: HostId) -> Self {
        self.host = Some(host);
        self
    }

    /// Attaches a read-only DoD finding source.
    #[must_use]
    pub fn with_findings(
        mut self,
        source: Arc<dyn DodFindingSource>,
        min_severity: ImpactSeverity,
        window: SignedDuration,
    ) -> Self {
        self.findings = Some(FindingsWiring {
            source,
            min_severity,
            window,
        });
        self
    }

    /// Builds the production state from a validated config.
    ///
    /// This is the hub's **single** production call site of
    /// [`SecurityContextIssuer::new_for_hub`]; the issuer moves into the
    /// [`PolicyEngine`] and never leaves it.
    ///
    /// # Errors
    /// [`HubError::Issuer`] or [`HubError::ConfigInvalid`].
    pub fn from_config(config: &HubConfig) -> Result<Self, HubError> {
        let issuer = SecurityContextIssuer::new_for_hub(config.server.issuer.clone())
            .map_err(|error| HubError::Issuer(error.to_string()))?;
        let engine = PolicyEngine::new(config.policy.clone(), issuer, config.server.node.clone())
            .map_err(HubError::ConfigInvalid)?;
        let mut state = Self::new(
            engine,
            ContextTable::with_capacity_limit(config.server.max_contexts),
            Arc::new(SystemClock),
        );
        if let Some(host) = &config.server.host {
            state = state.with_host(host.clone());
        }
        if let Some(findings) = &config.findings {
            let window =
                SignedDuration::from_secs(i64::try_from(findings.window_secs).unwrap_or(i64::MAX));
            state = state.with_findings(
                Arc::new(JsonlFindingSource::new(&findings.path, findings.limits())),
                findings.min_severity,
                window,
            );
        }
        Ok(state)
    }

    fn table(&self) -> MutexGuard<'_, ContextTable> {
        // A panic while holding the lock cannot leave the table logically
        // inconsistent (every mutation is a single map operation), so a
        // poisoned lock is recovered rather than propagated.
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn purge_expired(&self) -> usize {
        let now = self.clock.now();
        self.table().purge_expired(now)
    }
}

/// Binds the Unix socket at `path` with mode [`SOCKET_MODE`].
///
/// # Errors
/// [`HubError::NotASocket`] if something else occupies the path,
/// [`HubError::SocketInUse`] if a live process listens there, and
/// [`HubError::Socket`] for I/O failures.
pub async fn bind_listener(path: &Path) -> Result<UnixListener, HubError> {
    let socket_error = |source| HubError::Socket {
        path: path.to_path_buf(),
        source,
    };
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(HubError::NotASocket(path.to_path_buf()));
            }
            let probe =
                tokio::time::timeout(STALE_SOCKET_PROBE_TIMEOUT, UnixStream::connect(path)).await;
            match probe {
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(path).map_err(socket_error)?;
                }
                _ => return Err(HubError::SocketInUse(path.to_path_buf())),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(socket_error(error)),
    }
    let listener = UnixListener::bind(path).map_err(socket_error)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE))
        .map_err(socket_error)?;
    Ok(listener)
}

/// Adopts the single `AF_UNIX` listening socket systemd passed
/// (`LISTEN_PID`/`LISTEN_FDS`, see [`crate::systemd`]). Must run inside a
/// Tokio runtime.
///
/// # Errors
/// [`HubError::Systemd`] for an invalid activation environment or a
/// descriptor count other than one, [`HubError::Socket`] if the descriptor
/// cannot be registered with Tokio.
pub fn systemd_listener() -> Result<UnixListener, HubError> {
    let fd = systemd::acquire_listen_socket()?;
    let socket_error = |source| HubError::Socket {
        path: PathBuf::from("<systemd>"),
        source,
    };
    let std_listener = std::os::unix::net::UnixListener::from(fd);
    std_listener.set_nonblocking(true).map_err(socket_error)?;
    UnixListener::from_std(std_listener).map_err(socket_error)
}

/// Serves connections on `listener` until `shutdown` resolves.
///
/// # Errors
/// None today; transient `accept()` errors are logged and retried after a
/// short back-off. The `Result` is kept for future fatal conditions.
pub async fn serve<F>(
    listener: UnixListener,
    state: Arc<HubState>,
    shutdown: F,
) -> Result<(), HubError>
where
    F: Future<Output = ()>,
{
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut connections = JoinSet::new();
    let mut purge = tokio::time::interval(PURGE_INTERVAL);
    purge.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            () = &mut shutdown => break,
            _ = purge.tick() => {
                let purged = state.purge_expired();
                if purged > 0 {
                    tracing::debug!(purged, "purged expired security contexts");
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok((stream, _addr)) => stream,
                    Err(error) => {
                        tracing::warn!(%error, "accept() failed; backing off");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                        continue;
                    }
                };
                let peer = match stream.peer_cred() {
                    Ok(cred) => PeerIdentity::new(
                        cred.uid(),
                        cred.gid(),
                        cred.pid().and_then(|pid| u32::try_from(pid).ok()),
                    ),
                    Err(error) => {
                        tracing::warn!(%error, "SO_PEERCRED unavailable; dropping connection");
                        continue;
                    }
                };
                connections.spawn(serve_connection(
                    stream,
                    peer,
                    Arc::clone(&state),
                    stop_rx.clone(),
                ));
            }
        }
    }

    let _ = stop_tx.send(true);
    let drained = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_ok();
    if !drained {
        tracing::warn!("connections did not finish within the shutdown grace; aborting");
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    Ok(())
}

async fn serve_connection(
    stream: UnixStream,
    peer: PeerIdentity,
    state: Arc<HubState>,
    mut stop: watch::Receiver<bool>,
) {
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    let service = service_fn(move |request| handle(request, peer, Arc::clone(&state)));
    let connection = builder.serve_connection(TokioIo::new(stream), service);
    tokio::pin!(connection);
    let stopped = async move {
        let _stopped = stop.wait_for(|stopped| *stopped).await.is_ok();
    };
    tokio::select! {
        _ = connection.as_mut() => {}
        () = stopped => {
            connection.as_mut().graceful_shutdown();
            let _ = connection.await;
        }
    }
}

/// Builds the state from `config`, binds the socket, serves until
/// `SIGTERM`/`SIGINT`, then removes the socket file.
///
/// # Errors
/// Everything from [`HubState::from_config`], [`bind_listener`] and signal
/// registration.
pub async fn run(config: HubConfig) -> Result<(), HubError> {
    let state = Arc::new(HubState::from_config(&config)?);
    let shutdown = shutdown_signal()?;
    let listener = bind_listener(&config.server.socket).await?;
    tracing::info!(
        socket = %config.server.socket.display(),
        issuer = state.engine.issuer_name(),
        "harw-security-hub listening"
    );
    let result = serve(listener, state, shutdown).await;
    if let Err(error) = std::fs::remove_file(&config.server.socket) {
        tracing::warn!(%error, "could not remove socket file on shutdown");
    }
    tracing::info!("harw-security-hub stopped");
    result
}

/// Builds the state from `config`, adopts the socket systemd passed
/// ([`systemd_listener`]), serves until `SIGTERM`/`SIGINT`. `server.socket`
/// from the config is ignored and no socket file is touched: the socket unit
/// owns it.
///
/// # Errors
/// Everything from [`HubState::from_config`], [`systemd_listener`] and
/// signal registration.
pub async fn run_systemd(config: HubConfig) -> Result<(), HubError> {
    let state = Arc::new(HubState::from_config(&config)?);
    let shutdown = shutdown_signal()?;
    let listener = systemd_listener()?;
    tracing::info!(
        socket = "<systemd>",
        issuer = state.engine.issuer_name(),
        "harw-security-hub listening"
    );
    let result = serve(listener, state, shutdown).await;
    tracing::info!("harw-security-hub stopped");
    result
}

fn shutdown_signal() -> Result<impl Future<Output = ()>, HubError> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate()).map_err(HubError::Signal)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(HubError::Signal)?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => tracing::info!("SIGTERM received; shutting down"),
            _ = interrupt.recv() => tracing::info!("SIGINT received; shutting down"),
        }
    })
}

// ---------------------------------------------------------------------------
// Request handling
// ---------------------------------------------------------------------------

type HubResponse = Response<Full<Bytes>>;

async fn handle(
    request: Request<Incoming>,
    peer: PeerIdentity,
    state: Arc<HubState>,
) -> Result<HubResponse, Infallible> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let response = match (method, path.as_str()) {
        (Method::GET, "/v1/health") => json_response(StatusCode::OK, &json!({"status": "ok"})),
        (Method::GET, "/v1/version") => json_response(
            StatusCode::OK,
            &json!({
                "service": SERVICE_NAME,
                "version": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL_VERSION,
            }),
        ),
        (Method::GET, "/v1/capabilities") => capabilities(&state),
        (Method::POST, CONTEXTS_PATH) => issue_context(request, peer, &state).await,
        (Method::GET, "/v1/posture") => posture(peer, &state).await,
        (method, path) => match path.strip_prefix("/v1/contexts/") {
            Some(raw_id) => match parse_context_id(raw_id) {
                Some(id) => match method {
                    Method::GET => verify_context(&id, peer, &state),
                    Method::DELETE => revoke_context(&id, peer, &state),
                    _ => error_response(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
                },
                None => error_response(StatusCode::BAD_REQUEST, "invalid_context_id"),
            },
            None if is_known_path(path) => {
                error_response(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
            }
            None => error_response(StatusCode::NOT_FOUND, "not_found"),
        },
    };
    Ok(response)
}

fn is_known_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/health" | "/v1/version" | "/v1/capabilities" | CONTEXTS_PATH | "/v1/posture"
    )
}

fn parse_context_id(raw: &str) -> Option<SecurityContextId> {
    let valid = !raw.is_empty()
        && raw.len() <= MAX_CONTEXT_ID_LEN
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if valid {
        SecurityContextId::try_from_str(raw).ok()
    } else {
        None
    }
}

fn capabilities(state: &HubState) -> HubResponse {
    json_response(
        StatusCode::OK,
        &json!({
            "service": SERVICE_NAME,
            "protocol": PROTOCOL_VERSION,
            "authentication": ["peer_credential"],
            "operations": [
                "context.issue",
                "context.verify",
                "context.revoke",
                "posture.read",
            ],
            "dod_correlation": state.findings.is_some(),
        }),
    )
}

async fn issue_context(
    request: Request<Incoming>,
    peer: PeerIdentity,
    state: &HubState,
) -> HubResponse {
    let body = match read_body(request).await {
        Ok(body) => body,
        Err(response) => return *response,
    };
    let context_request = if body.iter().all(u8::is_ascii_whitespace) {
        ContextRequest::default()
    } else {
        match serde_json::from_slice::<ContextRequest>(&body) {
            Ok(parsed) => parsed,
            Err(_) => return error_response(StatusCode::BAD_REQUEST, "invalid_request"),
        }
    };

    let now = state.clock.now();
    let context = match state.engine.evaluate_at(peer, context_request, now) {
        Ok(context) => context,
        Err(denied) => {
            tracing::warn!(uid = peer.uid, reason = %denied, "security context denied");
            let status = match denied {
                PolicyDenied::InvalidTtl => StatusCode::BAD_REQUEST,
                PolicyDenied::Issue(_) => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::FORBIDDEN,
            };
            return error_response(status, denied.code());
        }
    };
    let summary = context.summary();
    let id = context.id().clone();
    if state.table().insert(context, peer.uid, now) == Err(TableFull) {
        tracing::warn!("context table full; refusing to issue");
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "table_full");
    }
    tracing::info!(
        uid = peer.uid,
        principal = %summary.principal_id,
        context = %id,
        expires_at = %summary.expires_at,
        "security context issued"
    );
    json_response(
        StatusCode::CREATED,
        &json!({"context_id": id, "context": summary}),
    )
}

fn may_access(state: &HubState, table: &ContextTable, id: &SecurityContextId, uid: u32) -> bool {
    state.engine.is_verifier(uid) || table.owner_of(id) == Some(uid)
}

fn verify_context(id: &SecurityContextId, peer: PeerIdentity, state: &HubState) -> HubResponse {
    let now = state.clock.now();
    let table = state.table();
    if !may_access(state, &table, id, peer.uid) {
        return error_response(StatusCode::NOT_FOUND, "not_found");
    }
    match table.lookup(id, now) {
        Lookup::Active(context) => json_response(
            StatusCode::OK,
            &json!({"status": "active", "context": context.summary()}),
        ),
        Lookup::Expired => json_response(StatusCode::GONE, &json!({"status": "expired"})),
        Lookup::Revoked => json_response(StatusCode::GONE, &json!({"status": "revoked"})),
        Lookup::Unknown => error_response(StatusCode::NOT_FOUND, "not_found"),
    }
}

fn revoke_context(id: &SecurityContextId, peer: PeerIdentity, state: &HubState) -> HubResponse {
    let now = state.clock.now();
    let mut table = state.table();
    if !may_access(state, &table, id, peer.uid) {
        return error_response(StatusCode::NOT_FOUND, "not_found");
    }
    match table.revoke(id, now) {
        RevokeOutcome::Revoked | RevokeOutcome::AlreadyRevoked => {
            tracing::info!(uid = peer.uid, context = %id, "security context revoked");
            json_response(StatusCode::OK, &json!({"status": "revoked"}))
        }
        RevokeOutcome::Expired => json_response(StatusCode::GONE, &json!({"status": "expired"})),
        RevokeOutcome::Unknown => error_response(StatusCode::NOT_FOUND, "not_found"),
    }
}

async fn posture(peer: PeerIdentity, state: &HubState) -> HubResponse {
    let known = state.engine.is_known_peer(peer.uid);
    if !known && !state.engine.is_verifier(peer.uid) {
        return error_response(StatusCode::FORBIDDEN, "unknown_peer");
    }
    let dod = match (&state.findings, &state.host) {
        (Some(wiring), Some(host)) => {
            let now: Timestamp = state.clock.now();
            let since = now.checked_sub(wiring.window).unwrap_or(Timestamp::MIN);
            let filter = FindingFilter {
                host: host.clone(),
                min_severity: wiring.min_severity,
                since,
            };
            let source = Arc::clone(&wiring.source);
            match tokio::task::spawn_blocking(move || source.recent(&filter)).await {
                Ok(Ok(batch)) => json!({
                    "status": "ok",
                    "min_severity": wiring.min_severity,
                    "since": since,
                    "findings": batch.findings,
                    "skipped_malformed": batch.skipped_malformed,
                    "skipped_oversize": batch.skipped_oversize,
                    "truncated": batch.truncated,
                }),
                Ok(Err(error)) => {
                    tracing::warn!(%error, "DoD finding source unavailable");
                    json!({"status": "unavailable", "findings": []})
                }
                Err(error) => {
                    tracing::warn!(%error, "DoD finding reader task failed");
                    json!({"status": "unavailable", "findings": []})
                }
            }
        }
        _ => json!({"status": "not_configured", "findings": []}),
    };
    json_response(
        StatusCode::OK,
        &json!({
            "peer": {"uid": peer.uid, "gid": peer.gid, "pid": peer.pid},
            "principal_id": state.engine.principal_id_for(peer.uid),
            "host": state.host,
            "dod": dod,
        }),
    )
}

async fn read_body(request: Request<Incoming>) -> Result<Bytes, Box<HubResponse>> {
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return Err(Box::new(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
        )));
    }
    let limited = Limited::new(request.into_body(), MAX_BODY_BYTES);
    match tokio::time::timeout(BODY_READ_TIMEOUT, limited.collect()).await {
        Err(_) => Err(Box::new(error_response(
            StatusCode::REQUEST_TIMEOUT,
            "body_timeout",
        ))),
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(error)) if error.downcast_ref::<LengthLimitError>().is_some() => Err(Box::new(
            error_response(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"),
        )),
        Ok(Err(_)) => Err(Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_body",
        ))),
    }
}

fn json_response(status: StatusCode, value: &Value) -> HubResponse {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn error_response(status: StatusCode, code: &str) -> HubResponse {
    json_response(status, &json!({"error": code}))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::header::HOST;
    use hyper::{Method, Request, StatusCode};
    use hyper_util::rt::TokioIo;
    use jiff::{SignedDuration, Timestamp};
    use serde_json::Value;
    use tokio::net::UnixStream;
    use tokio::sync::oneshot;

    use harw_types::{Clock, HostId, ImpactSeverity, NodeId, SecurityContextIssuer};

    use super::{HubState, SOCKET_MODE, bind_listener, parse_context_id, serve};
    use crate::findings::{FindingLimits, JsonlFindingSource};
    use crate::policy::{PolicyEngine, SecurityPolicy};
    use crate::table::ContextTable;
    use crate::test_support::{TestError, TestResult, ctx};

    const DEADLINE: Duration = Duration::from_secs(10);

    /// Test clock that can be advanced.
    struct ManualClock(Mutex<Timestamp>);

    impl ManualClock {
        fn advance(&self, by: SignedDuration) {
            let mut now = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            *now += by;
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Timestamp {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    /// Effective uid of this test process, read the same way the server
    /// reads it: `SO_PEERCRED` on a connected socket pair.
    fn own_uid() -> TestResult<u32> {
        let (left, _right) = UnixStream::pair().map_err(ctx("socketpair"))?;
        Ok(left.peer_cred().map_err(ctx("peer_cred"))?.uid())
    }

    fn state_for(policy_toml: &str, clock: Arc<ManualClock>) -> TestResult<HubState> {
        let policy = SecurityPolicy::from_toml_str(policy_toml).map_err(ctx("policy"))?;
        let issuer = SecurityContextIssuer::new_for_hub("security-hub").map_err(ctx("issuer"))?;
        let node = NodeId::try_from_str("node-1").map_err(ctx("node"))?;
        let engine = PolicyEngine::new(policy, issuer, Some(node)).map_err(ctx("engine"))?;
        Ok(HubState::new(engine, ContextTable::default(), clock))
    }

    fn peer_policy(uid: u32) -> String {
        format!(
            r#"
            max_ttl_secs = 600
            [[peers]]
            uid = {uid}
            tenant = "acme"
            trust_zone = "local"
            principal = {{ kind = "human", id = "alice", surface = "cli", tier = "owner" }}
            "#
        )
    }

    async fn call(
        socket: &Path,
        method: Method,
        uri: &str,
        body: &str,
    ) -> TestResult<(StatusCode, Value)> {
        let exchange = async {
            let stream = UnixStream::connect(socket).await.map_err(ctx("connect"))?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(ctx("handshake"))?;
            let driver = tokio::spawn(connection);
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .header(HOST, "localhost")
                .body(Full::new(Bytes::from(body.to_owned())))
                .map_err(ctx("build request"))?;
            let response = sender.send_request(request).await.map_err(ctx("send"))?;
            let status = response.status();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(ctx("read body"))?
                .to_bytes();
            driver.abort();
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).map_err(ctx("json body"))?
            };
            Ok::<_, TestError>((status, value))
        };
        tokio::time::timeout(DEADLINE, exchange)
            .await
            .map_err(ctx("request deadline"))?
    }

    fn field<'a>(value: &'a Value, pointer: &'static str) -> TestResult<&'a Value> {
        value.pointer(pointer).ok_or(TestError::Missing(pointer))
    }

    fn start_clock() -> TestResult<Arc<ManualClock>> {
        let now = Timestamp::from_second(1_790_000_000).map_err(ctx("timestamp"))?;
        Ok(Arc::new(ManualClock(Mutex::new(now))))
    }

    #[tokio::test]
    async fn test_e2e_issue_verify_revoke_and_expiry() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let socket = dir.path().join("security.sock");
        let clock = start_clock()?;
        let state = Arc::new(state_for(&peer_policy(own_uid()?), Arc::clone(&clock))?);
        let listener = bind_listener(&socket).await.map_err(ctx("bind"))?;

        let mode = std::fs::metadata(&socket)
            .map_err(ctx("socket metadata"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, SOCKET_MODE);

        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(serve(listener, state, async move {
            let _ = stop_rx.await;
        }));

        // Descriptive endpoints.
        let (status, health) = call(&socket, Method::GET, "/v1/health", "").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(field(&health, "/status")?, "ok");
        let (status, version) = call(&socket, Method::GET, "/v1/version", "").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(field(&version, "/service")?, "harw-security-hub");
        let (status, caps) = call(&socket, Method::GET, "/v1/capabilities", "").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(field(&caps, "/dod_correlation")?.as_bool(), Some(false));

        // Issue (narrowed to a workspace).
        let (status, issued) = call(
            &socket,
            Method::POST,
            "/v1/contexts",
            r#"{"workspace":"ws-1","ttl_secs":60}"#,
        )
        .await?;
        assert_eq!(status, StatusCode::CREATED, "{issued}");
        let id = field(&issued, "/context_id")?
            .as_str()
            .ok_or(TestError::Missing("context_id string"))?
            .to_owned();
        assert_eq!(field(&issued, "/context/principal_id")?, "alice");
        assert_eq!(field(&issued, "/context/workspace")?, "ws-1");
        assert_eq!(field(&issued, "/context/tenant")?, "acme");
        assert_eq!(field(&issued, "/context/auth_strength")?, "peer_credential");
        assert_eq!(field(&issued, "/context/node")?, "node-1");

        // Verify.
        let uri = format!("/v1/contexts/{id}");
        let (status, verified) = call(&socket, Method::GET, &uri, "").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(field(&verified, "/status")?, "active");
        assert_eq!(field(&verified, "/context/id")?.as_str(), Some(id.as_str()));

        // Revoke, then verification reports revoked.
        let (status, revoked) = call(&socket, Method::DELETE, &uri, "").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(field(&revoked, "/status")?, "revoked");
        let (status, after) = call(&socket, Method::GET, &uri, "").await?;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(field(&after, "/status")?, "revoked");

        // A second context expires with the clock.
        let (status, second) = call(&socket, Method::POST, "/v1/contexts", "").await?;
        assert_eq!(status, StatusCode::CREATED);
        let second_id = field(&second, "/context_id")?
            .as_str()
            .ok_or(TestError::Missing("second id"))?
            .to_owned();
        clock.advance(SignedDuration::from_secs(301));
        let (status, expired) = call(
            &socket,
            Method::GET,
            &format!("/v1/contexts/{second_id}"),
            "",
        )
        .await?;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(field(&expired, "/status")?, "expired");

        // Broadening and malformed requests.
        let (status, broad) = call(
            &socket,
            Method::POST,
            "/v1/contexts",
            r#"{"tenant":"other"}"#,
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(field(&broad, "/error")?, "invalid_request");
        let (status, _) = call(&socket, Method::GET, "/v1/contexts/unknown-id", "").await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&socket, Method::GET, "/v1/contexts/bad%2Fid", "").await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(&socket, Method::PUT, "/v1/health", "").await?;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        let (status, _) = call(&socket, Method::GET, "/v2/nothing", "").await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let oversized = format!(r#"{{"workspace":"{}"}}"#, "w".repeat(20 * 1024));
        let (status, _) = call(&socket, Method::POST, "/v1/contexts", &oversized).await?;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

        // Graceful shutdown.
        let _ = stop_tx.send(());
        tokio::time::timeout(DEADLINE, server)
            .await
            .map_err(ctx("server stops in time"))?
            .map_err(ctx("server task"))?
            .map_err(ctx("serve result"))?;
        Ok(())
    }

    #[tokio::test]
    async fn test_e2e_unknown_peer_is_denied() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let socket = dir.path().join("security.sock");
        let other_uid = own_uid()?.wrapping_add(1);
        let state = Arc::new(state_for(&peer_policy(other_uid), start_clock()?)?);
        let listener = bind_listener(&socket).await.map_err(ctx("bind"))?;
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(serve(listener, state, async move {
            let _ = stop_rx.await;
        }));

        let (status, denied) = call(&socket, Method::POST, "/v1/contexts", "{}").await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(field(&denied, "/error")?, "unknown_peer");
        let (status, _) = call(&socket, Method::GET, "/v1/posture", "").await?;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let _ = stop_tx.send(());
        tokio::time::timeout(DEADLINE, server)
            .await
            .map_err(ctx("server stops"))?
            .map_err(ctx("server task"))?
            .map_err(ctx("serve result"))?;
        Ok(())
    }

    #[tokio::test]
    async fn test_e2e_posture_attaches_host_findings() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let socket = dir.path().join("security.sock");
        let export = dir.path().join("findings.jsonl");
        let clock = start_clock()?;
        let recent = clock.now() - SignedDuration::from_mins(5);
        let old = clock.now() - SignedDuration::from_hours(48);
        let lines = [
            format!(
                r#"{{"finding_id":"f-old","host":"host-a","rule_id":"r","severity":"critical","summary":"old","observed_at":"{old}"}}"#
            ),
            format!(
                r#"{{"finding_id":"f-low","host":"host-a","rule_id":"r","severity":"low","summary":"low","observed_at":"{recent}"}}"#
            ),
            format!(
                r#"{{"finding_id":"f-other","host":"host-b","rule_id":"r","severity":"critical","summary":"b","observed_at":"{recent}"}}"#
            ),
            format!(
                r#"{{"finding_id":"f-hit","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"hit","observed_at":"{recent}"}}"#
            ),
            "garbage".to_owned(),
        ];
        std::fs::write(&export, lines.join("\n")).map_err(ctx("write export"))?;

        let host = HostId::try_from_str("host-a").map_err(ctx("host"))?;
        let state = state_for(&peer_policy(own_uid()?), Arc::clone(&clock))?
            .with_host(host)
            .with_findings(
                Arc::new(JsonlFindingSource::new(&export, FindingLimits::default())),
                ImpactSeverity::High,
                SignedDuration::from_hours(24),
            );
        let listener = bind_listener(&socket).await.map_err(ctx("bind"))?;
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(serve(listener, Arc::new(state), async move {
            let _ = stop_rx.await;
        }));

        let (status, posture) = call(&socket, Method::GET, "/v1/posture", "").await?;
        assert_eq!(status, StatusCode::OK, "{posture}");
        assert_eq!(field(&posture, "/principal_id")?, "alice");
        assert_eq!(field(&posture, "/host")?, "host-a");
        assert_eq!(field(&posture, "/dod/status")?, "ok");
        assert_eq!(field(&posture, "/dod/skipped_malformed")?.as_u64(), Some(1));
        let findings = field(&posture, "/dod/findings")?
            .as_array()
            .ok_or(TestError::Missing("findings array"))?;
        assert_eq!(findings.len(), 1);
        assert_eq!(field(&posture, "/dod/findings/0/finding_id")?, "f-hit");

        // The export disappearing degrades to "unavailable", not an error.
        std::fs::remove_file(&export).map_err(ctx("remove export"))?;
        let (status, degraded) = call(&socket, Method::GET, "/v1/posture", "").await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(field(&degraded, "/dod/status")?, "unavailable");

        let _ = stop_tx.send(());
        tokio::time::timeout(DEADLINE, server)
            .await
            .map_err(ctx("server stops"))?
            .map_err(ctx("server task"))?
            .map_err(ctx("serve result"))?;
        Ok(())
    }

    #[tokio::test]
    async fn test_bind_refuses_foreign_file_and_replaces_stale_socket() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let regular = dir.path().join("occupied");
        std::fs::write(&regular, b"keep me").map_err(ctx("write"))?;
        match bind_listener(&regular).await {
            Err(crate::error::HubError::NotASocket(_)) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert_eq!(
            std::fs::read(&regular).map_err(ctx("read back"))?,
            b"keep me"
        );

        // A socket file nobody listens on is stale and gets replaced.
        let stale = dir.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).map_err(ctx("stale bind"))?);
        let _listener = bind_listener(&stale).await.map_err(ctx("rebind stale"))?;

        // A live socket is never taken over.
        match bind_listener(&stale).await {
            Err(crate::error::HubError::SocketInUse(_)) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_parse_context_id_rejects_path_tricks() {
        assert!(parse_context_id("0b7c-uuid_like").is_some());
        for bad in ["", "a/b", "..", "a b", "%2F", &"x".repeat(129)] {
            assert!(parse_context_id(bad).is_none(), "{bad}");
        }
    }
}
