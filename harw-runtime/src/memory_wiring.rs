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

use std::sync::Arc;

use harw_core::capture::{ToolOutcome, ToolOutcomeObserver, ToolOutcomeStatus};
use harw_memory::capture::{ProjectMemoryCapture, consolidate_project_memories};
use harw_types::SessionId;

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
    let builder = std::thread::Builder::new().name("harw-memory-sweep".to_owned());
    let spawned = builder.spawn(move || {
        let root = capture.memories_root().to_path_buf();
        match consolidate_project_memories(&root) {
            Ok(report) => {
                tracing::info!(
                    merged = report.merged,
                    written = report.written,
                    deleted = report.deleted,
                    conflicts = report.conflicts,
                    "memory.startup_sweep.completed"
                );
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "memory.startup_sweep.failed"
                );
            }
        }
    });
    if let Err(error) = spawned {
        tracing::warn!(
            error = %error,
            "memory.startup_sweep.spawn_failed"
        );
    }
}
