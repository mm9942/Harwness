//! Verdrahtung des Projektgedächtnisses (Addendum B) in die Runtime-Montage.
//!
//! # Verantwortungsbereich
//! Dieses Modul kennt zwei Nähte zwischen `harw-core`/`harw-memory` und der
//! Montage in [`crate::assembly`]:
//!
//! - [`MemoryCaptureObserver`] implementiert
//!   [`harw_core::capture::ToolOutcomeObserver`] und leitet jedes
//!   Werkzeugergebnis der Wurzelsitzung an
//!   [`harw_memory::capture::ProjectMemoryCapture::record_tool_outcome`]
//!   weiter — die Erfassungsseite von „Erfassen → Konsolidieren → Abrufen“.
//! - [`MemoryConsolidationHook`] implementiert
//!   [`crate::assembly::SessionLifecycleHook`] und löst beim Ende einer
//!   Sitzung **synchron** den Flush und die Konsolidierung des
//!   Projektgedächtnisses aus (Addendum B, Präzisierung
//!   Konsolidierungszeitpunkt: kein `std::thread::spawn`, weil der Prozess
//!   sonst vor dem Thread enden kann, etwa bei einem TUI-Quit).
//!
//! Zusätzlich baut [`spawn_startup_sweep_job`] beim Aufbau der Wurzelsitzung
//! einen `memory_maintenance`-Job (Art des Job-Systems, Frist aus
//! `[memory]`), den ein Hintergrund-Thread ausführt, der liegengebliebene
//! `_incoming`-Kandidaten nach einem Absturz nachholt — dieser Fall läuft bewusst **nicht**
//! synchron, weil er den Start eines Laufs nicht verzögern darf.
//!
//! # Nebenläufigkeit
//! Beide Beobachter-Typen sind `Send + Sync` (sie halten nur ein geteiltes
//! `Arc<ProjectMemoryCapture>`, keine eigene innere Veränderlichkeit).
//! [`MemoryConsolidationHook::on_session_closed`] blockiert den aufrufenden
//! Thread bewusst (reine Dateioperationen); [`spawn_startup_sweep`] öffnet
//! stattdessen einen eigenen `std::thread`, benannt `"harw-memory-sweep"`.
//!
//! # Fehler
//! Kein eigener Fehlertyp: jeder Fehlschlag (Erfassung, Konsolidierung) wird
//! ausschließlich über `tracing::warn!` gemeldet — ein Problem im
//! Projektgedächtnis darf weder einen Tool-Aufruf noch das Ende einer
//! Sitzung scheitern lassen.

use std::path::Path;
use std::sync::Arc;

use harw_core::capture::{ToolOutcome, ToolOutcomeObserver, ToolOutcomeStatus};
use harw_core::{DurableJobRunner, ExecutionControl, JobExecutionRegistry};
use harw_job_runtime::{JobScope, WorkId};
use harw_memory::capture::{ProjectMemoryCapture, consolidate_project_memories};
use harw_memory::{FactScope, FactStore};
use harw_ops::memory_job::{
    MemoryMaintenanceOp, MemoryMaintenanceSpec, admit_memory_maintenance,
    execute_memory_maintenance, run_memory_maintenance_job,
};
use harw_session_store::{ClaimRequest, JobStore};
use harw_types::{ApprovalActor, SessionId, TenantId, WorkspaceId};
use jiff::{SignedDuration, Timestamp};

use crate::assembly::SessionLifecycleHook;

/// Leitet Werkzeugergebnisse der Wurzelsitzung in das Projektgedächtnis
/// weiter (Addendum B, „Erfassen“).
///
/// # Beschreibung
/// Hält nur eine geteilte [`ProjectMemoryCapture`] — die eigentliche
/// Erfassungslogik (Redaktion, Geheimnis-Pfade, episodischer Puffer) lebt
/// dort, dieser Typ ist ausschließlich die Naht zu
/// [`harw_core::capture::ToolOutcomeObserver`].
pub struct MemoryCaptureObserver {
    /// Die geteilte Erfassungsfläche des Projekts.
    capture: Arc<ProjectMemoryCapture>,
    /// Rückmeldungs-Tracker für gelieferte Fakten (optional).
    feedback: Option<Arc<harw_memory::feedback::FeedbackTracker>>,
    /// Digest-Schreiber der LLM-Extraktion (nur mit `[memory] llm_extraction`).
    digest: Option<harw_memory::llm_extract::DigestWriter>,
}

impl MemoryCaptureObserver {
    /// Baut einen Beobachter für die gegebene Erfassungsfläche.
    ///
    /// # Argumente
    /// - `capture` (`Arc<ProjectMemoryCapture>`): die geteilte
    ///   Erfassungsfläche des Projekts, wie sie
    ///   [`crate::assembly::RuntimeAssemblyBuilder::build`] beim Öffnen des
    ///   Projekt-Homes best-effort anlegt.
    ///
    /// # Rückgabe
    /// Einen einsatzbereiten [`MemoryCaptureObserver`].
    #[must_use]
    pub fn new(capture: Arc<ProjectMemoryCapture>) -> Self {
        Self {
            capture,
            feedback: None,
            digest: None,
        }
    }

    /// Hängt den Digest-Schreiber der LLM-Extraktion an.
    #[must_use]
    pub fn with_digest(mut self, digest: Option<harw_memory::llm_extract::DigestWriter>) -> Self {
        self.digest = digest;
        self
    }

    /// Hängt den Rückmeldungs-Tracker an.
    #[must_use]
    pub fn with_feedback(
        mut self,
        feedback: Option<Arc<harw_memory::feedback::FeedbackTracker>>,
    ) -> Self {
        self.feedback = feedback;
        self
    }
}

impl ToolOutcomeObserver for MemoryCaptureObserver {
    /// Meldet das Ergebnis eines ausgeführten Werkzeugaufrufs an das
    /// Projektgedächtnis.
    ///
    /// # Beschreibung
    /// Übersetzt [`ToolOutcomeStatus::Error`] in `is_error = true` (jeder
    /// andere Status, derzeit nur [`ToolOutcomeStatus::Success`], in
    /// `false`) und ruft
    /// [`ProjectMemoryCapture::record_tool_outcome`] synchron auf. Ein
    /// Fehlschlag der Erfassung selbst wird von `record_tool_outcome`
    /// bereits nur geloggt (siehe dessen eigene Dokumentation) — dieser
    /// Aufrufer propagiert ohnehin nichts, die Methode gibt nichts zurück.
    ///
    /// # Nebenläufigkeit
    /// Wird synchron aus dem Turn-Loop-Pfad heraus aufgerufen; muss billig
    /// bleiben und darf nicht blockieren.
    fn on_tool_outcome(&self, session_id: &SessionId, outcome: &ToolOutcome<'_>) {
        let is_error = matches!(outcome.status, ToolOutcomeStatus::Error);
        if let Some(feedback) = &self.feedback {
            feedback.on_tool_outcome(session_id.as_str(), is_error);
        }
        self.capture.record_tool_outcome(
            session_id.as_str(),
            outcome.tool_name,
            outcome.arguments,
            is_error,
            outcome.output_text,
        );
    }

    /// Meldet eine Nutzernachricht an den Rückmeldungs-Tracker (Korrekturen).
    fn on_user_message(&self, session_id: &SessionId, text: &str) {
        if let Some(feedback) = &self.feedback {
            feedback.on_user_message(session_id.as_str(), text);
        }
        if let Some(digest) = &self.digest {
            digest.append(
                session_id.as_str(),
                harw_memory::extraction::EntryRole::User,
                text,
            );
        }
    }

    /// Meldet eine Assistentenantwort an den Rückmeldungs-Tracker (Nutzung).
    fn on_assistant_message(&self, session_id: &SessionId, text: &str) {
        if let Some(feedback) = &self.feedback {
            feedback.on_assistant_message(session_id.as_str(), text);
        }
        if let Some(digest) = &self.digest {
            digest.append(
                session_id.as_str(),
                harw_memory::extraction::EntryRole::Assistant,
                text,
            );
        }
    }

    // `on_turn_finished` bleibt beim No-op-Standard aus
    // `harw_core::capture::ToolOutcomeObserver` — das Projektgedächtnis
    // braucht kein eigenes Signal am Turn-Ende, nur am Sitzungsende
    // ([`MemoryConsolidationHook`]).
}

/// Löst beim Ende einer Sitzung Flush und Konsolidierung des
/// Projektgedächtnisses aus (Addendum B, „Konsolidieren“).
///
/// # Beschreibung
/// Siehe die „Präzisierung Konsolidierungszeitpunkt“ in Addendum B: beide
/// Schritte laufen **synchron** in [`Self::on_session_closed`], nicht in
/// einem eigenen Thread — ein `std::thread::spawn` könnte den Prozess
/// überleben (TUI-Quit, `OneShot`-Ende) und würde die Konsolidierung dann
/// nie zu Ende bringen.
pub struct MemoryConsolidationHook {
    /// Die geteilte Erfassungsfläche des Projekts.
    capture: Arc<ProjectMemoryCapture>,
}

impl MemoryConsolidationHook {
    /// Baut einen Haken für die gegebene Erfassungsfläche.
    ///
    /// # Argumente
    /// - `capture` (`Arc<ProjectMemoryCapture>`): dieselbe Erfassungsfläche
    ///   wie bei [`MemoryCaptureObserver::new`] — beide teilen sich einen
    ///   `Arc`, damit der Flush denselben Sitzungszustand sieht, den die
    ///   Erfassung zuvor befüllt hat.
    ///
    /// # Rückgabe
    /// Einen einsatzbereiten [`MemoryConsolidationHook`].
    #[must_use]
    pub fn new(capture: Arc<ProjectMemoryCapture>) -> Self {
        Self { capture }
    }
}

impl SessionLifecycleHook for MemoryConsolidationHook {
    /// Schließt die Erfassung dieser Sitzung ab und konsolidiert das
    /// Projektgedächtnis synchron.
    ///
    /// # Beschreibung
    /// Ruft zuerst [`ProjectMemoryCapture::flush_session`] (schreibt den in
    /// dieser Sitzung gesammelten Zustand in den episodischen Puffer), dann
    /// [`consolidate_project_memories`] auf derselben Projekt-Wurzel. Beide
    /// Schritte sind reine Dateioperationen; ein interner
    /// `ConsolidationLock`-Konflikt (eine andere Sitzung konsolidiert
    /// gerade) wird von `consolidate_project_memories` selbst nur mit
    /// `tracing::debug!` übersprungen, nie propagiert — kein Ergebnis dieser
    /// Methode hängt davon ab.
    ///
    /// # Argumente
    /// - `id` (`&SessionId`): die beendete Sitzung (Wurzel oder Kind).
    ///
    /// # Fehler
    /// Kein propagierter Fehler: ein Fehlschlag der Konsolidierung wird nur
    /// mit `tracing::warn!` gemeldet, das Ende der Sitzung bleibt davon
    /// unberührt.
    fn on_session_closed(&self, id: &SessionId) {
        self.capture.flush_session(id.as_str());

        match consolidate_project_memories(self.capture.memories_root()) {
            Ok(report) => {
                tracing::info!(
                    session_id = %id,
                    merged = report.merged,
                    written = report.written,
                    deleted = report.deleted,
                    conflicts = report.conflicts,
                    "memory.consolidation.completed"
                );
            }
            Err(error) => {
                tracing::warn!(
                    session_id = %id,
                    error = %error,
                    "memory.consolidation.failed"
                );
            }
        }
    }
}

/// Holt beim Start liegengebliebene `_incoming`-Kandidaten nach (Addendum B,
/// „Präzisierung Konsolidierungszeitpunkt“).
///
/// # Beschreibung
/// Öffnet einen eigenen, mit `"harw-memory-sweep"` benannten Thread, der die
/// Projekt-Konsolidierung einmal ausführt (ohne Ledger-Eintrag; mit Ledger
/// siehe [`spawn_startup_sweep_job`]). Anders als
/// [`MemoryConsolidationHook::on_session_closed`] läuft dieser Aufruf
/// **nicht** synchron: er hängt am Aufbau der Wurzelsitzung und darf deren
/// Start nicht verzögern — ein liegengebliebener Kandidat aus einem
/// abgestürzten vorherigen Lauf ist nicht dringend, nur „sollte irgendwann
/// nachgeholt werden“.
///
/// # Argumente
/// - `capture` (`Arc<ProjectMemoryCapture>`): die geteilte Erfassungsfläche,
///   deren Projekt-Wurzel ([`ProjectMemoryCapture::memories_root`]) der
///   Sweep konsolidiert.
///
/// # Nebenläufigkeit
/// Spawnt genau einen `std::thread`; der `JoinHandle` wird bewusst nicht
/// aufbewahrt — der Aufrufer (Montage) muss nicht auf den Sweep warten, und
/// ein Panic im Sweep-Thread bleibt lokal (kein `.join()`, das ihn
/// propagieren könnte).
pub fn spawn_startup_sweep(capture: Arc<ProjectMemoryCapture>) {
    let _ = spawn_startup_sweep_job(capture, None, &harw_config::MemorySection::default());
}

/// Werthalter der In-Prozess-Ausführung des Startup-Sweeps (Worker-Id beim
/// Claim im Job-Store).
pub const MEMORY_SWEEP_WORKER_ID: &str = "harw-runtime-memory-sweep";

/// Reiht den Startup-Sweep als echten Job der Art
/// [`harw_ops::memory_job::MEMORY_MAINTENANCE_JOB_KIND`] im Job-Store ein und
/// führt ihn ohne Verzögerung der Montage aus — [`spawn_startup_sweep`] mit
/// Ledger und Konfiguration.
///
/// # Beschreibung
/// Es gibt keine eigene Job-Mechanik und keine eigene Claim-Schleife: Der
/// Eintrag ist ein gewöhnlicher `memory_maintenance`-Job
/// (`harw_ops::memory_job::admit_memory_maintenance`), die Frist kommt aus
/// `[memory] sweep_deadline_secs` (Vorgabe 120 s). Weil Einstiege wie die TUI
/// keinen Job-Worker betreiben, führt der Thread `harw-memory-sweep` den Job
/// über die Executor-API des Job-Systems aus (`DurableJobRunner`: ein
/// atomarer Claim unter [`MEMORY_SWEEP_WORKER_ID`], Lease-Heartbeat, Commit)
/// mit demselben Handler-Treiber wie der Job-Worker von `harw serve`
/// (`run_memory_maintenance_job`). Der Job ist damit über die Job-Werkzeuge
/// abbrech- und einsehbar (`/ps work`, `/stop`); Abbruch erreicht den Handler
/// kooperativ. Fristablauf endet typisiert als `timed_out`, Abbruch als
/// `cancelled`. Läuft zugleich ein Worker, gewinnt, wer den Job zuerst
/// claimt; der Verlierer tut nichts.
///
/// Ohne Ledger (`jobs == None`) läuft dieselbe Arbeit unprotokolliert auf dem
/// Thread. Scheitert die Zulassung bei vorhandenem Ledger, läuft die Arbeit
/// **nicht** (fail closed: kein unprotokolliertes Aufräumen). Mit
/// `settings.enabled == false` geschieht nichts.
///
/// # Rückgabe
/// Der `JoinHandle` des Threads (Tests joinen ihn; die Montage nicht), oder
/// `None`, wenn nichts gestartet wurde.
pub fn spawn_startup_sweep_job(
    capture: Arc<ProjectMemoryCapture>,
    jobs: Option<Arc<JobStore>>,
    settings: &harw_config::MemorySection,
) -> Option<std::thread::JoinHandle<()>> {
    if !settings.enabled {
        return None;
    }
    let mut spec = MemoryMaintenanceSpec::new(
        MemoryMaintenanceOp::Sweep,
        settings.sweep_deadline_secs.max(1),
    );
    spec.project_root = Some(capture.memories_root().to_path_buf());
    spec.max_unused_days = i64::from(settings.max_unused_days);

    let ledger = match jobs {
        Some(jobs) => {
            let scope = JobScope::new(
                TenantId::from_str("local"),
                WorkspaceId::from_str("memory"),
                ApprovalActor::Operator {
                    id: MEMORY_JOB_HOLDER.to_owned(),
                },
            );
            match admit_memory_maintenance(&jobs, scope, &spec) {
                Ok(work_id) => Some((jobs, work_id)),
                Err(error) => {
                    tracing::warn!(%error, "memory.sweep.admit_failed");
                    return None;
                }
            }
        }
        None => None,
    };
    let builder = std::thread::Builder::new().name("harw-memory-sweep".to_owned());
    match builder.spawn(move || run_sweep(&spec, ledger)) {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::warn!(%error, "memory.sweep.spawn_failed");
            None
        }
    }
}

/// Frist des Extraktionsjobs in Sekunden (ein Modellaufruf je Sitzung).
const LEARNING_EXTRACT_DEADLINE_SECS: u64 = 300;

/// Reiht – nur mit `[memory] llm_extraction` und vorhandenen Digests – einen
/// `learning_extract`-Job ein. Die Runtime führt ihn **nicht** selbst aus: ein
/// Job-Worker mit Modell-Provider (`harw serve`) claimt ihn; ohne Worker bleibt
/// er `Ready` und die (begrenzten) Digests liegen weiter. Nicht blockierend;
/// Fehler werden nur geloggt.
pub fn enqueue_learning_extract(
    memories_root: &std::path::Path,
    jobs: Option<&JobStore>,
    settings: &harw_config::MemorySection,
) {
    let Some(jobs) = jobs else { return };
    if !settings.enabled
        || !settings.llm_extraction
        || harw_memory::llm_extract::pending_sessions(memories_root).is_empty()
        || !memories_root.is_absolute()
    {
        return;
    }
    let spec = harw_ops::learning_job::LearningExtractSpec::new(
        memories_root.to_path_buf(),
        LEARNING_EXTRACT_DEADLINE_SECS,
    );
    let scope = JobScope::new(
        TenantId::from_str("local"),
        WorkspaceId::from_str("memory"),
        ApprovalActor::Operator {
            id: MEMORY_JOB_HOLDER.to_owned(),
        },
    );
    match harw_ops::learning_job::admit_learning_extract(jobs, scope, &spec) {
        Ok(work_id) => tracing::info!(%work_id, "memory.learning_extract.enqueued"),
        Err(error) => tracing::warn!(%error, "memory.learning_extract.admit_failed"),
    }
}

/// Werthalter (Submitter) der Wartungsjobs der Runtime.
const MEMORY_JOB_HOLDER: &str = "memory-maintenance";

// Steuerung der In-Prozess-Ausführung unter dem `DurableJobRunner`: der
// Abbruch erreicht den Handler über das Job-Token des Runners, das
// Abschlusssignal setzt der Handler-Wrapper.
struct SweepControl {
    completion: tokio::sync::watch::Receiver<bool>,
}

impl ExecutionControl for SweepControl {
    fn request_graceful_cancel(&self) {}

    fn force_abort(&self) {}

    fn completion(&self) -> tokio::sync::watch::Receiver<bool> {
        self.completion.clone()
    }
}

// Führt den zugelassenen Sweep-Job über die Executor-API des Job-Systems
// (`DurableJobRunner`: Claim, Registry-Bindung, Heartbeat, Commit) aus —
// derselbe Handler-Treiber wie im Job-Worker. Es gibt hier keine eigene
// Claim-Schleife: der Runner claimt atomar; ein Worker, der den Job zuerst
// bekommt, gewinnt, der Verlierer tut nichts.
async fn run_through_executor(spec: MemoryMaintenanceSpec, jobs: Arc<JobStore>, work_id: WorkId) {
    let ttl = SignedDuration::from_secs(
        i64::try_from(spec.deadline_secs.saturating_add(60)).unwrap_or(i64::MAX),
    );
    let request = ClaimRequest {
        worker_id: MEMORY_SWEEP_WORKER_ID.to_owned(),
        lease_ttl: ttl,
        now: Timestamp::now(),
    };
    let runner = DurableJobRunner::new(Arc::clone(&jobs), Arc::new(JobExecutionRegistry::new()));
    let store = Arc::clone(&jobs);
    let id = work_id.clone();
    let result = runner
        .run_with_cancel(&work_id, &request, move |_claim, token| {
            let (done_tx, done_rx) = tokio::sync::watch::channel(false);
            let control: Arc<dyn ExecutionControl> = Arc::new(SweepControl {
                completion: done_rx,
            });
            let operation = async move {
                let outcome = run_memory_maintenance_job(spec, id, store, async move {
                    token.cancelled().await;
                })
                .await;
                let _ = done_tx.send(true);
                outcome
            };
            (control, operation)
        })
        .await;
    match result {
        Ok(completion) => tracing::info!(
            %work_id,
            disposition = %completion.outcome.disposition(),
            "memory.startup_sweep.committed"
        ),
        Err(error) => {
            // Ein anderer Claimer (Job-Worker) war schneller, oder der Job
            // wurde abgebrochen: nichts zu tun.
            tracing::debug!(%error, %work_id, "memory.sweep.not_run");
        }
    }
}

// Thread-Körper des Sweeps. Mit Ledger läuft der Job über die Executor-API
// des Job-Systems; ohne Ledger läuft dieselbe Arbeit unprotokolliert.
fn run_sweep(spec: &MemoryMaintenanceSpec, ledger: Option<(Arc<JobStore>, WorkId)>) {
    let Some((jobs, work_id)) = ledger else {
        match execute_memory_maintenance(spec) {
            Ok(result) => tracing::info!(%result, "memory.startup_sweep.completed"),
            Err(failure) => {
                tracing::warn!(reason = %failure.reason(), "memory.startup_sweep.failed");
            }
        }
        return;
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%error, "memory.sweep.runtime_failed");
            return;
        }
    };
    runtime.block_on(run_through_executor(spec.clone(), jobs, work_id));
}

// ---------------------------------------------------------------------
// Fakten-Stores für jeden Einstieg
// ---------------------------------------------------------------------

/// Öffnet die Projekt-Fakten-Wurzel von `cwd` (best-effort).
///
/// `None` bei jedem Fehlschlag der Projekterkennung oder des Öffnens; nur
/// `tracing::warn!`.
#[must_use]
pub fn open_project_fact_store(cwd: &Path) -> Option<Arc<FactStore>> {
    let project = match harw_home::project::discover_project(cwd, &[]) {
        Ok(project) => project,
        Err(error) => {
            tracing::warn!(%error, "harw-memory: konnte Projekt-Root nicht ermitteln");
            return None;
        }
    };
    let project_home = harw_home::project::ProjectHome::at(&project);
    if let Err(error) = project_home.ensure() {
        tracing::warn!(%error, "harw-memory: konnte Projekt-Home nicht anlegen");
    }
    let root = project_home.memories_dir();
    match FactStore::open(&root, FactScope::Project) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            tracing::warn!(
                path = %root.display(),
                %error,
                "harw-memory: konnte Projekt-Fakten-Wurzel nicht öffnen"
            );
            None
        }
    }
}

/// Öffnet die globale Fakten-Wurzel des aktiven Profils
/// (`<home>/profiles/<profil>/memories`), best-effort.
///
/// `None`, wenn das Profilverzeichnis nicht aufgelöst oder die Wurzel nicht
/// geöffnet werden kann (nur `tracing::warn!`) — ein Gedächtnisproblem darf
/// nie einen Start verhindern.
#[must_use]
pub fn open_global_fact_store(home: &Path) -> Option<Arc<FactStore>> {
    let profile_name = harw_home::active_profile_name(home);
    let profile = match harw_home::profile_dir(home, &profile_name) {
        Ok(profile) => profile,
        Err(error) => {
            tracing::warn!(
                %error,
                "harw-memory: konnte Profilverzeichnis für Fakten nicht auflösen"
            );
            return None;
        }
    };
    let root = profile.join("memories");
    match FactStore::open(&root, FactScope::Global) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            tracing::warn!(
                path = %root.display(),
                %error,
                "harw-memory: konnte globale Fakten-Wurzel nicht öffnen"
            );
            None
        }
    }
}

/// Gemeinsamer Helfer aller Einstiege: `(projekt, global)`-Fakten-Stores.
///
/// Wird von `harw-cli` (Chat) und — als Vorgabe, wenn der Aufrufer keine
/// Stores übergibt — von `RuntimeAssemblyBuilder::build` für jeden Einstieg
/// genutzt, damit kein Einstieg (Gateway, Web, Jobs, …) den globalen
/// Gedächtnis-Recall verliert.
#[must_use]
pub fn open_fact_stores(
    home: &Path,
    cwd: &Path,
) -> (Option<Arc<FactStore>>, Option<Arc<FactStore>>) {
    (open_project_fact_store(cwd), open_global_fact_store(home))
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use harw_job_runtime::JobState;
    use harw_session_store::JobListQuery;

    type TestError = Box<dyn std::error::Error>;
    type TestResult<T = ()> = Result<T, TestError>;

    fn capture(root: &Path) -> TestResult<Arc<ProjectMemoryCapture>> {
        Ok(Arc::new(ProjectMemoryCapture::open(root)?))
    }

    #[test]
    fn sweep_is_a_real_memory_maintenance_job_completed_off_thread() -> TestResult {
        let state = tempfile::tempdir()?;
        let memories = tempfile::tempdir()?;
        let jobs = Arc::new(JobStore::new(state.path()));
        let handle = spawn_startup_sweep_job(
            capture(memories.path())?,
            Some(Arc::clone(&jobs)),
            &harw_config::MemorySection::default(),
        )
        .ok_or("sweep thread must start")?;
        handle.join().map_err(|_| "sweep thread panicked")?;

        let page = jobs.list(&JobListQuery::default())?;
        let record = page.jobs.first().ok_or("job record")?;
        assert!(harw_ops::memory_job::is_memory_maintenance_kind(
            &record.job.kind
        ));
        assert_eq!(record.job.state, JobState::Completed);
        // Frist aus der Job-Payload: Vorgabe 120 s.
        assert_eq!(record.input["deadline_secs"], 120);
        Ok(())
    }

    #[test]
    fn sweep_failure_is_recorded_as_failed() -> TestResult {
        let state = tempfile::tempdir()?;
        let memories = tempfile::tempdir()?;
        let jobs = Arc::new(JobStore::new(state.path()));
        let mut spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 30);
        // Wurzel fehlt: der Handler scheitert, der Job endet `Failed`.
        spec.project_root = None;
        let scope = JobScope::new(
            TenantId::from_str("local"),
            WorkspaceId::from_str("memory"),
            ApprovalActor::Operator {
                id: MEMORY_JOB_HOLDER.to_owned(),
            },
        );
        let work_id = admit_memory_maintenance(&jobs, scope, &spec)?;
        run_sweep(&spec, Some((Arc::clone(&jobs), work_id.clone())));
        assert_eq!(jobs.get(&work_id)?.job.state, JobState::Failed);
        drop(memories);
        Ok(())
    }

    #[test]
    fn disabled_memory_starts_nothing_and_admits_nothing() -> TestResult {
        let state = tempfile::tempdir()?;
        let memories = tempfile::tempdir()?;
        let jobs = Arc::new(JobStore::new(state.path()));
        let settings = harw_config::MemorySection {
            enabled: false,
            ..harw_config::MemorySection::default()
        };
        let handle = spawn_startup_sweep_job(
            capture(memories.path())?,
            Some(Arc::clone(&jobs)),
            &settings,
        );
        assert!(handle.is_none());
        assert!(jobs.list(&JobListQuery::default())?.jobs.is_empty());
        Ok(())
    }

    #[test]
    fn a_job_claimed_elsewhere_is_not_run_a_second_time() -> TestResult {
        let state = tempfile::tempdir()?;
        let jobs = Arc::new(JobStore::new(state.path()));
        let mut spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 30);
        spec.project_root = None;
        let scope = JobScope::new(
            TenantId::from_str("local"),
            WorkspaceId::from_str("memory"),
            ApprovalActor::Operator {
                id: MEMORY_JOB_HOLDER.to_owned(),
            },
        );
        let work_id = admit_memory_maintenance(&jobs, scope, &spec)?;
        // Ein anderer Worker claimt zuerst.
        let _other = jobs.claim(
            &work_id,
            &ClaimRequest {
                worker_id: "other-worker".to_owned(),
                lease_ttl: SignedDuration::from_secs(60),
                now: Timestamp::now(),
            },
        )?;
        run_sweep(&spec, Some((Arc::clone(&jobs), work_id.clone())));
        // Unser Thread hat nichts abgeschlossen: der Job läuft weiter.
        let record = jobs.get(&work_id)?;
        assert_eq!(record.job.state, JobState::Running);
        assert_eq!(
            record.lease.as_ref().map(|lease| lease.holder.as_str()),
            Some("other-worker"),
            "the foreign claim is untouched"
        );
        Ok(())
    }

    #[test]
    fn exactly_one_worker_id_claims_the_sweep_through_the_executor_api() -> TestResult {
        let state = tempfile::tempdir()?;
        let memories = tempfile::tempdir()?;
        let jobs = Arc::new(JobStore::new(state.path()));
        let handle = spawn_startup_sweep_job(
            capture(memories.path())?,
            Some(Arc::clone(&jobs)),
            &harw_config::MemorySection::default(),
        )
        .ok_or("sweep thread must start")?;
        handle.join().map_err(|_| "sweep thread panicked")?;
        let record = jobs.list(&JobListQuery::default())?.jobs.remove(0);
        // One claim (epoch 1), then committed by the same fenced lease.
        assert_eq!(record.lease_epoch, 1);
        assert_eq!(record.job.state, JobState::Completed);
        assert!(record.lease.is_none());
        assert_eq!(
            record.disposition(),
            harw_job_runtime::JobDisposition::Succeeded
        );
        Ok(())
    }

    #[test]
    fn a_sweep_cancelled_before_it_ran_stays_cancelled_and_runs_nothing() -> TestResult {
        let state = tempfile::tempdir()?;
        let jobs = Arc::new(JobStore::new(state.path()));
        let spec = MemoryMaintenanceSpec::new(MemoryMaintenanceOp::Sweep, 30);
        let scope = JobScope::new(
            TenantId::from_str("local"),
            WorkspaceId::from_str("memory"),
            ApprovalActor::Operator {
                id: MEMORY_JOB_HOLDER.to_owned(),
            },
        );
        let work_id = admit_memory_maintenance(&jobs, scope, &spec)?;
        jobs.cancel(
            &work_id,
            &harw_session_store::CancelRequest {
                cancelled_at: Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
                reason: "no thanks".to_owned(),
            },
        )?;
        run_sweep(&spec, Some((Arc::clone(&jobs), work_id.clone())));
        let record = jobs.get(&work_id)?;
        assert_eq!(record.job.state, JobState::Cancelled);
        assert_eq!(record.lease_epoch, 0, "never claimed");
        Ok(())
    }

    #[test]
    fn observer_writes_a_digest_only_when_given_a_writer_and_the_job_is_enqueued_only_with_the_flag()
    -> TestResult {
        use harw_core::capture::ToolOutcomeObserver;
        let root = tempfile::tempdir()?;
        let memories = root.path().join("memories");
        let capture = Arc::new(ProjectMemoryCapture::open(&memories)?);
        let session = SessionId::from_str("sess-1");
        let plain = MemoryCaptureObserver::new(Arc::clone(&capture));
        plain.on_user_message(&session, "Nutze immer nextest");
        assert!(harw_memory::llm_extract::pending_sessions(&memories).is_empty());

        let observer = MemoryCaptureObserver::new(capture)
            .with_digest(Some(harw_memory::llm_extract::DigestWriter::new(&memories)));
        observer.on_user_message(&session, "Nutze immer nextest");
        assert_eq!(
            harw_memory::llm_extract::pending_sessions(&memories),
            vec!["sess-1".to_owned()]
        );

        let state = tempfile::tempdir()?;
        let jobs = JobStore::new(state.path());
        let mut settings = harw_config::MemorySection::default();
        enqueue_learning_extract(&memories, Some(&jobs), &settings);
        let none = jobs.list(&harw_session_store::JobListQuery::default())?;
        assert!(none.jobs.is_empty(), "flag off: nothing enqueued");
        settings.llm_extraction = true;
        enqueue_learning_extract(&memories, Some(&jobs), &settings);
        let page = jobs.list(&harw_session_store::JobListQuery::default())?;
        assert_eq!(page.jobs.len(), 1);
        assert!(harw_ops::learning_job::is_learning_extract_kind(
            &page.jobs[0].job.kind
        ));
        Ok(())
    }
}
