//! Plan-Katalog: mehrere Pläne je Store, genau einer aktiv (Runde 5, Teil P).
//!
//! # Verantwortungsbereich
//! Bis Runde 5 hielt ein `PlanStore` genau **einen** Plan; ein zweites
//! `Create` scheiterte mit „Create ist nur für einen leeren Store erlaubt".
//! Hatte `/analyze` vorher automatisch `plan-analyze` angelegt, war die
//! Planung damit für das ganze Projekt blockiert. Seit Teil P verwaltet jeder
//! Store beliebig viele Pläne mit eindeutiger ID:
//!
//! | Aktion | Wirkung |
//! |---|---|
//! | `Create` (neue ID) | legt an und macht den Plan **aktiv** |
//! | `Create` (vergebene ID) | [`crate::PlanError::PlanExists`] mit Hinweis auf `switch` |
//! | [`crate::PlanStore::switch_plan`] | macht einen Plan aktiv (holt ihn auch aus dem Archiv) |
//! | [`crate::PlanStore::archive_plan`] | blendet ihn aus, **ohne** ihn zu löschen |
//! | [`crate::PlanStore::list_plans`] | alle Pläne als [`PlanSummary`] |
//! | [`crate::PlanStore::set_approval`] | Freigabestand `proposed`/`confirmed` |
//!
//! Alle Plan-Mutationen (`AddNode`, `SetStatus`, …) und `current()`/
//! `history()`/`revision()` beziehen sich unverändert auf den **aktiven**
//! Plan. Jeder Plan hat seine eigene Revisionsfolge ab 1 und seine eigene
//! History.
//!
//! # Verwaltungsdaten
//! Aktivität, Archiv-Flag und Freigabestand sind **Katalogdaten**, keine
//! Plandaten: sie stehen nicht im [`Plan`] (dessen Serialisierung und
//! Siegel bleiben unverändert), sondern in [`PlanMeta`] bzw. beim
//! `FilePlanStore` in `<root>/plan-index.json` ([`PlanIndex`]).
//!
//! # Migration
//! Ein Store ohne `plan-index.json` (Stand vor Teil P) wird ohne Datenverlust
//! gelesen: alle gültigen Plan-Verzeichnisse werden geladen, aktiv ist der
//! Plan, den der alte Store gewählt hätte (höchste Revision, Tie-Break
//! lexikografisch größere ID), alle gelten als bestätigt und nicht
//! archiviert. Der Index entsteht erst bei der ersten Katalogänderung.
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{PlanId, RevisionId};
use crate::types::{Plan, PlanNodeStatus};

/// Dateiname des Katalog-Index unter der Store-Wurzel (`FilePlanStore`).
pub const PLAN_INDEX_FILE: &str = "plan-index.json";

/// Aktuelle Formatversion von [`PlanIndex`].
pub const PLAN_INDEX_VERSION: u32 = 1;

/// Freigabestand eines Plans.
///
/// # Beschreibung
/// Ein von einem Modell angelegter Plan ist zunächst ein **Vorschlag**
/// (`Proposed`); erst die Bestätigung der Nutzerin macht ihn verbindlich
/// (`Confirmed`). Der Standard ist `Confirmed`: Altbestände, von Menschen
/// angelegte und intern erzeugte Pläne (etwa `plan-analyze`) brauchen keine
/// Rückfrage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanApproval {
    /// Bestätigt bzw. ohne Rückfrage verbindlich.
    #[default]
    Confirmed,
    /// Vorgeschlagen, noch nicht bestätigt.
    Proposed,
}

impl PlanApproval {
    /// Kurzes deutsches Label für Listen und Kopfzeilen.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Confirmed => "bestätigt",
            Self::Proposed => "vorgeschlagen",
        }
    }
}

/// Katalogdaten eines Plans (nicht Teil des [`Plan`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlanMeta {
    /// Ausgeblendet (archiviert), aber nicht gelöscht.
    #[serde(default)]
    pub archived: bool,
    /// Freigabestand.
    #[serde(default)]
    pub approval: PlanApproval,
}

/// Kurzübersicht eines Plans für `plan plans` und Anzeigen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSummary {
    /// Bezeichner.
    pub id: PlanId,
    /// Ziel-Statement.
    pub goal_statement: String,
    /// Aktuelle Revision dieses Plans.
    pub revision: RevisionId,
    /// Anzahl der Knoten.
    pub node_count: usize,
    /// Davon abgeschlossen (`Completed`).
    pub completed: usize,
    /// Gebundenes Goal.
    pub goal_id: Option<String>,
    /// `true` für den aktiven Plan.
    pub active: bool,
    /// Katalogdaten.
    pub meta: PlanMeta,
    /// Letzte Änderung.
    pub updated_at: OffsetDateTime,
}

impl PlanSummary {
    /// Baut die Übersicht aus einem Plan und seinen Katalogdaten.
    #[must_use]
    pub fn from_plan(plan: &Plan, active: bool, meta: PlanMeta) -> Self {
        Self {
            id: plan.id.clone(),
            goal_statement: plan.goal_statement.clone(),
            revision: plan.revision,
            node_count: plan.nodes.len(),
            completed: plan
                .nodes
                .iter()
                .filter(|node| node.status == PlanNodeStatus::Completed)
                .count(),
            goal_id: plan.goal_id.clone(),
            active,
            meta,
            updated_at: plan.updated_at,
        }
    }
}

/// Inhalt von `<root>/plan-index.json` (`FilePlanStore`).
///
/// # Beschreibung
/// Plan-IDs stehen als Strings im Index und werden beim Laden gegen die
/// [`PlanId`]-Grammatik geprüft; unbekannte oder ungültige Einträge werden
/// ignoriert. Fehlt ein Plan im Index, gilt [`PlanMeta::default`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanIndex {
    /// Formatversion ([`PLAN_INDEX_VERSION`]).
    #[serde(default = "default_index_version")]
    pub version: u32,
    /// Aktiver Plan; `None`, wenn keiner aktiv ist (z. B. nach `archive`).
    #[serde(default)]
    pub active: Option<String>,
    /// Katalogdaten je Plan-ID.
    #[serde(default)]
    pub plans: BTreeMap<String, PlanMeta>,
}

impl Default for PlanIndex {
    fn default() -> Self {
        Self {
            version: PLAN_INDEX_VERSION,
            active: None,
            plans: BTreeMap::new(),
        }
    }
}

// Serde-Standard für `PlanIndex::version`.
fn default_index_version() -> u32 {
    PLAN_INDEX_VERSION
}

impl PlanIndex {
    /// Die aktive Plan-ID, falls gesetzt und grammatisch gültig.
    #[must_use]
    pub fn active_id(&self) -> Option<PlanId> {
        self.active
            .as_deref()
            .and_then(|raw| PlanId::parse(raw).ok())
    }

    /// Die Katalogdaten zu `id` (Standard, wenn nicht eingetragen).
    #[must_use]
    pub fn meta(&self, id: &PlanId) -> PlanMeta {
        self.plans.get(id.as_str()).copied().unwrap_or_default()
    }
}

/// Fortschritt eines Plans: `(erledigt, gesamt)`.
///
/// # Beschreibung
/// Gezählt werden alle Knoten außer `Superseded` und `Invalidated` (die
/// gehören nicht mehr zum Plan). Erledigt ist ein Knoten nur mit Status
/// `Completed` — und `Completed` verlangt laut Validierung (Regel 4) immer
/// mindestens einen Nachweis.
#[must_use]
pub fn plan_progress(plan: &Plan) -> (usize, usize) {
    let counted = plan.nodes.iter().filter(|node| {
        !matches!(
            node.status,
            PlanNodeStatus::Superseded | PlanNodeStatus::Invalidated
        )
    });
    let mut done = 0_usize;
    let mut total = 0_usize;
    for node in counted {
        total = total.saturating_add(1);
        if node.status == PlanNodeStatus::Completed {
            done = done.saturating_add(1);
        }
    }
    (done, total)
}

/// Der „aktuelle Schritt“ eines Plans für Anzeigen.
///
/// # Beschreibung
/// Reihenfolge: der erste laufende Knoten (`InProgress`), sonst der erste
/// blockierte (`Blocked`), sonst der erste ausführbare laut
/// [`crate::graph::ready_nodes`]. `None`, wenn nichts davon zutrifft (etwa
/// alles erledigt).
#[must_use]
pub fn current_step(plan: &Plan) -> Option<&crate::types::PlanNode> {
    plan.nodes
        .iter()
        .find(|node| node.status == PlanNodeStatus::InProgress)
        .or_else(|| {
            plan.nodes
                .iter()
                .find(|node| node.status == PlanNodeStatus::Blocked)
        })
        .or_else(|| crate::graph::ready_nodes(plan).into_iter().next())
}

/// `true`, wenn mindestens ein (nicht abgelöster) Knoten blockiert ist.
#[must_use]
pub fn has_blocked_step(plan: &Plan) -> bool {
    plan.nodes
        .iter()
        .any(|node| node.status == PlanNodeStatus::Blocked)
}

/// Textbalken `[████░░░░] 2/5` für Fortschrittsanzeigen.
///
/// # Arguments
/// - `done`, `total`: aus [`plan_progress`].
/// - `width`: Anzahl der Balkenzellen (mindestens 1).
#[must_use]
pub fn progress_bar(done: usize, total: usize, width: usize) -> String {
    let width = width.max(1);
    let filled = done
        .min(total)
        .saturating_mul(width)
        .checked_div(total)
        .unwrap_or(0);
    let mut bar = String::with_capacity(width.saturating_mul(3).saturating_add(12));
    bar.push('[');
    for cell in 0..width {
        bar.push(if cell < filled { '█' } else { '░' });
    }
    bar.push_str(&format!("] {done}/{total}"));
    bar
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn progress_bar_scales_and_handles_empty_plans() {
        assert_eq!(progress_bar(2, 5, 5), "[██░░░] 2/5");
        assert_eq!(progress_bar(0, 0, 4), "[░░░░] 0/0");
        assert_eq!(progress_bar(5, 5, 5), "[█████] 5/5");
    }

    #[test]
    fn progress_counts_completed_and_ignores_superseded_nodes() {
        let mut done = crate::testing::base_node("t-1");
        done.status = PlanNodeStatus::Completed;
        let mut gone = crate::testing::base_node("t-2");
        gone.status = PlanNodeStatus::Superseded;
        let mut running = crate::testing::base_node("t-3");
        running.status = PlanNodeStatus::InProgress;
        let open = crate::testing::base_node("t-4");
        let plan = Plan {
            id: PlanId::new("p-prog"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "Fortschritt".to_owned(),
            goal_id: None,
            nodes: vec![done, gone, running, open],
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            tenant: None,
        };
        assert_eq!(plan_progress(&plan), (1, 3));
        assert_eq!(
            current_step(&plan).map(|node| node.id.as_str()),
            Some("t-3"),
            "der laufende Schritt ist der aktuelle"
        );
        assert!(!has_blocked_step(&plan));
    }

    #[test]
    fn approval_defaults_to_confirmed_for_legacy_entries() -> TestResult {
        let meta: PlanMeta = serde_json::from_str("{}")?;
        assert_eq!(meta, PlanMeta::default());
        assert_eq!(meta.approval, PlanApproval::Confirmed);
        assert!(!meta.archived);
        Ok(())
    }

    #[test]
    fn index_roundtrip_and_invalid_active_id_is_ignored() -> TestResult {
        let mut index = PlanIndex {
            active: Some("p-1".to_owned()),
            ..PlanIndex::default()
        };
        index.plans.insert(
            "p-1".to_owned(),
            PlanMeta {
                archived: false,
                approval: PlanApproval::Proposed,
            },
        );
        let text = serde_json::to_string(&index)?;
        let back: PlanIndex = serde_json::from_str(&text)?;
        assert_eq!(back, index);
        assert_eq!(back.active_id(), Some(PlanId::new("p-1")));
        assert_eq!(
            back.meta(&PlanId::new("p-1")).approval,
            PlanApproval::Proposed
        );
        assert_eq!(back.meta(&PlanId::new("p-2")), PlanMeta::default());

        let broken: PlanIndex = serde_json::from_str(r#"{"active":"../etc"}"#)?;
        assert_eq!(broken.active_id(), None);
        assert_eq!(broken.version, PLAN_INDEX_VERSION);
        Ok(())
    }
}
