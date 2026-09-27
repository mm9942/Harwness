//! Trusted boundary between untrusted MCP requests and durable job state.
//!
//! Transport sessions prove only protocol continuity. A configured ingress
//! resolver creates [`McpRequestContext`] from an authenticated identity; the
//! client-provided `clientInfo` field is never used as authority.
//!
//! # Admission (F-158-Parität)
//! [`DurableMcpSupervisor::submit_job`] muss dieselbe Admission-Disziplin wie
//! `harw_core::admission::JobAdmissionService::admit` durchsetzen: Idempotenz,
//! Ratenbegrenzung und eine Budget-Obergrenze, die nie umgangen werden kann.
//! Diese Datei kann `JobAdmissionService` selbst **nicht** verwenden — der
//! Dienst braucht eine `Arc<harw_authority::WorkspaceRegistry>` und eine
//! `JobAdmissionPolicy`-Implementierung, die an dieser MCP-Einreiseseite nicht
//! existieren, und ihre Beschaffung würde die Composition Root in
//! `harw-cli/src/main.rs` verändern — außerhalb des für diesen Fix erlaubten
//! Änderungsbereichs (`harw-mcp-server/`). Die Budget-Obergrenze gab es hier
//! bereits ([`McpJobBudgetLimits`]); Idempotenz und Ratenbegrenzung sind daher
//! als **eigenständige Zweitimplementierung** direkt in diesem Modul
//! nachgebildet, mit denselben zugrunde liegenden Bausteinen wie der Dienst
//! (BLAKE3 über [`harw_types::ContentDigest`] für die deterministische
//! Idempotenz-ID, ein gleitendes Zeitfenster pro `(Tenant, Submitter)` für die
//! Ratenbegrenzung) — siehe [`McpIdempotencyKey`], [`mcp_idempotent_work_id`]
//! und [`McpSubmissionRateLimiter`]. Eine Vereinheitlichung bräuchte: (1) die
//! Composition Root in `harw-cli` reicht eine `Arc<WorkspaceRegistry>` und
//! eine MCP-taugliche `JobAdmissionPolicy` an `DurableMcpSupervisor` durch,
//! und (2) `harw_core::admission::SubmissionLimiter`/`idempotent_work_id`
//! werden `pub(crate)` -> `pub` bzw. in eine gemeinsame Crate ausgelagert,
//! damit beide Seiten denselben Code statt nur dasselbe Verfahren teilen.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use harw_job_core::{
    Budget, Job, JobCompletion, JobKind, JobScope, JobState, Lease, RetryPolicy, StoredJob,
};
use harw_session_store::{CancelRequest, JobStore, SessionStoreError};
use harw_types::{ApprovalActor, ContentDigest, TenantId, WorkId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type McpSupervisorFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, McpSupervisorError>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpJobCapability {
    SubmitOwn,
    ReadOwn,
    ReadWorkspace,
    CancelOwn,
    CancelWorkspace,
}

/// Kinds an MCP principal may submit. Custom runtime kinds remain a
/// composition-owned policy decision and cannot be minted from MCP input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpSubmittedJobKind {
    Dream,
    Worker,
}

impl McpSubmittedJobKind {
    fn into_runtime_kind(self) -> JobKind {
        match self {
            Self::Dream => JobKind::Dream,
            Self::Worker => JobKind::Worker,
        }
    }
}

/// Harte Server-Obergrenze für Modell-Tokens eines per MCP eingereichten Jobs.
///
/// Gilt auch dann, wenn der Client kein Budget angibt: ein MCP-Job ist nie
/// unbegrenzt (Register F-123 / P0.12). `SubmitOwn` darf damit höchstens
/// diese Menge Modellzugang auf Serverkosten verbrauchen.
pub const MCP_JOB_MAX_TOKENS: u64 = 200_000;

/// Harte Server-Obergrenze für die Wanduhr eines per MCP eingereichten Jobs,
/// in Sekunden.
///
/// Der Job-Worker (`harw serve`) begrenzt die tatsächliche Laufzeit zusätzlich
/// auf die verbleibende Gültigkeit seiner Lease, solange es keine
/// Lease-Erneuerung gibt (Folgearbeit W4a).
pub const MCP_JOB_MAX_WALL_SECONDS: i64 = 600;

/// Harte Server-Obergrenze für Tool-Aufrufe eines per MCP eingereichten Jobs.
pub const MCP_JOB_MAX_TOOL_CALLS: u32 = 64;

/// Serverseitige Obergrenzen, gegen die ein vom Client angefordertes
/// [`Budget`] aufgelöst wird.
///
/// # Description
/// Die Grenzen stammen nie aus einer Anfrage. [`Self::server_default`] liefert
/// die harten Konstanten [`MCP_JOB_MAX_TOKENS`], [`MCP_JOB_MAX_WALL_SECONDS`]
/// und [`MCP_JOB_MAX_TOOL_CALLS`]; eine Komposition darf über [`Self::new`]
/// nur *strengere* Grenzen setzen. So bleibt die Deckelung im Job-Worker, der
/// die harten Konstanten kennt, immer mindestens so weit wie die hier
/// zugelassenen Budgets.
///
/// # Concurrency
/// Unveränderliche Daten; frei teilbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpJobBudgetLimits {
    max_tokens: u64,
    max_wall: SignedDuration,
    max_tool_calls: u32,
}

impl McpJobBudgetLimits {
    /// Die harten Server-Obergrenzen.
    ///
    /// # Returns
    /// Grenzen gleich [`MCP_JOB_MAX_TOKENS`], [`MCP_JOB_MAX_WALL_SECONDS`] und
    /// [`MCP_JOB_MAX_TOOL_CALLS`].
    #[must_use]
    pub const fn server_default() -> Self {
        Self {
            max_tokens: MCP_JOB_MAX_TOKENS,
            max_wall: SignedDuration::from_secs(MCP_JOB_MAX_WALL_SECONDS),
            max_tool_calls: MCP_JOB_MAX_TOOL_CALLS,
        }
    }

    /// Erzeugt strengere Grenzen als [`Self::server_default`].
    ///
    /// # Arguments
    /// - `max_tokens` (`u64`): Token-Obergrenze, `1..=MCP_JOB_MAX_TOKENS`.
    /// - `max_wall` (`SignedDuration`): Wanduhr-Obergrenze, positiv und
    ///   höchstens [`MCP_JOB_MAX_WALL_SECONDS`].
    /// - `max_tool_calls` (`u32`): Tool-Obergrenze, `1..=MCP_JOB_MAX_TOOL_CALLS`.
    ///
    /// # Returns
    /// `None`, wenn eine Grenze null/negativ ist oder die harte Obergrenze
    /// überschreitet (fail closed: eine Konfiguration kann den Deckel nie
    /// anheben).
    #[must_use]
    pub fn new(max_tokens: u64, max_wall: SignedDuration, max_tool_calls: u32) -> Option<Self> {
        let ceiling = Self::server_default();
        let valid = (1..=ceiling.max_tokens).contains(&max_tokens)
            && max_wall > SignedDuration::ZERO
            && max_wall <= ceiling.max_wall
            && (1..=ceiling.max_tool_calls).contains(&max_tool_calls);
        valid.then_some(Self {
            max_tokens,
            max_wall,
            max_tool_calls,
        })
    }

    /// Token-Obergrenze.
    #[must_use]
    pub const fn max_tokens(&self) -> u64 {
        self.max_tokens
    }

    /// Wanduhr-Obergrenze.
    #[must_use]
    pub const fn max_wall(&self) -> SignedDuration {
        self.max_wall
    }

    /// Tool-Aufruf-Obergrenze.
    #[must_use]
    pub const fn max_tool_calls(&self) -> u32 {
        self.max_tool_calls
    }

    /// Löst ein vom Client angefordertes Budget gegen diese Grenzen auf.
    ///
    /// # Description
    /// Fehlt das Budget oder eine einzelne Grenze, gilt die Server-Obergrenze:
    /// ein MCP-Job ist nie unbegrenzt. Eine angeforderte Grenze *über* der
    /// Obergrenze wird **abgelehnt**, nicht gekappt: der Client hat ausdrücklich
    /// mehr verlangt, und ein stilles Kappen würde seine eigene Kostenrechnung
    /// verfälschen. Die Fehlermeldung nennt die Obergrenze, damit der Client
    /// korrigiert neu einreichen kann. Null-/Negativwerte prüft
    /// `validate_submission` bereits vorher.
    ///
    /// # Arguments
    /// - `requested` (`Option<&Budget>`): das untrusted Budget der Anfrage.
    ///
    /// # Returns
    /// Das vollständig begrenzte [`Budget`] (alle Felder `Some`).
    ///
    /// # Errors
    /// [`McpSupervisorError::InvalidSubmission`], wenn eine angeforderte Grenze
    /// die Obergrenze überschreitet.
    pub fn resolve(&self, requested: Option<&Budget>) -> Result<Budget, McpSupervisorError> {
        let Some(requested) = requested else {
            return Ok(self.as_budget());
        };
        let max_tokens = bounded("max_tokens", requested.max_tokens, self.max_tokens)?;
        let max_tool_calls = bounded(
            "max_tool_calls",
            requested.max_tool_calls,
            self.max_tool_calls,
        )?;
        let max_wall = match requested.max_wall {
            None => self.max_wall,
            Some(wall) if wall <= self.max_wall => wall,
            Some(_) => {
                return Err(McpSupervisorError::InvalidSubmission(format!(
                    "budget max_wall_seconds exceeds the server maximum of {} seconds",
                    self.max_wall.as_secs()
                )));
            }
        };
        Ok(Budget {
            max_tokens: Some(max_tokens),
            max_wall: Some(max_wall),
            max_tool_calls: Some(max_tool_calls),
        })
    }

    /// Die Grenzen selbst als vollständig begrenztes [`Budget`].
    #[must_use]
    pub fn as_budget(&self) -> Budget {
        Budget {
            max_tokens: Some(self.max_tokens),
            max_wall: Some(self.max_wall),
            max_tool_calls: Some(self.max_tool_calls),
        }
    }
}

impl Default for McpJobBudgetLimits {
    fn default() -> Self {
        Self::server_default()
    }
}

// Eine angeforderte ganzzahlige Grenze: fehlt sie, gilt die Obergrenze; liegt
// sie darüber, wird die Einreichung abgelehnt (siehe `resolve`).
fn bounded<T>(name: &str, requested: Option<T>, ceiling: T) -> Result<T, McpSupervisorError>
where
    T: PartialOrd + Copy + fmt::Display,
{
    match requested {
        None => Ok(ceiling),
        Some(value) if value <= ceiling => Ok(value),
        Some(_) => Err(McpSupervisorError::InvalidSubmission(format!(
            "budget {name} exceeds the server maximum of {ceiling}"
        ))),
    }
}

/// Maximale Länge eines MCP-Idempotenzschlüssels in Byte.
///
/// # Description
/// Parität zu `harw_core::admission::IDEMPOTENCY_KEY_MAX_LEN`; siehe den
/// Moduldoc oben zur Frage, warum dieser Wert hier separat gepflegt wird.
pub const MCP_IDEMPOTENCY_KEY_MAX_LEN: usize = 128;

/// Präfix der aus einem MCP-Idempotenzschlüssel abgeleiteten Work-ID.
pub const MCP_IDEMPOTENT_WORK_ID_PREFIX: &str = "mcp-idem-";

// Domänen-Trenner der MCP-eigenen Idempotenz-Ableitung. Bewusst verschieden
// von `harw_core::admission`s Trenner, damit die beiden unabhängigen
// Implementierungen niemals kollidierende Work-IDs erzeugen. Bei
// Formatänderung erhöhen.
const MCP_IDEMPOTENCY_DOMAIN: &[u8] = b"harw-mcp-server/job-admission/idempotency/v1";

/// Client-gewählter Deduplizierungsschlüssel für eine MCP-Einreichung.
///
/// # Description
/// 1 bis [`MCP_IDEMPOTENCY_KEY_MAX_LEN`] Byte aus `[A-Za-z0-9._:-]`. Die
/// abgeleitete Work-ID ist über `(Tenant, Workspace, Submitter, Schlüssel)`
/// skopiert (siehe [`mcp_idempotent_work_id`]), sodass gleiche Schlüssel
/// verschiedener Principals sich niemals treffen.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct McpIdempotencyKey(String);

impl McpIdempotencyKey {
    /// Validiert und verpackt einen rohen Schlüssel.
    ///
    /// # Errors
    /// - [`McpSupervisorError::InvalidIdempotencyKey`]: leer, zu lang, oder
    ///   ein Byte außerhalb von `[A-Za-z0-9._:-]`.
    fn parse(raw: &str) -> Result<Self, McpSupervisorError> {
        if raw.is_empty()
            || raw.len() > MCP_IDEMPOTENCY_KEY_MAX_LEN
            || !raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(McpSupervisorError::InvalidIdempotencyKey { length: raw.len() });
        }
        Ok(Self(raw.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

// Längenpräfigiert ein Feld, damit die Verkettung injektiv bleibt (kein
// Feld kann durch Grenzverschiebung mit einem Nachbarn kollidieren). Gleicher
// Aufbau wie `harw_core::admission::push_field`.
fn push_idempotency_field(material: &mut Vec<u8>, bytes: &[u8]) {
    material.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    material.extend_from_slice(bytes);
}

/// Leitet die deterministische Work-ID einer MCP-Idempotenz-Einreichung ab.
///
/// # Description
/// `mcp-idem-` gefolgt vom hex-BLAKE3-Digest über den Domänen-Trenner sowie
/// die längenpräfigierten Felder Tenant, Workspace, Submitter und Schlüssel.
/// Gleiches Verfahren wie `harw_core::admission::idempotent_work_id`, aber
/// mit eigenem Domänen-Trenner und eigenem Präfix (siehe Moduldoc).
///
/// # Returns
/// Eine dateisystemsichere [`WorkId`].
fn mcp_idempotent_work_id(
    tenant: &TenantId,
    workspace: &WorkspaceId,
    submitter: &ApprovalActor,
    key: &McpIdempotencyKey,
) -> WorkId {
    let mut material = Vec::with_capacity(256);
    push_idempotency_field(&mut material, MCP_IDEMPOTENCY_DOMAIN);
    push_idempotency_field(&mut material, tenant.as_str().as_bytes());
    push_idempotency_field(&mut material, workspace.as_str().as_bytes());
    match submitter {
        ApprovalActor::Operator { id } => {
            push_idempotency_field(&mut material, b"operator");
            push_idempotency_field(&mut material, id.as_bytes());
        }
        ApprovalActor::ChannelPeer { channel, peer } => {
            push_idempotency_field(&mut material, b"channel_peer");
            push_idempotency_field(&mut material, channel.as_str().as_bytes());
            push_idempotency_field(&mut material, peer.as_str().as_bytes());
        }
    }
    push_idempotency_field(&mut material, key.as_str().as_bytes());
    WorkId::from_str(format!(
        "{MCP_IDEMPOTENT_WORK_ID_PREFIX}{}",
        ContentDigest::of(&material)
    ))
}

/// Serverseitige Grenzen des gleitenden Zeitfensters der MCP-Ratenbegrenzung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpAdmissionLimits {
    max_submissions: u32,
    window: SignedDuration,
}

impl McpAdmissionLimits {
    /// Vorgabe zulässiger Einreichungen pro Zeitfenster.
    pub const DEFAULT_MAX_SUBMISSIONS: u32 = 30;
    /// Vorgabe der Fensterlänge in Sekunden.
    pub const DEFAULT_WINDOW_SECONDS: i64 = 60;

    /// Erzeugt Grenzen; `None` bei `max_submissions == 0` oder `window <= 0`.
    #[must_use]
    pub fn new(max_submissions: u32, window: SignedDuration) -> Option<Self> {
        (max_submissions > 0 && window > SignedDuration::ZERO).then_some(Self {
            max_submissions,
            window,
        })
    }

    /// Maximale Einreichungen pro Fenster.
    #[must_use]
    pub fn max_submissions(&self) -> u32 {
        self.max_submissions
    }

    /// Fensterlänge.
    #[must_use]
    pub fn window(&self) -> SignedDuration {
        self.window
    }
}

impl Default for McpAdmissionLimits {
    fn default() -> Self {
        Self {
            max_submissions: Self::DEFAULT_MAX_SUBMISSIONS,
            window: SignedDuration::from_secs(Self::DEFAULT_WINDOW_SECONDS),
        }
    }
}

// Schlüssel eines Ratenfensters.
type McpSubmitterKey = (TenantId, ApprovalActor);

// Obergrenze verfolgter Submitter, bevor inaktive Fenster ausgekehrt werden.
const MCP_LIMITER_SWEEP_THRESHOLD: usize = 4096;

// Gleitendes Zeitfenster pro `(Tenant, Submitter)`. Zweitimplementierung von
// `harw_core::admission::SubmissionLimiter` (dort privat und an eine
// `JobAdmissionService`-Komposition gebunden, die hier nicht verfügbar ist;
// siehe Moduldoc). Ein Idempotenz-Treffer wird dem Aufrufer *vor* dem
// Reservieren eines Slots gemeldet und belastet das Fenster daher nie.
#[derive(Debug)]
struct McpSubmissionRateLimiter {
    limits: McpAdmissionLimits,
    windows: Mutex<HashMap<McpSubmitterKey, VecDeque<Timestamp>>>,
}

impl McpSubmissionRateLimiter {
    fn new(limits: McpAdmissionLimits) -> Self {
        Self {
            limits,
            windows: Mutex::new(HashMap::new()),
        }
    }

    // Reserviert einen Slot bei `now`, oder meldet, wann der nächste frei wird.
    fn try_acquire(&self, key: &McpSubmitterKey, now: Timestamp) -> Result<(), McpSupervisorError> {
        let window = self.limits.window;
        let mut windows = self
            .windows
            .lock()
            .map_err(|_| McpSupervisorError::LimiterUnavailable)?;
        if windows.len() > MCP_LIMITER_SWEEP_THRESHOLD {
            windows.retain(|_, entries| {
                entries
                    .back()
                    .is_some_and(|last| last.duration_until(now) < window)
            });
        }
        let entries = windows.entry(key.clone()).or_default();
        while entries
            .front()
            .is_some_and(|first| first.duration_until(now) >= window)
        {
            entries.pop_front();
        }
        if entries.len() >= self.limits.max_submissions as usize {
            let retry_after = entries.front().map_or(window, |first| {
                window.saturating_sub(first.duration_until(now))
            });
            return Err(McpSupervisorError::RateLimited { retry_after });
        }
        entries.push_back(now);
        Ok(())
    }

    // Gibt eine bei `at` reservierte Reservierung frei, deren Zulassung nicht
    // durchdrang (z. B. Race gegen eine gleichzeitige Einreichung verloren).
    fn release(&self, key: &McpSubmitterKey, at: Timestamp) {
        let Ok(mut windows) = self.windows.lock() else {
            return;
        };
        if let Some(entries) = windows.get_mut(key)
            && let Some(position) = entries.iter().rposition(|entry| *entry == at)
        {
            entries.remove(position);
        }
    }
}

/// Closed MCP submission input. Scope, identity, retry policy, scheduling,
/// and work identity are resolved at the durable supervisor boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpJobSubmission {
    pub kind: McpSubmittedJobKind,
    pub input: Value,
    /// Client-gewählter Deduplizierungsschlüssel (1..128 Byte aus
    /// `[A-Za-z0-9._:-]`). Fehlt er, wird bei jedem Aufruf ein frischer Job
    /// zugelassen; `None` verhält sich unverändert zum bisherigen Verhalten.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub budget: Option<Budget>,
}

/// Server-authenticated identity and its resolved workspace authority.
#[derive(Debug, Clone)]
pub struct McpPrincipal {
    actor: ApprovalActor,
    tenant: TenantId,
    workspace: WorkspaceId,
    capabilities: BTreeSet<McpJobCapability>,
}

impl McpPrincipal {
    /// This constructor is for trusted ingress/authentication resolvers only.
    #[must_use]
    pub fn from_trusted_ingress(
        actor: ApprovalActor,
        tenant: TenantId,
        workspace: WorkspaceId,
        capabilities: impl IntoIterator<Item = McpJobCapability>,
    ) -> Self {
        Self {
            actor,
            tenant,
            workspace,
            capabilities: capabilities.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn capabilities(&self) -> &BTreeSet<McpJobCapability> {
        &self.capabilities
    }
}

/// Context constructed by the server after authentication, never deserialized
/// from a tool argument.
#[derive(Debug, Clone)]
pub struct McpRequestContext {
    session_id: String,
    principal: McpPrincipal,
}

impl McpRequestContext {
    #[must_use]
    pub fn from_trusted_ingress(session_id: String, principal: McpPrincipal) -> Self {
        Self {
            session_id,
            principal,
        }
    }
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpJobStatus {
    pub work_id: WorkId,
    pub kind: String,
    pub state: JobState,
    pub submitted_at: Timestamp,
    pub updated_at: Timestamp,
    pub attempts: u32,
    pub revision: u64,
    pub completion: Option<JobCompletion>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpCancellationReceipt {
    pub work_id: WorkId,
    pub previous_state: JobState,
    pub cancelled_at: Timestamp,
    pub revision: u64,
    /// Whether the cancelled job had an active worker lease. This is metadata
    /// only; no lease token or worker identity is exposed to an MCP client.
    pub worker_signal_required: bool,
    /// Redacted outcome of asking the execution owner to stop a running
    /// worker. Durable cancellation has already succeeded regardless of this
    /// best-effort delivery result.
    pub worker_cancellation: WorkerCancellationStatus,
}

/// Redacted outcome of forwarding a persisted cancellation to the execution
/// owner. The MCP boundary deliberately never exposes lease credentials,
/// sandbox paths, or worker identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCancellationStatus {
    /// The job had no active lease, so there was no worker to signal.
    NotRequired,
    /// The owning child-controller accepted a request to stop the worker.
    Requested,
    /// No execution controller is currently connected to accept the request.
    Unavailable,
}

/// Execution-side bridge for a cancellation which is already durable and
/// fenced. Implementations may signal a child process, mark a remote worker
/// for termination, or enqueue an out-of-process control message.
///
/// This trait is intentionally synchronous at the MCP boundary: it accepts a
/// best-effort handoff after the store has atomically persisted cancellation
/// and revoked the old lease. A sink must not attempt durable state changes.
pub trait WorkerCancellationSink: Send + Sync {
    fn request_worker_cancellation(
        &self,
        work_id: &WorkId,
        prior_lease: &Lease,
    ) -> WorkerCancellationStatus;
}

/// Safe default when the harness has no live child controller. It makes the
/// durable cancellation visible without pretending that a process was killed.
#[derive(Debug, Default)]
pub struct UnavailableWorkerCancellationSink;

impl WorkerCancellationSink for UnavailableWorkerCancellationSink {
    fn request_worker_cancellation(
        &self,
        _work_id: &WorkId,
        _prior_lease: &Lease,
    ) -> WorkerCancellationStatus {
        WorkerCancellationStatus::Unavailable
    }
}

#[derive(Debug)]
pub enum McpSupervisorError {
    NotAuthorized,
    InvalidSubmission(String),
    JobStore(SessionStoreError),
    /// Idempotenzschlüssel verletzt die Schlüsselgrammatik (Schlüssel selbst
    /// wird nicht gespiegelt).
    InvalidIdempotencyKey {
        length: usize,
    },
    /// Idempotenzschlüssel benennt bereits einen Job mit anderem Input, Kind
    /// oder Scope. Fail closed: es wird nie ein Job stillschweigend zugelassen.
    IdempotencyConflict {
        work_id: WorkId,
    },
    /// Der Submitter hat sein Einreichungsfenster ausgeschöpft.
    RateLimited {
        retry_after: SignedDuration,
    },
    /// Der Sperrmechanismus der Ratenbegrenzung ist vergiftet (gesperrter
    /// Mutex nach einem Panic). Fail closed statt stillschweigend zuzulassen.
    LimiterUnavailable,
}

impl fmt::Display for McpSupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAuthorized => f.write_str("MCP principal is not authorized for this job"),
            Self::InvalidSubmission(detail) => write!(f, "invalid MCP job submission: {detail}"),
            Self::JobStore(error) => write!(f, "durable job operation failed: {error}"),
            Self::InvalidIdempotencyKey { length } => write!(
                f,
                "invalid idempotency key of {length} bytes: expected 1-{MCP_IDEMPOTENCY_KEY_MAX_LEN} bytes of [A-Za-z0-9._:-]"
            ),
            Self::IdempotencyConflict { work_id } => write!(
                f,
                "idempotency key already used for a different submission (job {work_id})"
            ),
            Self::RateLimited { retry_after } => {
                write!(
                    f,
                    "submission rate limit reached; retry after {retry_after}"
                )
            }
            Self::LimiterUnavailable => f.write_str("submission rate limiter is unavailable"),
        }
    }
}
impl std::error::Error for McpSupervisorError {}

/// Redacted, authority-checked job operations. Lease tokens, resolved input,
/// sandbox paths and worker identities never cross this interface.
pub trait McpSupervisor: Send + Sync {
    fn submit_job<'a>(
        &'a self,
        context: &'a McpRequestContext,
        submission: McpJobSubmission,
    ) -> McpSupervisorFuture<'a, McpJobStatus>;
    fn job_status<'a>(
        &'a self,
        context: &'a McpRequestContext,
        work_id: WorkId,
    ) -> McpSupervisorFuture<'a, McpJobStatus>;
    fn cancel_job<'a>(
        &'a self,
        context: &'a McpRequestContext,
        work_id: WorkId,
        reason: String,
    ) -> McpSupervisorFuture<'a, McpCancellationReceipt>;
}

/// Durable implementation. Process signalling remains the owning child
/// controller's job; `worker_signal_required` tells it to signal the exact
/// captured fenced lease after cancellation is persisted.
pub struct DurableMcpSupervisor {
    store: Arc<JobStore>,
    worker_cancellation_sink: Arc<dyn WorkerCancellationSink>,
    /// Serverseitige Budget-Obergrenzen; nie aus einer Anfrage.
    budget_limits: McpJobBudgetLimits,
    /// Gleitendes Ratenfenster pro `(Tenant, Submitter)`; siehe Moduldoc.
    rate_limiter: McpSubmissionRateLimiter,
}

impl DurableMcpSupervisor {
    #[must_use]
    pub fn new(store: Arc<JobStore>) -> Self {
        Self::with_worker_cancellation_sink(store, Arc::new(UnavailableWorkerCancellationSink))
    }

    /// Composes durable cancellation with the process-owning controller.
    /// The sink is called only after [`JobStore::cancel`] has committed the
    /// terminal cancellation and fenced any prior running lease.
    #[must_use]
    pub fn with_worker_cancellation_sink(
        store: Arc<JobStore>,
        worker_cancellation_sink: Arc<dyn WorkerCancellationSink>,
    ) -> Self {
        Self {
            store,
            worker_cancellation_sink,
            budget_limits: McpJobBudgetLimits::server_default(),
            rate_limiter: McpSubmissionRateLimiter::new(McpAdmissionLimits::default()),
        }
    }

    /// Setzt strengere Budget-Obergrenzen als die Server-Vorgabe.
    ///
    /// # Arguments
    /// - `budget_limits` (`McpJobBudgetLimits`): über
    ///   [`McpJobBudgetLimits::new`] erzeugt und damit nie weiter als
    ///   [`McpJobBudgetLimits::server_default`].
    ///
    /// # Returns
    /// Den Supervisor mit den neuen Grenzen.
    #[must_use]
    pub fn with_budget_limits(mut self, budget_limits: McpJobBudgetLimits) -> Self {
        self.budget_limits = budget_limits;
        self
    }

    /// Die aktiven Budget-Obergrenzen.
    #[must_use]
    pub fn budget_limits(&self) -> McpJobBudgetLimits {
        self.budget_limits
    }

    /// Setzt die Ratenbegrenzung der Einreichung auf ein anderes gleitendes
    /// Zeitfenster als [`McpAdmissionLimits::default`] (setzt alle Fenster
    /// zurück).
    ///
    /// # Returns
    /// Den Supervisor mit den neuen Grenzen.
    #[must_use]
    pub fn with_rate_limits(mut self, rate_limits: McpAdmissionLimits) -> Self {
        self.rate_limiter = McpSubmissionRateLimiter::new(rate_limits);
        self
    }

    /// Die aktive Ratenbegrenzung.
    #[must_use]
    pub fn rate_limits(&self) -> McpAdmissionLimits {
        self.rate_limiter.limits
    }
}

impl McpSupervisor for DurableMcpSupervisor {
    fn submit_job<'a>(
        &'a self,
        context: &'a McpRequestContext,
        submission: McpJobSubmission,
    ) -> McpSupervisorFuture<'a, McpJobStatus> {
        Box::pin(async move {
            if !context
                .principal
                .capabilities
                .contains(&McpJobCapability::SubmitOwn)
            {
                return Err(McpSupervisorError::NotAuthorized);
            }
            validate_submission(&submission)?;
            // P0.12: das Client-Budget wird gegen die Server-Obergrenzen
            // aufgelöst; ohne Angabe gilt die Obergrenze, nie `unbounded`.
            let budget = self.budget_limits.resolve(submission.budget.as_ref())?;
            let kind = submission.kind.into_runtime_kind();
            let scope = JobScope::new(
                context.principal.tenant.clone(),
                context.principal.workspace.clone(),
                context.principal.actor.clone(),
            );

            // Idempotenz zuerst (F-158-Parität mit
            // `JobAdmissionService::submit`): ein wiederholter Aufruf mit
            // demselben Schlüssel trifft denselben Job und kehrt hier
            // zurück, bevor die Ratenbegrenzung unten überhaupt erreicht
            // wird — sie belastet das Fenster daher nie. Die erste, echte
            // Zulassung unter diesem Schlüssel erreicht die Ratenbegrenzung
            // dagegen wie jede andere Einreichung und wird dort belastet
            // (siehe Begründung an der Ratenbegrenzung unten).
            let idempotency_key = match &submission.idempotency_key {
                Some(raw) => Some(McpIdempotencyKey::parse(raw)?),
                None => None,
            };
            let id = match &idempotency_key {
                Some(key) => {
                    let id = mcp_idempotent_work_id(
                        scope.tenant(),
                        scope.workspace(),
                        scope.submitter(),
                        key,
                    );
                    match self.store.get(&id) {
                        Ok(existing) => {
                            return duplicate_of(existing, &scope, &kind, &submission.input);
                        }
                        Err(SessionStoreError::JobNotFound { .. }) => id,
                        Err(error) => return Err(McpSupervisorError::JobStore(error)),
                    }
                }
                None => WorkId::new(),
            };

            // Ratenbegrenzung (F-158-Parität): fail closed bei ausgeschöpftem
            // Fenster oder vergiftetem Limiter-Lock. Nie stillschweigend
            // zulassen.
            //
            // Jede Zulassung, die diesen Punkt erreicht, belastet das
            // Fenster — mit oder ohne Idempotenzschlüssel. Nur ein Treffer
            // im Idempotenz-Cache oben (derselbe Schlüssel trifft bereits
            // einen existierenden Job) belastet es nicht, weil dieser
            // Lookup vor `try_acquire` passiert und die Funktion dort
            // bereits zurückkehrt. Ein Client könnte das Limit sonst durch
            // einen jeweils neuen Idempotenzschlüssel pro Einreichung
            // vollständig umgehen; diese Reihenfolge verhindert das.
            let limiter_key = (
                context.principal.tenant.clone(),
                context.principal.actor.clone(),
            );
            let now = Timestamp::now();
            self.rate_limiter.try_acquire(&limiter_key, now)?;

            let mut job = Job::new(
                id.clone(),
                kind.clone(),
                budget,
                default_retry_policy(),
                now,
            );
            if let Err(error) = job.mark_ready(now) {
                self.rate_limiter.release(&limiter_key, now);
                return Err(McpSupervisorError::InvalidSubmission(error.to_string()));
            }
            let record = StoredJob {
                job,
                scope: scope.clone(),
                input: submission.input,
                submitted_at: now,
                not_before: now,
                lease: None,
                lease_epoch: 0,
                completion: None,
                cancellation: None,
                revision: 0,
                // McpRequestContext (session_id + principal) carries no trace
                // context; nothing upstream of this MCP boundary threads one
                // through yet.
                trace: None,
            };
            match self.store.admit(&record) {
                Ok(()) => Ok(status(&record)),
                Err(SessionStoreError::JobAlreadyExists { .. }) if idempotency_key.is_some() => {
                    // Race gegen eine identische gleichzeitige Einreichung
                    // verloren: derselbe abgeleitete Idempotenz-Work-ID wurde
                    // in der Zwischenzeit bereits zugelassen. Der Slot oben
                    // wurde für diesen Versuch bereits belastet und wird
                    // hier wieder freigegeben, da kein neuer Job entsteht.
                    self.rate_limiter.release(&limiter_key, now);
                    let existing = self.store.get(&id).map_err(McpSupervisorError::JobStore)?;
                    duplicate_of(existing, &scope, &kind, &record.input)
                }
                Err(error) => {
                    self.rate_limiter.release(&limiter_key, now);
                    Err(McpSupervisorError::JobStore(error))
                }
            }
        })
    }

    fn job_status<'a>(
        &'a self,
        context: &'a McpRequestContext,
        work_id: WorkId,
    ) -> McpSupervisorFuture<'a, McpJobStatus> {
        Box::pin(async move {
            let record = self
                .store
                .get(&work_id)
                .map_err(McpSupervisorError::JobStore)?;
            if !can_read(&context.principal, &record.scope) {
                return Err(McpSupervisorError::NotAuthorized);
            }
            Ok(status(&record))
        })
    }

    fn cancel_job<'a>(
        &'a self,
        context: &'a McpRequestContext,
        work_id: WorkId,
        reason: String,
    ) -> McpSupervisorFuture<'a, McpCancellationReceipt> {
        Box::pin(async move {
            let record = self
                .store
                .get(&work_id)
                .map_err(McpSupervisorError::JobStore)?;
            if !can_cancel(&context.principal, &record.scope) {
                return Err(McpSupervisorError::NotAuthorized);
            }
            let cancelled_at = Timestamp::now();
            let transition = self
                .store
                .cancel(
                    &work_id,
                    &CancelRequest {
                        cancelled_at,
                        cancelled_by: context.principal.actor.clone(),
                        reason,
                    },
                )
                .map_err(McpSupervisorError::JobStore)?;
            let worker_signal_required = transition.prior_lease.is_some();
            let worker_cancellation = match transition.prior_lease.as_ref() {
                Some(prior_lease) => match self
                    .worker_cancellation_sink
                    .request_worker_cancellation(&transition.work_id, prior_lease)
                {
                    cancellation_status @ (WorkerCancellationStatus::Requested
                    | WorkerCancellationStatus::Unavailable) => cancellation_status,
                    // A prior lease proves a worker signal was required. Do
                    // not let a faulty execution bridge report otherwise.
                    WorkerCancellationStatus::NotRequired => WorkerCancellationStatus::Unavailable,
                },
                None => WorkerCancellationStatus::NotRequired,
            };
            let record = self
                .store
                .get(&work_id)
                .map_err(McpSupervisorError::JobStore)?;
            Ok(McpCancellationReceipt {
                work_id,
                previous_state: transition.previous_state,
                cancelled_at,
                revision: record.revision,
                worker_signal_required,
                worker_cancellation,
            })
        })
    }
}

fn same_scope(principal: &McpPrincipal, scope: &JobScope) -> bool {
    principal.tenant == *scope.tenant() && principal.workspace == *scope.workspace()
}
fn can_read(principal: &McpPrincipal, scope: &JobScope) -> bool {
    same_scope(principal, scope)
        && (principal
            .capabilities
            .contains(&McpJobCapability::ReadWorkspace)
            || (principal.capabilities.contains(&McpJobCapability::ReadOwn)
                && principal.actor == *scope.submitter()))
}
fn validate_submission(submission: &McpJobSubmission) -> Result<(), McpSupervisorError> {
    if !submission.input.is_object() {
        return Err(McpSupervisorError::InvalidSubmission(
            "input must be a JSON object".to_owned(),
        ));
    }
    if let Some(budget) = &submission.budget {
        if budget.max_tokens == Some(0)
            || budget.max_tool_calls == Some(0)
            || budget
                .max_wall
                .is_some_and(|wall| wall <= jiff::SignedDuration::ZERO)
        {
            return Err(McpSupervisorError::InvalidSubmission(
                "budget limits must be greater than zero when provided".to_owned(),
            ));
        }
    }
    Ok(())
}
fn default_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 2,
        base_delay: jiff::SignedDuration::from_secs(1),
        factor: 2.0,
        max_delay: jiff::SignedDuration::from_secs(10),
    }
}
fn can_cancel(principal: &McpPrincipal, scope: &JobScope) -> bool {
    same_scope(principal, scope)
        && (principal
            .capabilities
            .contains(&McpJobCapability::CancelWorkspace)
            || (principal
                .capabilities
                .contains(&McpJobCapability::CancelOwn)
                && principal.actor == *scope.submitter()))
}
// Akzeptiert einen bestehenden Datensatz nur, wenn es dieselbe logische
// Einreichung ist (gleicher Scope, gleiche Art, gleicher Input) — sonst
// [`McpSupervisorError::IdempotencyConflict`] statt eines still vertauschten
// Jobs. Gleiche Semantik wie `harw_core::admission::duplicate_of`.
fn duplicate_of(
    existing: StoredJob,
    scope: &JobScope,
    kind: &JobKind,
    input: &Value,
) -> Result<McpJobStatus, McpSupervisorError> {
    if existing.scope != *scope || existing.job.kind != *kind || existing.input != *input {
        return Err(McpSupervisorError::IdempotencyConflict {
            work_id: existing.job.id,
        });
    }
    Ok(status(&existing))
}
fn status(record: &StoredJob) -> McpJobStatus {
    McpJobStatus {
        work_id: record.job.id.clone(),
        kind: job_kind(&record.job.kind),
        state: record.job.state,
        submitted_at: record.submitted_at,
        updated_at: record.job.updated_at,
        attempts: record.job.attempts,
        revision: record.revision,
        completion: record.completion.clone(),
    }
}
fn job_kind(kind: &JobKind) -> String {
    match kind {
        JobKind::Dream => "dream".to_owned(),
        JobKind::Worker => "worker".to_owned(),
        JobKind::Custom(value) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use harw_job_core::{Budget, Job, RetryPolicy};
    use harw_session_store::ClaimRequest;
    use jiff::SignedDuration;

    use crate::test_support::{TestError, TestResult, ctx};

    fn scope() -> JobScope {
        JobScope::new(
            TenantId::from_str("tenant-a"),
            WorkspaceId::from_str("workspace-a"),
            ApprovalActor::Operator {
                id: "alice".to_owned(),
            },
        )
    }

    fn principal(
        actor: &str,
        tenant: &str,
        workspace: &str,
        capabilities: Vec<McpJobCapability>,
    ) -> McpPrincipal {
        McpPrincipal::from_trusted_ingress(
            ApprovalActor::Operator {
                id: actor.to_owned(),
            },
            TenantId::from_str(tenant),
            WorkspaceId::from_str(workspace),
            capabilities,
        )
    }

    fn context() -> McpRequestContext {
        McpRequestContext::from_trusted_ingress(
            "mcp-session-test".to_owned(),
            principal(
                "alice",
                "tenant-a",
                "workspace-a",
                vec![McpJobCapability::CancelOwn],
            ),
        )
    }

    fn submit_context() -> McpRequestContext {
        McpRequestContext::from_trusted_ingress(
            "mcp-submit-session-test".to_owned(),
            principal(
                "alice",
                "tenant-a",
                "workspace-a",
                vec![McpJobCapability::SubmitOwn, McpJobCapability::ReadOwn],
            ),
        )
    }

    fn record(id: &str) -> TestResult<StoredJob> {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 2,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(10),
            },
            now,
        );
        job.mark_ready(now).map_err(ctx("mark_ready"))?;
        Ok(StoredJob {
            job,
            scope: scope(),
            input: serde_json::json!({"task": "test"}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        })
    }

    struct PersistedTransitionSink {
        store: Arc<JobStore>,
        calls: Mutex<Vec<WorkId>>,
        // Der Trait `WorkerCancellationSink` gibt kein `Result` zurück (Produktionscode,
        // nicht in dieser Datei anpassbar); ein Fehlschlag beim Nachlesen des Stores
        // wird deshalb hier gesammelt statt gepanikt und danach im Test per
        // `assert!` geprüft (Bible R087/R165 — kein `.unwrap()`/`panic!` im Callback).
        load_failures: Mutex<Vec<String>>,
    }

    impl PersistedTransitionSink {
        fn new(store: Arc<JobStore>) -> Self {
            Self {
                store,
                calls: Mutex::new(Vec::new()),
                load_failures: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<WorkId> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }

        fn load_failures(&self) -> Vec<String> {
            self.load_failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl WorkerCancellationSink for PersistedTransitionSink {
        fn request_worker_cancellation(
            &self,
            work_id: &WorkId,
            prior_lease: &Lease,
        ) -> WorkerCancellationStatus {
            // This observes the durable store at signal time. It proves the
            // supervisor did not contact a worker before the cancellation and
            // its fence were committed.
            match self.store.get(work_id) {
                Ok(persisted) => {
                    assert_eq!(persisted.job.state, JobState::Cancelled);
                    assert!(persisted.lease.is_none());
                    assert!(persisted.lease_epoch > prior_lease.epoch);
                }
                Err(error) => self
                    .load_failures
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("store.get({work_id:?}) failed: {error}")),
            }
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(work_id.clone());
            WorkerCancellationStatus::Requested
        }
    }

    struct InvalidNotRequiredSink;

    impl WorkerCancellationSink for InvalidNotRequiredSink {
        fn request_worker_cancellation(
            &self,
            _work_id: &WorkId,
            _prior_lease: &Lease,
        ) -> WorkerCancellationStatus {
            WorkerCancellationStatus::NotRequired
        }
    }

    #[test]
    fn own_capabilities_do_not_cross_actor_or_scope() {
        let job_scope = scope();
        let owner = principal(
            "alice",
            "tenant-a",
            "workspace-a",
            vec![McpJobCapability::ReadOwn, McpJobCapability::CancelOwn],
        );
        assert!(can_read(&owner, &job_scope));
        assert!(can_cancel(&owner, &job_scope));
        let another_actor = principal(
            "other",
            "tenant-a",
            "workspace-a",
            vec![McpJobCapability::ReadOwn],
        );
        assert!(!can_read(&another_actor, &job_scope));
        let another_workspace = principal(
            "alice",
            "tenant-a",
            "workspace-b",
            vec![McpJobCapability::CancelWorkspace],
        );
        assert!(!can_cancel(&another_workspace, &job_scope));
    }

    #[test]
    fn workspace_grants_are_tenant_bounded() {
        let job_scope = scope();
        let workspace_reader = principal(
            "other",
            "tenant-a",
            "workspace-a",
            vec![McpJobCapability::ReadWorkspace],
        );
        assert!(can_read(&workspace_reader, &job_scope));
        let foreign_tenant = principal(
            "other",
            "tenant-b",
            "workspace-a",
            vec![McpJobCapability::ReadWorkspace],
        );
        assert!(!can_read(&foreign_tenant, &job_scope));
    }

    #[tokio::test]
    async fn pending_and_ready_cancellation_do_not_signal_a_worker() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let mut pending = record("pending-cancel")?;
        pending.job.state = JobState::Pending;
        store.admit(&pending).map_err(ctx("admit"))?;
        store
            .admit(&record("ready-cancel")?)
            .map_err(ctx("admit"))?;

        let sink = Arc::new(PersistedTransitionSink::new(Arc::clone(&store)));
        let supervisor =
            DurableMcpSupervisor::with_worker_cancellation_sink(Arc::clone(&store), sink.clone());
        for work_id in ["pending-cancel", "ready-cancel"] {
            let receipt = supervisor
                .cancel_job(
                    &context(),
                    WorkId::from_str(work_id),
                    "superseded".to_owned(),
                )
                .await
                .map_err(ctx("cancel_job"))?;
            assert!(!receipt.worker_signal_required);
            assert_eq!(
                receipt.worker_cancellation,
                WorkerCancellationStatus::NotRequired
            );
        }
        assert!(sink.calls().is_empty());
        assert!(sink.load_failures().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn running_cancellation_signals_only_after_durable_fence() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        store
            .admit(&record("running-cancel")?)
            .map_err(ctx("admit"))?;
        let now = Timestamp::now();
        store
            .claim(
                &WorkId::from_str("running-cancel"),
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now,
                },
            )
            .map_err(ctx("claim"))?;

        let sink = Arc::new(PersistedTransitionSink::new(Arc::clone(&store)));
        let supervisor =
            DurableMcpSupervisor::with_worker_cancellation_sink(Arc::clone(&store), sink.clone());
        let receipt = supervisor
            .cancel_job(
                &context(),
                WorkId::from_str("running-cancel"),
                "operator stopped task".to_owned(),
            )
            .await
            .map_err(ctx("cancel_job"))?;
        assert_eq!(receipt.previous_state, JobState::Running);
        assert!(receipt.worker_signal_required);
        assert_eq!(
            receipt.worker_cancellation,
            WorkerCancellationStatus::Requested
        );
        assert_eq!(sink.calls(), vec![WorkId::from_str("running-cancel")]);
        assert!(sink.load_failures().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn running_cancellation_reports_unavailable_without_a_live_controller() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        store
            .admit(&record("running-unavailable")?)
            .map_err(ctx("admit"))?;
        store
            .claim(
                &WorkId::from_str("running-unavailable"),
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: Timestamp::now(),
                },
            )
            .map_err(ctx("claim"))?;

        let receipt = DurableMcpSupervisor::new(store)
            .cancel_job(
                &context(),
                WorkId::from_str("running-unavailable"),
                "operator stopped task".to_owned(),
            )
            .await
            .map_err(ctx("cancel_job"))?;

        assert!(receipt.worker_signal_required);
        assert_eq!(
            receipt.worker_cancellation,
            WorkerCancellationStatus::Unavailable
        );
        Ok(())
    }

    #[tokio::test]
    async fn running_cancellation_fails_closed_for_an_invalid_sink_status() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        store
            .admit(&record("running-invalid-status")?)
            .map_err(ctx("admit"))?;
        store
            .claim(
                &WorkId::from_str("running-invalid-status"),
                &ClaimRequest {
                    worker_id: "worker-a".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now: Timestamp::now(),
                },
            )
            .map_err(ctx("claim"))?;

        let receipt = DurableMcpSupervisor::with_worker_cancellation_sink(
            store,
            Arc::new(InvalidNotRequiredSink),
        )
        .cancel_job(
            &context(),
            WorkId::from_str("running-invalid-status"),
            "operator stopped task".to_owned(),
        )
        .await
        .map_err(ctx("cancel_job"))?;

        assert!(receipt.worker_signal_required);
        assert_eq!(
            receipt.worker_cancellation,
            WorkerCancellationStatus::Unavailable
        );
        Ok(())
    }

    #[tokio::test]
    async fn submit_admits_a_ready_job_in_the_authenticated_scope() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store));

        let submitted = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "durable MCP work"}),
                    idempotency_key: None,
                    budget: None,
                },
            )
            .await
            .map_err(ctx("submit_job"))?;

        assert_eq!(submitted.kind, "worker");
        assert_eq!(submitted.state, JobState::Ready);
        let stored = store.get(&submitted.work_id).map_err(ctx("get"))?;
        assert_eq!(stored.scope, scope());
        assert_eq!(
            stored.input,
            serde_json::json!({"task": "durable MCP work"})
        );
        // P0.12: ohne Client-Budget gilt die Server-Obergrenze, nie `unbounded`.
        assert_eq!(
            stored.job.budget,
            McpJobBudgetLimits::server_default().as_budget()
        );
        assert_ne!(stored.job.budget, Budget::unbounded());
        assert_eq!(stored.job.retry, default_retry_policy());
        Ok(())
    }

    #[tokio::test]
    async fn submit_rejects_a_budget_above_the_server_maximum_without_admitting() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store));

        let too_many_tokens = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "expensive"}),
                    idempotency_key: None,
                    budget: Some(Budget {
                        max_tokens: Some(MCP_JOB_MAX_TOKENS + 1),
                        max_wall: None,
                        max_tool_calls: None,
                    }),
                },
            )
            .await;
        let Err(McpSupervisorError::InvalidSubmission(detail)) = too_many_tokens else {
            return Err(TestError::Unexpected(
                "a token budget above the server maximum must be rejected".to_owned(),
            ));
        };
        assert!(detail.contains("max_tokens"), "{detail}");
        assert!(detail.contains(&MCP_JOB_MAX_TOKENS.to_string()), "{detail}");

        let too_long = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Dream,
                    input: serde_json::json!({"task": "slow"}),
                    idempotency_key: None,
                    budget: Some(Budget {
                        max_tokens: None,
                        max_wall: Some(SignedDuration::from_secs(MCP_JOB_MAX_WALL_SECONDS + 1)),
                        max_tool_calls: None,
                    }),
                },
            )
            .await;
        assert!(matches!(
            too_long,
            Err(McpSupervisorError::InvalidSubmission(ref detail)) if detail.contains("max_wall_seconds")
        ));

        let too_many_tools = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "busy"}),
                    idempotency_key: None,
                    budget: Some(Budget {
                        max_tokens: None,
                        max_wall: None,
                        max_tool_calls: Some(MCP_JOB_MAX_TOOL_CALLS + 1),
                    }),
                },
            )
            .await;
        assert!(matches!(
            too_many_tools,
            Err(McpSupervisorError::InvalidSubmission(ref detail)) if detail.contains("max_tool_calls")
        ));

        // Nichts davon wurde zugelassen.
        let page = store
            .list(&harw_session_store::JobListQuery::default())
            .map_err(ctx("list"))?;
        assert!(page.jobs.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn submit_keeps_budgets_within_the_maximum_and_fills_missing_limits() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store));

        let submitted = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "bounded"}),
                    idempotency_key: None,
                    budget: Some(Budget {
                        max_tokens: Some(MCP_JOB_MAX_TOKENS),
                        max_wall: None,
                        max_tool_calls: Some(3),
                    }),
                },
            )
            .await
            .map_err(ctx("submit_job"))?;

        let stored = store.get(&submitted.work_id).map_err(ctx("get"))?;
        assert_eq!(stored.job.budget.max_tokens, Some(MCP_JOB_MAX_TOKENS));
        assert_eq!(
            stored.job.budget.max_wall,
            Some(SignedDuration::from_secs(MCP_JOB_MAX_WALL_SECONDS))
        );
        assert_eq!(stored.job.budget.max_tool_calls, Some(3));
        Ok(())
    }

    #[tokio::test]
    async fn stricter_composition_limits_apply_to_submissions() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let limits = McpJobBudgetLimits::new(1_000, SignedDuration::from_secs(30), 2)
            .ok_or(TestError::Missing("valid McpJobBudgetLimits"))?;
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store)).with_budget_limits(limits);
        assert_eq!(supervisor.budget_limits(), limits);

        let rejected = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "over the composed limit"}),
                    idempotency_key: None,
                    budget: Some(Budget {
                        max_tokens: Some(1_001),
                        max_wall: None,
                        max_tool_calls: None,
                    }),
                },
            )
            .await;
        assert!(matches!(
            rejected,
            Err(McpSupervisorError::InvalidSubmission(_))
        ));

        let defaulted = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "no budget"}),
                    idempotency_key: None,
                    budget: None,
                },
            )
            .await
            .map_err(ctx("submit_job"))?;
        assert_eq!(
            store
                .get(&defaulted.work_id)
                .map_err(ctx("get"))?
                .job
                .budget,
            limits.as_budget()
        );
        Ok(())
    }

    #[test]
    fn budget_limits_can_only_be_tightened() {
        let ceiling = McpJobBudgetLimits::server_default();
        assert_eq!(McpJobBudgetLimits::default(), ceiling);
        assert_eq!(ceiling.max_tokens(), MCP_JOB_MAX_TOKENS);
        assert_eq!(
            ceiling.max_wall(),
            SignedDuration::from_secs(MCP_JOB_MAX_WALL_SECONDS)
        );
        assert_eq!(ceiling.max_tool_calls(), MCP_JOB_MAX_TOOL_CALLS);
        assert_eq!(
            McpJobBudgetLimits::new(
                MCP_JOB_MAX_TOKENS,
                ceiling.max_wall(),
                MCP_JOB_MAX_TOOL_CALLS
            ),
            Some(ceiling)
        );
        assert!(McpJobBudgetLimits::new(MCP_JOB_MAX_TOKENS + 1, ceiling.max_wall(), 1).is_none());
        assert!(
            McpJobBudgetLimits::new(
                1,
                SignedDuration::from_secs(MCP_JOB_MAX_WALL_SECONDS + 1),
                1
            )
            .is_none()
        );
        assert!(
            McpJobBudgetLimits::new(1, ceiling.max_wall(), MCP_JOB_MAX_TOOL_CALLS + 1).is_none()
        );
        assert!(McpJobBudgetLimits::new(0, ceiling.max_wall(), 1).is_none());
        assert!(McpJobBudgetLimits::new(1, SignedDuration::ZERO, 1).is_none());
        assert!(McpJobBudgetLimits::new(1, ceiling.max_wall(), 0).is_none());
    }

    #[tokio::test]
    async fn submit_requires_capability_and_validates_untrusted_fields() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(store);

        let unauthorized = supervisor
            .submit_job(
                &context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Dream,
                    input: serde_json::json!({"task": "not allowed"}),
                    idempotency_key: None,
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            unauthorized,
            Err(McpSupervisorError::NotAuthorized)
        ));

        let invalid_input = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Dream,
                    input: serde_json::json!(["not", "an", "object"]),
                    idempotency_key: None,
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            invalid_input,
            Err(McpSupervisorError::InvalidSubmission(_))
        ));

        let invalid_budget = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Dream,
                    input: serde_json::json!({"task": "bad budget"}),
                    idempotency_key: None,
                    budget: Some(Budget {
                        max_tokens: Some(0),
                        max_wall: None,
                        max_tool_calls: None,
                    }),
                },
            )
            .await;
        assert!(matches!(
            invalid_budget,
            Err(McpSupervisorError::InvalidSubmission(_))
        ));
        Ok(())
    }

    // ── Idempotency (F-158-Parität) ─────────────────────────────────────

    #[tokio::test]
    async fn submit_with_idempotency_key_admits_once_and_is_idempotent_on_retry() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store));
        let submission = || McpJobSubmission {
            kind: McpSubmittedJobKind::Worker,
            input: serde_json::json!({"task": "idempotent"}),
            idempotency_key: Some("node-1".to_owned()),
            budget: None,
        };

        let first = supervisor
            .submit_job(&submit_context(), submission())
            .await
            .map_err(ctx("submit_job"))?;
        let second = supervisor
            .submit_job(&submit_context(), submission())
            .await
            .map_err(ctx("submit_job"))?;

        assert_eq!(first.work_id, second.work_id);
        assert!(
            first
                .work_id
                .as_str()
                .starts_with(MCP_IDEMPOTENT_WORK_ID_PREFIX)
        );
        let page = store
            .list(&harw_session_store::JobListQuery::default())
            .map_err(ctx("list"))?;
        assert_eq!(
            page.jobs.len(),
            1,
            "a retried idempotent submission must not create a second job"
        );
        Ok(())
    }

    #[tokio::test]
    async fn submit_with_same_idempotency_key_and_different_input_conflicts() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store));

        supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "a"}),
                    idempotency_key: Some("shared-key".to_owned()),
                    budget: None,
                },
            )
            .await
            .map_err(ctx("submit_job"))?;

        let conflict = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "b"}),
                    idempotency_key: Some("shared-key".to_owned()),
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            conflict,
            Err(McpSupervisorError::IdempotencyConflict { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn submit_rejects_an_invalid_idempotency_key() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let supervisor = DurableMcpSupervisor::new(store);

        let rejected = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "bad key"}),
                    idempotency_key: Some("not a valid key!".to_owned()),
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            rejected,
            Err(McpSupervisorError::InvalidIdempotencyKey { .. })
        ));
        Ok(())
    }

    // ── Rate limiting (F-158-Parität) ───────────────────────────────────

    #[tokio::test]
    async fn submit_rejects_once_the_submitter_rate_window_is_exhausted() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let limits = McpAdmissionLimits::new(1, SignedDuration::from_secs(60))
            .ok_or(TestError::Missing("valid McpAdmissionLimits"))?;
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store)).with_rate_limits(limits);
        assert_eq!(supervisor.rate_limits(), limits);

        supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "first"}),
                    idempotency_key: None,
                    budget: None,
                },
            )
            .await
            .map_err(ctx("submit_job"))?;

        let rejected = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "second"}),
                    idempotency_key: None,
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            rejected,
            Err(McpSupervisorError::RateLimited { .. })
        ));

        // Nichts vom zweiten (abgelehnten) Versuch wurde zugelassen.
        let page = store
            .list(&harw_session_store::JobListQuery::default())
            .map_err(ctx("list"))?;
        assert_eq!(page.jobs.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn idempotent_retries_do_not_charge_the_rate_limit_window() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(temp.path()));
        let limits = McpAdmissionLimits::new(1, SignedDuration::from_secs(60))
            .ok_or(TestError::Missing("valid McpAdmissionLimits"))?;
        let supervisor = DurableMcpSupervisor::new(Arc::clone(&store)).with_rate_limits(limits);
        let submission = || McpJobSubmission {
            kind: McpSubmittedJobKind::Worker,
            input: serde_json::json!({"task": "keyed"}),
            idempotency_key: Some("node-1".to_owned()),
            budget: None,
        };

        // (a) Erste Einreichung mit Schlüssel "node-1" verbraucht den
        // einzigen Slot des Fensters — eine neue Zulassung belastet das
        // Fenster immer, auch mit Idempotenzschlüssel.
        let first = supervisor
            .submit_job(&submit_context(), submission())
            .await
            .map_err(ctx("submit_job"))?;

        // (b) Dieselbe Einreichung noch einmal: trifft den Idempotenz-Cache
        // (der Lookup passiert vor `try_acquire`) und liefert denselben Job
        // zurück, obwohl das Fenster bereits ausgeschöpft ist.
        let duplicate = supervisor
            .submit_job(&submit_context(), submission())
            .await
            .map_err(ctx("submit_job"))?;
        assert_eq!(duplicate.work_id, first.work_id);

        // (c) Eine andere Einreichung ohne Schlüssel erreicht die
        // Ratenbegrenzung wie jede neue Zulassung und findet das Fenster
        // ausgeschöpft.
        let rejected_without_key = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "distinct"}),
                    idempotency_key: None,
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            rejected_without_key,
            Err(McpSupervisorError::RateLimited { .. })
        ));

        // (d) Eine andere Einreichung mit einem NEUEN Schlüssel darf das
        // Limit nicht umgehen: sie ist kein Idempotenz-Treffer (anderer
        // Schlüssel, kein bestehender Job) und muss daher ebenfalls die
        // Ratenbegrenzung durchlaufen und abgelehnt werden. Dies verhindert
        // eine Regression, bei der ein Client das Limit durch einen
        // jeweils neuen Idempotenzschlüssel pro Einreichung umgehen könnte.
        let rejected_with_new_key = supervisor
            .submit_job(
                &submit_context(),
                McpJobSubmission {
                    kind: McpSubmittedJobKind::Worker,
                    input: serde_json::json!({"task": "distinct"}),
                    idempotency_key: Some("node-2".to_owned()),
                    budget: None,
                },
            )
            .await;
        assert!(matches!(
            rejected_with_new_key,
            Err(McpSupervisorError::RateLimited { .. })
        ));

        // (e) Im Store liegt genau der eine Job aus (a)/(b).
        let page = store
            .list(&harw_session_store::JobListQuery::default())
            .map_err(ctx("list"))?;
        assert_eq!(page.jobs.len(), 1);
        Ok(())
    }

    #[test]
    fn admission_limits_new_rejects_zero() {
        assert!(McpAdmissionLimits::new(0, SignedDuration::from_secs(1)).is_none());
        assert!(McpAdmissionLimits::new(1, SignedDuration::ZERO).is_none());
        assert_eq!(
            McpAdmissionLimits::default().max_submissions(),
            McpAdmissionLimits::DEFAULT_MAX_SUBMISSIONS
        );
    }
}
