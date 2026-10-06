//! `learning_extract` — LLM-Extraktion gelernter Fakten als Job (X6).
//!
//! Läuft nie im Turn-Pfad: die Runtime reiht den Job nur ein
//! ([`admit_learning_extract`]), ein Job-Worker mit Modell-Provider führt ihn
//! aus ([`run_learning_extract_job`]). Der Job arbeitet die Digests
//! (`harw_memory::llm_extract`) abgeschlossener Sitzungen ab, ruft je Sitzung
//! einmal das Modell und schreibt zugelassene Kandidaten nach `_incoming`
//! (Gate L3; danach konsolidiert die reguläre Wartung).
//!
//! # Frist, Abbruch, Atomarität
//! Frist (`deadline_secs`) und Abbruch (Lease-Signal oder Operator-`cancel`)
//! werden vor jeder Sitzung und während des Modellaufrufs geprüft. Eine
//! Sitzung ist atomar: Kandidaten schreiben, als erledigt markieren, Digest
//! löschen; bei Abbruch oder Fristablauf bleibt ihr Digest unverändert liegen.
//! Fristablauf endet typisiert als `timed_out`, Abbruch als `cancelled`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use harw_job_runtime::{
    Budget, Job, JobKind, JobOutcome, JobScope, RetryPolicy, StoredJob, WorkId,
};
use harw_memory::extraction::{ExtractionPolicy, IncomingStore};
use harw_memory::llm_extract::{
    digest_age_secs, ingest, pending_sessions, prepare, read_digest, remove_digest,
};
use harw_session_store::JobStore;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::memory_job::{MaintenanceFailure, store_says_cancelled};

/// [`JobKind::Custom`]-Name der Extraktionsjobs.
pub const LEARNING_EXTRACT_JOB_KIND: &str = "learning_extract";
/// Version der Payload.
pub const LEARNING_EXTRACT_SCHEMA_VERSION: u32 = 1;
/// Digests älter als dies (Sekunden) werden ohne Extraktion verworfen.
pub const STALE_DIGEST_SECS: u64 = 7 * 24 * 3600;
const POLL: Duration = Duration::from_millis(150);

/// Modellaufruf der Extraktion (vom Worker bereitgestellt).
pub trait ExtractionModel: Send + Sync {
    /// Liefert die Textantwort auf `system`/`user` oder einen Fehlertext.
    fn complete<'a>(
        &'a self,
        system: &'a str,
        user: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

/// Payload eines `learning_extract`-Jobs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LearningExtractSpec {
    /// Siehe [`LEARNING_EXTRACT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Absolute Projekt-Fakt-Wurzel (`…/.harw/memories`).
    pub memories_root: PathBuf,
    /// Frist der Ausführung in Sekunden.
    pub deadline_secs: u64,
    /// Höchstzahl Sitzungen je Lauf.
    pub max_sessions: usize,
    /// Mindest-Ruhezeit eines Digests, bevor die Sitzung als beendet gilt.
    pub idle_min_secs: u64,
}

impl LearningExtractSpec {
    /// Spezifikation mit den Standardgrenzen der Extraktion.
    #[must_use]
    pub fn new(memories_root: PathBuf, deadline_secs: u64) -> Self {
        let policy = ExtractionPolicy::default();
        Self {
            schema_version: LEARNING_EXTRACT_SCHEMA_VERSION,
            memories_root,
            deadline_secs,
            max_sessions: policy.max_sessions_per_run,
            idle_min_secs: policy.idle_min_secs,
        }
    }
}

/// `true` für `JobKind::Custom("learning_extract")`.
#[must_use]
pub fn is_learning_extract_kind(kind: &JobKind) -> bool {
    matches!(kind, JobKind::Custom(name) if name == LEARNING_EXTRACT_JOB_KIND)
}

/// Legt einen `Ready`-Job an und gibt seine Id sofort zurück (führt nichts aus).
///
/// # Errors
/// Beschreibung bei ungültiger Payload (Frist 0, relativer Pfad) oder wenn
/// der Store den Job ablehnt.
pub fn admit_learning_extract(
    jobs: &JobStore,
    scope: JobScope,
    spec: &LearningExtractSpec,
) -> Result<WorkId, String> {
    if spec.deadline_secs == 0 {
        return Err("deadline_secs muss größer als 0 sein".to_owned());
    }
    if !spec.memories_root.is_absolute() {
        return Err("memories_root muss ein absoluter Pfad sein".to_owned());
    }
    let now = Timestamp::now();
    let wall = SignedDuration::from_secs(i64::try_from(spec.deadline_secs).unwrap_or(i64::MAX));
    let mut job = Job::new(
        WorkId::new(),
        JobKind::Custom(LEARNING_EXTRACT_JOB_KIND.to_owned()),
        Budget {
            max_tokens: None,
            max_wall: Some(wall),
            max_tool_calls: None,
        },
        RetryPolicy {
            max_attempts: 1,
            base_delay: SignedDuration::from_secs(30),
            factor: 2.0,
            max_delay: SignedDuration::from_secs(300),
            jitter: 0.0,
        },
        now,
    );
    job.mark_ready(now).map_err(|error| error.to_string())?;
    let input = serde_json::to_value(spec).map_err(|error| error.to_string())?;
    let work_id = job.id.clone();
    let record = StoredJob {
        job,
        scope,
        input,
        submitted_at: now,
        not_before: now,
        lease: None,
        lease_epoch: 0,
        completion: None,
        cancellation: None,
        revision: 0,
        trace: None,
    };
    jobs.admit(&record).map_err(|error| error.to_string())?;
    Ok(work_id)
}

/// Stopp-Gründe zwischen den Schritten.
enum Stop {
    Cancelled,
    TimedOut,
}

struct Gate<'a, F> {
    started: Instant,
    limit: Duration,
    store: &'a JobStore,
    work_id: &'a WorkId,
    signal: std::pin::Pin<&'a mut F>,
    signalled: bool,
}

impl<F: Future<Output = ()>> Gate<'_, F> {
    fn poll_signal(&mut self) {
        if !self.signalled {
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            if self.signal.as_mut().poll(&mut cx).is_ready() {
                self.signalled = true;
            }
        }
    }

    fn check(&mut self) -> Result<(), Stop> {
        self.poll_signal();
        if self.signalled || store_says_cancelled(self.store, self.work_id) {
            return Err(Stop::Cancelled);
        }
        if self.started.elapsed() >= self.limit {
            return Err(Stop::TimedOut);
        }
        Ok(())
    }

    fn remaining(&self) -> Duration {
        self.limit.saturating_sub(self.started.elapsed())
    }
}

/// Führt einen `learning_extract`-Job aus (asynchron, abbrechbar, fristgebunden).
///
/// # Returns
/// `Succeeded` mit Zählern, `Cancelled`, `Failed` mit `timed_out: …` oder
/// `Failed` (ungültige Payload).
pub async fn run_learning_extract_job<F>(
    spec: LearningExtractSpec,
    work_id: WorkId,
    store: Arc<JobStore>,
    cancel_signal: F,
    model: Arc<dyn ExtractionModel>,
) -> JobOutcome
where
    F: Future<Output = ()> + Send,
{
    if spec.schema_version != LEARNING_EXTRACT_SCHEMA_VERSION {
        return JobOutcome::Failed {
            reason: format!("unbekannte Payload-Version {}", spec.schema_version),
        };
    }
    if !spec.memories_root.is_absolute() || spec.deadline_secs == 0 {
        return JobOutcome::Failed {
            reason: "ungültige learning_extract-Payload".to_owned(),
        };
    }
    if store_says_cancelled(&store, &work_id) {
        return JobOutcome::Cancelled {
            reason: "cancelled before the extraction started".to_owned(),
        };
    }
    tokio::pin!(cancel_signal);
    let mut gate = Gate {
        started: Instant::now(),
        limit: Duration::from_secs(spec.deadline_secs),
        store: &store,
        work_id: &work_id,
        signal: cancel_signal,
        signalled: false,
    };
    match extract_sessions(&spec, &mut gate, model.as_ref()).await {
        Ok(result) => JobOutcome::Succeeded { result },
        Err(Stop::Cancelled) => JobOutcome::Cancelled {
            reason: "learning extraction cancelled".to_owned(),
        },
        Err(Stop::TimedOut) => MaintenanceFailure::TimedOut(
            "the learning extraction did not finish in time".to_owned(),
        )
        .into_outcome(),
    }
}

async fn extract_sessions<F: Future<Output = ()>>(
    spec: &LearningExtractSpec,
    gate: &mut Gate<'_, F>,
    model: &dyn ExtractionModel,
) -> Result<serde_json::Value, Stop> {
    let policy = ExtractionPolicy::default();
    let incoming = match IncomingStore::open(&spec.memories_root) {
        Ok(incoming) => incoming,
        Err(error) => {
            tracing::warn!(%error, "learning_extract.incoming_open_failed");
            return Ok(serde_json::json!({"extracted": 0, "error": "incoming store"}));
        }
    };
    let (mut extracted, mut written, mut skipped, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for session in pending_sessions(&spec.memories_root) {
        gate.check()?;
        if extracted + failed >= spec.max_sessions {
            break;
        }
        let Some(age) = digest_age_secs(&spec.memories_root, &session) else {
            continue;
        };
        if age > STALE_DIGEST_SECS || incoming.is_session_done(&session) {
            remove_digest(&spec.memories_root, &session);
            skipped += 1;
            continue;
        }
        if age < spec.idle_min_secs {
            skipped += 1;
            continue;
        }
        let entries = read_digest(&spec.memories_root, &session);
        let Some(prompt) = prepare(&entries, &policy) else {
            remove_digest(&spec.memories_root, &session);
            skipped += 1;
            continue;
        };
        let call = model.complete(&prompt.system, &prompt.user);
        tokio::pin!(call);
        let remaining = gate.remaining();
        let response = loop {
            tokio::select! {
                response = &mut call => break response,
                () = tokio::time::sleep(POLL) => gate.check()?,
                () = tokio::time::sleep(remaining) => return Err(Stop::TimedOut),
            }
        };
        match response {
            Ok(text) => match ingest(
                &incoming,
                &session,
                &text,
                &policy,
                time::OffsetDateTime::now_utc(),
            ) {
                Ok(report) => {
                    extracted += 1;
                    written += report.written;
                    remove_digest(&spec.memories_root, &session);
                }
                Err(error) => {
                    failed += 1;
                    tracing::warn!(%error, "learning_extract.ingest_failed");
                }
            },
            Err(error) => {
                failed += 1;
                tracing::warn!(%error, "learning_extract.model_failed");
            }
        }
    }
    Ok(serde_json::json!({
        "extracted": extracted,
        "candidates_written": written,
        "skipped": skipped,
        "failed": failed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_memory::extraction::EntryRole;
    use harw_memory::llm_extract::DigestWriter;

    struct Canned(&'static str);
    impl ExtractionModel for Canned {
        fn complete<'a>(
            &'a self,
            _system: &'a str,
            _user: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
            let text = self.0.to_owned();
            Box::pin(async move { Ok(text) })
        }
    }

    fn spec_for(root: &std::path::Path) -> LearningExtractSpec {
        let mut spec = LearningExtractSpec::new(root.to_path_buf(), 30);
        spec.idle_min_secs = 0;
        spec
    }

    fn tmp(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("harw-learning-job-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn spec_roundtrips_and_rejects_unknown_fields() {
        let spec = LearningExtractSpec::new(PathBuf::from("/tmp/x"), 60);
        let json = serde_json::to_value(&spec).unwrap_or_default();
        assert_eq!(
            serde_json::from_value::<LearningExtractSpec>(json.clone()).ok(),
            Some(spec)
        );
        let mut bad = json;
        bad["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<LearningExtractSpec>(bad).is_err());
        assert!(is_learning_extract_kind(&JobKind::Custom(
            LEARNING_EXTRACT_JOB_KIND.to_owned()
        )));
    }

    #[test]
    fn a_session_digest_becomes_a_gated_candidate_and_is_deleted() {
        let root = tmp("flow");
        DigestWriter::new(&root).append(
            "s-1",
            EntryRole::User,
            "Nutze immer nextest statt cargo test",
        );
        let response = r#"{"facts":[{"name":"prefer-nextest","description":"Tests mit nextest","type":"preference","body":"x","confidence":0.7,"sources":["session:s-1"]}]}"#;
        let spec = spec_for(&root);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build();
        let Ok(runtime) = runtime else { return };
        let temp_jobs =
            std::env::temp_dir().join(format!("harw-learning-jobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_jobs);
        let store = JobStore::new(&temp_jobs);
        let store = Arc::new(store);
        let outcome = runtime.block_on(run_learning_extract_job(
            spec,
            WorkId::new(),
            Arc::clone(&store),
            std::future::pending::<()>(),
            Arc::new(Canned(response)),
        ));
        let JobOutcome::Succeeded { result } = outcome else {
            panic!("expected success, got {outcome:?}");
        };
        assert_eq!(result["extracted"], 1);
        assert_eq!(result["candidates_written"], 1);
        assert!(
            read_digest(&root, "s-1").is_empty(),
            "digest removed after extraction"
        );
        let incoming = IncomingStore::open(&root).ok();
        assert_eq!(
            incoming.and_then(|i| i.list().ok()).map(|l| l.len()),
            Some(1)
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&temp_jobs);
    }

    #[test]
    fn cancel_signal_stops_before_touching_a_digest() {
        let root = tmp("cancel");
        DigestWriter::new(&root).append("s-1", EntryRole::User, "Nutze immer nextest");
        let spec = spec_for(&root);
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        else {
            return;
        };
        let temp_jobs =
            std::env::temp_dir().join(format!("harw-learning-jobs-c-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_jobs);
        let store = JobStore::new(&temp_jobs);
        let outcome = runtime.block_on(run_learning_extract_job(
            spec,
            WorkId::new(),
            Arc::new(store),
            std::future::ready(()),
            Arc::new(Canned("{}")),
        ));
        assert!(
            matches!(outcome, JobOutcome::Cancelled { .. }),
            "{outcome:?}"
        );
        assert_eq!(read_digest(&root, "s-1").len(), 1, "digest untouched");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&temp_jobs);
    }
}
