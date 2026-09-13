//! `PlanStore`-Trait — gemeinsames Interface für alle Store-Implementierungen.
//!
//! Verantwortungsbereich: Definiert die öffentliche API, über die Caller
//! Plan-Mutationen anwenden, Pläne lesen und History abrufen.
//!
//! Die Graph-Abfragen [`PlanStore::ready_nodes`] und [`PlanStore::waves`] sind
//! Default-Methoden über `current()` und `crate::graph` — jede Implementierung
//! erbt sie, ohne die Topologie-Logik zu duplizieren.
//!
//! Beide Implementierungen (`InMemoryPlanStore`, `FilePlanStore`) sind
//! `Send + Sync` (Design-Doc §4).
//!
//! Exportierte Typen: [`PlanStore`].

use crate::actions::{PlanAction, PlanEvent};
use crate::error::PlanResult;
use crate::ids::{RevisionId, TaskId};
use crate::types::Plan;

/// Einheitliches Interface für Plan-Stores.
///
/// # Description
/// Alle Mutationen laufen über `apply`; `current` und `history` sind Lesezugriffe.
/// Implementierungen müssen thread-sicher sein (`Send + Sync`).
///
/// # Concurrency
/// `Send + Sync` — alle Methoden nehmen `&self` und sichern intern via `RwLock`
/// oder vergleichbare Mechanismen.
pub trait PlanStore: Send + Sync {
    /// Gibt den aktuellen Planzustand zurück.
    ///
    /// # Errors
    /// - [`PlanError::PlanNotFound`] wenn noch kein Plan angelegt wurde.
    fn current(&self) -> PlanResult<Plan>;

    /// Gibt die aktuelle Revisionsnummer zurück.
    ///
    /// # Returns
    /// `RevisionId` der aktuellen Revision.
    fn revision(&self) -> RevisionId;

    /// Wendet eine Aktion auf den Plan an und gibt das resultierende Event zurück.
    ///
    /// # Description
    /// Ruft vor der Mutation `validate(plan, action)` auf.
    /// Untrusted Felder (`created_at`, `updated_at`, `revision`, `actor`) werden
    /// vom Store gesetzt — nicht aus der Action übernommen (Design-Doc §7).
    ///
    /// # Arguments
    /// - `action` (`PlanAction`): anzuwendende Aktion.
    /// - `actor` (`&str`): Bezeichner des Akteurs (runtime-gesetzt).
    ///
    /// # Returns
    /// `PlanEvent` mit der angewendeten Aktion und Metadaten.
    ///
    /// # Errors
    /// - Alle `PlanError`-Varianten aus `validate`.
    /// - [`PlanError::Io`] / [`PlanError::Serde`] bei Persistenzfehlern.
    fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent>;

    /// Gibt die Event-History zurück.
    ///
    /// # Arguments
    /// - `since` (`Option<RevisionId>`): filtert Events nach Revision (inklusiv).
    ///   `None` gibt alle Events zurück.
    ///
    /// # Returns
    /// `Vec<PlanEvent>` in chronologischer Reihenfolge.
    ///
    /// # Errors
    /// - [`PlanError::Io`] / [`PlanError::Serde`] bei Lesefehler.
    fn history(&self, since: Option<RevisionId>) -> PlanResult<Vec<PlanEvent>>;

    /// Gibt die IDs aller aktuell ausführbaren Knoten zurück.
    ///
    /// # Description
    /// Default-Implementierung über `current()` und
    /// [`crate::graph::ready_nodes`]. Ein Knoten ist ausführbar, wenn er
    /// weder terminal noch blockiert ist und alle seine Abhängigkeiten
    /// abgeschlossen sind — die Regel gehört dem `graph`-Modul, nicht dem
    /// Store. Implementierungen überschreiben diese Methode nur, wenn sie die
    /// Antwort günstiger als über einen vollständigen Plan-Snapshot liefern
    /// können.
    ///
    /// # Returns
    /// `Vec<TaskId>` in der Reihenfolge, die `graph::ready_nodes` liefert;
    /// leer, wenn kein Knoten ausführbar ist.
    ///
    /// # Errors
    /// - [`PlanError::PlanNotFound`] wenn noch kein Plan angelegt wurde.
    /// - Alle Fehler, die `current()` der jeweiligen Implementierung meldet.
    ///
    /// # Concurrency
    /// Hält nur den Lese-Lock von `current()`; die Auswertung erfolgt auf dem
    /// zurückgegebenen Snapshot ohne gehaltenen Lock.
    fn ready_nodes(&self) -> PlanResult<Vec<TaskId>> {
        let plan = self.current()?;
        Ok(crate::graph::ready_nodes(&plan)
            .into_iter()
            .map(|node| node.id.clone())
            .collect())
    }

    /// Gibt die topologischen Ausführungswellen des Plans zurück.
    ///
    /// # Description
    /// Default-Implementierung über `current()` und
    /// [`crate::graph::topological_waves`]. Welle `n` enthält alle Knoten,
    /// deren Abhängigkeiten vollständig in den Wellen `0..n` liegen; die
    /// Knoten einer Welle sind untereinander unabhängig und damit parallel
    /// ausführbar.
    ///
    /// # Returns
    /// `Vec<Vec<TaskId>>` — äußerer Index ist die Wellennummer.
    ///
    /// # Errors
    /// - [`PlanError::PlanNotFound`] wenn noch kein Plan angelegt wurde.
    /// - [`PlanError::CycleDetected`] wenn der Dependency-Graph zyklisch ist
    ///   und sich deshalb nicht in Wellen zerlegen lässt.
    ///
    /// # Concurrency
    /// Hält nur den Lese-Lock von `current()`.
    fn waves(&self) -> PlanResult<Vec<Vec<TaskId>>> {
        let plan = self.current()?;
        crate::graph::topological_waves(&plan)
    }
}
