//! Laufzeit-Adapter für Traumläufe (Plan D5).
//!
//! # Beschreibung
//! Der provider- und transkriptfreie Kern liegt in
//! [`harw_ops::dream_run`]; dieses Modul liefert die beiden Zutaten, die nur
//! die Laufzeit hat, und bündelt sie zu einem [`RuntimeDreamLauncher`]:
//!
//! - [`build_recent_dream_context`]: gedeckelte, deterministische Sicht auf
//!   die jüngsten dauerhaften Gespräche des Profils (fail-closed bei
//!   Symlinks, Provenienz-Brüchen oder Fehlern; Größenlimits beenden den Scan
//!   Traum-Transkripte sind ausgeschlossen, damit kein Bericht rekursiv
//!   Input wird).
//! - [`ProviderDreamReasoner`]: ein werkzeugloser Reflexions-Turn über
//!   `harw_core::run_turn` mit eigener Transkript-Provenienz
//!   (`gateway-dream:<job-id>`) und der internen Modellstelle
//!   `InternalModelPoint::DreamReflection`.
//!
//! Der Gateway-Scheduler (`harw-cli/src/gateway.rs`) und `/dream run`
//! (über `Arc<dyn DreamLauncher>` im `ServiceMap`) laufen beide über
//! [`RuntimeDreamLauncher`] und damit über denselben Kern.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_config::{InternalModelPoint, ResolvedConfig, resolve_internal_model};
use harw_core::{
    AgentSession, ModelProvider, PinnedModelProvider, TranscriptStateStore, TurnInput, TurnOutcome,
    run_turn,
};
use harw_extension_api::empty_extension_registry;
use harw_knowledge::KnowledgeStore;
use harw_ops::dream_run::{
    DreamFuture, DreamLauncher, DreamReasoner, DreamRunError, DreamRunOutcome, DreamRunRequest,
    DreamSettings, DreamTrigger, run_dream,
};
use harw_session_store::{JobStore, RecordKind, TranscriptStore};
use harw_types::{AgentRole, SessionId, ThreadRef};
use jiff::Timestamp;

/// Maximale Zahl dauerhafter Datensätze, die ein Traum-Kontext prüft; bei
/// Überschreitung wird der Scan kontrolliert abgebrochen.
pub const DREAM_CONTEXT_MAX_RECORDS: usize = 256;
/// Maximale UTF-8-Bytes des gerenderten Transkript-Kontexts.
pub const DREAM_CONTEXT_MAX_BYTES: usize = 16 * 1024;
/// Meldung jeder Sicherheitsverletzung beim Kontextaufbau (bewusst ohne
/// Details aus den Transkripten).
pub const DREAM_CONTEXT_SAFETY_VIOLATION: &str =
    "Dream-Kontext konnte wegen einer Sicherheitsverletzung nicht gebaut werden";
/// Präfix der Thread-Provenienz von Traum-Transkripten.
pub const DREAM_THREAD_PREFIX: &str = "gateway-dream:";

/// Transkript-Session eines Traum-Turns (`label` ist die pfadsichere
/// Job-Id bzw. `<job-id>-repair`).
#[must_use]
pub fn dream_session_id(label: &str) -> SessionId {
    SessionId::from_str(label)
}

/// Thread-Provenienz eines Traum-Turns, reproduzierbar über Neustarts.
#[must_use]
pub fn dream_thread_for_session(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("{DREAM_THREAD_PREFIX}{}", session_id.as_str()))
}

/// Dauerhafte Zustandsgrenze der Traum-Turns unter `transcript_root`.
#[must_use]
pub fn build_dream_state_store(transcript_root: &Path) -> TranscriptStateStore {
    TranscriptStateStore::new(
        TranscriptStore::new(transcript_root),
        dream_thread_for_session,
    )
}

/// Session-Wurzel eines Profils nach `[session] store_dir` (leer →
/// `sessions`, relativ → unter dem Profil, absolut → wie angegeben) — die
/// Auflösung von `harw-cli`s `profile_sessions_root`, ohne anzulegen.
#[must_use]
pub fn transcript_root_for_profile(profile_dir: &Path, config: &ResolvedConfig) -> PathBuf {
    let configured = config.harness.session.store_dir.trim();
    if configured.is_empty() {
        return profile_dir.join("sessions");
    }
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        profile_dir.join(path)
    }
}

/// Baut eine gedeckelte, deterministische Projektion der jüngsten
/// dauerhaften Gespräche des Profils für einen Traum-Turn.
///
/// # Beschreibung
/// Transkript-Inhalte sind untrusted historische Daten. Defekte oder
/// unlesbare reguläre Dateien werden als fremdes Rauschen ignoriert; ein
/// Symlink oder ein Provenienz-Bruch ist eine Sicherheitsverletzung und
/// lässt den ganzen Lauf fail-closed scheitern. Traum-eigene Transkripte
/// werden als ganze Session ausgeschlossen.
///
/// # Fehler
/// Gibt [`DREAM_CONTEXT_SAFETY_VIOLATION`] zurück, wenn ein Transkript ein
/// Symlink ist, das Transkriptverzeichnis oder Dateimetadaten nicht gelesen
/// werden können, ein Eintrag während der Verzeichnisauflistung fehlschlägt
/// oder die Provenienz eines Datensatzes nicht zur Session passt.
/// Überschreitungen von [`DREAM_CONTEXT_MAX_RECORDS`] oder
/// [`DREAM_CONTEXT_MAX_BYTES`] beenden den Scan dagegen kontrolliert und geben
/// den bis dahin gesammelten Kontext zurück.
pub fn build_recent_dream_context(transcript_root: &Path) -> Result<String, String> {
    let entries = match fs::read_dir(transcript_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(_) => return Err(DREAM_CONTEXT_SAFETY_VIOLATION.to_owned()),
    };

    let mut sessions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| DREAM_CONTEXT_SAFETY_VIOLATION.to_owned())?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }

        let metadata =
            fs::symlink_metadata(&path).map_err(|_| DREAM_CONTEXT_SAFETY_VIOLATION.to_owned())?;
        if metadata.file_type().is_symlink() {
            return Err(DREAM_CONTEXT_SAFETY_VIOLATION.to_owned());
        }
        if !metadata.is_file() {
            continue;
        }

        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Ok(session_id) = SessionId::try_from(stem.to_owned()) else {
            continue;
        };
        let modified_at = metadata
            .modified()
            .map_err(|_| DREAM_CONTEXT_SAFETY_VIOLATION.to_owned())?;
        sessions.push((session_id, modified_at));
    }

    // Neueste Sessions zuerst; die Id als Tie-Break macht gleiche mtimes
    // über Dateisysteme und Auflistungsreihenfolgen reproduzierbar.
    sessions.sort_by(|(left_id, left_modified), (right_id, right_modified)| {
        right_modified
            .cmp(left_modified)
            .then_with(|| left_id.as_str().cmp(right_id.as_str()))
    });

    let transcripts = TranscriptStore::new(transcript_root);
    let mut examined_records = 0_usize;
    let mut context = String::new();
    'sessions: for (session_id, _) in sessions {
        let reader = match transcripts.reader(&session_id) {
            Ok(reader) => reader,
            // Eine Datei kann während des Scans verschwinden oder defekt
            // werden; sie ist kein Modell-Input, ihr Rohfehler bleibt draußen.
            Err(_) => continue,
        };

        let mut history = harw_core::ConversationHistory::new();
        let mut discard_session = false;
        for record in reader {
            examined_records = examined_records.saturating_add(1);
            if examined_records > DREAM_CONTEXT_MAX_RECORDS {
                break 'sessions;
            }

            let record = match record {
                Ok(record) => record,
                Err(_) => {
                    // Nie ein Präfix eines defekten Transkripts nutzen: ein
                    // kaputter letzter Datensatz ließe einen halben Turn
                    // autoritativ aussehen.
                    discard_session = true;
                    break;
                }
            };
            if record.session_id != session_id {
                return Err(DREAM_CONTEXT_SAFETY_VIOLATION.to_owned());
            }
            if record.thread.as_str().starts_with(DREAM_THREAD_PREFIX) {
                discard_session = true;
                break;
            }
            if record.kind != RecordKind::Item {
                continue;
            }
            match serde_json::from_value(record.payload) {
                Ok(item) => history.push(item),
                Err(_) => {
                    discard_session = true;
                    break;
                }
            }
        }
        if discard_session {
            continue;
        }

        for message in history.to_model_messages() {
            let (role, text) = match message {
                harw_core::ModelMessage::User { text } => ("Nutzerin", text),
                harw_core::ModelMessage::Assistant { text } => ("Assistent", text),
                harw_core::ModelMessage::ToolCall { .. }
                | harw_core::ModelMessage::ToolResult { .. } => continue,
            };
            let rendered = format!("[{role} | {}]\n{text}\n\n", session_id.as_str());
            if context.len().saturating_add(rendered.len()) > DREAM_CONTEXT_MAX_BYTES {
                break 'sessions;
            }
            context.push_str(&rendered);
        }
    }

    Ok(context)
}

/// Werkzeugloser Reflexions-Turn über einen [`ModelProvider`].
///
/// # Beschreibung
/// Jeder Aufruf ist eine frische `AgentSession` (Rolle `Assistant`, leere
/// Extension-Registry, also keine Werkzeuge) mit Session-Id = `label` und
/// Transkript unter `gateway-dream:<label>`. Ist für
/// `InternalModelPoint::DreamReflection` ein eigenes Modell konfiguriert,
/// wird der Provider darauf gepinnt.
pub struct ProviderDreamReasoner {
    provider: Arc<dyn ModelProvider>,
    transcript_root: PathBuf,
    config: Arc<ResolvedConfig>,
}

impl std::fmt::Debug for ProviderDreamReasoner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderDreamReasoner")
            .field("transcript_root", &self.transcript_root)
            .finish_non_exhaustive()
    }
}

impl ProviderDreamReasoner {
    /// Baut den Reasoner.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        transcript_root: PathBuf,
        config: Arc<ResolvedConfig>,
    ) -> Self {
        Self {
            provider,
            transcript_root,
            config,
        }
    }

    /// Der wirksame Provider (Hauptmodell oder gepinnte interne Modellstelle).
    fn effective_provider(&self) -> Arc<dyn ModelProvider> {
        let resolved = resolve_internal_model(&self.config, InternalModelPoint::DreamReflection);
        if resolved.is_main_model() {
            return Arc::clone(&self.provider);
        }
        tracing::debug!(
            point = InternalModelPoint::DreamReflection.key(),
            model = resolved.model.as_deref().unwrap_or(""),
            "dream.internal_model"
        );
        Arc::new(PinnedModelProvider::new(
            Arc::clone(&self.provider),
            resolved
                .provider
                .as_deref()
                .map(harw_types::ProviderId::from),
            resolved.model.as_deref().map(harw_types::ModelId::from),
        ))
    }
}

impl DreamReasoner for ProviderDreamReasoner {
    fn reflect<'a>(
        &'a self,
        label: &'a str,
        prompt: &'a str,
    ) -> DreamFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let store = build_dream_state_store(&self.transcript_root);
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut session = AgentSession::new_with_id(
                dream_session_id(label),
                AgentRole::Assistant,
                None,
                empty_extension_registry(),
                event_tx,
            );
            let provider = self.effective_provider();
            match run_turn(
                &mut session,
                provider.as_ref(),
                &store,
                TurnInput::user(prompt),
            )
            .await
            {
                Ok(TurnOutcome::Completed) => Ok(last_assistant_text(&session)),
                Ok(other) => Err(format!("Traum-Turn nicht abgeschlossen ({other:?})")),
                Err(error) => Err(error.to_string()),
            }
        })
    }
}

/// Letzter nicht leerer Assistententext einer Session.
fn last_assistant_text(session: &AgentSession) -> String {
    session
        .history()
        .to_model_messages()
        .into_iter()
        .rev()
        .find_map(|message| match message {
            harw_core::ModelMessage::Assistant { text } if !text.trim().is_empty() => Some(text),
            _ => None,
        })
        .unwrap_or_default()
}

/// Der gemeinsame Starter für Gateway-Scheduler und `/dream run`.
///
/// # Beschreibung
/// Baut je Lauf den Transkript-Kontext ([`build_recent_dream_context`]),
/// liest die Einstellungen aus der Konfiguration und ruft
/// [`harw_ops::dream_run::run_dream`] mit dem [`ProviderDreamReasoner`].
/// Mit `jobs` erscheint jeder Lauf als `JobKind::Dream` im Ledger.
pub struct RuntimeDreamLauncher {
    reasoner: Arc<dyn DreamReasoner>,
    knowledge: Arc<KnowledgeStore>,
    jobs: Option<Arc<JobStore>>,
    transcript_root: PathBuf,
    settings: DreamSettings,
}

impl std::fmt::Debug for RuntimeDreamLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeDreamLauncher")
            .field("knowledge", &self.knowledge.root())
            .field("ledger", &self.jobs.is_some())
            .field("transcript_root", &self.transcript_root)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl RuntimeDreamLauncher {
    /// Baut den Starter über einem Modell.
    ///
    /// # Argumente
    /// - `provider`: Modell der Montage (Traum-Turns sind werkzeuglos).
    /// - `knowledge`: Wissensspeicher des Profils.
    /// - `jobs`: Job-Ledger des Profils (`JobStore::new(<profil>)`).
    /// - `transcript_root`: Session-Wurzel des Profils
    ///   ([`transcript_root_for_profile`]).
    /// - `config`: aufgelöste Konfiguration (`[dream]`, Diary-Aufbewahrung,
    ///   interne Modellstelle).
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        knowledge: Arc<KnowledgeStore>,
        jobs: Option<Arc<JobStore>>,
        transcript_root: PathBuf,
        config: Arc<ResolvedConfig>,
    ) -> Self {
        let settings = DreamSettings::from_config(&config);
        let reasoner = Arc::new(ProviderDreamReasoner::new(
            provider,
            transcript_root.clone(),
            config,
        ));
        Self::with_reasoner(reasoner, knowledge, jobs, transcript_root, settings)
    }

    /// Baut den Starter über einem beliebigen [`DreamReasoner`] (Tests,
    /// fremde Kompositionen).
    #[must_use]
    pub fn with_reasoner(
        reasoner: Arc<dyn DreamReasoner>,
        knowledge: Arc<KnowledgeStore>,
        jobs: Option<Arc<JobStore>>,
        transcript_root: PathBuf,
        settings: DreamSettings,
    ) -> Self {
        Self {
            reasoner,
            knowledge,
            jobs,
            transcript_root,
            settings,
        }
    }

    /// Die wirksamen Einstellungen.
    #[must_use]
    pub fn settings(&self) -> &DreamSettings {
        &self.settings
    }

    /// Der Wissensspeicher.
    #[must_use]
    pub fn knowledge(&self) -> &Arc<KnowledgeStore> {
        &self.knowledge
    }
}

impl DreamLauncher for RuntimeDreamLauncher {
    fn launch(
        &self,
        trigger: DreamTrigger,
    ) -> DreamFuture<'_, Result<DreamRunOutcome, DreamRunError>> {
        Box::pin(async move {
            // Vor jedem Turn bauen: eine Sicherheitsverletzung endet sofort,
            // es entsteht kein Bericht aus einer partiellen Sicht.
            let transcript_context =
                build_recent_dream_context(&self.transcript_root).map_err(DreamRunError::Failed)?;
            run_dream(
                self.reasoner.as_ref(),
                DreamRunRequest {
                    knowledge: &self.knowledge,
                    jobs: self.jobs.as_deref(),
                    transcript_context: &transcript_context,
                    settings: &self.settings,
                    trigger,
                    now: Timestamp::now(),
                },
            )
            .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_knowledge::dream::read_scheduler_state;

    fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
        move |error| TestError::Unexpected(format!("{context}: {error}"))
    }

    fn append_user_transcript_item(
        root: &Path,
        session_id: &SessionId,
        thread: ThreadRef,
        sequence: u64,
        text: &str,
    ) -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text(text);
        let item = history
            .items()
            .first()
            .ok_or_else(|| TestError::Unexpected("history has a turn item".into()))?;
        let record = harw_session_store::TranscriptRecord::new(
            session_id.clone(),
            thread,
            sequence,
            Timestamp::now(),
            RecordKind::Item,
            serde_json::to_value(item).map_err(ctx("serialize transcript item"))?,
        );
        TranscriptStore::new(root)
            .append(&record)
            .map_err(ctx("append transcript record"))?;
        Ok(())
    }

    #[test]
    fn dream_context_uses_durable_conversation_and_excludes_dream_transcripts() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let interactive = SessionId::from_str("interactive-session");
        append_user_transcript_item(
            tmp.path(),
            &interactive,
            ThreadRef::from_str("cli:interactive-session"),
            0,
            "offener Faden: sichere Transkripte",
        )?;
        let dream = dream_session_id("dream-20260718T081500");
        append_user_transcript_item(
            tmp.path(),
            &dream,
            dream_thread_for_session(&dream),
            0,
            "DIESER TRAUM DARF NICHT ZURUECK IN DEN PROMPT",
        )?;

        let context = build_recent_dream_context(tmp.path()).map_err(ctx("build context"))?;
        assert!(context.contains("offener Faden: sichere Transkripte"));
        assert!(!context.contains("DIESER TRAUM DARF NICHT ZURUECK IN DEN PROMPT"));
        Ok(())
    }

    #[test]
    fn dream_context_ignores_corrupt_unrelated_transcript_without_error_text() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let session = SessionId::from_str("healthy-session");
        append_user_transcript_item(
            tmp.path(),
            &session,
            ThreadRef::from_str("cli:healthy-session"),
            0,
            "nur der valide Verlauf",
        )?;
        fs::write(tmp.path().join("unrelated.jsonl"), b"not json\n")
            .map_err(ctx("write unrelated file"))?;

        let context = build_recent_dream_context(tmp.path()).map_err(ctx("build context"))?;
        assert!(context.contains("nur der valide Verlauf"));
        assert!(!context.contains("not json"));
        assert!(!context.contains("CorruptRecord"));
        Ok(())
    }

    #[test]
    fn dream_context_fails_closed_on_limits_and_provenance() -> TestResult {
        let records = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let session = SessionId::from_str("long-session");
        for sequence in 0..=DREAM_CONTEXT_MAX_RECORDS as u64 {
            append_user_transcript_item(
                records.path(),
                &session,
                ThreadRef::from_str("cli:long-session"),
                sequence,
                "bounded",
            )?;
        }
        let oversized = build_recent_dream_context(records.path()).map_err(ctx("dream context"))?;
        assert!(!oversized.contains("bounded"));

        let bytes = tempfile::tempdir().map_err(ctx("temp dir"))?;
        append_user_transcript_item(
            bytes.path(),
            &SessionId::from_str("large-session"),
            ThreadRef::from_str("cli:large-session"),
            0,
            &"x".repeat(DREAM_CONTEXT_MAX_BYTES),
        )?;
        assert!(
            build_recent_dream_context(bytes.path())
                .map_err(ctx("dream context"))?
                .is_empty()
        );

        let provenance = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let expected = SessionId::from_str("expected-session");
        let mismatched = harw_session_store::TranscriptRecord::new(
            SessionId::from_str("other-session"),
            ThreadRef::from_str("cli:other-session"),
            0,
            Timestamp::now(),
            RecordKind::Lifecycle,
            serde_json::json!({ "event": "opened" }),
        );
        fs::write(
            TranscriptStore::new(provenance.path())
                .transcript_path(&expected)
                .map_err(ctx("transcript path"))?,
            mismatched
                .to_jsonl_line()
                .map_err(ctx("render mismatched record"))?,
        )
        .map_err(ctx("write mismatched transcript"))?;
        assert_eq!(
            build_recent_dream_context(provenance.path()),
            Err(DREAM_CONTEXT_SAFETY_VIOLATION.to_owned())
        );
        Ok(())
    }

    #[tokio::test]
    async fn dream_state_store_persists_under_its_thread() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let session = dream_session_id("dream-20260718T081500");
        let store = build_dream_state_store(tmp.path());
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("persist this dream turn");
        let item = history
            .items()
            .first()
            .ok_or_else(|| TestError::Unexpected("history has a turn item".into()))?;
        harw_core::StateStore::save_turn(&store, &session, item)
            .await
            .map_err(ctx("persist dream turn"))?;

        let records = TranscriptStore::new(tmp.path())
            .reader(&session)
            .map_err(ctx("open transcript"))?
            .collect::<harw_session_store::SessionStoreResult<Vec<_>>>()
            .map_err(ctx("read transcript"))?;
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].thread,
            ThreadRef::from_str("gateway-dream:dream-20260718T081500")
        );
        Ok(())
    }

    #[test]
    fn transcript_root_follows_the_session_store_dir() {
        let profile = Path::new("/p");
        let mut config = ResolvedConfig::default();
        config.harness.session.store_dir = "   ".to_owned();
        assert_eq!(
            transcript_root_for_profile(profile, &config),
            PathBuf::from("/p/sessions")
        );
        config.harness.session.store_dir = "verlauf".to_owned();
        assert_eq!(
            transcript_root_for_profile(profile, &config),
            PathBuf::from("/p/verlauf")
        );
        config.harness.session.store_dir = "/abs".to_owned();
        assert_eq!(
            transcript_root_for_profile(profile, &config),
            PathBuf::from("/abs")
        );
    }

    #[tokio::test]
    async fn the_launcher_runs_the_shared_core_with_a_provider_turn() -> TestResult {
        let knowledge_dir = tempfile::tempdir().map_err(ctx("knowledge dir"))?;
        let sessions = tempfile::tempdir().map_err(ctx("sessions dir"))?;
        let profile = tempfile::tempdir().map_err(ctx("profile dir"))?;
        append_user_transcript_item(
            sessions.path(),
            &SessionId::from_str("s1"),
            ThreadRef::from_str("cli:s1"),
            0,
            "Bitte an den Deploy denken",
        )?;
        let knowledge = Arc::new(KnowledgeStore::new(knowledge_dir.path()));
        let jobs = Arc::new(JobStore::new(profile.path()));
        let provider: Arc<dyn ModelProvider> = Arc::new(harw_core::EchoModelProvider::new(
            r#"{"summary":"Deploy steht an.","suggestions":[{"kind":"follow_up","text":"Deploy prüfen"}]}"#,
        ));
        let launcher = RuntimeDreamLauncher::new(
            provider,
            Arc::clone(&knowledge),
            Some(Arc::clone(&jobs)),
            sessions.path().to_path_buf(),
            Arc::new(ResolvedConfig::default()),
        );
        let outcome = launcher
            .launch(DreamTrigger::Manual)
            .await
            .map_err(ctx("launch"))?;
        assert!(outcome.structured, "{outcome:?}");
        assert!(outcome.in_ledger);
        assert_eq!(outcome.data.suggestions.len(), 1);
        let job = jobs
            .get(&harw_job_runtime::WorkId::from_str(&outcome.work_id))
            .map_err(ctx("ledger job"))?;
        assert_eq!(job.job.kind, harw_job_runtime::JobKind::Dream);
        let state = read_scheduler_state(&knowledge).map_err(ctx("state"))?;
        assert_eq!(
            state.last_work_id.as_deref(),
            Some(outcome.work_id.as_str())
        );
        // Der Traum-Turn hat sein eigenes Transkript und taucht im nächsten
        // Kontext nicht auf.
        let next = build_recent_dream_context(sessions.path()).map_err(ctx("next context"))?;
        assert!(next.contains("Bitte an den Deploy denken"));
        assert!(!next.contains("Deploy steht an."));
        Ok(())
    }
}
