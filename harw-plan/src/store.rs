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
//! [`PlanStore::apply_batch`] wendet mehrere Aktionen atomar an (alles oder
//! nichts, optimistische Revisionsprüfung). Die gemeinsame, crate-private
//! Batch-Mechanik (`stage_actions`) liegt ebenfalls hier, damit beide Stores
//! dieselbe Validierungs- und Revisionsvergabe-Reihenfolge nutzen.
//!
//! # Errors
//! [`PlanError::RevisionConflict`], [`PlanError::BatchActionRejected`],
//! [`PlanError::PlanNotFound`] sowie alle Validierungs- und Persistenzfehler.
//!
//! Exportierte Typen: [`PlanStore`], [`PlanRevision`].

use time::OffsetDateTime;

use crate::actions::{PlanAction, PlanEvent};
use crate::catalog::{PlanApproval, PlanMeta, PlanSummary};
use crate::config::PlanToolConfig;
use crate::error::{PlanError, PlanResult};
use crate::ids::{PlanId, RevisionId, TaskId};
use crate::mutation::apply_mutation;
use crate::types::Plan;
use crate::validate::validate_with;

/// Ergebnis eines erfolgreich angewendeten Batches.
///
/// # Description
/// Jede Aktion eines Batches erhält eine eigene, lückenlos aufsteigende
/// Revision (ein [`PlanEvent`] je Aktion, wie bei [`PlanStore::apply`]);
/// `revision` ist die Revision des Plans nach der letzten Aktion. Bei einem
/// leeren Batch ist `events` leer und `revision` die unveränderte aktuelle
/// Revision.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
#[derive(Debug, Clone)]
pub struct PlanRevision {
    /// Revision des Plans nach dem Batch.
    pub revision: RevisionId,
    /// Die angewendeten Events in Anwendungsreihenfolge.
    pub events: Vec<PlanEvent>,
}

/// Wendet `actions` nacheinander auf eine Kopie von `base` an (crate-privat).
///
/// # Description
/// Für jede Aktion in Reihenfolge: `Create` wird abgewiesen (ein Batch
/// arbeitet immer auf einem bestehenden Plan), dann Knotenlimit
/// (`cfg.validate_action` mit laufender Knotenzahl), `validate_with` auf dem
/// **laufenden** Kandidaten, `apply_mutation`, Revision setzen. Der Kandidat
/// wird nur zurückgegeben, wenn *alle* Aktionen gültig sind; `base` bleibt
/// immer unverändert.
///
/// # Returns
/// `(Kandidat, Events)` — der Aufrufer veröffentlicht beides atomar.
///
/// # Errors
/// `(index, fehler)` der ersten abgewiesenen Aktion.
pub(crate) fn stage_actions(
    base: &Plan,
    actions: Vec<PlanAction>,
    actor: &str,
    cfg: &PlanToolConfig,
    first_revision: RevisionId,
    now: OffsetDateTime,
) -> Result<(Plan, Vec<PlanEvent>), (usize, PlanError)> {
    let mut candidate = base.clone();
    let mut events = Vec::with_capacity(actions.len());
    let mut revision = first_revision;
    for (index, action) in actions.into_iter().enumerate() {
        if matches!(action, PlanAction::Create { .. }) {
            return Err((
                index,
                PlanError::PlanExists {
                    id: candidate.id.clone(),
                },
            ));
        }
        cfg.validate_action(&action, candidate.nodes.len())
            .map_err(|error| (index, PlanError::Config(error)))?;
        validate_with(&candidate, &action, cfg, now).map_err(|error| (index, error))?;
        apply_mutation(&mut candidate, &action, actor, now);
        candidate.updated_at = now;
        candidate.revision = revision;
        events.push(PlanEvent {
            revision,
            action,
            actor: actor.to_owned(),
            applied_at: now,
        });
        revision = revision.next();
    }
    Ok((candidate, events))
}

/// Prüft Plan-Identität und erwartete Revision eines Batches (crate-privat).
///
/// # Errors
/// - [`PlanError::PlanNotFound`]: kein Plan oder ein anderer Plan als `plan`.
/// - [`PlanError::RevisionConflict`]: `expected_rev` ≠ aktuelle Revision.
pub(crate) fn check_batch_target<'a>(
    current: Option<&'a Plan>,
    plan: &PlanId,
    expected_rev: RevisionId,
) -> PlanResult<&'a Plan> {
    let current = current.ok_or(PlanError::PlanNotFound)?;
    if &current.id != plan {
        return Err(PlanError::PlanNotFound);
    }
    if current.revision != expected_rev {
        return Err(PlanError::RevisionConflict {
            plan: plan.clone(),
            expected: expected_rev,
            actual: current.revision,
        });
    }
    Ok(current)
}

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

    /// Legt einen Plan an, der dem Mandanten `tenant` gehört (H12).
    ///
    /// # Description
    /// Wie `apply(PlanAction::Create { plan_id, goal }, actor)`, setzt aber
    /// zusätzlich [`Plan::tenant`] — im selben Schreibvorgang, sodass nie ein
    /// ungescopter Zwischenstand sichtbar ist. Das Event bleibt ein
    /// gewöhnliches `Create` (der Mandant steht im Snapshot, nicht im Event).
    ///
    /// Die Standardimplementierung (Fremd- und Test-Stores) kann einen
    /// Mandanten nicht speichern: ohne Mandant delegiert sie an
    /// [`Self::apply`], mit Mandant weist sie fail-closed mit
    /// [`PlanError::CatalogUnsupported`] ab — ein still ungescopter Plan wäre
    /// für seinen eigenen Anleger unsichtbar und für Ungescopte offen.
    /// `InMemoryPlanStore` und `FilePlanStore` überschreiben die Methode.
    ///
    /// # Arguments
    /// - `plan_id` (`PlanId`): Bezeichner des neuen Plans.
    /// - `goal` (`String`): Ziel-Statement.
    /// - `tenant` (`Option<TenantId>`): Mandant aus dem serverseitigen
    ///   Kontext; `None` legt einen ungescopten Plan an (bisheriges Verhalten).
    /// - `actor` (`&str`): Akteur (runtime-gesetzt).
    ///
    /// # Errors
    /// Wie [`Self::apply`] für `Create`; zusätzlich
    /// [`PlanError::CatalogUnsupported`] (Standardimplementierung mit Mandant).
    fn create_for_tenant(
        &self,
        plan_id: PlanId,
        goal: String,
        tenant: Option<harw_types::TenantId>,
        actor: &str,
    ) -> PlanResult<PlanEvent> {
        match tenant {
            None => self.apply(PlanAction::Create { plan_id, goal }, actor),
            Some(_) => Err(PlanError::CatalogUnsupported {
                operation: "create_for_tenant",
            }),
        }
    }

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

    /// Wendet mehrere Aktionen atomar an — alles oder nichts.
    ///
    /// # Description
    /// Optimistische Nebenläufigkeit: der Batch wird nur angewendet, wenn der
    /// aktuelle Plan `plan` ist und seine Revision exakt `expected_rev`
    /// entspricht. Alle Aktionen werden unter dem Schreib-Lock auf einem
    /// Kandidaten (Kopie des Plans) validiert und angewendet; jede Aktion sieht
    /// den Zustand nach ihren Vorgängern (z. B. `AddNode` gefolgt von
    /// `AddDependency` auf den neuen Knoten). Scheitert eine Aktion, bleibt der
    /// Plan unverändert, es entsteht kein Event und (bei `FilePlanStore`) kein
    /// Snapshot und keine History-Zeile. Jede Aktion erhält eine eigene
    /// Revision. `Create` ist im Batch nicht erlaubt.
    ///
    /// # Arguments
    /// - `plan` (`&PlanId`): Plan, auf den sich der Batch bezieht.
    /// - `actions` (`Vec<PlanAction>`): anzuwendende Aktionen in Reihenfolge.
    /// - `actor` (`&str`): Akteur (runtime-gesetzt).
    /// - `expected_rev` (`RevisionId`): Revision, auf der der Aufrufer plant.
    ///
    /// # Returns
    /// [`PlanRevision`] mit neuer Revision und allen Events.
    ///
    /// # Errors
    /// - [`PlanError::PlanNotFound`]: kein Plan oder ein anderer Plan.
    /// - [`PlanError::RevisionConflict`]: Revision passt nicht.
    /// - [`PlanError::BatchActionRejected`]: eine Aktion ist ungültig
    ///   (Index + Ursache; auch `PlanExists` für `Create`, `Config` für das
    ///   Knotenlimit).
    /// - [`PlanError::Io`] / [`PlanError::Serde`] bei Persistenzfehlern.
    ///
    /// # Concurrency
    /// Hält den Schreib-Lock über Prüfung, Validierung und Veröffentlichung.
    fn apply_batch(
        &self,
        plan: &PlanId,
        actions: Vec<PlanAction>,
        actor: &str,
        expected_rev: RevisionId,
    ) -> PlanResult<PlanRevision>;

    // ── Plan-Katalog (Runde 5, Teil P) ──────────────────────────────────────
    //
    // Die Standardimplementierungen beschreiben einen Store mit höchstens
    // einem Plan (Test- und Fremd-Stores). `InMemoryPlanStore` und
    // `FilePlanStore` überschreiben alle sechs Methoden.

    /// Listet alle Pläne des Stores (auch archivierte) in Anlagereihenfolge.
    ///
    /// # Returns
    /// Eine [`PlanSummary`] je Plan; genau eine trägt `active = true`, sofern
    /// ein Plan aktiv ist. Ein leerer Store liefert eine leere Liste.
    ///
    /// # Errors
    /// Lese- bzw. Lock-Fehler der Implementierung.
    fn list_plans(&self) -> PlanResult<Vec<PlanSummary>> {
        match self.current() {
            Ok(plan) => Ok(vec![PlanSummary::from_plan(
                &plan,
                true,
                PlanMeta::default(),
            )]),
            Err(PlanError::PlanNotFound) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// Liest einen Plan per ID — aktiv oder nicht.
    ///
    /// # Errors
    /// - [`PlanError::PlanUnknown`]: kein Plan mit dieser ID.
    fn plan_by_id(&self, id: &PlanId) -> PlanResult<Plan> {
        match self.current() {
            Ok(plan) if &plan.id == id => Ok(plan),
            Ok(_) | Err(PlanError::PlanNotFound) => Err(PlanError::PlanUnknown { id: id.clone() }),
            Err(error) => Err(error),
        }
    }

    /// Liest die Katalogdaten eines Plans (Archiv-Flag, Freigabestand).
    ///
    /// # Errors
    /// - [`PlanError::PlanUnknown`]: kein Plan mit dieser ID.
    fn plan_meta(&self, id: &PlanId) -> PlanResult<PlanMeta> {
        self.plan_by_id(id).map(|_| PlanMeta::default())
    }

    /// Macht den Plan `id` aktiv; ein archivierter Plan wird dabei wieder
    /// eingeblendet.
    ///
    /// # Arguments
    /// - `id` (`&PlanId`): Ziel-Plan.
    /// - `actor` (`&str`): Akteur (nur Protokoll; Katalogänderungen erzeugen
    ///   kein Plan-Event und keine neue Revision).
    ///
    /// # Returns
    /// Den nun aktiven Plan.
    ///
    /// # Errors
    /// - [`PlanError::PlanUnknown`]: kein Plan mit dieser ID.
    /// - [`PlanError::Io`] / [`PlanError::Serde`] beim Schreiben des Index.
    fn switch_plan(&self, id: &PlanId, actor: &str) -> PlanResult<Plan> {
        let _ = actor;
        self.plan_by_id(id)
    }

    /// Blendet den Plan `id` aus, ohne ihn zu löschen. War er aktiv, ist
    /// danach **kein** Plan aktiv, bis `create` oder `switch` einen wählt.
    ///
    /// # Errors
    /// - [`PlanError::PlanUnknown`]: kein Plan mit dieser ID.
    /// - [`PlanError::CatalogUnsupported`] bei einem Einzelplan-Store.
    /// - [`PlanError::Io`] / [`PlanError::Serde`] beim Schreiben des Index.
    fn archive_plan(&self, id: &PlanId, actor: &str) -> PlanResult<()> {
        let _ = (id, actor);
        Err(PlanError::CatalogUnsupported {
            operation: "archive",
        })
    }

    /// Setzt den Freigabestand des Plans `id`.
    ///
    /// # Errors
    /// - [`PlanError::PlanUnknown`]: kein Plan mit dieser ID.
    /// - [`PlanError::CatalogUnsupported`]: Einzelplan-Store und `Proposed`.
    /// - [`PlanError::Io`] / [`PlanError::Serde`] beim Schreiben des Index.
    fn set_approval(&self, id: &PlanId, approval: PlanApproval, actor: &str) -> PlanResult<()> {
        let _ = actor;
        self.plan_by_id(id)?;
        match approval {
            PlanApproval::Confirmed => Ok(()),
            PlanApproval::Proposed => Err(PlanError::CatalogUnsupported {
                operation: "set_approval",
            }),
        }
    }

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
