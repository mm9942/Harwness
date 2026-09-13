//! Goal-Typen für `harw-plan` — Desired State statt Plan.
//!
//! # Goal ≠ Plan (philosophy.md §5)
//! Ein Goal beschreibt, was am Ende **wahr sein muss**: Invarianten, Akzeptanzkriterien,
//! Constraints. Es ist kein langer Prompt und kein Plan — der Plan ist nur die aktuell
//! gewählte Strategie, um das Goal zu erreichen. Ein Context-Compact, ein Modellwechsel
//! oder ein gescheiterter Worker dürfen das Goal deshalb **niemals** reduzieren:
//! [`GoalPatch`] (für [`GoalAction::Refine`]) und [`GoalAction::Condense`] können
//! `statement` und `open_questions` verdichten, erreichen `acceptance_criteria` und
//! `invariants` aber gar nicht — diese können nur über [`GoalAction::AddCriterion`] /
//! [`GoalAction::AddInvariant`] wachsen, niemals schrumpfen. Das ist keine Konvention,
//! sondern durch die Typen selbst erzwungen.
//!
//! # Runtime besitzt Status, Modell schlägt vor
//! Ein Modell darf ein Goal per [`GoalAction::SetStatus`] vorschlagen, aber nur ein
//! menschlicher Akteur (`actor` beginnt nicht mit `"model:"`) darf es als `Achieved`
//! oder `Abandoned` erklären. [`validate_goal_action`] weist den Versuch eines
//! Modell-Akteurs mit [`PlanError::ActorNotAuthorized`] zurück — die Runtime (bzw.
//! der Mensch, der sie bedient) entscheidet über Zielerreichung, das Modell schlägt nur
//! vor.
//!
//! # Verantwortungsbereich
//! - Typisierte Kennung [`GoalId`] und Lebenszyklus-Status [`GoalStatus`].
//! - Domänentypen [`Invariant`], [`ConstraintKind`], [`Constraint`] und der
//!   Aggregat-Typ [`Goal`] selbst.
//! - Mutationsvokabular [`GoalAction`] / [`GoalPatch`] und das persistente Ergebnis
//!   [`GoalEvent`].
//! - Store-Schnittstelle [`GoalStore`].
//! - Reine Funktionen [`validate_goal_action`], [`apply_goal_action`],
//!   [`evaluate_goal`] und der Report-Typ [`GoalReport`].
//!
//! # Nicht in dieser Datei
//! Die persistenten Store-Implementierungen [`crate::goal_store::InMemoryGoalStore`]
//! und [`crate::goal_store::FileGoalStore`] liegen in `goal_store.rs`. Diese Datei
//! bleibt bewusst frei von Persistenz: [`validate_goal_action`] und
//! [`apply_goal_action`] sind reine Funktionen und dadurch ohne Dateisystem
//! testbar — die Stores rufen sie nur auf.
//!
//! # Concurrency
//! Alle Typen sind reine Werttypen (`Clone`, kein internes Mutex). [`GoalStore`] ist
//! `Send + Sync`; Implementierungen müssen selbst für Thread-Sicherheit sorgen
//! (analog zu `PlanStore`).
//!
//! # Fehler
//! [`validate_goal_action`] und [`GoalStore`] geben [`PlanError`] zurück, insbesondere
//! die goal-spezifischen Varianten `GoalNotFound`, `GoalTransitionReserved`,
//! `ActorNotAuthorized` und `InvalidId`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use time::OffsetDateTime;

use crate::error::{PlanError, PlanResult};
use crate::ids::{PlanId, RevisionId, TaskId};
use crate::types::{Criterion, EvidenceKind, EvidenceRef, Plan, PlanNodeStatus, VerificationStep};

/// Eindeutiger Bezeichner eines Goals.
///
/// # Description
/// Newtype über `String`, im Stil von [`crate::ids::PlanId`]. Formatierung ist nicht
/// erzwungen; empfohlen wird eine UUID v4 oder eine monotone Kurz-ID.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::goal::GoalId;
/// let id: GoalId = "goal-001".parse().unwrap();
/// assert_eq!(id.as_str(), "goal-001");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GoalId(String);

impl GoalId {
    /// Erstellt eine neue `GoalId` aus einem String.
    ///
    /// # Arguments
    /// - `s` (`impl Into<String>`): roher Bezeichner.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Gibt die innere String-Repräsentation zurück.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GoalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for GoalId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

/// Lebenszyklus-Status eines Goals.
///
/// # Description
/// Übergänge werden durch die Status-Matrix in [`validate_goal_action`] geprüft.
/// `Achieved`, `Abandoned` und `Superseded` sind terminal.
///
/// # Concurrency
/// `Copy` + `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// Entwurf — noch nicht aktiv verfolgt.
    Draft,
    /// Aktiv verfolgt.
    Active,
    /// Blockiert durch ein externes Hindernis.
    Blocked,
    /// Erreicht (terminal, nur durch einen menschlichen Akteur setzbar).
    Achieved,
    /// Aufgegeben (terminal, nur durch einen menschlichen Akteur setzbar).
    Abandoned,
    /// Durch ein neueres Goal ersetzt (terminal).
    Superseded,
}

/// Eine Invariante, die über die gesamte Lebensdauer des Goals gelten muss.
///
/// # Description
/// Im Unterschied zu [`Criterion`] beschreibt eine Invariante keinen einmaligen
/// Endzustand, sondern eine Bedingung, die während der gesamten Zielverfolgung nicht
/// verletzt werden darf (z. B. "keine Downtime", "Budget X nicht überschreiten").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invariant {
    /// Stabiler Bezeichner der Invariante (innerhalb des Goals eindeutig).
    pub id: String,
    /// Menschenlesbare Beschreibung der Invariante.
    pub statement: String,
    /// Verifikationsschritte, die belegen, dass die Invariante (weiterhin) gilt.
    pub verification: Vec<VerificationStep>,
}

/// Art eines Constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    /// Budget- bzw. Kostenrahmen.
    Budget,
    /// Scope-Grenze (was explizit nicht angefasst werden darf).
    Scope,
    /// Policy- bzw. Compliance-Vorgabe.
    Policy,
    /// Zeitliche Vorgabe (Deadline, Zeitfenster).
    Time,
    /// Abhängigkeit von einer externen Ressource oder einem anderen Team.
    Dependency,
}

/// Eine Rahmenbedingung, innerhalb derer das Goal verfolgt werden muss.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraint {
    /// Art der Rahmenbedingung.
    pub kind: ConstraintKind,
    /// Menschenlesbare Beschreibung der Rahmenbedingung.
    pub statement: String,
}

/// Ein Goal — Desired State, kein Plan (siehe Modulkopf).
///
/// # Description
/// `created_at`/`updated_at` und `revision` werden vom [`GoalStore`] gesetzt, nicht von
/// [`apply_goal_action`] (siehe dort).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    /// Eindeutiger Bezeichner des Goals.
    pub id: GoalId,
    /// Monoton steigende Revisionsnummer (wird vom Store gesetzt).
    pub revision: u64,
    /// Das Ziel-Statement — was am Ende wahr sein muss.
    pub statement: String,
    /// Explizit ausgeschlossene Ziele (verhindert Scope-Creep durch Auslegung).
    pub non_goals: Vec<String>,
    /// Invarianten, die während der gesamten Zielverfolgung gelten müssen.
    pub invariants: Vec<Invariant>,
    /// Akzeptanzkriterien, die vor `Achieved` erfüllt sein müssen.
    pub acceptance_criteria: Vec<Criterion>,
    /// Rahmenbedingungen (Budget, Scope, Policy, Zeit, Abhängigkeit).
    pub constraints: Vec<Constraint>,
    /// Offene, noch ungeklärte Fragen.
    pub open_questions: Vec<String>,
    /// Aktueller Lebenszyklus-Status.
    pub status: GoalStatus,
    /// Aktuell gebundener Plan (falls vorhanden).
    pub plan_id: Option<PlanId>,
    /// Revision des gebundenen Plans (falls vorhanden).
    pub plan_revision: Option<RevisionId>,
    /// Direkt am Goal angehängte Evidenz (zusätzlich zur Evidenz der Plan-Knoten).
    pub evidence: Vec<EvidenceRef>,
    /// Zeitpunkt der Erstellung (wird vom Store gesetzt).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Zeitpunkt der letzten Aktualisierung (wird vom Store gesetzt).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Teilaktualisierung eines Goals für [`GoalAction::Refine`].
///
/// # Description
/// `acceptance_criteria` und `invariants` sind bewusst **nicht** Teil dieses Typs:
/// Kriterien und Invarianten können nie über `Refine` entfernt oder ersetzt werden,
/// sondern nur über [`GoalAction::AddCriterion`] / [`GoalAction::AddInvariant`]
/// wachsen. Das erzwingt die "Goal darf nicht schrumpfen"-Regel auf Typebene.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoalPatch {
    /// Neues Ziel-Statement (falls gesetzt).
    pub statement: Option<String>,
    /// Neue Liste ausgeschlossener Ziele (falls gesetzt, ersetzt die alte Liste).
    pub non_goals: Option<Vec<String>>,
    /// Neue Liste von Rahmenbedingungen (falls gesetzt, ersetzt die alte Liste).
    pub constraints: Option<Vec<Constraint>>,
    /// Neue Liste offener Fragen (falls gesetzt, ersetzt die alte Liste).
    pub open_questions: Option<Vec<String>>,
}

/// Eine Goal-Aktion — die einzige Art, ein Goal zu mutieren.
///
/// # Description
/// Reine Werttypen, analog zu [`crate::actions::PlanAction`]. Serialisiert mit
/// `#[serde(tag = "op", rename_all = "snake_case")]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GoalAction {
    /// Legt ein neues Goal an bzw. ersetzt es vollständig.
    Set {
        /// Das anzulegende Goal.
        goal: Goal,
    },
    /// Verfeinert Statement/Non-Goals/Constraints/offene Fragen (siehe [`GoalPatch`]).
    Refine {
        /// Die anzuwendende Teilaktualisierung.
        patch: GoalPatch,
    },
    /// Fügt ein Akzeptanzkriterium hinzu (kann nie entfernt werden).
    AddCriterion {
        /// Das hinzuzufügende Kriterium.
        criterion: Criterion,
    },
    /// Fügt eine Invariante hinzu (kann nie entfernt werden).
    AddInvariant {
        /// Die hinzuzufügende Invariante.
        invariant: Invariant,
    },
    /// Hängt einen Evidenz-Nachweis direkt an das Goal an.
    AttachEvidence {
        /// Anzuhängender Nachweis.
        evidence: EvidenceRef,
    },
    /// Bindet das Goal an einen konkreten Plan (Revision).
    BindPlan {
        /// Bezeichner des zu bindenden Plans.
        plan_id: PlanId,
        /// Revision des Plans zum Bindungszeitpunkt.
        revision: RevisionId,
    },
    /// Setzt den Status des Goals.
    SetStatus {
        /// Neuer Status.
        status: GoalStatus,
        /// Optionaler Grund für den Statuswechsel.
        reason: Option<String>,
    },
    /// Verdichtet `statement`/`open_questions`; Kriterien und Invarianten bleiben
    /// unverändert (durch Typ garantiert, siehe Modulkopf).
    Condense {
        /// Die verdichtete Zusammenfassung, die `statement` ersetzt.
        summary: String,
    },
    /// Lese-Aktion: gibt das aktuelle Goal zurück, ohne es zu mutieren.
    Inspect,
}

/// Das persistente Ergebnis einer angewendeten Goal-Aktion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalEvent {
    /// Revision nach Anwendung der Aktion.
    pub revision: u64,
    /// Die angewendete Aktion.
    pub action: GoalAction,
    /// Akteur, der die Aktion ausgelöst hat (runtime-gesetzt).
    pub actor: String,
    /// Zeitpunkt der Anwendung (runtime-gesetzt).
    #[serde(with = "time::serde::rfc3339")]
    pub applied_at: OffsetDateTime,
}

/// Einheitliches Interface für Goal-Stores.
///
/// # Description
/// Alle Mutationen laufen über `apply`, welches intern [`validate_goal_action`] und
/// [`apply_goal_action`] nutzen sollte. Implementierungen setzen `updated_at` und
/// `revision` selbst (siehe [`apply_goal_action`]).
///
/// # Concurrency
/// `Send + Sync` — Implementierungen müssen thread-sicher sein.
pub trait GoalStore: Send + Sync {
    /// Gibt das aktuelle Goal zurück.
    ///
    /// # Errors
    /// - [`PlanError::GoalNotFound`] wenn noch kein Goal per `Set` angelegt wurde.
    fn current(&self) -> PlanResult<Goal>;

    /// Gibt die aktuelle Revisionsnummer zurück.
    fn revision(&self) -> u64;

    /// Wendet eine Aktion auf das Goal an und gibt das resultierende Event zurück.
    ///
    /// # Arguments
    /// - `action` (`GoalAction`): anzuwendende Aktion.
    /// - `actor` (`&str`): Bezeichner des Akteurs (z. B. `"human:mia"`,
    ///   `"model:gpt"`).
    ///
    /// # Errors
    /// - Alle [`PlanError`]-Varianten aus [`validate_goal_action`], insbesondere
    ///   [`PlanError::GoalTransitionReserved`] und [`PlanError::InvalidId`].
    /// - [`PlanError::ActorNotAuthorized`] falls die Implementierung zusätzliche,
    ///   store-spezifische Rechteprüfungen jenseits der reinen Statuslogik durchführt.
    /// - [`PlanError::Io`] / [`PlanError::Serde`] bei Persistenzfehlern.
    fn apply(&self, action: GoalAction, actor: &str) -> PlanResult<GoalEvent>;

    /// Gibt die Event-History zurück.
    ///
    /// # Arguments
    /// - `since` (`Option<u64>`): filtert Events nach Revision (inklusiv). `None`
    ///   gibt alle Events zurück.
    fn history(&self, since: Option<u64>) -> PlanResult<Vec<GoalEvent>>;
}

/// Auswertung eines Goals gegen einen Plan.
#[derive(Debug, Clone)]
pub struct GoalReport {
    /// Indizes (in `Goal::acceptance_criteria`) der erfüllten Kriterien.
    pub criteria_met: Vec<usize>,
    /// Indizes (in `Goal::acceptance_criteria`) der noch offenen Kriterien.
    pub criteria_open: Vec<usize>,
    /// IDs der Invarianten, die (noch) nicht durch Evidenz belegt sind.
    pub invariants_violated: Vec<String>,
    /// Anteil erfüllter Kriterien an allen Kriterien (`1.0` wenn keine definiert sind).
    pub coverage: f32,
    /// Ready/Blocked-Knoten, deren Abschluss offene Kriterien belegen könnte.
    pub blocking_nodes: Vec<TaskId>,
    /// Menschenlesbare Vorschläge für die nächsten Schritte.
    pub next_actions: Vec<String>,
}

/// Legale Statusübergänge für [`GoalStatus`]: (Von, Nach).
///
/// Terminal-Zustände (`Achieved`, `Abandoned`, `Superseded`) haben keine ausgehenden
/// Kanten.
const GOAL_STATUS_MATRIX: &[(GoalStatus, GoalStatus)] = &[
    (GoalStatus::Draft, GoalStatus::Active),
    (GoalStatus::Draft, GoalStatus::Abandoned),
    (GoalStatus::Active, GoalStatus::Blocked),
    (GoalStatus::Active, GoalStatus::Achieved),
    (GoalStatus::Active, GoalStatus::Abandoned),
    (GoalStatus::Active, GoalStatus::Superseded),
    (GoalStatus::Blocked, GoalStatus::Active),
    (GoalStatus::Blocked, GoalStatus::Abandoned),
    (GoalStatus::Blocked, GoalStatus::Superseded),
];

/// Prüft, ob ein Goal-Statusübergang laut Matrix legal ist.
fn is_legal_goal_transition(from: GoalStatus, to: GoalStatus) -> bool {
    GOAL_STATUS_MATRIX.iter().any(|&(f, t)| f == from && t == to)
}

/// Prüft einen `SetStatus`-Übergang: Matrix, Kriterien-Voraussetzung, Actor-Policy.
///
/// Reihenfolge (erste verletzte Regel gewinnt): FSM-Matrix → Kriterien-Voraussetzung
/// für `Achieved` → Actor-Policy für `Achieved`/`Abandoned`.
fn validate_status_transition(
    from: GoalStatus,
    to: GoalStatus,
    criteria_count: usize,
    actor: &str,
) -> PlanResult<()> {
    if !is_legal_goal_transition(from, to) {
        // `PlanError::IllegalTransition` passt hier nicht: ihr `id`-Feld ist
        // `TaskId`, ein Goal hat aber nur eine `GoalId` — beide Newtypes sind
        // nicht austauschbar, und diese Funktion kennt ohnehin keine `TaskId`.
        // `error.rs` gilt als Wahrheit und wird nicht geändert, daher weichen
        // wir auf `GoalTransitionReserved { status }` aus und packen den sonst
        // verlorenen Kontext (Ausgangsstatus, Grund) direkt in den
        // `status`-String.
        return Err(PlanError::GoalTransitionReserved {
            status: format!(
                "{to:?} (Übergang von {from:?} ist in der Goal-Status-Matrix nicht vorgesehen)"
            ),
        });
    }

    if to == GoalStatus::Achieved && criteria_count == 0 {
        // Für Goals existiert keine zu `EvidenceMissing` analoge Variante;
        // auch hier ist `GoalTransitionReserved { status }` die einzig
        // passende vorhandene Variante (siehe Begründung oben).
        return Err(PlanError::GoalTransitionReserved {
            status: format!("{to:?} (Achieved verlangt mindestens ein Akzeptanzkriterium)"),
        });
    }

    if matches!(to, GoalStatus::Achieved | GoalStatus::Abandoned) && actor.starts_with("model:") {
        // Das ist exakt der Zweck von `ActorNotAuthorized`: ein Modell-Akteur
        // versucht eine Owner-reservierte Aktion (Runtime besitzt Status,
        // Modell schlägt nur vor).
        return Err(PlanError::ActorNotAuthorized {
            action: format!("Goal::SetStatus({to:?})"),
            actor: actor.to_owned(),
        });
    }

    Ok(())
}

/// Validiert eine Goal-Aktion (rein).
///
/// # Description
/// Regeln:
/// - Status-Matrix: `Draft`→`Active`|`Abandoned`; `Active`→`Blocked`|`Achieved`|
///   `Abandoned`|`Superseded`; `Blocked`→`Active`|`Abandoned`|`Superseded`;
///   `Achieved`/`Abandoned`/`Superseded` terminal.
/// - `Achieved` verlangt mindestens ein Akzeptanzkriterium (die vollständige
///   "alle Kriterien belegt"-Prüfung erfordert einen [`GoalReport`] und liegt beim
///   Aufrufer, siehe [`evaluate_goal`]).
/// - `Achieved`/`Abandoned` sind nur mit einem `actor` erlaubt, der nicht mit
///   `"model:"` beginnt — sonst [`PlanError::ActorNotAuthorized`].
/// - `Refine`/`Condense` können `acceptance_criteria`/`invariants` gar nicht
///   berühren (durch die Typen [`GoalPatch`] und [`GoalAction::Condense`]
///   garantiert).
/// - Eine leere `statement` bei `Set` → [`PlanError::InvalidId`] mit
///   `field = "statement"`.
/// - Jede Aktion außer `Set` verlangt ein bereits existierendes Goal, sonst
///   [`PlanError::GoalNotFound`].
///
/// # Arguments
/// - `goal` (`Option<&Goal>`): aktuelles Goal, `None` wenn noch keins angelegt wurde.
/// - `action` (`&GoalAction`): zu prüfende Aktion.
/// - `actor` (`&str`): Akteur, der die Aktion auslöst.
///
/// # Returns
/// `Ok(())` wenn die Aktion gültig ist.
///
/// # Errors
/// - [`PlanError::GoalNotFound`], [`PlanError::GoalTransitionReserved`],
///   [`PlanError::ActorNotAuthorized`], [`PlanError::InvalidId`] — siehe Regeln oben.
pub fn validate_goal_action(goal: Option<&Goal>, action: &GoalAction, actor: &str) -> PlanResult<()> {
    match action {
        GoalAction::Set { goal: new_goal } => {
            if new_goal.statement.trim().is_empty() {
                return Err(PlanError::InvalidId {
                    field: "statement",
                    value: new_goal.statement.to_owned(),
                });
            }
            Ok(())
        }

        GoalAction::SetStatus { status, .. } => {
            let current = goal.ok_or(PlanError::GoalNotFound)?;
            validate_status_transition(
                current.status,
                *status,
                current.acceptance_criteria.len(),
                actor,
            )
        }

        GoalAction::Refine { .. }
        | GoalAction::AddCriterion { .. }
        | GoalAction::AddInvariant { .. }
        | GoalAction::AttachEvidence { .. }
        | GoalAction::BindPlan { .. }
        | GoalAction::Condense { .. }
        | GoalAction::Inspect => {
            goal.ok_or(PlanError::GoalNotFound)?;
            Ok(())
        }
    }
}

/// Wendet eine validierte Aktion an (rein).
///
/// # Description
/// Setzt `updated_at` und `revision` **nicht** — das macht der [`GoalStore`], der
/// diese Felder nach erfolgreicher Validierung runtime-seitig zuweist. `now` wird
/// bewusst entgegengenommen (Symmetrie zur Store-Signatur, Erweiterbarkeit für
/// künftige Aktionen mit eigenem Zeitstempelbedarf), aber hier nicht auf `Goal`
/// geschrieben.
///
/// # Arguments
/// - `goal` (`&mut Goal`): zu mutierendes Goal (muss bereits validiert sein).
/// - `action` (`&GoalAction`): anzuwendende Aktion.
/// - `now` (`OffsetDateTime`): aktueller Zeitpunkt (vom Store durchgereicht, hier
///   ungenutzt — siehe oben).
pub fn apply_goal_action(goal: &mut Goal, action: &GoalAction, now: OffsetDateTime) {
    // Zeitstempel- und Revisionshoheit liegt beim Store (Runtime besitzt Status).
    let _ = now;

    match action {
        GoalAction::Set { goal: new_goal } => {
            *goal = new_goal.clone();
        }
        GoalAction::Refine { patch } => {
            if let Some(statement) = &patch.statement {
                goal.statement = statement.clone();
            }
            if let Some(non_goals) = &patch.non_goals {
                goal.non_goals = non_goals.clone();
            }
            if let Some(constraints) = &patch.constraints {
                goal.constraints = constraints.clone();
            }
            if let Some(open_questions) = &patch.open_questions {
                goal.open_questions = open_questions.clone();
            }
        }
        GoalAction::AddCriterion { criterion } => {
            goal.acceptance_criteria.push(criterion.clone());
        }
        GoalAction::AddInvariant { invariant } => {
            goal.invariants.push(invariant.clone());
        }
        GoalAction::AttachEvidence { evidence } => {
            goal.evidence.push(evidence.clone());
        }
        GoalAction::BindPlan { plan_id, revision } => {
            goal.plan_id = Some(plan_id.clone());
            goal.plan_revision = Some(*revision);
        }
        GoalAction::SetStatus { status, .. } => {
            goal.status = *status;
        }
        GoalAction::Condense { summary } => {
            goal.statement = summary.clone();
            goal.open_questions.clear();
        }
        GoalAction::Inspect => {}
    }
}

/// Prüft, ob ein einzelner Verifikationsschritt durch eine der übergebenen
/// Evidenz-Einträge belegt wird.
///
/// Zuordnung (Design-Doc §5): `Command` → Evidenz mit `kind` [`EvidenceKind::CargoTest`]
/// oder [`EvidenceKind::Clippy`] und `locator == cmd`; `Artifact` → Evidenz mit `kind`
/// [`EvidenceKind::Diff`] oder [`EvidenceKind::Job`] und `locator == path`;
/// `TraceEvent` → Evidenz mit `kind == `[`EvidenceKind::TraceSpan`] und
/// `locator == name`; `Manual` → Evidenz mit `kind == `[`EvidenceKind::Manual`] und
/// `locator == note`. `EvidenceKind` ist ein Enum (kein `String`), daher wird direkt
/// auf die Varianten gematcht statt auf String-Literale verglichen.
fn verification_satisfied_by(step: &VerificationStep, evidence: &[&EvidenceRef]) -> bool {
    match step {
        VerificationStep::Command { cmd, .. } => evidence.iter().any(|e| {
            matches!(e.kind, EvidenceKind::CargoTest | EvidenceKind::Clippy) && &e.locator == cmd
        }),
        VerificationStep::Artifact { path } => evidence
            .iter()
            .any(|e| matches!(e.kind, EvidenceKind::Diff | EvidenceKind::Job) && &e.locator == path),
        VerificationStep::TraceEvent { name } => evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::TraceSpan && &e.locator == name),
        VerificationStep::Manual { note } => evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::Manual && &e.locator == note),
    }
}

/// Prüft, ob **alle** Verifikationsschritte durch die übergebene Evidenz belegt sind.
///
/// Eine leere Verifikationsliste gilt als nicht belegt (es gibt nichts, das die
/// Erfüllung nachweisen könnte) — sicherer Default statt stillschweigendem "erfüllt".
fn all_verification_satisfied(steps: &[VerificationStep], evidence: &[&EvidenceRef]) -> bool {
    !steps.is_empty() && steps.iter().all(|s| verification_satisfied_by(s, evidence))
}

/// Bewertet das Goal gegen einen Plan (rein).
///
/// # Description
/// Ein Kriterium gilt als erfüllt, wenn all seine `verification`-Schritte durch
/// Evidenz eines `Completed`-Knotens belegt sind (siehe
/// [`all_verification_satisfied`]). Invarianten werden identisch geprüft;
/// `invariants_violated` enthält die IDs der (noch) nicht belegten Invarianten.
/// `blocking_nodes` sammelt alle `Ready`/`Blocked`-Knoten, solange mindestens ein
/// Kriterium offen ist — sie sind Kandidaten, deren Abschluss die Coverage erhöhen
/// könnte; eine engere Kausalaussage trifft diese reine Funktion bewusst nicht.
///
/// # Arguments
/// - `goal` (`&Goal`): zu bewertendes Goal.
/// - `plan` (`&Plan`): Plan, dessen Knoten-Evidenz gegen die Kriterien gematcht wird.
///
/// # Returns
/// [`GoalReport`] mit erfüllten/offenen Kriterien, verletzten Invarianten,
/// `coverage` (`criteria_met.len() / acceptance_criteria.len()`, `1.0` wenn keine
/// Kriterien definiert sind), blockierenden Knoten und Vorschlägen für nächste
/// Schritte.
pub fn evaluate_goal(goal: &Goal, plan: &Plan) -> GoalReport {
    let completed_evidence: Vec<&EvidenceRef> = plan
        .nodes
        .iter()
        .filter(|n| n.status == PlanNodeStatus::Completed)
        .flat_map(|n| n.evidence.iter())
        .collect();

    let mut criteria_met = Vec::new();
    let mut criteria_open = Vec::new();
    for (idx, criterion) in goal.acceptance_criteria.iter().enumerate() {
        if all_verification_satisfied(&criterion.verification, &completed_evidence) {
            criteria_met.push(idx);
        } else {
            criteria_open.push(idx);
        }
    }

    let invariants_violated: Vec<String> = goal
        .invariants
        .iter()
        .filter(|inv| !all_verification_satisfied(&inv.verification, &completed_evidence))
        .map(|inv| inv.id.clone())
        .collect();

    let coverage = if goal.acceptance_criteria.is_empty() {
        1.0
    } else {
        criteria_met.len() as f32 / goal.acceptance_criteria.len() as f32
    };

    let blocking_nodes: Vec<TaskId> = if criteria_open.is_empty() {
        Vec::new()
    } else {
        plan.nodes
            .iter()
            .filter(|n| matches!(n.status, PlanNodeStatus::Ready | PlanNodeStatus::Blocked))
            .map(|n| n.id.clone())
            .collect()
    };

    let mut next_actions = Vec::new();
    for &idx in &criteria_open {
        next_actions.push(format!(
            "Kriterium {idx} ('{}') ist noch offen.",
            goal.acceptance_criteria[idx].description
        ));
    }
    for name in &invariants_violated {
        next_actions.push(format!("Invariante '{name}' ist (noch) nicht belegt."));
    }

    GoalReport {
        criteria_met,
        criteria_open,
        invariants_violated,
        coverage,
        blocking_nodes,
        next_actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::PathOrSymbol;
    use crate::types::{PlanNode, PlanNodeKind};

    /// Baut ein minimales Goal mit genau einem Akzeptanzkriterium (ohne
    /// Verifikationsschritte) — ausreichend für Matrix-/Actor-Tests, die nicht auf
    /// `evaluate_goal` abzielen.
    fn make_goal(status: GoalStatus) -> Goal {
        Goal {
            id: GoalId::new("g-test"),
            revision: 1,
            statement: "Testziel".to_owned(),
            non_goals: vec![],
            invariants: vec![],
            acceptance_criteria: vec![Criterion {
                description: "mindestens ein Kriterium".to_owned(),
                verification: vec![],
            }],
            constraints: vec![],
            open_questions: vec![],
            status,
            plan_id: None,
            plan_revision: None,
            evidence: vec![],
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn make_node(id: &str, status: PlanNodeStatus) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "obj".to_owned(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: vec![PathOrSymbol::new(format!("src/{id}.rs"))],
            forbidden_scope: vec![],
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status,
            evidence: vec![],
            kind: PlanNodeKind::default(),
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn make_plan(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            id: PlanId::new("p-goal-test"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "Test".to_owned(),
            goal_id: None,
            nodes,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_goal_id_display_and_from_str_roundtrip() {
        let id: GoalId = "goal-1".parse().unwrap();
        assert_eq!(id.as_str(), "goal-1");
        assert_eq!(id.to_string(), "goal-1");
    }

    #[test]
    fn test_goal_status_snake_case_serialization() {
        let cases = [
            (GoalStatus::Draft, "\"draft\""),
            (GoalStatus::Active, "\"active\""),
            (GoalStatus::Blocked, "\"blocked\""),
            (GoalStatus::Achieved, "\"achieved\""),
            (GoalStatus::Abandoned, "\"abandoned\""),
            (GoalStatus::Superseded, "\"superseded\""),
        ];
        for (status, expected) in cases {
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(json, expected, "Status {:?} serialisiert nicht korrekt", status);
        }
    }

    #[test]
    fn test_goal_action_op_tag_serialization() {
        let json = serde_json::to_string(&GoalAction::Inspect).unwrap();
        assert!(json.contains("\"op\":\"inspect\""), "op-Tag fehlt: {json}");
    }

    #[test]
    fn test_validate_goal_action_requires_existing_goal_for_non_set_actions() {
        assert!(matches!(
            validate_goal_action(None, &GoalAction::Inspect, "human:tester"),
            Err(PlanError::GoalNotFound)
        ));
    }

    #[test]
    fn test_validate_goal_action_set_rejects_empty_statement() {
        let mut goal = make_goal(GoalStatus::Draft);
        goal.statement = "   ".to_owned();
        let action = GoalAction::Set { goal };

        match validate_goal_action(None, &action, "human:tester") {
            Err(PlanError::InvalidId { field, value }) => {
                assert_eq!(field, "statement");
                assert_eq!(value, "   ");
            }
            other => panic!("Erwartet InvalidId{{field: \"statement\", ..}}, bekam {other:?}"),
        }
    }

    #[test]
    fn test_validate_goal_action_status_matrix_legal_and_illegal() {
        let legal_pairs = [
            (GoalStatus::Draft, GoalStatus::Active),
            (GoalStatus::Draft, GoalStatus::Abandoned),
            (GoalStatus::Active, GoalStatus::Blocked),
            (GoalStatus::Active, GoalStatus::Achieved),
            (GoalStatus::Active, GoalStatus::Abandoned),
            (GoalStatus::Active, GoalStatus::Superseded),
            (GoalStatus::Blocked, GoalStatus::Active),
            (GoalStatus::Blocked, GoalStatus::Abandoned),
            (GoalStatus::Blocked, GoalStatus::Superseded),
        ];
        let all_statuses = [
            GoalStatus::Draft,
            GoalStatus::Active,
            GoalStatus::Blocked,
            GoalStatus::Achieved,
            GoalStatus::Abandoned,
            GoalStatus::Superseded,
        ];

        for &from in &all_statuses {
            for &to in &all_statuses {
                let goal = make_goal(from);
                let action = GoalAction::SetStatus {
                    status: to,
                    reason: None,
                };
                // actor bewusst menschlich, um den Actor-Policy-Pfad hier nicht zu treffen.
                let result = validate_goal_action(Some(&goal), &action, "human:tester");
                let expected_legal = legal_pairs.contains(&(from, to));

                if expected_legal {
                    assert!(result.is_ok(), "{from:?} -> {to:?} sollte legal sein, war {result:?}");
                } else {
                    assert!(
                        matches!(result, Err(PlanError::GoalTransitionReserved { .. })),
                        "{from:?} -> {to:?} sollte illegal sein, war {result:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_validate_goal_action_achieved_reserved_for_model_actor_ok_for_human() {
        let goal = make_goal(GoalStatus::Active);
        let action = GoalAction::SetStatus {
            status: GoalStatus::Achieved,
            reason: None,
        };

        // Ein Modell-Akteur darf ein Goal nicht auf `Achieved` setzen — das bleibt
        // die Testabsicht. Die Fehlervariante ist `ActorNotAuthorized`, weil das
        // exakt ihr Zweck ist (Owner-reservierte Aktion durch nicht-menschlichen
        // Akteur), nicht `GoalTransitionReserved` (die für die Matrix- und
        // Kriterien-Prüfung reserviert bleibt, siehe `validate_status_transition`).
        assert!(matches!(
            validate_goal_action(Some(&goal), &action, "model:gpt"),
            Err(PlanError::ActorNotAuthorized { .. })
        ));
        assert!(validate_goal_action(Some(&goal), &action, "human:mia").is_ok());
    }

    #[test]
    fn test_apply_goal_action_refine_preserves_criteria_and_invariants() {
        let mut goal = make_goal(GoalStatus::Active);
        goal.invariants.push(Invariant {
            id: "inv-1".to_owned(),
            statement: "keine Downtime".to_owned(),
            verification: vec![],
        });
        let criteria_before = goal.acceptance_criteria.len();
        let invariants_before = goal.invariants.len();

        let patch = GoalPatch {
            statement: Some("neuer Wortlaut".to_owned()),
            ..GoalPatch::default()
        };
        apply_goal_action(
            &mut goal,
            &GoalAction::Refine { patch },
            OffsetDateTime::UNIX_EPOCH,
        );

        assert_eq!(goal.statement, "neuer Wortlaut");
        assert_eq!(goal.acceptance_criteria.len(), criteria_before);
        assert_eq!(goal.invariants.len(), invariants_before);
    }

    #[test]
    fn test_evaluate_goal_coverage_half_when_one_of_two_criteria_met() {
        let mut goal = make_goal(GoalStatus::Active);
        goal.acceptance_criteria = vec![
            Criterion {
                description: "Tests grün".to_owned(),
                verification: vec![VerificationStep::Command {
                    cmd: "cargo test".to_owned(),
                    expect_exit: 0,
                }],
            },
            Criterion {
                description: "Doku vollständig".to_owned(),
                verification: vec![VerificationStep::Manual {
                    note: "Review durch Mia".to_owned(),
                }],
            },
        ];

        let mut node = make_node("t1", PlanNodeStatus::Completed);
        node.evidence = vec![EvidenceRef {
            kind: EvidenceKind::CargoTest,
            locator: "cargo test".to_owned(),
            attached_at: OffsetDateTime::UNIX_EPOCH,
            actor: "ci".to_owned(),
            digest: None,
        }];
        let plan = make_plan(vec![node]);

        let report = evaluate_goal(&goal, &plan);

        assert_eq!(report.criteria_met, vec![0]);
        assert_eq!(report.criteria_open, vec![1]);
        assert!(
            (report.coverage - 0.5).abs() < f32::EPSILON,
            "Erwartet coverage 0.5, war {}",
            report.coverage
        );
    }
}
