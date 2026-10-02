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
use harw_job_runtime::{JobOutcome, JobScope, WorkId};
use harw_memory::capture::{ProjectMemoryCapture, consolidate_project_memories};
use harw_memory::{FactScope, FactStore};
use harw_ops::memory_job::{
    MemoryMaintenanceOp, MemoryMaintenanceSpec, admit_memory_maintenance,
    execute_memory_maintenance,
};
use harw_session_store::{ClaimRequest, CompleteRequest, JobStore};
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
        Self { capture }
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
        self.capture.record_tool_outcome(
            session_id.as_str(),
            outcome.tool_name,
            outcome.arguments,
            is_error,
            outcome.output_text,
        );
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
/// Es gibt keine eigene Job-Mechanik: Der Eintrag ist ein gewöhnlicher
/// `memory_maintenance`-Job (`harw_ops::memory_job::admit_memory_maintenance`),
/// derselbe Handler-Kern
/// ([`harw_ops::memory_job::execute_memory_maintenance`]) wie im Job-Worker von
/// `harw serve`, die Frist kommt aus `[memory] sweep_deadline_secs` (Vorgabe
/// 120 s). Weil Einstiege wie die TUI keinen Job-Worker betreiben, claimt und
/// führt ein eigener Thread (`harw-memory-sweep`) den Job selbst aus
/// (`JobStore::claim`/`complete`, Halter [`MEMORY_SWEEP_WORKER_ID`]) — nie im
/// Build-Pfad. Läuft zugleich ein Worker, gewinnt, wer den Job zuerst claimt;
/// der Verlierer tut nichts. Fristablauf bricht ohne Teilzustand ab und
/// schließt den Job als `Failed` (`timed_out: …`) ab.
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

/// Werthalter (Submitter) der Wartungsjobs der Runtime.
const MEMORY_JOB_HOLDER: &str = "memory-maintenance";

// Thread-Körper des Sweeps: claimt den zugelassenen Job (falls es ein Ledger
// gibt), führt den gemeinsamen Handler-Kern aus und schließt den Job ab.
fn run_sweep(spec: &MemoryMaintenanceSpec, ledger: Option<(Arc<JobStore>, WorkId)>) {
    let claim = match &ledger {
        Some((jobs, work_id)) => {
            let ttl = SignedDuration::from_secs(
                i64::try_from(spec.deadline_secs.saturating_add(60)).unwrap_or(i64::MAX),
            );
            let request = ClaimRequest {
                worker_id: MEMORY_SWEEP_WORKER_ID.to_owned(),
                lease_ttl: ttl,
                now: Timestamp::now(),
            };
            match jobs.claim(work_id, &request) {
                Ok(claim) => Some(claim),
                Err(error) => {
                    // Ein Job-Worker hat ihn bereits geclaimt (oder der Job
                    // ist weg): nichts zu tun.
                    tracing::debug!(%error, %work_id, "memory.sweep.claim_skipped");
                    return;
                }
            }
        }
        None => None,
    };
    let outcome = match execute_memory_maintenance(spec) {
        Ok(result) => {
            tracing::info!(%result, "memory.startup_sweep.completed");
            JobOutcome::Succeeded { result }
        }
        Err(failure) => {
            let reason = failure.reason();
            tracing::warn!(%reason, "memory.startup_sweep.failed");
            JobOutcome::Failed { reason }
        }
    };
    if let (Some((jobs, work_id)), Some(claim)) = (&ledger, claim) {
        let request = CompleteRequest {
            token: claim.token,
            completed_at: Timestamp::now(),
            outcome,
        };
        if let Err(error) = jobs.complete(work_id, &request) {
            tracing::warn!(%error, %work_id, "memory.sweep.complete_failed");
        }
    }
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
        let handle =
            spawn_startup_sweep_job(capture(memories.path())?, Some(Arc::clone(&jobs)), &settings);
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
        assert_eq!(jobs.get(&work_id)?.job.state, JobState::Running);
        Ok(())
    }
}
