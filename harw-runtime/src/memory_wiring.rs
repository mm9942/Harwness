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
//! Zusätzlich baut [`spawn_startup_sweep`] beim Aufbau der Wurzelsitzung
//! einen Hintergrund-Thread, der liegengebliebene `_incoming`-Kandidaten
//! nach einem Absturz nachholt — dieser Fall läuft bewusst **nicht**
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
use harw_job_runtime::{
    Budget, Job, JobKind, JobOutcome, JobScope, JobState, Lease, LeaseToken, RetryPolicy,
    StoredJob, WorkId,
};
use harw_memory::capture::{ProjectMemoryCapture, consolidate_project_memories};
use harw_memory::{FactScope, FactStore};
use harw_session_store::{CompleteRequest, JobStore};
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
/// Öffnet einen eigenen, mit `"harw-memory-sweep"` benannten Thread, der
/// [`consolidate_project_memories`] einmal ausführt. Anders als
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
    spawn_startup_sweep_job(capture, None);
}

/// Werthalter des Wartungsjobs im Job-Ledger.
pub const MEMORY_JOB_HOLDER: &str = "memory-maintenance";
/// [`harw_job_runtime::JobKind::Custom`]-Art des Startup-Sweeps.
pub const MEMORY_SWEEP_JOB_KIND: &str = "memory.startup_sweep";
/// Frist (Wanduhr) des Startup-Sweeps als Jobbudget.
pub const MEMORY_SWEEP_MAX_WALL: std::time::Duration = std::time::Duration::from_secs(120);

/// Ein im [`JobStore`] zugelassener Wartungsjob samt eigener Lease (Halter
/// [`MEMORY_JOB_HOLDER`], daher von keinem Worker beanspruchbar) — dasselbe
/// Muster wie der Traum-Lauf (`harw_ops::dream_run`), keine zweite
/// Job-Infrastruktur.
struct LedgerJob {
    jobs: Arc<JobStore>,
    work_id: WorkId,
    token: LeaseToken,
}

impl LedgerJob {
    fn admit(
        jobs: Arc<JobStore>,
        kind: &str,
        max_wall: std::time::Duration,
    ) -> Result<Self, String> {
        let id = WorkId::new();
        let now = Timestamp::now();
        let wall = SignedDuration::try_from(max_wall).unwrap_or(SignedDuration::MAX);
        let mut job = Job::new(
            id.clone(),
            JobKind::Custom(kind.to_owned()),
            Budget {
                max_tokens: Some(0),
                max_wall: Some(wall),
                max_tool_calls: Some(0),
            },
            RetryPolicy {
                max_attempts: 1,
                base_delay: SignedDuration::from_secs(30),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(300),
            },
            now,
        );
        job.state = JobState::Running;
        let ttl = wall
            .checked_add(SignedDuration::from_secs(60))
            .unwrap_or(wall);
        let lease = Lease::acquire_fenced(
            id.clone(),
            MEMORY_JOB_HOLDER,
            now,
            ttl,
            1,
            WorkId::new().as_str(),
        )
        .map_err(|error| error.to_string())?;
        let token = lease.token();
        let record = StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("local"),
                WorkspaceId::from_str("memory"),
                ApprovalActor::Operator {
                    id: MEMORY_JOB_HOLDER.to_owned(),
                },
            ),
            input: serde_json::json!({ "kind": kind }),
            submitted_at: now,
            not_before: now,
            lease: Some(lease),
            lease_epoch: 1,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        };
        jobs.admit(&record).map_err(|error| error.to_string())?;
        Ok(Self {
            jobs,
            work_id: id,
            token,
        })
    }

    fn finish(&self, outcome: JobOutcome) {
        let request = CompleteRequest {
            token: self.token.clone(),
            completed_at: Timestamp::now(),
            outcome,
        };
        if let Err(error) = self.jobs.complete(&self.work_id, &request) {
            tracing::warn!(%error, work_id = %self.work_id, "memory.job.complete_failed");
        }
    }
}

/// Führt `work` als Wartungsjob im Job-Ledger aus: zugelassen mit
/// Wanduhr-Budget `max_wall`, ausgeführt in einem eigenen Thread (nie im
/// Turn-/Build-Pfad), abgeschlossen als `Succeeded`/`Failed`.
///
/// # Beschreibung
/// `work` erhält die Frist (`Instant`) und muss sie selbst einhalten und
/// ohne Teilzustand abbrechen (siehe
/// [`harw_memory::FactStore::decay_with_deadline`]). Mit `jobs == None`
/// (kein Ledger greifbar) läuft die Arbeit unverändert, nur ohne
/// Ledger-Eintrag. Scheitert die Zulassung bei vorhandenem Ledger, läuft die
/// Arbeit **nicht** (fail closed: kein unprotokolliertes Aufräumen).
/// Läuft `work` über die Frist hinaus, wird der Job `Failed` abgeschlossen.
///
/// # Rückgabe
/// Der `JoinHandle` des Threads (Tests joinen ihn; die Montage nicht), oder
/// `None`, wenn Zulassung oder Thread-Start scheiterten.
pub fn spawn_memory_job<F>(
    jobs: Option<Arc<JobStore>>,
    kind: &'static str,
    max_wall: std::time::Duration,
    work: F,
) -> Option<std::thread::JoinHandle<()>>
where
    F: FnOnce(std::time::Instant) -> Result<serde_json::Value, String> + Send + 'static,
{
    let ledger = match jobs {
        Some(jobs) => match LedgerJob::admit(jobs, kind, max_wall) {
            Ok(ledger) => Some(ledger),
            Err(error) => {
                tracing::warn!(%error, kind, "memory.job.admit_failed");
                return None;
            }
        },
        None => None,
    };
    let builder = std::thread::Builder::new().name("harw-memory-job".to_owned());
    let spawned = builder.spawn(move || {
        let started = std::time::Instant::now();
        let deadline = started + max_wall;
        let result = match work(deadline) {
            Ok(_) if started.elapsed() > max_wall => Err("deadline exceeded".to_owned()),
            other => other,
        };
        match result {
            Ok(value) => {
                if let Some(ledger) = &ledger {
                    ledger.finish(JobOutcome::Succeeded { result: value });
                }
            }
            Err(reason) => {
                tracing::warn!(%reason, kind, "memory.job.failed");
                if let Some(ledger) = &ledger {
                    ledger.finish(JobOutcome::Failed { reason });
                }
            }
        }
    });
    match spawned {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::warn!(%error, kind, "memory.job.spawn_failed");
            None
        }
    }
}

/// Wie [`spawn_startup_sweep`], aber als Job im Ledger `jobs` (siehe
/// [`spawn_memory_job`]) mit Frist [`MEMORY_SWEEP_MAX_WALL`].
///
/// # Beschreibung
/// Die Frist wird vor dem Start geprüft; die Konsolidierung selbst
/// (`harw_memory::capture`) ist keine abbrechbare Arbeit — ein Überschreiten
/// wird im Ledger als `Failed` sichtbar, aber nicht mitten im Lauf
/// unterbrochen.
pub fn spawn_startup_sweep_job(
    capture: Arc<ProjectMemoryCapture>,
    jobs: Option<Arc<JobStore>>,
) -> Option<std::thread::JoinHandle<()>> {
    spawn_memory_job(
        jobs,
        MEMORY_SWEEP_JOB_KIND,
        MEMORY_SWEEP_MAX_WALL,
        move |deadline| {
            if std::time::Instant::now() >= deadline {
                return Err("deadline exceeded before start".to_owned());
            }
            let root = capture.memories_root().to_path_buf();
            consolidate_project_memories(&root)
                .map(|report| {
                    tracing::info!(
                        merged = report.merged,
                        written = report.written,
                        deleted = report.deleted,
                        conflicts = report.conflicts,
                        "memory.startup_sweep.completed"
                    );
                    serde_json::json!({
                        "merged": report.merged,
                        "written": report.written,
                        "deleted": report.deleted,
                        "conflicts": report.conflicts,
                    })
                })
                .map_err(|error| error.to_string())
        },
    )
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
