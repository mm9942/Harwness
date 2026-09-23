//! Telegram-Auftrags-Launcher: lässt einen genehmigten Telegram-Auftrag als
//! durablen Job im Job-Store des aktiven Profils zu.
//!
//! # Verantwortung
//! `harw-channel-telegram` hängt bewusst weder von `harw-session-store`s
//! [`JobStore`] noch von `harw-job-runtime` ab; es definiert nur die Naht
//! [`WorkLauncher`]. Dieses Modul ist die Implementierung dieser Naht für die
//! Komposition (`gateway.rs` installiert sie über
//! `WorkRequestStore::with_launcher`).
//!
//! # Job-Format
//! - Art: [`JobKind::Custom`] mit [`TELEGRAM_WORK_REQUEST_JOB_KIND`] — bewusst
//!   nicht `JobKind::Worker` (siehe Doku der Konstante).
//! - Job-ID: exakt die `WorkId` des Auftrags (so wie es die Doku von
//!   [`LaunchReceipt::job_id`] für die Referenz-Implementierung beschreibt).
//!   Ein wiederholter Launch trifft damit auf `JobAlreadyExists` und liefert
//!   die Quittung des bestehenden Jobs.
//! - Scope: Tenant/Workspace aus dem Datensatz, Einreicher
//!   `ApprovalActor::ChannelPeer { channel, peer: requester }`.
//! - `StoredJob::input`: [`ApprovedWorkRequest::job_input`] (Tenant/Workspace
//!   deckungsgleich mit dem Scope, wie `job_worker.rs`s
//!   `check_input_declared_scope` es verlangt), ergänzt um die Top-Level-Felder
//!   von [`TelegramWorkPayload`] (`work_id`, `task`, `requested_by`). Ein
//!   Worker liest den Payload daher mit
//!   `decode_payload(&serde_json::to_vec(&stored.input)?)` — unbekannte
//!   Felder ignoriert `serde` dabei.
//!
//! # Nebenläufigkeit
//! [`JobStore`] ist eine synchrone, dateibasierte API mit Sperre pro Job;
//! [`WorkLauncher::launch`] ist synchron. Es wird daher weder `block_on` noch
//! `block_in_place` benötigt — der Aufruf ist auch innerhalb einer laufenden
//! Tokio-Runtime unkritisch (er blockiert lediglich kurz für Datei-I/O).

use std::path::Path;
use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use harw_channel_telegram::{
    ApprovedWorkRequest, LaunchReceipt, TELEGRAM_WORK_REQUEST_JOB_KIND, WorkLaunchError,
    WorkLauncher,
};
use harw_job_runtime::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob, WorkId};
use harw_session_store::{JobStore, SessionStoreError};
use harw_types::ApprovalActor;

/// Der für den Worker bestimmte Kern eines genehmigten Telegram-Auftrags.
///
/// Liegt als Top-Level-Felder in `StoredJob::input` eines Jobs der Art
/// [`TELEGRAM_WORK_REQUEST_JOB_KIND`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TelegramWorkPayload {
    /// `WorkRequestRecord::work_id` des Auftrags.
    pub work_id: String,
    /// Der digest-geprüfte, genehmigte Aufgabentext. Bleibt unvertrauenswürdige
    /// Nutzereingabe — Daten für den Worker, nie eine Autorität.
    pub task: String,
    /// Identität des Anfragenden (`WorkRequestRecord::requester`).
    pub requested_by: String,
}

/// Serialisiert einen [`TelegramWorkPayload`] als JSON-Bytes.
///
/// # Errors
/// [`serde_json::Error`], falls die Serialisierung scheitert.
pub(crate) fn encode_payload(p: &TelegramWorkPayload) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(p)
}

/// Liest einen [`TelegramWorkPayload`] aus JSON-Bytes; zusätzliche Felder
/// (etwa der restliche `StoredJob::input`) werden ignoriert.
///
/// # Errors
/// [`serde_json::Error`] bei ungültigem JSON oder fehlenden Pflichtfeldern.
pub(crate) fn decode_payload(bytes: &[u8]) -> Result<TelegramWorkPayload, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// [`WorkLauncher`], der genehmigte Telegram-Aufträge als durable Jobs in
/// einen [`JobStore`] zulässt.
///
/// Idempotent pro `work_id`: ein bereits vorhandener Job derselben Art und
/// desselben Auftrags gilt als erfolgreicher Launch.
pub(crate) struct JobStoreWorkLauncher {
    store: Arc<JobStore>,
    budget: Budget,
    retry: RetryPolicy,
}

impl JobStoreWorkLauncher {
    /// Erzeugt einen Launcher über einem bestehenden [`JobStore`].
    ///
    /// Vorgaben: unbegrenztes [`Budget`] und genau ein Ausführungsversuch
    /// (kein automatischer Retry eines Telegram-Auftrags).
    #[must_use]
    pub(crate) fn new(store: Arc<JobStore>) -> Self {
        Self {
            store,
            budget: Budget::unbounded(),
            retry: RetryPolicy {
                max_attempts: 1,
                base_delay: SignedDuration::from_secs(1),
                factor: 1.0,
                max_delay: SignedDuration::from_secs(1),
            },
        }
    }

    /// Erzeugt einen Launcher für das Profilverzeichnis `profile_dir`
    /// (`<home>/profiles/<aktives-profil>`); [`JobStore`] hängt selbst die
    /// Komponente `jobs` an — dieselbe Wurzel, die `chat.rs`
    /// (`active_profile_job_store_root`) verwendet.
    #[must_use]
    pub(crate) fn for_profile_dir(profile_dir: &Path) -> Self {
        Self::new(Arc::new(JobStore::new(profile_dir)))
    }

    /// Ersetzt Budget und Retry-Policy neu zugelassener Jobs.
    #[must_use]
    pub(crate) fn with_policy(mut self, budget: Budget, retry: RetryPolicy) -> Self {
        self.budget = budget;
        self.retry = retry;
        self
    }

    /// Baut den durablen Datensatz für `request`.
    fn stored_job(
        &self,
        request: &ApprovedWorkRequest,
        now: Timestamp,
    ) -> Result<StoredJob, WorkLaunchError> {
        let record = request.record();
        let mut job = Job::new(
            job_id_for(request),
            JobKind::Custom(TELEGRAM_WORK_REQUEST_JOB_KIND.to_owned()),
            self.budget.clone(),
            self.retry.clone(),
            now,
        );
        job.mark_ready(now).map_err(|error| {
            tracing::error!(error = %error, "telegram work job could not be marked ready");
            WorkLaunchError::new("the job could not be prepared")
        })?;
        Ok(StoredJob {
            job,
            scope: JobScope::new(
                record.tenant.clone(),
                record.workspace.clone(),
                ApprovalActor::ChannelPeer {
                    channel: record.channel.clone(),
                    peer: record.requester.clone(),
                },
            ),
            input: job_input(request)?,
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            // Die Launch-Naht trägt keinen Trace-Kontext.
            trace: None,
        })
    }

    /// Behandelt einen bereits vorhandenen Job unter der Auftrags-ID: gehört
    /// er zu genau diesem Auftrag, ist der Launch erfolgreich (Idempotenz).
    fn existing_receipt(
        &self,
        request: &ApprovedWorkRequest,
    ) -> Result<LaunchReceipt, WorkLaunchError> {
        let job_id = job_id_for(request);
        let existing = self.store.get(&job_id).map_err(|error| {
            tracing::error!(error = %error, "existing telegram work job could not be read");
            WorkLaunchError::new("the existing job could not be read")
        })?;
        let belongs = existing.job.kind
            == JobKind::Custom(TELEGRAM_WORK_REQUEST_JOB_KIND.to_owned())
            && existing
                .input
                .get("work_id")
                .and_then(serde_json::Value::as_str)
                == Some(request.record().work_id.as_str());
        if !belongs {
            tracing::error!(
                work_id = job_id.as_str(),
                "job id of a telegram work request is occupied by a foreign job"
            );
            return Err(WorkLaunchError::new(
                "the job id is already used by an unrelated job",
            ));
        }
        Ok(LaunchReceipt {
            job_id,
            launched_at: existing.submitted_at,
        })
    }
}

impl WorkLauncher for JobStoreWorkLauncher {
    fn launch(&self, request: &ApprovedWorkRequest) -> Result<LaunchReceipt, WorkLaunchError> {
        let now = Timestamp::now();
        let record = self.stored_job(request, now)?;
        match self.store.admit(&record) {
            Ok(()) => Ok(LaunchReceipt {
                job_id: record.job.id,
                launched_at: now,
            }),
            Err(SessionStoreError::JobAlreadyExists { .. }) => self.existing_receipt(request),
            Err(error) => {
                // Die Store-Meldung kann Pfade enthalten; dem Telegram-Nutzer
                // wird nur eine generische Begründung gezeigt.
                tracing::error!(error = %error, "telegram work job could not be admitted");
                Err(WorkLaunchError::new("the job store rejected the job"))
            }
        }
    }
}

/// Deterministische Job-ID eines Auftrags: seine eigene `WorkId`.
fn job_id_for(request: &ApprovedWorkRequest) -> WorkId {
    request.record().work_id.clone()
}

/// [`ApprovedWorkRequest::job_input`] plus die Felder von
/// [`TelegramWorkPayload`] auf oberster Ebene.
fn job_input(request: &ApprovedWorkRequest) -> Result<serde_json::Value, WorkLaunchError> {
    let record = request.record();
    let payload = TelegramWorkPayload {
        work_id: record.work_id.as_str().to_owned(),
        task: request.task().to_owned(),
        requested_by: record.requester.as_str().to_owned(),
    };
    let malformed = || WorkLaunchError::new("the job input could not be built");
    let payload = serde_json::to_value(&payload).map_err(|error| {
        tracing::error!(error = %error, "telegram work payload could not be serialized");
        malformed()
    })?;
    let serde_json::Value::Object(fields) = payload else {
        return Err(malformed());
    };
    let mut input = request.job_input();
    let Some(object) = input.as_object_mut() else {
        return Err(malformed());
    };
    object.extend(fields);
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    use harw_channel_telegram::{WorkRequestRecord, WorkRequestState};
    use harw_job_runtime::JobState;
    use harw_types::{ChannelId, PeerId, TenantId, WorkspaceId};

    use crate::test_support::{TestError, TestResult, ctx};

    // Normalisierter SHA-256-Hex-Digest wie `work_request::task_digest`.
    fn digest(task: &str) -> String {
        harw_secrets::audit::chain::sha256(task.trim().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn approved(work_id: &str, task: &str) -> TestResult<ApprovedWorkRequest> {
        let now = Timestamp::now();
        let record = WorkRequestRecord {
            work_id: WorkId::from_str(work_id),
            channel: ChannelId::from_str("telegram"),
            requester: PeerId::from_str("peer-42"),
            tenant: TenantId::from_str("tenant"),
            workspace: WorkspaceId::from_str("workspace"),
            role: "coder".to_owned(),
            task_digest: digest(task),
            source_update_id: "1001".to_owned(),
            state: WorkRequestState::Approved,
            created_at: now,
            updated_at: now,
            launch: None,
        };
        ApprovedWorkRequest::new(record, task).map_err(ctx("approved request"))
    }

    #[test]
    fn test_payload_round_trip() -> TestResult {
        let payload = TelegramWorkPayload {
            work_id: "work-1".to_owned(),
            task: "fix the build".to_owned(),
            requested_by: "peer-42".to_owned(),
        };
        let bytes = encode_payload(&payload).map_err(ctx("encode"))?;
        let decoded = decode_payload(&bytes).map_err(ctx("decode"))?;
        if decoded != payload {
            return Err(TestError::Unexpected(format!("{decoded:?} != {payload:?}")));
        }
        Ok(())
    }

    #[test]
    fn test_decode_payload_rejects_missing_fields() -> TestResult {
        match decode_payload(br#"{"work_id":"w"}"#) {
            Err(_) => Ok(()),
            Ok(payload) => Err(TestError::Unexpected(format!("decoded {payload:?}"))),
        }
    }

    #[test]
    fn test_launch_admits_ready_custom_job_with_decodable_payload() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let store = Arc::new(JobStore::new(dir.path()));
        let launcher = JobStoreWorkLauncher::new(Arc::clone(&store));
        let request = approved("work-abc", "  fix the build  ")?;

        let receipt = launcher.launch(&request).map_err(ctx("launch"))?;
        if receipt.job_id != WorkId::from_str("work-abc") {
            return Err(TestError::Unexpected(format!(
                "job id {:?}",
                receipt.job_id
            )));
        }
        let stored = store.get(&receipt.job_id).map_err(ctx("get job"))?;
        if stored.job.kind != JobKind::Custom(TELEGRAM_WORK_REQUEST_JOB_KIND.to_owned()) {
            return Err(TestError::Unexpected(format!("kind {:?}", stored.job.kind)));
        }
        if stored.job.state != JobState::Ready {
            return Err(TestError::Unexpected(format!(
                "state {:?}",
                stored.job.state
            )));
        }
        let expected_submitter = ApprovalActor::ChannelPeer {
            channel: ChannelId::from_str("telegram"),
            peer: PeerId::from_str("peer-42"),
        };
        if stored.scope.submitter() != &expected_submitter
            || stored.scope.tenant().as_str() != "tenant"
            || stored.scope.workspace().as_str() != "workspace"
        {
            return Err(TestError::Unexpected(format!("scope {:?}", stored.scope)));
        }
        let bytes = serde_json::to_vec(&stored.input).map_err(ctx("input bytes"))?;
        let payload = decode_payload(&bytes).map_err(ctx("decode input"))?;
        let expected = TelegramWorkPayload {
            work_id: "work-abc".to_owned(),
            task: "fix the build".to_owned(),
            requested_by: "peer-42".to_owned(),
        };
        if payload != expected {
            return Err(TestError::Unexpected(format!("payload {payload:?}")));
        }
        Ok(())
    }

    #[test]
    fn test_repeated_launch_is_idempotent() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let launcher = JobStoreWorkLauncher::for_profile_dir(dir.path());
        let request = approved("work-idem", "run the tests")?;

        let first = launcher.launch(&request).map_err(ctx("first launch"))?;
        let second = launcher.launch(&request).map_err(ctx("second launch"))?;
        if first != second {
            return Err(TestError::Unexpected(format!("{first:?} != {second:?}")));
        }
        Ok(())
    }

    #[test]
    fn test_launch_rejects_foreign_job_under_same_id() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let store = Arc::new(JobStore::new(dir.path()));
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str("work-taken"),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 1,
                base_delay: SignedDuration::from_secs(1),
                factor: 1.0,
                max_delay: SignedDuration::from_secs(1),
            },
            now,
        );
        job.mark_ready(now).map_err(ctx("mark ready"))?;
        let foreign = StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("tenant"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
            ),
            input: serde_json::json!({}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        };
        store.admit(&foreign).map_err(ctx("admit foreign"))?;

        let launcher = JobStoreWorkLauncher::new(store);
        let request = approved("work-taken", "do it")?;
        match launcher.launch(&request) {
            Err(_) => Ok(()),
            Ok(receipt) => Err(TestError::Unexpected(format!("launched {receipt:?}"))),
        }
    }
}
