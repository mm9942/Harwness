//! Typed `WorkRequest` lifecycle for the `/request /review /approve /deny
//! /cancel` grammar (docs/design/telegram-sandbox-work-requests.md).
//!
//! # Responsibility
//! This module owns the *typed* boundary between a parsed
//! [`harw_channel_telegram_transport::TelegramCommand`] and a sandboxed
//! worker launch: it resolves a closed-grammar workspace alias through the
//! authoritative `harw_authority::WorkspaceRegistry` (never trusting the alias
//! string directly as a filesystem selector), mints a [`WorkId`], durably
//! tracks the request through its lifecycle states and, on `/approve`, hands
//! the immutable approved payload to an injected [`WorkLauncher`].
//!
//! Der Launch selbst ist bewusst entkoppelt: dieses Crate hängt weder von
//! `harw-session-store`s `JobStore` noch von `harw-job-runtime` ab. Die
//! Komposition (`harw-cli/src/gateway.rs`) installiert über
//! [`WorkRequestStore::with_launcher`] eine [`WorkLauncher`]-Implementierung,
//! die den genehmigten Auftrag als durablen Job zulässt. Ohne installierten
//! Launcher bleibt ein genehmigter Auftrag `Approved` und
//! [`launch_sandboxed_worker`] meldet
//! [`TelegramChannelError::LaunchNotYetAvailable`].
//!
//! # Key types
//! - [`WorkRequestRecord`] — the durable, typed request.
//! - [`WorkRequestState`] — its closed lifecycle.
//! - [`ApprovedWorkRequest`] — the digest-verified, immutable payload a
//!   launcher receives.
//! - [`WorkLauncher`] / [`LaunchReceipt`] / [`WorkLaunchError`] — the
//!   injectable launch seam.
//! - [`WorkRequestStore`] — a small file-backed durable store keyed by
//!   [`WorkId`], modelled after `harw_channel::PairingStore`'s atomic
//!   temp-file-plus-rename persistence but with a single in-process
//!   [`Mutex`] instead of per-key `fs4` advisory locks: this binding's
//!   production wiring drives one Telegram binding's admitted events through
//!   a single dedicated thread (see `harw-cli/src/gateway.rs`'s
//!   `start_telegram_long_poll`), so there is no cross-process writer to
//!   arbitrate.
//!
//! # Invariant
//! Only the closed `/request <workspace-alias> <role> <task>` grammar
//! (already validated by
//! `harw_channel_telegram_transport::mapping::parse_command`) can select a
//! workspace, and only through [`WorkspaceRegistry::resolve`] against the
//! requester's *paired* tenant. Free-form message text, attachments, and
//! callback payloads never reach [`WorkRequestStore::submit`]'s alias
//! parameter.
//!
//! Ein Launch verwendet ausschließlich die bei `/request` gespeicherte
//! Aufgabe, und nur, wenn ihr SHA-256-Digest mit dem im Datensatz fixierten
//! `task_digest` übereinstimmt (Design-Doc §"Telegram lifecycle" Schritt 7:
//! "resume only the stored request payload"). Ein fehlender oder
//! manipulierter Payload schlägt geschlossen fehl, bevor der Launcher
//! aufgerufen wird.
//!
//! # Concurrency
//! [`WorkRequestStore`] is `Clone` (`Arc`-backed) and `Send + Sync`; every
//! mutation is serialized through its internal [`Mutex`]. `/approve` hält
//! diesen Mutex auch über den Launcher-Aufruf, damit ein gleichzeitiges
//! `/cancel` nie zwischen Zulassung des Jobs und dem Übergang nach
//! `Launched` greifen kann.
//!
//! # Errors
//! All fallible paths return [`TelegramChannelResult`]; see
//! [`TelegramChannelError`]'s `WorkspaceUnresolved`, `InvalidWorkRequestRole`,
//! `WorkRequestNotFound`, `WorkRequestInvalidTransition`,
//! `LaunchNotYetAvailable`, `Io`, and `Serde` variants. Ein vom Launcher
//! gemeldeter [`WorkLaunchError`] wird von [`launch_sandboxed_worker`] bis zu
//! einer eigenen Fehlervariante als `Io` (`ErrorKind::Other`, Quelle ist der
//! `WorkLaunchError`) weitergereicht.
//!
//! # Audit
//! Every admitted `/request /review /approve /deny /cancel` invocation
//! appends a `channel.command` record to a `harw_session_store::TranscriptStore`
//! journal under this store's root (§5 "Full audit of inbound commands"),
//! the same durable append-only mechanism `harw_channel::PairingStore`
//! already uses for pairing lifecycle events; launch attempts add `launch`
//! bzw. `launch_failed` records. This is **not** the
//! `harw-secrets` cryptographic audit chain (hash-chained, tamper-evident,
//! used for secret create/rotate/delete): `harw-cli/src/gateway.rs`
//! deliberately holds no writable handle to that chain (see its
//! `audit_chain_scheduler` doc comment — this gateway process never calls
//! `create`/`rotate`/`delete` against a `SecretStore`, precisely to avoid
//! holding decrypted KEK material outside a secret's own lifecycle). Mirroring
//! that chain's tamper-evidence for channel commands would need a write path
//! added at `harw-cli/src/secret_store.rs`, outside this change's file scope.
//! A journal write failure is logged (`tracing::error!`) but never fails the
//! command it audits — the durable lifecycle transition already succeeded by
//! the time the journal is written.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use harw_authority::WorkspaceRegistry;
use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
use harw_types::{ChannelId, PeerId, SessionId, TenantId, ThreadRef, WorkId, WorkspaceId};

use crate::error::{TelegramChannelError, TelegramChannelResult};

/// Empfohlener `JobKind::Custom`-Diskriminator für durable Jobs, die aus
/// einem genehmigten Telegram-Auftrag entstehen.
///
/// Bewusst **nicht** `JobKind::Worker`: `harw-cli/src/job_worker.rs`
/// (`check_prompt_claim_scope`) führt Prompt-Jobs nur für konfigurierte
/// MCP-Operatoren aus und lehnt `ApprovalActor::ChannelPeer`-Einreicher ab.
/// Ein Telegram-Auftrag braucht einen eigenen, sandboxed Ausführungspfad.
pub const TELEGRAM_WORK_REQUEST_JOB_KIND: &str = "telegram-work-request";

/// Closed lifecycle of a Telegram-originated work request
/// (docs/design/telegram-sandbox-work-requests.md's integration order + the
/// `/review /approve /deny /cancel` grammar).
///
/// # Variants
/// - `Requested` — durably persisted by `/request`, not yet previewed.
/// - `UnderReview` — `/review` rendered the resolved workspace/role/sandbox
///   preview; the immutable request digest an `/approve` acts on is fixed
///   from this point.
/// - `Approved` — `/approve` accepted the reviewed request, but no worker has
///   been launched yet (no [`WorkLauncher`] installed, or the last launch
///   attempt failed; a repeated `/approve` retries the launch).
/// - `Launched` — terminal for this store: the installed [`WorkLauncher`]
///   accepted the request; [`WorkRequestRecord::launch`] names the durable
///   job. Weitere Steuerung (z. B. Abbruch) läuft über den Job-Store.
/// - `Denied` — terminal: `/deny` rejected the request.
/// - `Cancelled` — terminal: `/cancel` withdrew the request (by the
///   requester or an operator) before launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkRequestState {
    Requested,
    UnderReview,
    Approved,
    Launched,
    Denied,
    Cancelled,
}

impl WorkRequestState {
    /// A short, stable label for error messages and audit strings.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::UnderReview => "under_review",
            Self::Approved => "approved",
            Self::Launched => "launched",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A durable, typed Telegram work request
/// (docs/design/telegram-sandbox-work-requests.md, "Typed request boundary").
///
/// The task text itself is never stored here — only its normalized digest —
/// mirroring the audit rule elsewhere in this codebase that a durable record
/// keeps a content hash rather than a second copy of possibly-sensitive
/// message bodies. Der für den Launch nötige Aufgabentext liegt getrennt in
/// einer privaten Payload-Datei des [`WorkRequestStore`] und wird nach
/// erfolgreichem Launch bzw. bei `/deny`/`/cancel` gelöscht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkRequestRecord {
    pub work_id: WorkId,
    pub channel: ChannelId,
    /// The actual acting human (`SenderRef::id` when present), never a
    /// group's `PeerId` — a work request must be attributable to one person.
    pub requester: PeerId,
    /// The requester's *paired* tenant at request time (never derived from
    /// message content).
    pub tenant: TenantId,
    /// The canonical workspace identity, resolved once through
    /// [`WorkspaceRegistry::resolve`] at submission time.
    pub workspace: WorkspaceId,
    pub role: String,
    /// SHA-256 hex digest of the normalized (trimmed) task text.
    pub task_digest: String,
    /// The Telegram `update_id` that created this request, for replay/audit
    /// correlation (§5).
    pub source_update_id: String,
    pub state: WorkRequestState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Quittung des Launchers; gesetzt genau dann, wenn `state == Launched`.
    /// Fehlt in Datensätzen, die vor dieser Erweiterung geschrieben wurden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchReceipt>,
}

/// Der handelnde Telegram-Nutzer einer Lebenszyklus-Aktion
/// (`/review /approve /deny /cancel`) bzw. einer Auflistung.
///
/// Alle Felder stammen aus dem bereits zugelassenen Ereignis (Binding,
/// gepaarter Tenant, `SenderRef::id` des Absenders) und der
/// serverseitigen Admin-Konfiguration — nie aus Nachrichteninhalt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRequestActor {
    /// Binding, über das die Aktion eintraf.
    pub channel: ChannelId,
    /// Gepaarter Tenant des Handelnden.
    pub tenant: TenantId,
    /// Telegram-User-ID des handelnden Menschen (nie eine Gruppen-`PeerId`).
    pub peer: PeerId,
    /// `true`, wenn der Handelnde als Admin dieses Bindings konfiguriert ist.
    pub is_admin: bool,
}

impl WorkRequestActor {
    /// Ob `record` für diesen Handelnden sichtbar und steuerbar ist: gleiches
    /// Binding, gleicher Tenant und Anfragender selbst oder Admin.
    fn may_access(&self, record: &WorkRequestRecord) -> bool {
        record.channel == self.channel
            && record.tenant == self.tenant
            && (self.is_admin || record.requester == self.peer)
    }
}

/// Quittung eines erfolgreichen Launches: der durable Job, unter dem der
/// genehmigte Auftrag nun läuft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchReceipt {
    /// `WorkId` des zugelassenen durablen Jobs. Die Referenz-Implementierung
    /// verwendet die `WorkId` des Auftrags selbst, damit ein wiederholter
    /// Launch idempotent auf denselben Job trifft.
    pub job_id: WorkId,
    /// Zeitpunkt der Zulassung durch den Launcher.
    pub launched_at: Timestamp,
}

/// Vom [`WorkLauncher`] gemeldeter Fehlschlag; der Auftrag bleibt `Approved`
/// und ein erneutes `/approve` versucht den Launch nochmals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkLaunchError {
    reason: String,
}

impl WorkLaunchError {
    /// Erzeugt einen Fehler mit einer operator-lesbaren Begründung. Die
    /// Begründung wird dem Telegram-Anfragenden angezeigt und darf daher
    /// keine Pfade, Secrets oder Tokens enthalten.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// Die Begründung dieses Fehlschlags.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for WorkLaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "work launch failed: {}", self.reason)
    }
}

impl std::error::Error for WorkLaunchError {}

/// Ein genehmigter Auftrag samt der digest-geprüften, gespeicherten Aufgabe
/// — die einzige Eingabe, die ein [`WorkLauncher`] erhält.
///
/// Konstruierbar nur über [`ApprovedWorkRequest::new`], das den Zustand
/// `Approved` und die Übereinstimmung von Aufgabe und `task_digest` erzwingt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedWorkRequest {
    record: WorkRequestRecord,
    task: String,
}

impl ApprovedWorkRequest {
    /// Bindet `task` an einen genehmigten `record`.
    ///
    /// # Errors
    /// - [`TelegramChannelError::WorkRequestInvalidTransition`]: `record` ist
    ///   nicht im Zustand `Approved`.
    /// - [`TelegramChannelError::Io`] (`InvalidData`): der normalisierte
    ///   Digest von `task` weicht vom fixierten `task_digest` ab.
    pub fn new(record: WorkRequestRecord, task: impl Into<String>) -> TelegramChannelResult<Self> {
        if record.state != WorkRequestState::Approved {
            return Err(TelegramChannelError::WorkRequestInvalidTransition {
                work_id: record.work_id.clone(),
                from: record.state.as_str(),
                action: "launched",
            });
        }
        let task = task.into();
        if task_digest(&task) != record.task_digest {
            return Err(TelegramChannelError::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stored task payload does not match the approved task digest",
            )));
        }
        Ok(Self {
            record,
            task: task.trim().to_owned(),
        })
    }

    /// Der genehmigte Datensatz (Tenant, Workspace, Rolle, Anfragender, …).
    #[must_use]
    pub fn record(&self) -> &WorkRequestRecord {
        &self.record
    }

    /// Der normalisierte (getrimmte) Aufgabentext. Bleibt unvertrauenswürdige
    /// Nutzereingabe: er ist Daten für den Worker, nie eine Autorität über
    /// Pfade oder Berechtigungen.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    /// Serverseitig aufgelöste Job-Eingabe für `StoredJob::input`.
    ///
    /// `tenant` und `workspace` entsprechen exakt dem `JobScope`, den der
    /// Launcher aus demselben Datensatz bildet — `harw-cli/src/job_worker.rs`
    /// (`check_input_declared_scope`) lehnt jede Abweichung ab.
    #[must_use]
    pub fn job_input(&self) -> serde_json::Value {
        serde_json::json!({
            "task": self.task,
            "role": self.record.role,
            "tenant": self.record.tenant.as_str(),
            "workspace": self.record.workspace.as_str(),
            "work_request": {
                "work_id": self.record.work_id.as_str(),
                "channel": self.record.channel.as_str(),
                "requester": self.record.requester.as_str(),
                "task_digest": self.record.task_digest,
                "source_update_id": self.record.source_update_id,
            },
        })
    }
}

/// Injizierbare Launch-Naht: übergibt einen genehmigten Auftrag an ein
/// durables Job-System.
///
/// # Contract
/// - Idempotent pro `request.record().work_id`: ein zweiter Aufruf für
///   denselben Auftrag (z. B. nach einem Absturz zwischen Job-Zulassung und
///   dem Übergang nach `Launched`) darf keinen zweiten Job erzeugen, sondern
///   muss die Quittung des bestehenden liefern.
/// - Der Launcher leitet Scope und Berechtigungen ausschließlich aus
///   `request.record()` und der serverseitigen Konfiguration ab — nie aus
///   `request.task()`.
/// - Blockierend erlaubt: [`WorkRequestStore::approve`] ruft ihn synchron
///   unter dem Store-Mutex auf.
pub trait WorkLauncher: Send + Sync {
    /// Lässt `request` als durablen Job zu.
    ///
    /// # Errors
    /// [`WorkLaunchError`], wenn der Job nicht zugelassen werden konnte; der
    /// Auftrag bleibt dann `Approved`.
    fn launch(&self, request: &ApprovedWorkRequest) -> Result<LaunchReceipt, WorkLaunchError>;
}

/// Launches the sandboxed worker for an `Approved` work request through the
/// configured [`WorkLauncher`].
///
/// # Arguments
/// - `launcher`: der installierte Launcher, oder `None`, wenn die
///   Komposition keinen installiert hat.
/// - `request`: der digest-geprüfte genehmigte Auftrag.
///
/// # Errors
/// - [`TelegramChannelError::LaunchNotYetAvailable`]: `launcher` ist `None`.
/// - [`TelegramChannelError::Io`] (`ErrorKind::Other`, Quelle
///   [`WorkLaunchError`]): der Launcher hat den Auftrag abgelehnt.
pub fn launch_sandboxed_worker(
    launcher: Option<&dyn WorkLauncher>,
    request: &ApprovedWorkRequest,
) -> TelegramChannelResult<LaunchReceipt> {
    let Some(launcher) = launcher else {
        return Err(TelegramChannelError::LaunchNotYetAvailable {
            work_id: request.record.work_id.clone(),
        });
    };
    launcher
        .launch(request)
        .map_err(|error| TelegramChannelError::from(std::io::Error::other(error)))
}

/// Normalizes and hashes task text for durable storage (never the raw text).
fn task_digest(task: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(task.trim().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// The closed role-argument grammar `/request` already enforces at parse
/// time (`is_atom` in `harw_channel_telegram_transport::mapping`); re-checked
/// here so this module never trusts a caller that bypasses that parser.
fn is_valid_role(role: &str) -> bool {
    !role.is_empty() && !role.chars().any(char::is_whitespace)
}

// Hex-encodes a `WorkId` into a filesystem-safe filename. `WorkId::new()`
// values are UUIDs (already filename-safe), but a caller could in principle
// construct one from arbitrary non-empty text via `try_from_str`; encoding
// defensively keeps this store's on-disk layout independent from that.
fn record_filename(work_id: &WorkId) -> String {
    let mut out = String::with_capacity(work_id.as_str().len() * 2);
    for byte in work_id.as_str().as_bytes() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> TelegramChannelResult<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TelegramChannelError::from(error)),
    }
}

// `NamedTempFile` legt die Datei unter Unix mit Modus 0600 an; der
// Aufgaben-Payload ist damit nur für den Gateway-Nutzer lesbar.
fn persist_json<T: Serialize>(path: &Path, value: &T) -> TelegramChannelResult<()> {
    let parent = path.parent().ok_or_else(|| {
        TelegramChannelError::from(std::io::Error::other(
            "work-request record path has no parent",
        ))
    })?;
    std::fs::create_dir_all(parent)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temp.as_file_mut(), value)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| TelegramChannelError::from(error.error))?;
    Ok(())
}

/// Gespeicherter Aufgabentext eines Auftrags, getrennt vom Datensatz.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTaskPayload {
    work_id: WorkId,
    /// Normalisierter (getrimmter) Aufgabentext.
    task: String,
}

/// Durable, file-backed store for [`WorkRequestRecord`]s. See the module
/// docs for why this uses one in-process [`Mutex`] rather than `fs4`
/// cross-process advisory locks.
#[derive(Clone)]
pub struct WorkRequestStore {
    root: PathBuf,
    guard: Arc<Mutex<()>>,
    launcher: Option<Arc<dyn WorkLauncher>>,
}

impl fmt::Debug for WorkRequestStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkRequestStore")
            .field("root", &self.root)
            .field("launcher_installed", &self.launcher.is_some())
            .finish()
    }
}

impl WorkRequestStore {
    /// Creates a store rooted at `root` (created lazily on first write),
    /// ohne installierten [`WorkLauncher`].
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            guard: Arc::new(Mutex::new(())),
            launcher: None,
        }
    }

    /// Installiert den [`WorkLauncher`], über den `/approve` genehmigte
    /// Aufträge startet.
    #[must_use]
    pub fn with_launcher(mut self, launcher: Arc<dyn WorkLauncher>) -> Self {
        self.launcher = Some(launcher);
        self
    }

    fn path(&self, work_id: &WorkId) -> PathBuf {
        self.root
            .join("records")
            .join(record_filename(work_id))
            .with_extension("json")
    }

    fn payload_path(&self, work_id: &WorkId) -> PathBuf {
        self.root
            .join("payloads")
            .join(record_filename(work_id))
            .with_extension("json")
    }

    /// Reads the current record for `work_id`, or `Ok(None)` if unknown.
    pub fn get(&self, work_id: &WorkId) -> TelegramChannelResult<Option<WorkRequestRecord>> {
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        read_json(&self.path(work_id))
    }

    /// Builds and durably persists a new `Requested` work request (§"Typed
    /// request boundary").
    ///
    /// # Arguments
    /// - `channel`/`requester`/`tenant`/`source_update_id`: trusted context
    ///   the admission pipeline already resolved for this event.
    /// - `workspace_alias` (`&str`): the closed-grammar alias from `/request`
    ///   — resolved through `workspaces`, never trusted directly.
    /// - `role` (`&str`): the closed-grammar role atom from `/request`.
    /// - `task` (`&str`): untrusted task text; der Datensatz enthält nur den
    ///   normalisierten Digest, der normalisierte Text selbst liegt in einer
    ///   separaten privaten Payload-Datei für den späteren Launch.
    /// - `workspaces` (`&WorkspaceRegistry`): the authoritative registry;
    ///   resolution failure fails closed.
    ///
    /// # Errors
    /// - [`TelegramChannelError::InvalidWorkRequestRole`]: `role` is empty or
    ///   contains whitespace.
    /// - [`TelegramChannelError::WorkspaceUnresolved`]: the alias does not
    ///   resolve for `tenant` in `workspaces` (unknown, disabled,
    ///   cross-tenant, or otherwise unbound alias — fails closed per the
    ///   design doc's acceptance gates).
    /// - [`TelegramChannelError::Io`] / [`TelegramChannelError::Serde`]:
    ///   persistence failure.
    #[allow(clippy::too_many_arguments)]
    pub fn submit(
        &self,
        channel: &ChannelId,
        requester: &PeerId,
        tenant: &TenantId,
        workspace_alias: &str,
        role: &str,
        task: &str,
        source_update_id: &str,
        workspaces: &WorkspaceRegistry,
        now: Timestamp,
    ) -> TelegramChannelResult<WorkRequestRecord> {
        if !is_valid_role(role) {
            return Err(TelegramChannelError::InvalidWorkRequestRole {
                role: role.to_owned(),
            });
        }
        let workspace_id = WorkspaceId::from_str(workspace_alias);
        workspaces
            .resolve(tenant, &workspace_id)
            .map_err(|source| TelegramChannelError::WorkspaceUnresolved {
                alias: workspace_alias.to_owned(),
                tenant: tenant.clone(),
                source,
            })?;

        let record = WorkRequestRecord {
            work_id: WorkId::new(),
            channel: channel.clone(),
            requester: requester.clone(),
            tenant: tenant.clone(),
            workspace: workspace_id,
            role: role.to_owned(),
            task_digest: task_digest(task),
            source_update_id: source_update_id.to_owned(),
            state: WorkRequestState::Requested,
            created_at: now,
            updated_at: now,
            launch: None,
        };
        let payload = StoredTaskPayload {
            work_id: record.work_id.clone(),
            task: task.trim().to_owned(),
        };

        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        // Payload zuerst: ein Datensatz ohne Payload wäre nie startbar.
        persist_json(&self.payload_path(&record.work_id), &payload)?;
        if let Err(error) = persist_json(&self.path(&record.work_id), &record) {
            self.remove_payload(&record.work_id);
            return Err(error);
        }
        drop(_guard);
        self.record_command_audit("request", now, &record);
        Ok(record)
    }

    /// Atomically transitions `work_id` from one of `expected` states to
    /// `next`, or fails without mutating anything.
    fn transition(
        &self,
        work_id: &WorkId,
        expected: &[WorkRequestState],
        next: WorkRequestState,
        now: Timestamp,
    ) -> TelegramChannelResult<WorkRequestRecord> {
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        let path = self.path(work_id);
        let mut record: WorkRequestRecord =
            read_json(&path)?.ok_or_else(|| TelegramChannelError::WorkRequestNotFound {
                work_id: work_id.clone(),
            })?;
        if !expected.contains(&record.state) {
            return Err(TelegramChannelError::WorkRequestInvalidTransition {
                work_id: work_id.clone(),
                from: record.state.as_str(),
                action: transition_action(next),
            });
        }
        record.state = next;
        record.updated_at = now;
        persist_json(&path, &record)?;
        Ok(record)
    }

    /// `/review <work-id>`: renders the resolved-workspace/role preview and
    /// transitions `Requested -> UnderReview` (idempotent if already under
    /// review).
    pub fn review(&self, work_id: &WorkId, now: Timestamp) -> TelegramChannelResult<String> {
        let record = if let Some(existing) = self.get(work_id)? {
            if existing.state == WorkRequestState::UnderReview {
                existing
            } else {
                self.transition(
                    work_id,
                    &[WorkRequestState::Requested],
                    WorkRequestState::UnderReview,
                    now,
                )?
            }
        } else {
            return Err(TelegramChannelError::WorkRequestNotFound {
                work_id: work_id.clone(),
            });
        };
        self.record_command_audit("review", now, &record);
        Ok(format!(
            "Review {}: workspace='{}' role='{}' state={}",
            record.work_id,
            record.workspace,
            record.role,
            record.state.as_str()
        ))
    }

    /// `/approve <work-id>`: accepts a reviewed request (`UnderReview ->
    /// Approved`) and launches it through the installed [`WorkLauncher`]
    /// (`Approved -> Launched`).
    ///
    /// # Description
    /// - Ohne installierten Launcher bleibt der Auftrag `Approved`; die
    ///   Antwort meldet, dass der Launch noch nicht verfügbar ist.
    /// - Schlägt der Launch fehl (Launcher-Fehler, fehlender oder
    ///   manipulierter Payload), bleibt der Auftrag `Approved`; ein erneutes
    ///   `/approve` auf einen `Approved`-Auftrag wiederholt nur den Launch.
    /// - Ein `/approve` auf einen bereits `Launched`-Auftrag ist idempotent
    ///   und startet nichts erneut.
    ///
    /// # Errors
    /// - [`TelegramChannelError::WorkRequestNotFound`]: unbekannte `WorkId`.
    /// - [`TelegramChannelError::WorkRequestInvalidTransition`] bei Zustand
    ///   `Requested`, `Denied` oder `Cancelled`.
    /// - [`TelegramChannelError::Io`] / [`TelegramChannelError::Serde`]:
    ///   Persistenzfehler des Datensatzes. Ein Launch-Fehlschlag ist **kein**
    ///   `Err`, sondern wird in der Antwort gemeldet.
    pub fn approve(&self, work_id: &WorkId, now: Timestamp) -> TelegramChannelResult<String> {
        let guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        let path = self.path(work_id);
        let mut record: WorkRequestRecord =
            read_json(&path)?.ok_or_else(|| TelegramChannelError::WorkRequestNotFound {
                work_id: work_id.clone(),
            })?;
        let current = record.state;
        let newly_approved = match current {
            WorkRequestState::UnderReview => {
                record.state = WorkRequestState::Approved;
                record.updated_at = now;
                persist_json(&path, &record)?;
                true
            }
            WorkRequestState::Approved => false,
            WorkRequestState::Launched => {
                drop(guard);
                let job = record
                    .launch
                    .as_ref()
                    .map(|receipt| receipt.job_id.as_str().to_owned())
                    .unwrap_or_default();
                return Ok(format!(
                    "{} is already launched as job {job}",
                    record.work_id
                ));
            }
            WorkRequestState::Requested
            | WorkRequestState::Denied
            | WorkRequestState::Cancelled => {
                return Err(TelegramChannelError::WorkRequestInvalidTransition {
                    work_id: work_id.clone(),
                    from: current.as_str(),
                    action: transition_action(WorkRequestState::Approved),
                });
            }
        };

        let outcome = match self.launcher.as_deref() {
            Some(launcher) => self
                .approved_request(&record)
                .and_then(|approved| launch_sandboxed_worker(Some(launcher), &approved)),
            None => Err(TelegramChannelError::LaunchNotYetAvailable {
                work_id: record.work_id.clone(),
            }),
        };

        match outcome {
            Ok(receipt) => {
                let approved_record = record.clone();
                record.state = WorkRequestState::Launched;
                record.updated_at = now;
                record.launch = Some(receipt.clone());
                persist_json(&path, &record)?;
                self.remove_payload(work_id);
                drop(guard);
                if newly_approved {
                    self.record_command_audit("approve", now, &approved_record);
                }
                self.record_command_audit("launch", now, &record);
                Ok(format!(
                    "Approved {}, launched as job {}",
                    record.work_id, receipt.job_id
                ))
            }
            Err(TelegramChannelError::LaunchNotYetAvailable { work_id }) => {
                drop(guard);
                if newly_approved {
                    self.record_command_audit("approve", now, &record);
                }
                Ok(format!(
                    "Approved {work_id}, but sandboxed launch is not yet available"
                ))
            }
            Err(error) => {
                drop(guard);
                if newly_approved {
                    self.record_command_audit("approve", now, &record);
                }
                self.record_command_audit("launch_failed", now, &record);
                tracing::warn!(
                    work_id = %record.work_id,
                    error = %error,
                    "approved Telegram work request could not be launched"
                );
                Ok(format!(
                    "Approved {id}, but launch failed: {error}; /approve {id} retries the launch",
                    id = record.work_id
                ))
            }
        }
    }

    /// Lädt den gespeicherten Payload von `record` und bindet ihn
    /// digest-geprüft an den Datensatz. Aufrufer hält den Store-Mutex.
    fn approved_request(
        &self,
        record: &WorkRequestRecord,
    ) -> TelegramChannelResult<ApprovedWorkRequest> {
        let payload: StoredTaskPayload = read_json(&self.payload_path(&record.work_id))?
            .ok_or_else(|| {
                TelegramChannelError::from(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "stored task payload for the approved work request is missing",
                ))
            })?;
        if payload.work_id != record.work_id {
            return Err(TelegramChannelError::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stored task payload belongs to another work request",
            )));
        }
        ApprovedWorkRequest::new(record.clone(), payload.task)
    }

    /// `/deny <work-id>`: terminally rejects a request that has not yet been
    /// approved.
    pub fn deny(&self, work_id: &WorkId, now: Timestamp) -> TelegramChannelResult<String> {
        let record = self.transition(
            work_id,
            &[WorkRequestState::Requested, WorkRequestState::UnderReview],
            WorkRequestState::Denied,
            now,
        )?;
        self.remove_payload(work_id);
        self.record_command_audit("deny", now, &record);
        Ok(format!("Denied {}", record.work_id))
    }

    /// `/cancel <work-id>`: terminally withdraws a request that has not yet
    /// been launched. Ein `Launched`-Auftrag ist hier ein ungültiger
    /// Übergang: sein durabler Job muss über den Job-Store abgebrochen
    /// werden. Da `/approve` den Store-Mutex über den gesamten Launch hält,
    /// kann ein `/cancel` nie mit einem laufenden Launch verschränken.
    pub fn cancel(&self, work_id: &WorkId, now: Timestamp) -> TelegramChannelResult<String> {
        let record = self.transition(
            work_id,
            &[
                WorkRequestState::Requested,
                WorkRequestState::UnderReview,
                WorkRequestState::Approved,
            ],
            WorkRequestState::Cancelled,
            now,
        )?;
        self.remove_payload(work_id);
        self.record_command_audit("cancel", now, &record);
        Ok(format!("Cancelled {}", record.work_id))
    }

    /// Liefert den Datensatz `work_id`, sofern `actor` ihn sehen und steuern
    /// darf: Binding und Tenant müssen übereinstimmen, und `actor` muss der
    /// Anfragende oder ein Admin sein.
    ///
    /// # Errors
    /// - [`TelegramChannelError::WorkRequestNotFound`]: unbekannte `WorkId`
    ///   **oder** fehlende Berechtigung — beide Fälle sind bewusst nicht
    ///   unterscheidbar, damit fremde Aufträge ihre Existenz nicht verraten.
    /// - [`TelegramChannelError::Io`] / [`TelegramChannelError::Serde`]:
    ///   Lesefehler des Datensatzes.
    pub fn authorize(
        &self,
        work_id: &WorkId,
        actor: &WorkRequestActor,
    ) -> TelegramChannelResult<WorkRequestRecord> {
        match self.get(work_id)? {
            Some(record) if actor.may_access(&record) => Ok(record),
            _ => Err(TelegramChannelError::WorkRequestNotFound {
                work_id: work_id.clone(),
            }),
        }
    }

    /// [`Self::review`] nach erfolgreicher [`Self::authorize`]-Prüfung.
    ///
    /// # Errors
    /// Wie [`Self::authorize`] und [`Self::review`].
    pub fn review_as(
        &self,
        work_id: &WorkId,
        actor: &WorkRequestActor,
        now: Timestamp,
    ) -> TelegramChannelResult<String> {
        self.authorize(work_id, actor)?;
        self.review(work_id, now)
    }

    /// [`Self::approve`] nach erfolgreicher [`Self::authorize`]-Prüfung.
    ///
    /// # Errors
    /// Wie [`Self::authorize`] und [`Self::approve`].
    pub fn approve_as(
        &self,
        work_id: &WorkId,
        actor: &WorkRequestActor,
        now: Timestamp,
    ) -> TelegramChannelResult<String> {
        self.authorize(work_id, actor)?;
        self.approve(work_id, now)
    }

    /// [`Self::deny`] nach erfolgreicher [`Self::authorize`]-Prüfung.
    ///
    /// # Errors
    /// Wie [`Self::authorize`] und [`Self::deny`].
    pub fn deny_as(
        &self,
        work_id: &WorkId,
        actor: &WorkRequestActor,
        now: Timestamp,
    ) -> TelegramChannelResult<String> {
        self.authorize(work_id, actor)?;
        self.deny(work_id, now)
    }

    /// [`Self::cancel`] nach erfolgreicher [`Self::authorize`]-Prüfung.
    ///
    /// # Errors
    /// Wie [`Self::authorize`] und [`Self::cancel`].
    pub fn cancel_as(
        &self,
        work_id: &WorkId,
        actor: &WorkRequestActor,
        now: Timestamp,
    ) -> TelegramChannelResult<String> {
        self.authorize(work_id, actor)?;
        self.cancel(work_id, now)
    }

    /// Listet höchstens `limit` für `actor` sichtbare Aufträge, neueste zuerst
    /// (nach `created_at`, bei Gleichstand nach `WorkId`).
    ///
    /// Unlesbare oder beschädigte Einzeldatensätze werden protokolliert und
    /// übersprungen, damit ein defekter Datensatz die Liste nicht blockiert.
    ///
    /// # Errors
    /// [`TelegramChannelError::Io`], wenn das Datensatzverzeichnis nicht
    /// gelesen werden kann (ein noch nicht existierendes Verzeichnis ergibt
    /// eine leere Liste).
    pub fn list_for(
        &self,
        actor: &WorkRequestActor,
        limit: usize,
    ) -> TelegramChannelResult<Vec<WorkRequestRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        let entries = match std::fs::read_dir(self.root.join("records")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(TelegramChannelError::from(error)),
        };
        let mut visible = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            match read_json::<WorkRequestRecord>(&path) {
                Ok(Some(record)) if actor.may_access(&record) => visible.push(record),
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    error = %error,
                    "work-request record could not be read while listing"
                ),
            }
        }
        visible.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.work_id.as_str().cmp(a.work_id.as_str()))
        });
        visible.truncate(limit);
        Ok(visible)
    }

    /// Löscht den gespeicherten Aufgabentext (best effort): nach Launch,
    /// `/deny` oder `/cancel` wird er von diesem Store nicht mehr gebraucht.
    fn remove_payload(&self, work_id: &WorkId) {
        match std::fs::remove_file(self.payload_path(work_id)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                work_id = %work_id,
                error = %error,
                "work-request task payload could not be removed"
            ),
        }
    }

    /// Appends a `channel.command` record to this store's audit journal
    /// (see module docs' "Audit" section). Best-effort: a journal write
    /// failure is logged, never returned, since the durable lifecycle
    /// mutation this audits has already succeeded.
    fn record_command_audit(
        &self,
        action: &'static str,
        now: Timestamp,
        record: &WorkRequestRecord,
    ) {
        let body = serde_json::json!({
            "event": "channel.command",
            "action": action,
            "work_id": record.work_id.as_str(),
            "channel": record.channel.as_str(),
            "requester": record.requester.as_str(),
            "tenant": record.tenant.as_str(),
            "workspace": record.workspace.as_str(),
            "role": record.role,
            "task_digest": record.task_digest,
            "source_update_id": record.source_update_id,
            "state": record.state.as_str(),
            "launch_job_id": record.launch.as_ref().map(|receipt| receipt.job_id.as_str()),
        });
        let transcript_record = TranscriptRecord::new(
            command_audit_session_id(&record.channel),
            ThreadRef::from_str(""),
            0,
            now,
            RecordKind::Lifecycle,
            body,
        );
        if let Err(error) =
            TranscriptStore::new(&self.root.join("journal")).append(&transcript_record)
        {
            tracing::error!(
                work_id = %record.work_id,
                action,
                error = %error,
                "channel.command audit journal write failed"
            );
        }
    }
}

// Deterministic, portable journal-session id for a channel's `channel.command`
// audit trail (mirrors `harw_channel::pairing_store`'s hex-encoded session-id
// convention so arbitrary channel identifiers cannot introduce path
// separators into the transcript store's filenames).
fn command_audit_session_id(channel: &ChannelId) -> SessionId {
    let mut hex = String::with_capacity(channel.as_str().len() * 2);
    for byte in channel.as_str().as_bytes() {
        hex.push_str(&format!("{byte:02x}"));
    }
    SessionId::from_str(format!("channel-command-audit-{hex}"))
}

fn transition_action(next: WorkRequestState) -> &'static str {
    match next {
        WorkRequestState::Requested => "reset to requested",
        WorkRequestState::UnderReview => "reviewed",
        WorkRequestState::Approved => "approved",
        WorkRequestState::Launched => "launched",
        WorkRequestState::Denied => "denied",
        WorkRequestState::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{WorkspaceRegistration, WorkspaceRegistry};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn registry(
        tenant: &TenantId,
        workspace: &WorkspaceId,
        root: &Path,
    ) -> TestResult<WorkspaceRegistry> {
        WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from("."),
            }],
        )
        .map_err(ctx("registry builds"))
    }

    #[test]
    fn submit_resolves_workspace_and_persists_requested_state() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());

        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                &tenant,
                "ops-room",
                "implementer",
                "  fix the failing test  ",
                "42",
                &registry,
                Timestamp::now(),
            )
            .map_err(ctx("submit succeeds"))?;

        assert_eq!(record.state, WorkRequestState::Requested);
        assert_eq!(record.workspace, workspace);
        assert_eq!(record.task_digest, task_digest("fix the failing test"));
        assert_eq!(
            store
                .get(&record.work_id)
                .map_err(ctx("get record"))?
                .ok_or(TestError::Missing("record"))?
                .work_id,
            record.work_id
        );
        Ok(())
    }

    #[test]
    fn submit_fails_closed_for_unresolved_workspace_alias() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let registry =
            WorkspaceRegistry::build(harness.path(), []).map_err(ctx("empty registry"))?;
        let store = WorkRequestStore::new(store_dir.path());

        let result = store.submit(
            &ChannelId::from_str("telegram:ops"),
            &PeerId::from_str("100"),
            &tenant,
            "unknown-room",
            "implementer",
            "task",
            "1",
            &registry,
            Timestamp::now(),
        );

        assert!(matches!(
            result,
            Err(TelegramChannelError::WorkspaceUnresolved { alias, .. }) if alias == "unknown-room"
        ));
        Ok(())
    }

    #[test]
    fn submit_rejects_whitespace_role() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());

        let result = store.submit(
            &ChannelId::from_str("telegram:ops"),
            &PeerId::from_str("100"),
            &tenant,
            "ops-room",
            "not a role",
            "task",
            "1",
            &registry,
            Timestamp::now(),
        );

        assert!(matches!(
            result,
            Err(TelegramChannelError::InvalidWorkRequestRole { role }) if role == "not a role"
        ));
        Ok(())
    }

    #[test]
    fn full_lifecycle_review_approve_reports_launch_not_yet_available() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());
        let now = Timestamp::now();

        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                &tenant,
                "ops-room",
                "implementer",
                "task",
                "1",
                &registry,
                now,
            )
            .map_err(ctx("submit"))?;

        let review = store.review(&record.work_id, now).map_err(ctx("review"))?;
        assert!(review.contains("ops-room"));
        assert_eq!(
            store
                .get(&record.work_id)
                .map_err(ctx("get record"))?
                .ok_or(TestError::Missing("record"))?
                .state,
            WorkRequestState::UnderReview
        );

        let approve = store
            .approve(&record.work_id, now)
            .map_err(ctx("approve"))?;
        assert!(approve.contains("not yet available"));
        assert_eq!(
            store
                .get(&record.work_id)
                .map_err(ctx("get record"))?
                .ok_or(TestError::Missing("record"))?
                .state,
            WorkRequestState::Approved
        );
        Ok(())
    }

    #[test]
    fn deny_after_approve_is_an_invalid_transition() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());
        let now = Timestamp::now();

        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                &tenant,
                "ops-room",
                "implementer",
                "task",
                "1",
                &registry,
                now,
            )
            .map_err(ctx("submit"))?;
        store.review(&record.work_id, now).map_err(ctx("review"))?;
        store
            .approve(&record.work_id, now)
            .map_err(ctx("approve"))?;

        let result = store.deny(&record.work_id, now);
        assert!(matches!(
            result,
            Err(TelegramChannelError::WorkRequestInvalidTransition { from, .. }) if from == "approved"
        ));
        Ok(())
    }

    #[test]
    fn cancel_after_approve_succeeds() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());
        let now = Timestamp::now();

        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                &tenant,
                "ops-room",
                "implementer",
                "task",
                "1",
                &registry,
                now,
            )
            .map_err(ctx("submit"))?;
        store.review(&record.work_id, now).map_err(ctx("review"))?;
        store
            .approve(&record.work_id, now)
            .map_err(ctx("approve"))?;

        let cancelled = store.cancel(&record.work_id, now).map_err(ctx("cancel"))?;
        assert!(cancelled.starts_with("Cancelled"));
        assert_eq!(
            store
                .get(&record.work_id)
                .map_err(ctx("get record"))?
                .ok_or(TestError::Missing("record"))?
                .state,
            WorkRequestState::Cancelled
        );
        Ok(())
    }

    #[test]
    fn unknown_work_id_is_reported_not_found() -> TestResult {
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let store = WorkRequestStore::new(store_dir.path());
        let missing = WorkId::from_str("missing-work-id");

        assert!(matches!(
            store.review(&missing, Timestamp::now()),
            Err(TelegramChannelError::WorkRequestNotFound { .. })
        ));
        Ok(())
    }

    fn approved_record(task: &str) -> WorkRequestRecord {
        WorkRequestRecord {
            work_id: WorkId::new(),
            channel: ChannelId::from_str("telegram:ops"),
            requester: PeerId::from_str("100"),
            tenant: TenantId::from_str("ops"),
            workspace: WorkspaceId::from_str("ops-room"),
            role: "implementer".to_owned(),
            task_digest: task_digest(task),
            source_update_id: "1".to_owned(),
            state: WorkRequestState::Approved,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            launch: None,
        }
    }

    #[test]
    fn launch_sandboxed_worker_reports_not_yet_available_without_launcher() -> TestResult {
        let record = approved_record("task");
        let approved =
            ApprovedWorkRequest::new(record.clone(), "task").map_err(ctx("approved request"))?;

        assert!(matches!(
            launch_sandboxed_worker(None, &approved),
            Err(TelegramChannelError::LaunchNotYetAvailable { work_id }) if work_id == record.work_id
        ));
        Ok(())
    }

    /// Test-Launcher: zeichnet jeden Aufruf auf und schlägt auf Wunsch fehl.
    #[derive(Default)]
    struct FakeLauncher {
        calls: Mutex<Vec<ApprovedWorkRequest>>,
        fail: AtomicBool,
    }

    impl FakeLauncher {
        fn calls(&self) -> Vec<ApprovedWorkRequest> {
            self.calls.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }
    }

    impl WorkLauncher for FakeLauncher {
        fn launch(&self, request: &ApprovedWorkRequest) -> Result<LaunchReceipt, WorkLaunchError> {
            self.calls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(request.clone());
            if self.fail.load(Ordering::SeqCst) {
                return Err(WorkLaunchError::new("job store unavailable"));
            }
            Ok(LaunchReceipt {
                job_id: request.record().work_id.clone(),
                launched_at: Timestamp::now(),
            })
        }
    }

    #[test]
    fn launch_sandboxed_worker_uses_configured_launcher() -> TestResult {
        let record = approved_record("task");
        let approved =
            ApprovedWorkRequest::new(record.clone(), "  task ").map_err(ctx("approved request"))?;
        let launcher = FakeLauncher::default();

        let receipt = launch_sandboxed_worker(Some(&launcher as &dyn WorkLauncher), &approved)
            .map_err(ctx("launch"))?;

        assert_eq!(receipt.job_id, record.work_id);
        assert_eq!(launcher.calls().len(), 1);

        launcher.fail.store(true, Ordering::SeqCst);
        let Err(TelegramChannelError::Io(error)) =
            launch_sandboxed_worker(Some(&launcher as &dyn WorkLauncher), &approved)
        else {
            return Err(TestError::Unexpected(
                "launcher failure must surface as an error".into(),
            ));
        };
        assert!(error.to_string().contains("job store unavailable"));
        Ok(())
    }

    // Reicht einen Auftrag ein und bringt ihn nach `UnderReview`.
    fn reviewed_request(
        store: &WorkRequestStore,
        registry: &WorkspaceRegistry,
        tenant: &TenantId,
        task: &str,
        now: Timestamp,
    ) -> TestResult<WorkRequestRecord> {
        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                tenant,
                "ops-room",
                "implementer",
                task,
                "7",
                registry,
                now,
            )
            .map_err(ctx("submit"))?;
        store.review(&record.work_id, now).map_err(ctx("review"))?;
        Ok(record)
    }

    fn state_of(store: &WorkRequestStore, work_id: &WorkId) -> TestResult<WorkRequestRecord> {
        store
            .get(work_id)
            .map_err(ctx("get record"))?
            .ok_or(TestError::Missing("record"))
    }

    #[test]
    fn approve_with_launcher_launches_stored_task_once() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let launcher = Arc::new(FakeLauncher::default());
        let store = WorkRequestStore::new(store_dir.path())
            .with_launcher(Arc::clone(&launcher) as Arc<dyn WorkLauncher>);
        let now = Timestamp::now();
        let record = reviewed_request(&store, &registry, &tenant, "  fix the build  ", now)?;

        let reply = store
            .approve(&record.work_id, now)
            .map_err(ctx("approve"))?;

        assert!(reply.contains("launched as job"));
        let calls = launcher.calls();
        assert_eq!(calls.len(), 1);
        let call = calls.first().ok_or(TestError::Missing("launch call"))?;
        assert_eq!(call.task(), "fix the build");
        assert_eq!(call.record().tenant, tenant);
        assert_eq!(call.record().workspace, workspace);
        let input = call.job_input();
        assert_eq!(input["task"], "fix the build");
        assert_eq!(input["tenant"], "ops");
        assert_eq!(input["workspace"], "ops-room");
        assert_eq!(input["work_request"]["work_id"], record.work_id.as_str());

        let stored = state_of(&store, &record.work_id)?;
        assert_eq!(stored.state, WorkRequestState::Launched);
        assert_eq!(
            stored.launch.map(|receipt| receipt.job_id),
            Some(record.work_id.clone())
        );
        assert!(!store.payload_path(&record.work_id).exists());

        // Ein wiederholtes `/approve` startet nichts erneut.
        let again = store
            .approve(&record.work_id, now)
            .map_err(ctx("approve again"))?;
        assert!(again.contains("already launched"));
        assert_eq!(launcher.calls().len(), 1);

        // Ein gestarteter Auftrag lässt sich hier nicht mehr abbrechen.
        assert!(matches!(
            store.cancel(&record.work_id, now),
            Err(TelegramChannelError::WorkRequestInvalidTransition { from, .. }) if from == "launched"
        ));
        Ok(())
    }

    #[test]
    fn failed_launch_keeps_approved_and_retry_launches() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let launcher = Arc::new(FakeLauncher::default());
        launcher.fail.store(true, Ordering::SeqCst);
        let store = WorkRequestStore::new(store_dir.path())
            .with_launcher(Arc::clone(&launcher) as Arc<dyn WorkLauncher>);
        let now = Timestamp::now();
        let record = reviewed_request(&store, &registry, &tenant, "task", now)?;

        let reply = store
            .approve(&record.work_id, now)
            .map_err(ctx("approve"))?;
        assert!(reply.contains("launch failed"));
        assert_eq!(
            state_of(&store, &record.work_id)?.state,
            WorkRequestState::Approved
        );
        assert!(store.payload_path(&record.work_id).exists());

        launcher.fail.store(false, Ordering::SeqCst);
        let retry = store
            .approve(&record.work_id, now)
            .map_err(ctx("approve retry"))?;
        assert!(retry.contains("launched as job"));
        assert_eq!(launcher.calls().len(), 2);
        assert_eq!(
            state_of(&store, &record.work_id)?.state,
            WorkRequestState::Launched
        );
        Ok(())
    }

    #[test]
    fn tampered_payload_fails_closed_without_calling_launcher() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let launcher = Arc::new(FakeLauncher::default());
        let store = WorkRequestStore::new(store_dir.path())
            .with_launcher(Arc::clone(&launcher) as Arc<dyn WorkLauncher>);
        let now = Timestamp::now();
        let record = reviewed_request(&store, &registry, &tenant, "read the docs", now)?;

        persist_json(
            &store.payload_path(&record.work_id),
            &StoredTaskPayload {
                work_id: record.work_id.clone(),
                task: "delete everything".to_owned(),
            },
        )
        .map_err(ctx("tamper payload"))?;

        let reply = store
            .approve(&record.work_id, now)
            .map_err(ctx("approve"))?;
        assert!(reply.contains("launch failed"));
        assert!(launcher.calls().is_empty());
        assert_eq!(
            state_of(&store, &record.work_id)?.state,
            WorkRequestState::Approved
        );
        Ok(())
    }

    #[test]
    fn approved_request_rejects_wrong_state_and_digest() -> TestResult {
        let mut record = approved_record("task");
        assert!(matches!(
            ApprovedWorkRequest::new(record.clone(), "other task"),
            Err(TelegramChannelError::Io(_))
        ));
        record.state = WorkRequestState::UnderReview;
        assert!(matches!(
            ApprovedWorkRequest::new(record, "task"),
            Err(TelegramChannelError::WorkRequestInvalidTransition { from, .. }) if from == "under_review"
        ));
        Ok(())
    }

    #[test]
    fn deny_and_cancel_remove_stored_payload() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());
        let now = Timestamp::now();

        let denied = reviewed_request(&store, &registry, &tenant, "task", now)?;
        assert!(store.payload_path(&denied.work_id).exists());
        store.deny(&denied.work_id, now).map_err(ctx("deny"))?;
        assert!(!store.payload_path(&denied.work_id).exists());

        let cancelled = reviewed_request(&store, &registry, &tenant, "task", now)?;
        store
            .cancel(&cancelled.work_id, now)
            .map_err(ctx("cancel"))?;
        assert!(!store.payload_path(&cancelled.work_id).exists());
        Ok(())
    }

    fn submit_as(
        store: &WorkRequestStore,
        registry: &WorkspaceRegistry,
        requester: &str,
        now: Timestamp,
    ) -> TestResult<WorkRequestRecord> {
        store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str(requester),
                &TenantId::from_str("ops"),
                "ops-room",
                "implementer",
                "task",
                "9",
                registry,
                now,
            )
            .map_err(ctx("submit"))
    }

    fn actor(peer: &str, is_admin: bool) -> WorkRequestActor {
        WorkRequestActor {
            channel: ChannelId::from_str("telegram:ops"),
            tenant: TenantId::from_str("ops"),
            peer: PeerId::from_str(peer),
            is_admin,
        }
    }

    #[test]
    fn authorize_admits_requester_and_admin_only() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let registry = registry(&tenant, &WorkspaceId::from_str("ops-room"), harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());
        let record = submit_as(&store, &registry, "100", Timestamp::now())?;

        assert_eq!(
            store
                .authorize(&record.work_id, &actor("100", false))
                .map_err(ctx("requester"))?
                .work_id,
            record.work_id
        );
        assert!(
            store
                .authorize(&record.work_id, &actor("999", true))
                .is_ok()
        );
        assert!(matches!(
            store.authorize(&record.work_id, &actor("200", false)),
            Err(TelegramChannelError::WorkRequestNotFound { .. })
        ));

        let mut other_tenant = actor("100", true);
        other_tenant.tenant = TenantId::from_str("other");
        assert!(matches!(
            store.authorize(&record.work_id, &other_tenant),
            Err(TelegramChannelError::WorkRequestNotFound { .. })
        ));
        let mut other_channel = actor("100", true);
        other_channel.channel = ChannelId::from_str("telegram:elsewhere");
        assert!(matches!(
            store.authorize(&record.work_id, &other_channel),
            Err(TelegramChannelError::WorkRequestNotFound { .. })
        ));
        Ok(())
    }

    #[test]
    fn foreign_actor_cannot_mutate_and_learns_nothing() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let registry = registry(&tenant, &WorkspaceId::from_str("ops-room"), harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());
        let now = Timestamp::now();
        let record = submit_as(&store, &registry, "100", now)?;
        let stranger = actor("200", false);

        let denied = store.review_as(&record.work_id, &stranger, now);
        let missing = store.review_as(&WorkId::from_str("missing"), &stranger, now);
        assert!(matches!(
            denied,
            Err(TelegramChannelError::WorkRequestNotFound { .. })
        ));
        assert!(matches!(
            missing,
            Err(TelegramChannelError::WorkRequestNotFound { .. })
        ));
        assert!(store.approve_as(&record.work_id, &stranger, now).is_err());
        assert!(store.deny_as(&record.work_id, &stranger, now).is_err());
        assert!(store.cancel_as(&record.work_id, &stranger, now).is_err());
        assert_eq!(
            state_of(&store, &record.work_id)?.state,
            WorkRequestState::Requested
        );

        let owner = actor("100", false);
        store
            .review_as(&record.work_id, &owner, now)
            .map_err(ctx("owner review"))?;
        let reply = store
            .cancel_as(&record.work_id, &actor("999", true), now)
            .map_err(ctx("admin cancel"))?;
        assert!(reply.starts_with("Cancelled"));
        Ok(())
    }

    #[test]
    fn list_for_filters_sorts_newest_first_and_limits() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let store_dir = tempfile::tempdir().map_err(ctx("store dir"))?;
        let tenant = TenantId::from_str("ops");
        let registry = registry(&tenant, &WorkspaceId::from_str("ops-room"), harness.path())?;
        let store = WorkRequestStore::new(store_dir.path());

        assert!(
            store
                .list_for(&actor("100", false), 10)
                .map_err(ctx("empty list"))?
                .is_empty()
        );

        let base = Timestamp::from_second(1_700_000_000).map_err(ctx("base"))?;
        let later = Timestamp::from_second(1_700_000_100).map_err(ctx("later"))?;
        let latest = Timestamp::from_second(1_700_000_200).map_err(ctx("latest"))?;
        let first = submit_as(&store, &registry, "100", base)?;
        let foreign = submit_as(&store, &registry, "200", later)?;
        let second = submit_as(&store, &registry, "100", latest)?;

        let own = store
            .list_for(&actor("100", false), 10)
            .map_err(ctx("own list"))?;
        let ids: Vec<_> = own.iter().map(|record| record.work_id.clone()).collect();
        assert_eq!(ids, vec![second.work_id.clone(), first.work_id.clone()]);

        let admin = store
            .list_for(&actor("999", true), 2)
            .map_err(ctx("admin list"))?;
        let ids: Vec<_> = admin.iter().map(|record| record.work_id.clone()).collect();
        assert_eq!(ids, vec![second.work_id, foreign.work_id]);

        let mut other_tenant = actor("100", true);
        other_tenant.tenant = TenantId::from_str("other");
        assert!(
            store
                .list_for(&other_tenant, 10)
                .map_err(ctx("other tenant"))?
                .is_empty()
        );
        assert!(
            store
                .list_for(&actor("100", false), 0)
                .map_err(ctx("zero limit"))?
                .is_empty()
        );
        Ok(())
    }
}
