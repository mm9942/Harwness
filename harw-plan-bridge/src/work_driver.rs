//! Der Arbeitstreiber: aus Ziel, Bericht und Worker-Zuständen wird die nächste
//! Welle.
//!
//! # Verantwortungsbereich
//! [`WorkDriver::decide`] ist der reine Entscheidungskern eines Supervisors,
//! der Worker-Agenten auf ein [`Goal`] hin treibt. Er liest einen Snapshot
//! ([`WorkDriveInput`]: Goal, [`GoalReport`], Iteration, Worker, Budget,
//! Grenzen, Verifikations- und Bewerterstand, `now`) und liefert einen
//! [`WorkDrivePlan`] — eine **geordnete** Liste von [`WorkDriveStep`]s plus
//! Begründungszeilen. Ausführen (Worker starten, Nachrichten schicken, bauen,
//! den Bewerter fragen) ist Aufgabe des Aufrufers.
//!
//! # Die Arbeitsweise, die hier kodiert ist
//! Der Treiber bildet eine bewährte Arbeitsweise mit parallelen Agenten ab:
//!
//! - **Kleine, isolierte Scopes parallel delegieren.** Jedes offene
//!   Kriterium, das kein Worker abdeckt, bekommt einen
//!   [`WorkDriveStep::Delegate`] — höchstens so viele, wie
//!   [`WorkDriveLimits::max_parallel_workers`] freie Plätze lässt. Scopes
//!   besitzen disjunkte Pfade: überschneidet ein neuer Scope einen laufenden
//!   Worker, wird er zurückgestellt; überschneiden sich zwei neue Scopes
//!   derselben Runde, werden sie zu einem verschmolzen. Pfade kommen aus
//!   Scope-Hinweisen ([`scope_hints_from_plan`]), sonst aus den
//!   `Artifact`-Schritten des Kriteriums, sonst ist es der Workspace-Scope
//!   ([`WORKSPACE_SCOPE`]).
//! - **Wer ein Kriterium beansprucht, bekommt sein Feedback.** Ein Worker
//!   deckt die Kriterien seines Scopes plus die ab, die er in
//!   `work_driver.report` als bearbeitet meldet
//!   ([`WorkerResultSummary::criteria_addressed`]); für sie wird kein zweiter
//!   Worker delegiert, und ihre offenen Punkte gehen an ihn.
//! - **Denselben Worker weiterführen statt neu starten.**
//!   [`WorkDriveStep::Continue`] schickt knappes Feedback (nur die offenen
//!   Kriterien seines Scopes und die fehlschlagende Evidenz) an *denselben*
//!   Worker — sein Prompt-Cache bleibt warm, das Feedback wird nur angehängt.
//!   [`WorkDriveStep::Respawn`] ist die Ausnahme: nach ausgeschöpften
//!   Versuchen, bei zu großem Kontext oder nach einem harten Fehlschlag; der
//!   frische Worker bekommt eine kompakte Übergabe statt des alten Verlaufs.
//! - **Zentral und selten verifizieren.** [`WorkDriveStep::Verify`] erscheint
//!   genau dann, wenn *alle* Worker der Welle `Done` oder `Blocked` gemeldet
//!   haben — nie pro Worker. Delegierte Worker bauen nicht selbst (ihre
//!   Aufgabe sagt es ausdrücklich); eine einzige Verifikation über den
//!   vollständigen, kombinierten Stand ist die einzige, die etwas bedeutet.
//! - **Auf verifizierte Ergebnisse hin treiben.** Scheitert die
//!   Verifikation, gehen die fehlschlagenden Zeilen an die zuständigen Worker
//!   zurück (per Pfad-Erwähnung zugeordnet; nicht zuordenbare an alle).
//!   Kriterien, die sich aus Evidenz allein nicht entscheiden lassen (ohne
//!   Verifikationsschritt oder mit manuellem Schritt), gehen an den Bewerter
//!   ([`WorkDriveStep::Judge`]); sein `missing` wird wieder Feedback.
//!
//! # Reinheit
//! Keine I/O, kein Zufall, keine Systemzeit — `now` wird injiziert, und die
//! Wandzeit ergibt sich allein aus `now - usage.started_at`. Worker werden vor
//! der Auswertung nach `worker_id` sortiert, Kriterien nach Index, Scope-
//! Hinweise nach `id`: dieselbe Eingabe — auch in anderer Worker-Reihenfolge —
//! liefert denselben Plan.
//!
//! # Wer darf was
//! Wie [`crate::PlanController`] **schlägt** der Treiber nur vor. Er setzt nie
//! einen Goal-Status: [`WorkDriveStep::ProposeAchieved`] ist ein Vorschlag an
//! einen menschlichen Akteur, denn nur ein solcher darf ein Goal auf
//! `Achieved` setzen (`harw_plan::goal::validate_goal_action`).
//!
//! # Was der Aufrufer zwischen zwei Runden pflegt
//! Der Treiber ist zustandslos; der Aufrufer trägt den Zustand:
//!
//! - Nach `Delegate`/`Continue`/`Respawn` steht `last_result` des
//!   betroffenen Workers auf `None` (läuft); `attempts` zählt die
//!   Fortsetzungskette und wird bei `Respawn` auf `0` zurückgesetzt.
//! - [`VerificationState`] und `judge` beschreiben den **aktuellen**
//!   Wellenstand: sobald ein Worker neue Arbeit bekommt, setzt der Aufrufer
//!   beide zurück (`NotRun` / `None`). Nach `Verify` wertet er das Goal mit der
//!   neuen Evidenz neu aus (`harw_plan::goal::evaluate_goal`).
//! - `usage.iterations_without_progress` zählt Runden ohne Fortschritt —
//!   weder ein neu erfülltes Kriterium noch weniger fehlschlagende
//!   Verifikationsschritte als bisher bestenfalls; der Treiber vergleicht nur
//!   mit [`WorkDriveLimits::stall_iterations`].
//!
//! # Reihenfolge der Schritte
//! `ProposeAchieved` oder `GiveUp` stehen jeweils allein. Sonst:
//! `Escalate` → `Respawn`/`Continue` (je Worker, nach `worker_id`) →
//! `Delegate` (nach Kriterium) → `Verify` bzw. `Judge`.
//!
//! # Concurrency
//! [`WorkDriver`] ist ein zustandsloser Namensraum (`Send + Sync`); alle
//! übrigen Typen sind reine Werttypen.
//!
//! # Provider-Grenzen
//! [`WorkDriveLimits::max_parallel_workers`] ist die von der Spec erlaubte
//! Obergrenze; der Job-Worker, der `decide` aufruft, kennt zusätzlich die
//! Provider-Grenzen (`max_concurrency`, `rate_limit.max_concurrent`,
//! TPM-Taktung, Halbierung nach HTTP 429) und meldet sie über
//! [`WorkDriveInput::effective_parallel`]. `decide` bildet stets das Minimum
//! aus beiden und weitet die Provider-Grenze nie auf: sowohl ein neuer
//! `Delegate` als auch eine Bewerter-Nachfolge-Delegation zählen laufende
//! Worker mit; Scopes über der Grenze bleiben wie am Spec-Limit für spätere
//! Runden zurückgestellt.
//!
//! # Examples
//! ```rust,no_run
//! use harw_plan_bridge::{WorkDriveInput, WorkDriveStep, WorkDriver};
//!
//! # fn demo(input: WorkDriveInput<'_>) {
//! let plan = WorkDriver::decide(&input);
//! for step in &plan.steps {
//!     if let WorkDriveStep::Verify { workers } = step {
//!         println!("eine zentrale Verifikation über {} Worker", workers.len());
//!     }
//! }
//! # }
//! ```

use std::collections::BTreeSet;

use harw_plan::goal::{Goal, GoalReport, GoalStatus};
use harw_plan::ids::PathOrSymbol;
use harw_plan::types::{Criterion, Plan, VerificationStep};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

/// Rolle, unter der ein delegierter Worker startet, wenn die Spec
/// (`[work_driver] worker_role` der IR, [`WorkDriveInput::worker_role`])
/// keine nennt.
pub const DEFAULT_WORKER_ROLE: &str = "implementer";

/// Scope-ID des Workers, der Bewerter-Lücken ohne zuständigen Worker übernimmt.
pub const JUDGE_FOLLOWUP_SCOPE: &str = "judge-followup";

/// `owned_paths`-Eintrag „der Workspace": der Worker darf jede Datei ändern,
/// die kein anderer Worker besitzt.
///
/// # Description
/// Ein Kriterium, das weder ein Scope-Hinweis noch ein `Artifact`-Schritt an
/// Pfade bindet (etwa eines, das nur ein `Command` nachweist), bekäme sonst
/// keinen Schreibbereich und damit einen Worker, der nichts ändern darf. Der
/// Eintrag überschneidet in [`WorkDriver::decide`] keinen anderen Scope (auch
/// keinen zweiten Workspace-Scope); die Trennung leistet der Aufrufer bei der
/// Ausführung: höchstens ein Workspace-Worker je gleichzeitig laufendem
/// Block, und jede Änderung wird danach gegen die `owned_paths` aller anderen
/// Worker geprüft (fail closed).
pub const WORKSPACE_SCOPE: &str = ".";

/// `true`, wenn `owned_paths` den Workspace-Scope ([`WORKSPACE_SCOPE`]) trägt.
#[must_use]
pub fn is_workspace_scope(owned_paths: &[String]) -> bool {
    owned_paths
        .iter()
        .any(|path| path.trim() == WORKSPACE_SCOPE)
}

/// Grenzen eines Treiberlaufs.
///
/// # Description
/// Jede Grenze ist absolut: erreicht oder überschritten heißt
/// [`WorkDriveStep::GiveUp`] (es sei denn, das Ziel ist im selben Snapshot
/// bereits belegt — dann gewinnt [`WorkDriveStep::ProposeAchieved`]).
///
/// # Concurrency
/// `Copy` + `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkDriveLimits {
    /// Höchstzahl der Treiberrunden (`iteration >= max_iterations` → aufgeben).
    pub max_iterations: u32,
    /// Länge einer Fortsetzungskette je Worker, bevor er neu gestartet wird.
    pub max_attempts_per_worker: u32,
    /// Höchstzahl gleichzeitig existierender Worker.
    pub max_parallel_workers: usize,
    /// Token-Budget des gesamten Laufs.
    pub token_budget: u64,
    /// Wandzeit-Budget des gesamten Laufs (gemessen als `now - started_at`).
    pub wall_budget: Duration,
    /// Runden ohne Fortschritt, nach denen der Lauf als festgefahren gilt.
    pub stall_iterations: u32,
    /// Kontextgröße eines Workers, ab der er statt fortgesetzt neu gestartet
    /// wird (strikt größer).
    pub respawn_context_tokens: u64,
}

impl Default for WorkDriveLimits {
    fn default() -> Self {
        Self {
            max_iterations: 20,
            max_attempts_per_worker: 3,
            max_parallel_workers: 4,
            token_budget: 2_000_000,
            wall_budget: Duration::hours(4),
            stall_iterations: 3,
            respawn_context_tokens: 150_000,
        }
    }
}

/// Ein isolierter Arbeitsbereich eines Workers.
///
/// # Description
/// `owned_paths` sind die Pfade (Dateien oder Verzeichnisse, `/`-getrennt),
/// die nur dieser Worker ändert; zwei Scopes überschneiden sich, wenn ein
/// Pfad gleich einem anderen ist oder in dessen Verzeichnis liegt.
/// `criteria` sind Indizes in `Goal::acceptance_criteria`, für die der Scope
/// zuständig ist — daran erkennt der Treiber, welche offenen Kriterien schon
/// abgedeckt sind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkScope {
    /// Stabile Kennung des Scopes.
    pub id: String,
    /// Einzeilige Zusammenfassung.
    pub summary: String,
    /// Exklusiv besessene Pfade.
    pub owned_paths: Vec<String>,
    /// Zuständige Akzeptanzkriterien (Indizes).
    #[serde(default)]
    pub criteria: Vec<usize>,
}

/// Wie ein Worker seine letzte Runde abgeschlossen hat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorkerOutcome {
    /// Fertig; wartet auf die zentrale Verifikation.
    Done,
    /// Braucht eine menschliche Entscheidung.
    Blocked {
        /// Was entschieden werden muss.
        reason: String,
    },
    /// Hart gescheitert.
    Failed {
        /// Warum.
        reason: String,
    },
    /// Teilweise erledigt; kann fortgesetzt werden.
    Partial,
}

/// Kompakte Rückmeldung eines Workers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerResultSummary {
    /// Ausgang der Runde.
    pub outcome: WorkerOutcome,
    /// Kurze Zusammenfassung des Erreichten.
    pub summary: String,
    /// Erzeugte oder geänderte Artefakte (Pfade, Commits, Berichte).
    pub artifacts: Vec<String>,
    /// Vorschlag des Workers für seinen nächsten Schritt.
    pub suggested_next: Option<String>,
    /// Kriterien, die der Worker laut `work_driver.report` bearbeitet hat
    /// (Indizes, sortiert, ohne Doppelte). Speist nur das Routing (wer
    /// Feedback zu welchem Kriterium bekommt, welches Kriterium schon
    /// abgedeckt ist) — nie den Goal-Status: den setzen nur Verifikation und
    /// Mensch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria_addressed: Vec<usize>,
}

impl WorkerResultSummary {
    /// Ergebnis einer Runde, die **ohne** `work_driver.report` endete
    /// (R18 D-E): `Partial` mit dem ausdrücklichen Grund
    /// [`NO_REPORT_REASON`] am Anfang der Zusammenfassung — nie still als
    /// `Done` gedeutet, nie aus Freitext geraten.
    ///
    /// # Arguments
    /// - `last_text`: letzte Worker-Antwort, vom Aufrufer bereits gekürzt;
    ///   darf leer sein.
    #[must_use]
    pub fn no_report(last_text: &str) -> Self {
        let text = last_text.trim();
        let summary = if text.is_empty() {
            NO_REPORT_REASON.to_owned()
        } else {
            format!("{NO_REPORT_REASON}: {text}")
        };
        Self {
            outcome: WorkerOutcome::Partial,
            summary,
            artifacts: Vec::new(),
            suggested_next: Some(format!(
                "Schließe die Runde mit dem Werkzeug `{WORK_DRIVER_REPORT_TOOL}` ab."
            )),
            criteria_addressed: Vec::new(),
        }
    }
}

/// Name des Werkzeugs, mit dem ein WorkDriver-Worker seine Runde abschließt
/// (R18 D-E, Vertrag
/// `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §8).
pub const WORK_DRIVER_REPORT_TOOL: &str = "work_driver.report";

/// Ausdrücklicher Grund, wenn ein Worker ohne [`WORK_DRIVER_REPORT_TOOL`]
/// endet (siehe [`WorkerResultSummary::no_report`]).
pub const NO_REPORT_REASON: &str = "no report";

/// Status, den ein Worker in [`WorkerReport`] meldet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerReportStatus {
    /// Scope erledigt; wartet auf die zentrale Verifikation.
    Done,
    /// Teilweise erledigt; kann fortgesetzt werden.
    Partial,
    /// Braucht eine Entscheidung; `blockers` nennt sie.
    Blocked,
    /// Hart gescheitert.
    Failed,
}

/// Strukturierte Rückmeldung eines WorkDriver-Workers (R18 D-E).
///
/// # Description
/// Die Argumente des Werkzeugs [`WORK_DRIVER_REPORT_TOOL`]. Ersetzt das
/// tolerante Deuten von Statuszeilen und JSON-Blöcken im Freitext: der
/// Worker **muss** das Werkzeug aufrufen; fehlt der Aufruf, gilt
/// [`WorkerResultSummary::no_report`]. Unbekannte Felder werden abgelehnt
/// (`deny_unknown_fields`), damit ein Tippfehler nicht still verloren geht.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_plan_bridge::work_driver::{WorkerOutcome, WorkerReport};
///
/// let report: WorkerReport = serde_json::from_str(
///     r#"{"status":"blocked","summary":"API unklar","blockers":["Welche Version?"]}"#,
/// )?;
/// assert!(report.validate(3).is_ok());
/// let summary = report.into_summary();
/// assert_eq!(summary.outcome, WorkerOutcome::Blocked { reason: "Welche Version?".into() });
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerReport {
    /// Ausgang der Runde.
    pub status: WorkerReportStatus,
    /// Bearbeitete Akzeptanzkriterien (Indizes in
    /// `Goal::acceptance_criteria`).
    #[serde(default)]
    pub criteria_addressed: Vec<usize>,
    /// Geänderte Pfade, relativ zum Workspace, `/`-getrennt.
    #[serde(default)]
    pub changed_paths: Vec<String>,
    /// Kurze Zusammenfassung des Erreichten.
    pub summary: String,
    /// Offene Entscheidungen bzw. Hindernisse; Pflicht bei `blocked`.
    #[serde(default)]
    pub blockers: Vec<String>,
}

/// Warum ein [`WorkerReport`] abgelehnt wird.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerReportError {
    /// `summary` ist leer.
    EmptySummary,
    /// `status = blocked` ohne einen einzigen nicht leeren Blocker.
    BlockedWithoutBlocker,
    /// Ein Kriterium-Index liegt außerhalb des Goals.
    CriterionOutOfRange {
        /// Gemeldeter Index.
        index: usize,
        /// Anzahl der Kriterien des Goals.
        total: usize,
    },
    /// Ein Pfad ist leer, absolut, enthält `..` oder `:` (Laufwerk/Schema).
    InvalidPath {
        /// Der gemeldete Pfad.
        path: String,
    },
}

impl std::fmt::Display for WorkerReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptySummary => f.write_str("work_driver.report: summary is empty"),
            Self::BlockedWithoutBlocker => {
                f.write_str("work_driver.report: status `blocked` needs at least one blocker")
            }
            Self::CriterionOutOfRange { index, total } => write!(
                f,
                "work_driver.report: criterion {index} is out of range (goal has {total})"
            ),
            Self::InvalidPath { path } => write!(
                f,
                "work_driver.report: path `{path}` must be relative, non-empty, without `..` and `:`"
            ),
        }
    }
}

impl std::error::Error for WorkerReportError {}

impl WorkerReport {
    /// Prüft den Bericht gegen ein Goal mit `criteria_total` Kriterien.
    ///
    /// # Errors
    /// Die erste verletzte Regel als [`WorkerReportError`]. Der Aufrufer
    /// meldet sie dem Worker als Werkzeugfehler zurück (der Worker kann
    /// korrigieren); ein abgelehnter Bericht zählt nicht als Bericht.
    pub fn validate(&self, criteria_total: usize) -> Result<(), WorkerReportError> {
        if self.summary.trim().is_empty() {
            return Err(WorkerReportError::EmptySummary);
        }
        if self.status == WorkerReportStatus::Blocked
            && self
                .blockers
                .iter()
                .all(|blocker| blocker.trim().is_empty())
        {
            return Err(WorkerReportError::BlockedWithoutBlocker);
        }
        if let Some(&index) = self
            .criteria_addressed
            .iter()
            .find(|&&index| index >= criteria_total)
        {
            return Err(WorkerReportError::CriterionOutOfRange {
                index,
                total: criteria_total,
            });
        }
        if let Some(path) = self
            .changed_paths
            .iter()
            .find(|path| !is_relative_path(path))
        {
            return Err(WorkerReportError::InvalidPath { path: path.clone() });
        }
        Ok(())
    }

    /// Übersetzt den Bericht in die Rückmeldung, die [`WorkDriver::decide`]
    /// liest.
    ///
    /// `blocked`/`failed` tragen die nicht leeren `blockers` (mit `; `
    /// verbunden) als Grund, ohne Blocker die Zusammenfassung;
    /// `changed_paths` werden zu `artifacts`, `criteria_addressed` sortiert
    /// und ohne Doppelte übernommen.
    #[must_use]
    pub fn into_summary(self) -> WorkerResultSummary {
        let summary = self.summary.trim().to_owned();
        let blockers: Vec<&str> = self
            .blockers
            .iter()
            .map(|blocker| blocker.trim())
            .filter(|blocker| !blocker.is_empty())
            .collect();
        let reason = if blockers.is_empty() {
            summary.clone()
        } else {
            blockers.join("; ")
        };
        let outcome = match self.status {
            WorkerReportStatus::Done => WorkerOutcome::Done,
            WorkerReportStatus::Partial => WorkerOutcome::Partial,
            WorkerReportStatus::Blocked => WorkerOutcome::Blocked { reason },
            WorkerReportStatus::Failed => WorkerOutcome::Failed { reason },
        };
        let mut criteria_addressed = self.criteria_addressed;
        criteria_addressed.sort_unstable();
        criteria_addressed.dedup();
        WorkerResultSummary {
            outcome,
            summary,
            artifacts: self.changed_paths,
            suggested_next: None,
            criteria_addressed,
        }
    }

    /// JSON-Schema der Werkzeugargumente von [`WORK_DRIVER_REPORT_TOOL`].
    #[must_use]
    pub fn input_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["status", "summary"],
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["done", "partial", "blocked", "failed"]
                },
                "criteria_addressed": {
                    "type": "array",
                    "items": {"type": "integer", "minimum": 0}
                },
                "changed_paths": {
                    "type": "array",
                    "items": {"type": "string"}
                },
                "summary": {"type": "string", "minLength": 1},
                "blockers": {
                    "type": "array",
                    "items": {"type": "string"}
                }
            }
        })
    }
}

/// Relativer, nicht leerer Pfad ohne `..`-Bestandteil und ohne `:`.
fn is_relative_path(path: &str) -> bool {
    let path = path.trim();
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains(':')
        && path.split(['/', '\\']).all(|part| part != "..")
}

/// Beobachteter Zustand eines Workers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerState {
    /// Kennung des Workers (Sitzung, Job oder Agent).
    pub worker_id: String,
    /// Der zugewiesene Scope.
    pub scope: WorkScope,
    /// Bisherige Runden in der aktuellen Fortsetzungskette.
    pub attempts: u32,
    /// Letzte Rückmeldung; `None` heißt: läuft noch.
    pub last_result: Option<WorkerResultSummary>,
    /// Aktuelle Kontextgröße des Workers in Tokens.
    pub context_tokens_used: u64,
    /// Beobachtete Prompt-Cache-Trefferquote (`0.0..=1.0`), falls bekannt.
    pub cache_hit_ratio: Option<f32>,
}

/// Urteil des Bewerters über Kriterien, die Evidenz allein nicht entscheidet.
///
/// # Description
/// Der Bewerter ist ein kleiner interner Worker mit eigenem Modell und
/// Provider. Er antwortet ausschließlich mit einem minimalen, positiven
/// Urteil: `{"passed": bool, "comment": "..."}`. Die Feldnamen `met` und
/// `rationale` bleiben über `serde(alias = ...)` lesbar, damit ältere
/// gespeicherte Urteile weiter deserialisieren.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeVerdict {
    /// Sind die bewerteten Kriterien erfüllt?
    #[serde(alias = "met")]
    pub passed: bool,
    /// Begründung des Bewerters; optional. Die kürzeste gültige Antwort ist
    /// `{"passed": false}`: findet der Bewerter nichts, gibt er (fast) nichts
    /// aus — das Lesen der Evidenz ist beim Provider überwiegend Cache-Read,
    /// teuer ist nur die Ausgabe.
    #[serde(default, alias = "rationale")]
    pub comment: String,
    /// Was konkret fehlt (wird zu Feedback); optional in der Antwort des
    /// Bewerters.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// Stabiler Präfix der Bewerter-Frage (für den Prompt-Cache): Antwortformat
/// und Regeln, ohne die fallspezifischen Kriterien.
///
/// # Description
/// Der Bewerter bekommt ausschließlich diesen Text plus die Kriterien
/// angehängt — keine Tools, keine Rückfragen. Er soll `passed: true` nur
/// wählen, wenn die Evidenz das zeigt.
pub const JUDGE_INSTRUCTION: &str = "Du bist der Bewerter. Antworte AUSSCHLIESSLICH mit einem \
JSON-Objekt der Form {\"passed\": bool, \"comment\": string, \"missing\": [string]} (missing \
optional) — kein Fließtext, keine Tools, keine Rückfragen. Wähle `passed: true` nur, wenn die \
vorliegende Evidenz das eindeutig zeigt; im Zweifel `passed: false` mit einem knappen `comment` \
und, falls bekannt, `missing`.";

/// Stand der zentralen Verifikation für die aktuelle Welle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum VerificationState {
    /// Für den aktuellen Stand noch nicht gelaufen.
    #[default]
    NotRun,
    /// Gelaufen und grün.
    Passed,
    /// Gelaufen und rot.
    Failed {
        /// Die fehlschlagenden Zeilen (Test, Lint, Gate …), knapp.
        failing: Vec<String>,
    },
}

/// Verbrauch des Laufs bis `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetUsageSnapshot {
    /// Verbrauchte Tokens über alle Worker.
    pub tokens_used: u64,
    /// Start des Laufs (Bezug für das Wandzeit-Budget).
    pub started_at: OffsetDateTime,
    /// Aufeinanderfolgende Runden ohne Fortschritt (kein neu erfülltes
    /// Kriterium, keine verbesserte Verifikation).
    pub iterations_without_progress: u32,
}

/// Der vollständige Eingabezustand einer Treiberrunde.
///
/// # Description
/// Alles, was [`WorkDriver::decide`] liest, steht hier.
#[derive(Debug, Clone, Copy)]
pub struct WorkDriveInput<'a> {
    /// Das zu erreichende Goal.
    pub goal: &'a Goal,
    /// Auswertung des Goals (`harw_plan::goal::evaluate_goal`).
    pub report: &'a GoalReport,
    /// Aktuelle Runde (ab `0`).
    pub iteration: u32,
    /// Alle aktuell existierenden Worker der Welle.
    pub workers: &'a [WorkerState],
    /// Vorgeschlagene Scopes (aus `write_scope` der Plan-Knoten, siehe
    /// [`scope_hints_from_plan`]); für ein Kriterium ohne Hinweis leitet der
    /// Treiber einen Scope aus dessen Artefakt-Schritten ab, ohne solche den
    /// Workspace-Scope ([`WORKSPACE_SCOPE`]).
    pub scope_hints: &'a [WorkScope],
    /// Verbrauch bis `now`.
    pub usage: BudgetUsageSnapshot,
    /// Grenzen des Laufs.
    pub limits: WorkDriveLimits,
    /// Stand der zentralen Verifikation der aktuellen Welle.
    pub verification: &'a VerificationState,
    /// Urteil des Bewerters zum aktuellen Wellenstand, falls eingeholt.
    pub judge: Option<&'a JudgeVerdict>,
    /// Rolle, unter der ein delegierter Worker startet (`[work_driver]
    /// worker_role` der IR). `None`: die Spec kennt keine — dann gilt
    /// [`DEFAULT_WORKER_ROLE`].
    pub worker_role: Option<&'a str>,
    /// Obergrenze gleichzeitig aktiver Worker, die der Aufrufer aus
    /// Provider-Limits ableitet (`max_concurrency`, `rate_limit.max_concurrent`,
    /// TPM-Taktung, Halbierung nach HTTP 429). `None` heißt: nur
    /// [`WorkDriveLimits::max_parallel_workers`] der Spec gilt. `decide`
    /// nimmt stets das Minimum aus beiden — es erweitert die Provider-Grenze
    /// nie, egal wie groß die Spec sie erlaubt.
    pub effective_parallel: Option<core::num::NonZeroUsize>,
    /// Der injizierte Referenzzeitpunkt.
    pub now: OffsetDateTime,
}

/// Warum ein Worker neu gestartet wird.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RespawnReason {
    /// Die Fortsetzungskette hat `max_attempts_per_worker` erreicht.
    AttemptsExhausted {
        /// Bisherige Runden der Kette.
        attempts: u32,
    },
    /// Der Kontext ist größer als `respawn_context_tokens`.
    ContextTooLarge {
        /// Aktuelle Kontextgröße.
        tokens: u64,
        /// Die überschrittene Schwelle.
        limit: u64,
    },
    /// Der Worker ist hart gescheitert.
    Failed {
        /// Gemeldeter Grund.
        reason: String,
    },
}

/// Warum der Lauf aufgegeben wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GiveUpReason {
    /// `max_iterations` erreicht.
    IterationLimit,
    /// `token_budget` erreicht.
    TokenBudget,
    /// `wall_budget` erreicht.
    WallBudget,
    /// `stall_iterations` Runden ohne Fortschritt.
    Stalled,
}

/// Ein einzelner Schritt, den der Treiber vorschlägt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum WorkDriveStep {
    /// Neuen parallelen Worker für einen noch nicht abgedeckten Scope starten.
    Delegate {
        /// Der Scope (disjunkt zu allen laufenden Scopes).
        scope: WorkScope,
        /// Rolle des Workers.
        role: String,
        /// Vollständige, in sich geschlossene Aufgabe.
        task: String,
    },
    /// Denselben Worker mit knappem, angehängtem Feedback fortsetzen.
    Continue {
        /// Der fortzusetzende Worker.
        worker_id: String,
        /// Nur offene Kriterien und fehlschlagende Evidenz.
        feedback: String,
    },
    /// Worker verwerfen und frisch mit einer Übergabe starten.
    Respawn {
        /// Der zu ersetzende Worker.
        worker_id: String,
        /// Warum.
        reason: RespawnReason,
        /// Kompakte Übergabe für den frischen Worker.
        handoff: String,
    },
    /// Eine zentrale Verifikation über den kombinierten Stand der Welle.
    Verify {
        /// Die Worker der abgeschlossenen Welle.
        workers: Vec<String>,
    },
    /// Den Bewerter zu Kriterien fragen, die Evidenz allein nicht entscheidet.
    Judge {
        /// Die zu bewertenden Kriterien (Indizes).
        criteria: Vec<usize>,
        /// Die präzise Frage.
        question: String,
    },
    /// Vorschlag an einen Menschen, das Goal für erreicht zu erklären.
    ProposeAchieved {
        /// Zusammenfassung der Belege.
        evidence_summary: String,
    },
    /// Eine menschliche Entscheidung ist nötig.
    Escalate {
        /// Der blockierte Worker, falls einer betroffen ist.
        worker_id: Option<String>,
        /// Die Frage an den Menschen.
        question: String,
    },
    /// Den Lauf beenden.
    GiveUp {
        /// Welche Grenze gegriffen hat.
        reason: GiveUpReason,
        /// Messwerte zur Grenze.
        detail: String,
    },
}

/// Ergebnis einer Treiberrunde.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkDrivePlan {
    /// Die geordneten Schritte.
    pub steps: Vec<WorkDriveStep>,
    /// Eine Zeile Begründung je Entscheidung, in Entscheidungsreihenfolge.
    pub rationale: Vec<String>,
}

/// Der Treiber — ein zustandsloser Namensraum.
///
/// # Concurrency
/// Enthält keinen Zustand; aus beliebig vielen Threads gleichzeitig aufrufbar.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkDriver;

impl WorkDriver {
    /// Leitet aus dem Snapshot die nächsten Schritte ab (rein, deterministisch).
    ///
    /// # Arguments
    /// - `input` (`&WorkDriveInput`): der vollständige Rundenzustand.
    ///
    /// # Returns
    /// Einen [`WorkDrivePlan`]; leer (mit Begründung), wenn das Goal terminal
    /// ist oder nur auf laufende Worker gewartet wird.
    pub fn decide(input: &WorkDriveInput<'_>) -> WorkDrivePlan {
        let mut out = WorkDrivePlan::default();
        if is_terminal(input.goal.status) {
            out.rationale.push(format!(
                "Goal ist terminal ({:?}); nichts zu treiben.",
                input.goal.status
            ));
            return out;
        }

        let mut workers: Vec<&WorkerState> = input.workers.iter().collect();
        workers.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
        let view = GoalView::new(input);
        let quiescent = workers.iter().all(|worker| is_settled(worker));

        if quiescent && view.ready_to_propose(input) {
            out.steps.push(WorkDriveStep::ProposeAchieved {
                evidence_summary: evidence_summary(input, &workers),
            });
            out.rationale.push(
                "Alle Kriterien belegt, Invarianten belegt, zentrale Verifikation grün: \
                 Zielerreichung wird vorgeschlagen, nicht gesetzt."
                    .to_owned(),
            );
            return out;
        }

        if let Some((reason, detail)) = limit_hit(input) {
            out.rationale
                .push(format!("Grenze erreicht ({reason:?}): {detail}"));
            out.steps.push(WorkDriveStep::GiveUp { reason, detail });
            return out;
        }

        escalate_blocked(&workers, &mut out);
        if quiescent {
            drive_settled_wave(input, &workers, &view, &mut out);
        } else {
            drive_running_wave(input, &workers, &view, &mut out);
        }
        if out.steps.is_empty() {
            out.rationale
                .push("Nichts zu tun: warte auf laufende Worker.".to_owned());
        }
        out
    }
}

/// Vorberechnete Sicht auf offene Kriterien.
struct GoalView {
    /// Alle offenen, im Goal existierenden Kriterien.
    open: BTreeSet<usize>,
    /// Offene Kriterien, die Evidenz entscheiden kann.
    evidence_open: Vec<usize>,
    /// Offene Kriterien, die nur der Bewerter entscheiden kann.
    judge_open: Vec<usize>,
}

impl GoalView {
    fn new(input: &WorkDriveInput<'_>) -> Self {
        let open: BTreeSet<usize> = input
            .report
            .criteria_open
            .iter()
            .copied()
            .filter(|idx| *idx < input.goal.acceptance_criteria.len())
            .collect();
        let (judge_open, evidence_open): (Vec<usize>, Vec<usize>) =
            open.iter().copied().partition(|idx| {
                input
                    .goal
                    .acceptance_criteria
                    .get(*idx)
                    .is_some_and(needs_judge)
            });
        Self {
            open,
            evidence_open,
            judge_open,
        }
    }

    fn ready_to_propose(&self, input: &WorkDriveInput<'_>) -> bool {
        let judge_ok =
            self.judge_open.is_empty() || input.judge.is_some_and(|verdict| verdict.passed);
        self.evidence_open.is_empty()
            && input.report.invariants_violated.is_empty()
            && *input.verification == VerificationState::Passed
            && judge_ok
    }
}

/// Ein Kriterium ohne Verifikationsschritt oder mit manuellem Schritt lässt
/// sich aus Evidenz allein nicht entscheiden.
fn needs_judge(criterion: &Criterion) -> bool {
    criterion.verification.is_empty()
        || criterion
            .verification
            .iter()
            .any(|step| matches!(step, VerificationStep::Manual { .. }))
}

fn is_terminal(status: GoalStatus) -> bool {
    matches!(
        status,
        GoalStatus::Achieved | GoalStatus::Abandoned | GoalStatus::Superseded
    )
}

/// Die konfigurierte Worker-Rolle, oder [`DEFAULT_WORKER_ROLE`], wenn die
/// Spec keine nennt.
fn worker_role<'a>(input: &WorkDriveInput<'a>) -> &'a str {
    input.worker_role.unwrap_or(DEFAULT_WORKER_ROLE)
}

fn outcome(worker: &WorkerState) -> Option<&WorkerOutcome> {
    worker.last_result.as_ref().map(|result| &result.outcome)
}

/// `Done` oder `Blocked`: der Worker wartet nicht mehr auf den Treiber.
fn is_settled(worker: &WorkerState) -> bool {
    matches!(
        outcome(worker),
        Some(WorkerOutcome::Done | WorkerOutcome::Blocked { .. })
    )
}

fn is_done(worker: &WorkerState) -> bool {
    matches!(outcome(worker), Some(WorkerOutcome::Done))
}

/// Die erste gegriffene Grenze in fester Reihenfolge.
fn limit_hit(input: &WorkDriveInput<'_>) -> Option<(GiveUpReason, String)> {
    let limits = &input.limits;
    if input.iteration >= limits.max_iterations {
        return Some((
            GiveUpReason::IterationLimit,
            format!(
                "Runde {} von höchstens {}",
                input.iteration, limits.max_iterations
            ),
        ));
    }
    if input.usage.tokens_used >= limits.token_budget {
        return Some((
            GiveUpReason::TokenBudget,
            format!(
                "{} von {} Tokens verbraucht",
                input.usage.tokens_used, limits.token_budget
            ),
        ));
    }
    let elapsed = input.now - input.usage.started_at;
    if elapsed >= limits.wall_budget {
        return Some((
            GiveUpReason::WallBudget,
            format!(
                "{} s von {} s Wandzeit verbraucht",
                elapsed.whole_seconds(),
                limits.wall_budget.whole_seconds()
            ),
        ));
    }
    if input.usage.iterations_without_progress >= limits.stall_iterations {
        return Some((
            GiveUpReason::Stalled,
            format!(
                "{} Runden ohne Fortschritt (Schwelle {})",
                input.usage.iterations_without_progress, limits.stall_iterations
            ),
        ));
    }
    None
}

fn escalate_blocked(workers: &[&WorkerState], out: &mut WorkDrivePlan) {
    for worker in workers {
        if let Some(WorkerOutcome::Blocked { reason }) = outcome(worker) {
            out.steps.push(WorkDriveStep::Escalate {
                worker_id: Some(worker.worker_id.clone()),
                question: format!(
                    "Worker {} (Scope {}) ist blockiert: {reason}. Wie soll es weitergehen?",
                    worker.worker_id, worker.scope.id
                ),
            });
            out.rationale.push(format!(
                "{} blockiert: menschliche Entscheidung nötig.",
                worker.worker_id
            ));
        }
    }
}

/// Alle Worker sind `Done`/`Blocked` (oder es gibt keine).
fn drive_settled_wave(
    input: &WorkDriveInput<'_>,
    workers: &[&WorkerState],
    view: &GoalView,
    out: &mut WorkDrivePlan,
) {
    let invariants_ok = input.report.invariants_violated.is_empty();
    match input.verification {
        VerificationState::NotRun => {
            if !workers.is_empty() || (view.open.is_empty() && invariants_ok) {
                let ids: Vec<String> = workers.iter().map(|w| w.worker_id.clone()).collect();
                out.rationale.push(format!(
                    "Welle abgeschlossen ({} Worker): eine zentrale Verifikation.",
                    ids.len()
                ));
                out.steps.push(WorkDriveStep::Verify { workers: ids });
                return;
            }
            delegate_uncovered(input, workers, view, out);
        }
        VerificationState::Failed { failing } => {
            let continued = drive_done_workers(input, workers, out, |worker| {
                let mut lines = scope_open_lines(input.goal, worker, &view.open);
                lines.extend(failing_lines_for(worker, workers, failing));
                lines
            });
            if continued == 0 && view.open.is_empty() {
                out.steps.push(WorkDriveStep::Escalate {
                    worker_id: None,
                    question: format!(
                        "Zentrale Verifikation scheitert ohne zuständigen Worker: {}",
                        failing.join("; ")
                    ),
                });
                out.rationale
                    .push("Rote Verifikation ohne Worker: Eskalation.".to_owned());
            }
            delegate_uncovered(input, workers, view, out);
        }
        VerificationState::Passed => {
            if view.evidence_open.is_empty() && invariants_ok {
                drive_judge(input, workers, view, out);
            } else {
                let continued = drive_done_workers(input, workers, out, |worker| {
                    let mut lines = scope_open_lines(input.goal, worker, &view.open);
                    if !lines.is_empty() || !invariants_ok {
                        lines.extend(invariant_lines(input.report));
                    }
                    lines
                });
                if continued == 0 && !invariants_ok && view.open.is_empty() {
                    out.steps.push(WorkDriveStep::Escalate {
                        worker_id: None,
                        question: format!(
                            "Verifikation grün, aber Invarianten nicht belegt: {}. \
                             Wie sollen sie belegt werden?",
                            input.report.invariants_violated.join(", ")
                        ),
                    });
                    out.rationale
                        .push("Unbelegte Invarianten ohne Worker: Eskalation.".to_owned());
                }
                delegate_uncovered(input, workers, view, out);
            }
        }
    }
}

/// Evidenz ist vollständig; übrig sind Kriterien für den Bewerter.
fn drive_judge(
    input: &WorkDriveInput<'_>,
    workers: &[&WorkerState],
    view: &GoalView,
    out: &mut WorkDrivePlan,
) {
    match input.judge {
        None if !workers.is_empty() => {
            let listed: Vec<String> = view
                .judge_open
                .iter()
                .filter_map(|idx| {
                    input
                        .goal
                        .acceptance_criteria
                        .get(*idx)
                        .map(|c| format!("[{idx}] {}", c.description))
                })
                .collect();
            out.steps.push(WorkDriveStep::Judge {
                criteria: view.judge_open.clone(),
                question: format!(
                    "{JUDGE_INSTRUCTION}\n\nSind diese Kriterien nach der grünen Verifikation \
                     erfüllt? {}",
                    listed.join("; ")
                ),
            });
            out.rationale.push(
                "Evidenz vollständig, verbleibende Kriterien brauchen den Bewerter.".to_owned(),
            );
        }
        None => delegate_uncovered(input, workers, view, out),
        Some(verdict) => {
            let judge_set: BTreeSet<usize> = view.judge_open.iter().copied().collect();
            let continued = drive_done_workers(input, workers, out, |worker| {
                let mut lines: Vec<String> = verdict
                    .missing
                    .iter()
                    .map(|missing| format!("Bewerter vermisst: {missing}"))
                    .collect();
                lines.extend(scope_open_lines(input.goal, worker, &judge_set));
                lines
            });
            if continued == 0 {
                delegate_judge_followup(input, workers, view, verdict, out);
            } else {
                out.rationale.push(format!(
                    "Bewerter: nicht erfüllt ({}); Lücken gehen als Feedback zurück.",
                    verdict.comment
                ));
            }
        }
    }
}

/// Mindestens ein Worker arbeitet noch oder wartet auf Fortsetzung.
fn drive_running_wave(
    input: &WorkDriveInput<'_>,
    workers: &[&WorkerState],
    view: &GoalView,
    out: &mut WorkDrivePlan,
) {
    let failing: &[String] = match input.verification {
        VerificationState::Failed { failing } => failing.as_slice(),
        VerificationState::NotRun | VerificationState::Passed => &[],
    };
    for worker in workers {
        match outcome(worker) {
            Some(WorkerOutcome::Partial) => {
                let mut lines = scope_open_lines(input.goal, worker, &view.open);
                lines.extend(failing_lines_for(worker, workers, failing));
                if lines.is_empty() {
                    lines.push(format!(
                        "Alle Kriterien deines Scopes sind belegt — schließe ab und melde \
                         `done` über `{WORK_DRIVER_REPORT_TOOL}`."
                    ));
                }
                continue_or_respawn(input, worker, &lines, out);
            }
            Some(WorkerOutcome::Failed { reason }) => {
                let mut lines = scope_open_lines(input.goal, worker, &view.open);
                lines.push(format!("Vorgänger scheiterte: {reason}"));
                out.rationale.push(format!(
                    "{} hart gescheitert: frischer Worker mit Übergabe.",
                    worker.worker_id
                ));
                out.steps.push(WorkDriveStep::Respawn {
                    worker_id: worker.worker_id.clone(),
                    reason: RespawnReason::Failed {
                        reason: reason.clone(),
                    },
                    handoff: handoff(worker, &lines),
                });
            }
            None | Some(WorkerOutcome::Done | WorkerOutcome::Blocked { .. }) => {}
        }
    }
    delegate_uncovered(input, workers, view, out);
}

/// Setzt jeden `Done`-Worker fort, für den `feedback_for` Zeilen liefert.
/// Gibt die Zahl der fortgesetzten (oder neu gestarteten) Worker zurück.
fn drive_done_workers<F>(
    input: &WorkDriveInput<'_>,
    workers: &[&WorkerState],
    out: &mut WorkDrivePlan,
    feedback_for: F,
) -> usize
where
    F: Fn(&WorkerState) -> Vec<String>,
{
    let mut driven = 0;
    for worker in workers.iter().filter(|worker| is_done(worker)) {
        let lines = feedback_for(worker);
        if lines.is_empty() {
            continue;
        }
        continue_or_respawn(input, worker, &lines, out);
        driven += 1;
    }
    driven
}

/// `Continue` ist der Normalfall; `Respawn` nur bei zu großem Kontext oder
/// ausgeschöpfter Fortsetzungskette.
fn continue_or_respawn(
    input: &WorkDriveInput<'_>,
    worker: &WorkerState,
    lines: &[String],
    out: &mut WorkDrivePlan,
) {
    let limits = &input.limits;
    if worker.context_tokens_used > limits.respawn_context_tokens {
        out.rationale.push(format!(
            "{}: Kontext {} > {} Tokens — Neustart mit Übergabe.",
            worker.worker_id, worker.context_tokens_used, limits.respawn_context_tokens
        ));
        out.steps.push(WorkDriveStep::Respawn {
            worker_id: worker.worker_id.clone(),
            reason: RespawnReason::ContextTooLarge {
                tokens: worker.context_tokens_used,
                limit: limits.respawn_context_tokens,
            },
            handoff: handoff(worker, lines),
        });
    } else if worker.attempts >= limits.max_attempts_per_worker {
        out.rationale.push(format!(
            "{}: {} Fortsetzungen ausgeschöpft — Neustart mit Übergabe.",
            worker.worker_id, worker.attempts
        ));
        out.steps.push(WorkDriveStep::Respawn {
            worker_id: worker.worker_id.clone(),
            reason: RespawnReason::AttemptsExhausted {
                attempts: worker.attempts,
            },
            handoff: handoff(worker, lines),
        });
    } else {
        let cache = worker.cache_hit_ratio.map_or_else(String::new, |ratio| {
            format!(", Cache-Trefferquote {ratio:.2}")
        });
        out.rationale.push(format!(
            "{}: Fortsetzung statt Neustart (Versuch {}{cache}).",
            worker.worker_id,
            worker.attempts.saturating_add(1)
        ));
        out.steps.push(WorkDriveStep::Continue {
            worker_id: worker.worker_id.clone(),
            feedback: lines.join("\n"),
        });
    }
}

/// Kompakte Übergabe an einen frischen Worker: Scope, Stand, offene Punkte —
/// nicht der Verlauf des Vorgängers.
fn handoff(worker: &WorkerState, lines: &[String]) -> String {
    let mut text = format!("Scope {}: {}", worker.scope.id, worker.scope.summary);
    if is_workspace_scope(&worker.scope.owned_paths) {
        text.push_str("\nSchreibbereich: der Workspace, außer Pfaden anderer Worker");
    } else if !worker.scope.owned_paths.is_empty() {
        text.push_str(&format!(
            "\nNur diese Pfade ändern: {}",
            worker.scope.owned_paths.join(", ")
        ));
    }
    if let Some(result) = &worker.last_result {
        text.push_str(&format!("\nBisher: {}", result.summary));
        if !result.artifacts.is_empty() {
            text.push_str(&format!("\nArtefakte: {}", result.artifacts.join(", ")));
        }
        if let Some(next) = &result.suggested_next {
            text.push_str(&format!("\nVorgeschlagen: {next}"));
        }
    }
    for line in lines {
        text.push('\n');
        text.push_str(line);
    }
    text
}

/// Kriterien, die `worker` beansprucht: die seines Scopes plus die, die er in
/// seiner letzten Rückmeldung als bearbeitet gemeldet hat
/// ([`WorkerResultSummary::criteria_addressed`]).
fn claimed_criteria(worker: &WorkerState) -> BTreeSet<usize> {
    let mut claimed: BTreeSet<usize> = worker.scope.criteria.iter().copied().collect();
    if let Some(result) = &worker.last_result {
        claimed.extend(result.criteria_addressed.iter().copied());
    }
    claimed
}

/// Offene Kriterien, die `worker` beansprucht ([`claimed_criteria`]), je eine
/// Zeile in Indexreihenfolge.
fn scope_open_lines(goal: &Goal, worker: &WorkerState, open: &BTreeSet<usize>) -> Vec<String> {
    claimed_criteria(worker)
        .into_iter()
        .filter(|idx| open.contains(idx))
        .filter_map(|idx| {
            goal.acceptance_criteria
                .get(idx)
                .map(|c| format!("Offen: [{idx}] {}", c.description))
        })
        .collect()
}

fn invariant_lines(report: &GoalReport) -> Vec<String> {
    report
        .invariants_violated
        .iter()
        .map(|id| format!("Invariante nicht belegt: {id}"))
        .collect()
}

/// Fehlschlagzeilen für `worker`: die, die einen seiner Pfade nennen, plus
/// die, die keinen Pfad irgendeines Workers nennen. Der Workspace-Scope
/// ([`WORKSPACE_SCOPE`]) nennt keinen Pfad: ein Workspace-Worker bekommt nur
/// die keinem Worker zuordenbaren Zeilen.
fn failing_lines_for(
    worker: &WorkerState,
    workers: &[&WorkerState],
    failing: &[String],
) -> Vec<String> {
    let mentions = |line: &str, candidate: &WorkerState| {
        normalize_paths(&candidate.scope.owned_paths)
            .iter()
            .filter(|path| path.as_str() != WORKSPACE_SCOPE)
            .any(|path| line.contains(path.as_str()))
    };
    failing
        .iter()
        .filter(|line| {
            mentions(line.as_str(), worker)
                || !workers.iter().any(|other| mentions(line.as_str(), other))
        })
        .map(|line| format!("Fehlschlag: {line}"))
        .collect()
}

/// Der wirksame Höchstwert gleichzeitig aktiver Worker: das Minimum aus der
/// Spec-Grenze ([`WorkDriveLimits::max_parallel_workers`]) und dem vom
/// Aufrufer gemeldeten Provider-Limit ([`WorkDriveInput::effective_parallel`]).
/// `None` heißt: nur die Spec-Grenze gilt. Laufende Worker zählen wie heute
/// gegen diesen Wert; Scopes über der Grenze bleiben für spätere Runden
/// zurückgestellt.
fn effective_cap(input: &WorkDriveInput<'_>) -> usize {
    match input.effective_parallel {
        Some(cap) => input.limits.max_parallel_workers.min(cap.get()),
        None => input.limits.max_parallel_workers,
    }
}

/// Delegiert offene, von keinem Worker abgedeckte Kriterien in freie Plätze.
fn delegate_uncovered(
    input: &WorkDriveInput<'_>,
    workers: &[&WorkerState],
    view: &GoalView,
    out: &mut WorkDrivePlan,
) {
    let covered: BTreeSet<usize> = workers
        .iter()
        .flat_map(|worker| claimed_criteria(worker))
        .collect();
    let judge_met = input.judge.is_some_and(|verdict| verdict.passed);
    let uncovered: Vec<usize> = view
        .open
        .iter()
        .copied()
        .filter(|idx| !covered.contains(idx))
        .filter(|idx| !(judge_met && view.judge_open.contains(idx)))
        .collect();
    if uncovered.is_empty() {
        return;
    }

    let mut hints: Vec<&WorkScope> = input.scope_hints.iter().collect();
    hints.sort_by(|a, b| a.id.cmp(&b.id));
    let cap = effective_cap(input);
    let free = cap.saturating_sub(workers.len());
    let uncovered_set: BTreeSet<usize> = uncovered.iter().copied().collect();
    let mut assigned: BTreeSet<usize> = BTreeSet::new();
    let mut pending: Vec<WorkScope> = Vec::new();

    for idx in uncovered {
        if assigned.contains(&idx) {
            continue;
        }
        let candidate = scope_for(input.goal, idx, &hints, &uncovered_set, &assigned);
        assigned.extend(candidate.criteria.iter().copied());

        if let Some((worker, path)) = conflicting_worker(&candidate, workers) {
            out.rationale.push(format!(
                "Scope {} überschneidet Worker {worker} (Pfad {path}) — zurückgestellt.",
                candidate.id
            ));
            continue;
        }
        let overlapping: Vec<usize> = pending
            .iter()
            .enumerate()
            .filter(|(_, scope)| scopes_overlap(scope, &candidate))
            .map(|(pos, _)| pos)
            .collect();
        if let Some(first) = overlapping.first().copied() {
            let mut merged = candidate;
            for pos in overlapping.iter().rev() {
                if *pos != first {
                    let other = pending.remove(*pos);
                    merged = merge_scopes(other, merged);
                }
            }
            if let Some(base) = pending.get_mut(first) {
                out.rationale.push(format!(
                    "Scope {} überschneidet {} — verschmolzen.",
                    merged.id, base.id
                ));
                *base = merge_scopes(base.clone(), merged);
            }
            continue;
        }
        if pending.len() >= free {
            out.rationale.push(format!(
                "Kein freier Platz für Scope {} ({} von {} Workern belegt).",
                candidate.id,
                workers.len().saturating_add(pending.len()),
                cap
            ));
            continue;
        }
        pending.push(candidate);
    }

    for scope in pending {
        out.rationale.push(format!(
            "Delegiere Scope {} für Kriterien {:?}.",
            scope.id, scope.criteria
        ));
        let task = render_task(input.goal, &scope, &[]);
        out.steps.push(WorkDriveStep::Delegate {
            scope,
            role: worker_role(input).to_owned(),
            task,
        });
    }
}

/// Bewerter-Lücken ohne `Done`-Worker: ein eigener Nachfolge-Scope.
fn delegate_judge_followup(
    input: &WorkDriveInput<'_>,
    workers: &[&WorkerState],
    view: &GoalView,
    verdict: &JudgeVerdict,
    out: &mut WorkDrivePlan,
) {
    if workers.len() >= effective_cap(input) {
        out.rationale.push(format!(
            "Bewerter: nicht erfüllt ({}), aber kein freier Platz.",
            verdict.comment
        ));
        return;
    }
    let scope = WorkScope {
        id: JUDGE_FOLLOWUP_SCOPE.to_owned(),
        summary: "Lücken aus dem Bewerterurteil schließen".to_owned(),
        // Bewerter-Lücken (Doku, Begründungen …) nennen keine Pfade; ohne
        // Schreibbereich könnte der Worker sie nicht schließen.
        owned_paths: vec![WORKSPACE_SCOPE.to_owned()],
        criteria: view.judge_open.clone(),
    };
    let missing: Vec<String> = verdict
        .missing
        .iter()
        .map(|missing| format!("Bewerter vermisst: {missing}"))
        .collect();
    out.rationale.push(format!(
        "Bewerter: nicht erfüllt ({}); kein fertiger Worker — eigener Scope.",
        verdict.comment
    ));
    let task = render_task(input.goal, &scope, &missing);
    out.steps.push(WorkDriveStep::Delegate {
        scope,
        role: worker_role(input).to_owned(),
        task,
    });
}

/// Scope für ein Kriterium: erster passender Hinweis (nach `id`), sonst aus
/// den Artefakt-Schritten des Kriteriums abgeleitet, ohne solche der
/// Workspace-Scope ([`WORKSPACE_SCOPE`]) — ein nur per `Command`
/// nachgewiesenes Kriterium bekommt so einen Worker, der schreiben darf.
fn scope_for(
    goal: &Goal,
    idx: usize,
    hints: &[&WorkScope],
    uncovered: &BTreeSet<usize>,
    assigned: &BTreeSet<usize>,
) -> WorkScope {
    if let Some(hint) = hints.iter().find(|hint| hint.criteria.contains(&idx)) {
        let mut criteria: Vec<usize> = hint
            .criteria
            .iter()
            .copied()
            .filter(|c| uncovered.contains(c) && !assigned.contains(c))
            .collect();
        criteria.sort_unstable();
        criteria.dedup();
        return WorkScope {
            id: hint.id.clone(),
            summary: hint.summary.clone(),
            owned_paths: normalize_paths(&hint.owned_paths),
            criteria,
        };
    }
    let criterion = goal.acceptance_criteria.get(idx);
    let paths: Vec<String> = criterion
        .map(|c| {
            c.verification
                .iter()
                .filter_map(|step| match step {
                    VerificationStep::Artifact { path } => Some(path.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut owned_paths = normalize_paths(&paths);
    if owned_paths.is_empty() {
        owned_paths.push(WORKSPACE_SCOPE.to_owned());
    }
    WorkScope {
        id: format!("criterion-{idx}"),
        summary: criterion.map_or_else(String::new, |c| c.description.clone()),
        owned_paths,
        criteria: vec![idx],
    }
}

/// Leitet Scope-Hinweise aus den Knoten eines Plans ab.
///
/// # Description
/// Ein Knoten wird zum Hinweis `plan-<knoten-id>`, wenn er
/// - einen Schreibbereich (`write_scope`) mit mindestens einem konkreten
///   Pfad hat — Glob-Muster (`*`, `?`, `[`) werden übersprungen, weil der
///   Schreibbereichs-Abgleich des Aufrufers nur Datei- und
///   Verzeichnispfade kennt (fail closed: lieber kein Hinweis als ein zu
///   weiter), bei Symbol-Einträgen zählt deren Datei — und
/// - mindestens ein Akzeptanzkriterium des Goals trägt: gleiche
///   (getrimmte, nicht leere) Beschreibung oder gleiche, nicht leere
///   Verifikationsschritte.
///
/// `summary` ist das Ziel des Knotens. Die Reihenfolge folgt dem Plan;
/// [`WorkDriver::decide`] sortiert Hinweise ohnehin nach `id`.
///
/// # Arguments
/// - `goal`: das getriebene Goal (Kriterien-Indizes beziehen sich darauf).
/// - `plan`: der an das Goal gebundene Plan.
///
/// # Returns
/// Die Hinweise; leer, wenn kein Knoten beide Bedingungen erfüllt.
#[must_use]
pub fn scope_hints_from_plan(goal: &Goal, plan: &Plan) -> Vec<WorkScope> {
    let mut hints = Vec::new();
    for node in &plan.nodes {
        let paths: Vec<String> = node
            .write_scope
            .iter()
            .map(|entry| match entry {
                PathOrSymbol::Path(path) | PathOrSymbol::Symbol { path, .. } => path.clone(),
            })
            .filter(|path| !path.contains(['*', '?', '[']))
            .collect();
        let owned_paths = normalize_paths(&paths);
        if owned_paths.is_empty() {
            continue;
        }
        let criteria: Vec<usize> = goal
            .acceptance_criteria
            .iter()
            .enumerate()
            .filter(|(_, criterion)| {
                node.acceptance_criteria
                    .iter()
                    .any(|own| same_criterion(own, criterion))
            })
            .map(|(idx, _)| idx)
            .collect();
        if criteria.is_empty() {
            continue;
        }
        hints.push(WorkScope {
            id: format!("plan-{}", node.id.as_str()),
            summary: node.objective.clone(),
            owned_paths,
            criteria,
        });
    }
    hints
}

/// Gleiche (getrimmte, nicht leere) Beschreibung oder gleiche, nicht leere
/// Verifikationsschritte.
fn same_criterion(a: &Criterion, b: &Criterion) -> bool {
    let description = a.description.trim();
    if !description.is_empty() && description == b.description.trim() {
        return true;
    }
    if a.verification.is_empty() || b.verification.is_empty() {
        return false;
    }
    match (
        serde_json::to_value(&a.verification),
        serde_json::to_value(&b.verification),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn merge_scopes(base: WorkScope, other: WorkScope) -> WorkScope {
    let mut owned_paths = base.owned_paths;
    owned_paths.extend(other.owned_paths);
    let mut criteria = base.criteria;
    criteria.extend(other.criteria);
    criteria.sort_unstable();
    criteria.dedup();
    WorkScope {
        id: base.id,
        summary: format!("{} + {}", base.summary, other.summary),
        owned_paths: normalize_paths(&owned_paths),
        criteria,
    }
}

/// Regelblock am Ende jeder Aufgabe (Teil des stabilen Präfixes).
pub const TASK_RULES: &str = "- Do not build or test yourself: one central verification runs \
per wave over the combined state of all workers.\n- Change only your owned paths; a change \
elsewhere blocks the whole run.\n- Finish by calling the tool `work_driver.report`; a status \
line in your text is not read.";

/// Die in sich geschlossene Aufgabe eines neuen Workers, in festen Abschnitten
/// (Ziel, Scope, Kriterien mit Index und Nachweis, Schreibbereich, Hinweise,
/// Regeln). Die Kriterien-Indizes sind die, die der Worker in
/// `work_driver.report` unter `criteria_addressed` meldet.
fn render_task(goal: &Goal, scope: &WorkScope, extra: &[String]) -> String {
    let mut text = format!(
        "## Goal\n{}\n\n## Scope `{}`\n{}\n\n## Acceptance criteria (index = position in the goal)",
        goal.statement, scope.id, scope.summary
    );
    for idx in &scope.criteria {
        if let Some(criterion) = goal.acceptance_criteria.get(*idx) {
            text.push_str(&format!("\n- [{idx}] {}", criterion.description));
            let checks: Vec<String> = criterion.verification.iter().map(render_check).collect();
            if !checks.is_empty() {
                text.push_str(&format!("\n  verified by: {}", checks.join("; ")));
            }
        }
    }
    text.push_str("\n\n## Owned paths\n");
    text.push_str(&owned_paths_text(&scope.owned_paths));
    if !extra.is_empty() {
        text.push_str("\n\n## Notes");
        for line in extra {
            text.push_str("\n- ");
            text.push_str(line);
        }
    }
    text.push_str("\n\n## Rules\n");
    text.push_str(TASK_RULES);
    text
}

/// Schreibbereich eines Scopes als Text: keiner, der Workspace oder die
/// Pfadliste.
fn owned_paths_text(owned_paths: &[String]) -> String {
    if owned_paths.is_empty() {
        "none: read and report only, change no file.".to_owned()
    } else if is_workspace_scope(owned_paths) {
        "the workspace: any file that no other worker owns; every change is checked after the \
         wave."
            .to_owned()
    } else {
        owned_paths
            .iter()
            .map(|path| format!("- {path}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn render_check(step: &VerificationStep) -> String {
    match step {
        VerificationStep::Command { cmd, expect_exit } => {
            format!("`{cmd}` exits {expect_exit}")
        }
        VerificationStep::Artifact { path } => format!("file `{path}` exists"),
        VerificationStep::TraceEvent { name } => format!("trace event `{name}`"),
        VerificationStep::Manual { note } => format!("manual review: {note}"),
    }
}

fn evidence_summary(input: &WorkDriveInput<'_>, workers: &[&WorkerState]) -> String {
    let mut text = format!(
        "{} von {} Kriterien belegt (Coverage {:.2}); Invarianten belegt; \
         zentrale Verifikation grün.",
        input.report.criteria_met.len(),
        input.goal.acceptance_criteria.len(),
        input.report.coverage
    );
    if let Some(verdict) = input.judge {
        text.push_str(&format!(" Bewerter: {}.", verdict.comment));
    }
    let with_artifacts = workers.iter().filter_map(|worker| {
        worker
            .last_result
            .as_ref()
            .filter(|result| !result.artifacts.is_empty())
            .map(|result| (worker, result))
    });
    for (worker, result) in with_artifacts {
        text.push_str(&format!(
            " {}: {}.",
            worker.worker_id,
            result.artifacts.join(", ")
        ));
    }
    text.push_str(" Den Status `Achieved` setzt nur ein menschlicher Akteur.");
    text
}

/// Normalisiert Pfade: ohne `./`-Präfix und abschließendes `/`, leere
/// Einträge fallen weg, sortiert und dedupliziert.
fn normalize_paths(paths: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = paths
        .iter()
        .map(|path| {
            let trimmed = path.trim();
            let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
            trimmed.trim_end_matches('/').to_owned()
        })
        .filter(|path| !path.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

/// Gleich, oder einer liegt im Verzeichnis des anderen.
fn paths_overlap(a: &str, b: &str) -> bool {
    let within = |child: &str, parent: &str| {
        child
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    a == b || within(a, b) || within(b, a)
}

/// Erster Pfad aus `a`, der einen aus `b` überschneidet. Der Workspace-Scope
/// ([`WORKSPACE_SCOPE`]) überschneidet hier nichts: seine Trennung übernimmt
/// der Aufrufer bei der Ausführung (siehe dort).
fn first_path_overlap(a: &[String], b: &[String]) -> Option<String> {
    let concrete = |paths: &[String]| -> Vec<String> {
        normalize_paths(paths)
            .into_iter()
            .filter(|path| path != WORKSPACE_SCOPE)
            .collect()
    };
    let a = concrete(a);
    let b = concrete(b);
    a.iter()
        .find(|left| {
            b.iter()
                .any(|right| paths_overlap(left.as_str(), right.as_str()))
        })
        .cloned()
}

fn scopes_overlap(a: &WorkScope, b: &WorkScope) -> bool {
    first_path_overlap(&a.owned_paths, &b.owned_paths).is_some()
}

fn conflicting_worker(candidate: &WorkScope, workers: &[&WorkerState]) -> Option<(String, String)> {
    workers.iter().find_map(|worker| {
        first_path_overlap(&candidate.owned_paths, &worker.scope.owned_paths)
            .map(|path| (worker.worker_id.clone(), path))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_plan::PlanNode;
    use harw_plan::goal::GoalId;

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    fn command_criterion(desc: &str) -> Criterion {
        Criterion {
            description: desc.to_owned(),
            verification: vec![VerificationStep::Command {
                cmd: format!("check {desc}"),
                expect_exit: 0,
            }],
        }
    }

    fn manual_criterion(desc: &str) -> Criterion {
        Criterion {
            description: desc.to_owned(),
            verification: vec![VerificationStep::Manual {
                note: desc.to_owned(),
            }],
        }
    }

    fn goal(criteria: Vec<Criterion>) -> Goal {
        Goal {
            id: GoalId::new("g-drive"),
            revision: 1,
            statement: "Treiberziel".to_owned(),
            non_goals: vec![],
            invariants: vec![],
            acceptance_criteria: criteria,
            constraints: vec![],
            open_questions: vec![],
            status: GoalStatus::Active,
            plan_id: None,
            plan_revision: None,
            evidence: vec![],
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            tenant: None,
        }
    }

    fn goal_n(n: usize) -> Goal {
        goal(
            (0..n)
                .map(|idx| command_criterion(&format!("k{idx}")))
                .collect(),
        )
    }

    fn report(total: usize, open: &[usize]) -> GoalReport {
        let criteria_met: Vec<usize> = (0..total).filter(|idx| !open.contains(idx)).collect();
        let coverage = if total == 0 {
            1.0
        } else {
            criteria_met.len() as f32 / total as f32
        };
        GoalReport {
            criteria_met,
            criteria_open: open.to_vec(),
            invariants_violated: vec![],
            coverage,
            blocking_nodes: vec![],
            next_actions: vec![],
        }
    }

    fn scope(id: &str, criteria: &[usize], paths: &[&str]) -> WorkScope {
        WorkScope {
            id: id.to_owned(),
            summary: format!("Scope {id}"),
            owned_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            criteria: criteria.to_vec(),
        }
    }

    fn result(outcome: WorkerOutcome) -> WorkerResultSummary {
        WorkerResultSummary {
            outcome,
            summary: "erledigt, was ging".to_owned(),
            artifacts: vec!["src/a.rs".to_owned()],
            suggested_next: Some("Tests ergänzen".to_owned()),
            criteria_addressed: Vec::new(),
        }
    }

    fn worker(
        id: &str,
        criteria: &[usize],
        paths: &[&str],
        outcome: Option<WorkerOutcome>,
    ) -> WorkerState {
        WorkerState {
            worker_id: id.to_owned(),
            scope: scope(&format!("s-{id}"), criteria, paths),
            attempts: 1,
            last_result: outcome.map(result),
            context_tokens_used: 10_000,
            cache_hit_ratio: Some(0.9),
        }
    }

    /// Besitzt alle Eingabewerte, damit `input()` nur Referenzen verteilt.
    struct Fixture {
        goal: Goal,
        report: GoalReport,
        iteration: u32,
        workers: Vec<WorkerState>,
        hints: Vec<WorkScope>,
        usage: BudgetUsageSnapshot,
        limits: WorkDriveLimits,
        verification: VerificationState,
        judge: Option<JudgeVerdict>,
        worker_role: Option<String>,
        effective_parallel: Option<core::num::NonZeroUsize>,
    }

    impl Fixture {
        fn new(goal: Goal, report: GoalReport) -> Self {
            Self {
                goal,
                report,
                iteration: 0,
                workers: vec![],
                hints: vec![],
                usage: BudgetUsageSnapshot {
                    tokens_used: 0,
                    started_at: now(),
                    iterations_without_progress: 0,
                },
                limits: WorkDriveLimits::default(),
                verification: VerificationState::NotRun,
                judge: None,
                worker_role: None,
                effective_parallel: None,
            }
        }

        fn input(&self) -> WorkDriveInput<'_> {
            WorkDriveInput {
                goal: &self.goal,
                report: &self.report,
                iteration: self.iteration,
                workers: &self.workers,
                scope_hints: &self.hints,
                usage: self.usage,
                limits: self.limits,
                verification: &self.verification,
                judge: self.judge.as_ref(),
                worker_role: self.worker_role.as_deref(),
                effective_parallel: self.effective_parallel,
                now: now(),
            }
        }

        fn decide(&self) -> WorkDrivePlan {
            WorkDriver::decide(&self.input())
        }
    }

    fn delegates(plan: &WorkDrivePlan) -> Vec<&WorkScope> {
        plan.steps
            .iter()
            .filter_map(|step| match step {
                WorkDriveStep::Delegate { scope, .. } => Some(scope),
                _ => None,
            })
            .collect()
    }

    fn count<F: Fn(&WorkDriveStep) -> bool>(plan: &WorkDrivePlan, pred: F) -> usize {
        plan.steps.iter().filter(|step| pred(step)).count()
    }

    fn only_step(plan: &WorkDrivePlan) -> TestResult<&WorkDriveStep> {
        match plan.steps.as_slice() {
            [step] => Ok(step),
            _ => Err(TestError::Unexpected(format!(
                "erwartet genau einen Schritt, bekommen: {plan:?}"
            ))),
        }
    }

    #[test]
    fn test_first_iteration_fans_out_one_delegate_per_open_criterion_up_to_max_parallel()
    -> TestResult {
        let mut fx = Fixture::new(goal_n(4), report(4, &[0, 1, 2, 3]));
        fx.limits.max_parallel_workers = 3;
        // Keine Rolle konfiguriert: der dokumentierte Default greift.
        fx.worker_role = None;

        let plan = fx.decide();

        let criteria: Vec<Vec<usize>> = delegates(&plan)
            .iter()
            .map(|s| s.criteria.clone())
            .collect();
        assert_eq!(criteria, vec![vec![0], vec![1], vec![2]], "{plan:?}");
        assert_eq!(plan.steps.len(), 3);
        if !plan
            .rationale
            .iter()
            .any(|line| line.contains("Kein freier Platz"))
        {
            return Err(TestError::Unexpected(format!(
                "Slot-Grenze nicht begründet: {:?}",
                plan.rationale
            )));
        }
        for step in &plan.steps {
            if let WorkDriveStep::Delegate { task, role, .. } = step {
                assert_eq!(role, DEFAULT_WORKER_ROLE);
                assert!(task.contains("Do not build or test yourself"), "{task}");
            }
        }
        Ok(())
    }

    fn nonzero(n: usize) -> TestResult<core::num::NonZeroUsize> {
        core::num::NonZeroUsize::new(n)
            .ok_or_else(|| TestError::Unexpected(format!("{n} ist kein NonZeroUsize")))
    }

    #[test]
    fn test_effective_parallel_below_spec_caps_delegates_to_one() -> TestResult {
        let mut fx = Fixture::new(goal_n(4), report(4, &[0, 1, 2, 3]));
        fx.limits.max_parallel_workers = 4;
        fx.effective_parallel = Some(nonzero(1)?);

        let plan = fx.decide();

        assert_eq!(delegates(&plan).len(), 1, "{plan:?}");
        match only_step(&plan)? {
            WorkDriveStep::Delegate { .. } => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_effective_parallel_counts_a_still_running_worker_against_the_cap() -> TestResult {
        let mut fx = Fixture::new(goal_n(3), report(3, &[0, 1, 2]));
        fx.limits.max_parallel_workers = 4;
        fx.workers = vec![worker("w1", &[0], &["src/a"], None)];
        fx.effective_parallel = Some(nonzero(2)?);

        let plan = fx.decide();

        assert!(delegates(&plan).len() <= 1, "{plan:?}");
        Ok(())
    }

    #[test]
    fn test_effective_parallel_larger_than_spec_limit_leaves_the_spec_in_charge() -> TestResult {
        let mut fx = Fixture::new(goal_n(4), report(4, &[0, 1, 2, 3]));
        fx.limits.max_parallel_workers = 2;
        fx.effective_parallel = Some(nonzero(10)?);

        let plan = fx.decide();

        assert_eq!(delegates(&plan).len(), 2, "{plan:?}");
        Ok(())
    }

    #[test]
    fn test_delegate_uses_the_configured_worker_role_instead_of_the_default() -> TestResult {
        let mut fx = Fixture::new(goal_n(1), report(1, &[0]));
        fx.worker_role = Some("executor".to_owned());

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Delegate { role, .. } => {
                assert_eq!(role, "executor");
                assert_ne!(role, DEFAULT_WORKER_ROLE);
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_delegate_skips_criteria_covered_by_a_running_worker() -> TestResult {
        let mut fx = Fixture::new(goal_n(3), report(3, &[0, 1, 2]));
        fx.workers = vec![worker("w1", &[1], &["src/one"], None)];

        let plan = fx.decide();

        let criteria: Vec<Vec<usize>> = delegates(&plan)
            .iter()
            .map(|s| s.criteria.clone())
            .collect();
        assert_eq!(criteria, vec![vec![0], vec![2]], "{plan:?}");
        Ok(())
    }

    #[test]
    fn test_scope_overlapping_a_running_worker_is_rejected() -> TestResult {
        let mut fx = Fixture::new(goal_n(2), report(2, &[0, 1]));
        fx.workers = vec![worker("w1", &[0], &["src/a"], None)];
        fx.hints = vec![scope("h-1", &[1], &["src/a/b.rs"])];

        let plan = fx.decide();

        assert!(delegates(&plan).is_empty(), "{plan:?}");
        if !plan
            .rationale
            .iter()
            .any(|line| line.contains("zurückgestellt"))
        {
            return Err(TestError::Unexpected(format!("{:?}", plan.rationale)));
        }
        Ok(())
    }

    #[test]
    fn test_overlapping_new_scopes_are_merged_into_one_delegate() -> TestResult {
        let mut fx = Fixture::new(goal_n(3), report(3, &[0, 1, 2]));
        fx.hints = vec![
            scope("h-a", &[0], &["src/shared.rs", "src/a.rs"]),
            scope("h-b", &[1], &["./src/shared.rs"]),
            scope("h-c", &[2], &["src/c.rs"]),
        ];

        let plan = fx.decide();

        let scopes = delegates(&plan);
        assert_eq!(scopes.len(), 2, "{plan:?}");
        let merged = scopes
            .iter()
            .find(|s| s.id == "h-a")
            .ok_or(TestError::Missing("verschmolzener Scope h-a"))?;
        assert_eq!(merged.criteria, vec![0, 1]);
        assert_eq!(
            merged.owned_paths,
            vec!["src/a.rs".to_owned(), "src/shared.rs".to_owned()]
        );
        for (i, left) in scopes.iter().enumerate() {
            for right in scopes.iter().skip(i + 1) {
                assert!(!scopes_overlap(left, right), "{left:?} / {right:?}");
            }
        }
        Ok(())
    }

    #[test]
    fn test_partial_worker_is_continued_not_respawned() -> TestResult {
        let mut fx = Fixture::new(goal_n(2), report(2, &[0, 1]));
        fx.workers = vec![
            worker("w1", &[0], &["src/a"], Some(WorkerOutcome::Partial)),
            worker("w2", &[1], &["src/b"], None),
        ];

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Continue {
                worker_id,
                feedback,
            } => {
                assert_eq!(worker_id, "w1");
                assert_eq!(feedback, "Offen: [0] k0");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_respawn_when_attempts_are_exhausted() -> TestResult {
        let mut fx = Fixture::new(goal_n(1), report(1, &[0]));
        let mut w = worker("w1", &[0], &["src/a"], Some(WorkerOutcome::Partial));
        w.attempts = fx.limits.max_attempts_per_worker;
        fx.workers = vec![w];

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Respawn {
                worker_id,
                reason,
                handoff,
            } => {
                assert_eq!(worker_id, "w1");
                assert_eq!(reason, &RespawnReason::AttemptsExhausted { attempts: 3 });
                assert!(handoff.contains("Offen: [0] k0"), "{handoff}");
                assert!(handoff.contains("Bisher: erledigt, was ging"), "{handoff}");
                assert!(
                    handoff.contains("Nur diese Pfade ändern: src/a"),
                    "{handoff}"
                );
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_respawn_when_context_exceeds_threshold() -> TestResult {
        let mut fx = Fixture::new(goal_n(1), report(1, &[0]));
        let mut w = worker("w1", &[0], &["src/a"], Some(WorkerOutcome::Partial));
        w.context_tokens_used = fx.limits.respawn_context_tokens + 1;
        fx.workers = vec![w];

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Respawn { reason, .. } => assert!(
                matches!(reason, RespawnReason::ContextTooLarge { .. }),
                "{reason:?}"
            ),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_hard_failure_is_respawned_with_handoff() -> TestResult {
        let mut fx = Fixture::new(goal_n(1), report(1, &[0]));
        fx.workers = vec![worker(
            "w1",
            &[0],
            &["src/a"],
            Some(WorkerOutcome::Failed {
                reason: "Werkzeug abgestürzt".to_owned(),
            }),
        )];

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Respawn {
                reason, handoff, ..
            } => {
                assert_eq!(
                    reason,
                    &RespawnReason::Failed {
                        reason: "Werkzeug abgestürzt".to_owned()
                    }
                );
                assert!(handoff.contains("Vorgänger scheiterte"), "{handoff}");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_verify_only_after_the_whole_wave_settled() -> TestResult {
        let mut fx = Fixture::new(goal_n(2), report(2, &[0, 1]));
        fx.workers = vec![
            worker("w1", &[0], &["src/a"], Some(WorkerOutcome::Done)),
            worker("w2", &[1], &["src/b"], None),
        ];
        let running = fx.decide();
        assert_eq!(
            count(&running, |s| matches!(s, WorkDriveStep::Verify { .. })),
            0,
            "{running:?}"
        );

        fx.workers[1].last_result = Some(result(WorkerOutcome::Done));
        let settled = fx.decide();
        match only_step(&settled)? {
            WorkDriveStep::Verify { workers } => {
                assert_eq!(workers, &vec!["w1".to_owned(), "w2".to_owned()]);
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_blocked_worker_counts_as_settled_and_escalates() -> TestResult {
        let mut fx = Fixture::new(goal_n(2), report(2, &[0, 1]));
        fx.workers = vec![
            worker("w1", &[0], &["src/a"], Some(WorkerOutcome::Done)),
            worker(
                "w2",
                &[1],
                &["src/b"],
                Some(WorkerOutcome::Blocked {
                    reason: "API-Entscheidung offen".to_owned(),
                }),
            ),
        ];

        let plan = fx.decide();

        match plan.steps.as_slice() {
            [
                WorkDriveStep::Escalate {
                    worker_id,
                    question,
                },
                WorkDriveStep::Verify { .. },
            ] => {
                assert_eq!(worker_id.as_deref(), Some("w2"));
                assert!(question.contains("API-Entscheidung offen"), "{question}");
            }
            _ => return Err(TestError::Unexpected(format!("{plan:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_failed_verification_continues_done_workers_with_routed_evidence() -> TestResult {
        let mut fx = Fixture::new(goal_n(2), report(2, &[0]));
        fx.workers = vec![
            worker("w1", &[0], &["src/a"], Some(WorkerOutcome::Done)),
            worker("w2", &[1], &["src/b"], Some(WorkerOutcome::Done)),
        ];
        fx.verification = VerificationState::Failed {
            failing: vec!["test src/a/lib.rs::x schlägt fehl".to_owned()],
        };

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Continue {
                worker_id,
                feedback,
            } => {
                assert_eq!(worker_id, "w1");
                assert_eq!(
                    feedback,
                    "Offen: [0] k0\nFehlschlag: test src/a/lib.rs::x schlägt fehl"
                );
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_manual_criterion_asks_the_judge_after_green_verification() -> TestResult {
        let mut fx = Fixture::new(
            goal(vec![command_criterion("k0"), manual_criterion("lesbar")]),
            report(2, &[1]),
        );
        fx.workers = vec![worker("w1", &[0, 1], &["src/a"], Some(WorkerOutcome::Done))];
        fx.verification = VerificationState::Passed;

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Judge { criteria, question } => {
                assert_eq!(criteria, &vec![1]);
                assert!(question.contains("lesbar"), "{question}");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_unmet_judge_continues_with_missing_as_feedback() -> TestResult {
        let mut fx = Fixture::new(
            goal(vec![command_criterion("k0"), manual_criterion("lesbar")]),
            report(2, &[1]),
        );
        fx.workers = vec![worker("w1", &[0, 1], &["src/a"], Some(WorkerOutcome::Done))];
        fx.verification = VerificationState::Passed;
        fx.judge = Some(JudgeVerdict {
            passed: false,
            comment: "Doku fehlt".to_owned(),
            missing: vec!["Modulkommentar".to_owned()],
        });

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Continue {
                worker_id,
                feedback,
            } => {
                assert_eq!(worker_id, "w1");
                assert_eq!(
                    feedback,
                    "Bewerter vermisst: Modulkommentar\nOffen: [1] lesbar"
                );
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_judge_verdict_deserializes_new_field_names() -> TestResult {
        let verdict: JudgeVerdict =
            serde_json::from_str(r#"{"passed": true, "comment": "sieht gut aus", "missing": []}"#)
                .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(verdict.passed);
        assert_eq!(verdict.comment, "sieht gut aus");
        assert!(verdict.missing.is_empty());
        Ok(())
    }

    #[test]
    fn test_judge_verdict_deserializes_old_field_names_via_alias() -> TestResult {
        let verdict: JudgeVerdict = serde_json::from_str(
            r#"{"met": false, "rationale": "Doku fehlt", "missing": ["Modulkommentar"]}"#,
        )
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(!verdict.passed);
        assert_eq!(verdict.comment, "Doku fehlt");
        assert_eq!(verdict.missing, vec!["Modulkommentar".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_judge_verdict_minimal_answer_is_only_passed() -> TestResult {
        let verdict: JudgeVerdict = serde_json::from_str(r#"{"passed": false}"#)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(!verdict.passed);
        assert!(verdict.comment.is_empty());
        assert!(verdict.missing.is_empty());
        Ok(())
    }

    #[test]
    fn test_judge_verdict_missing_is_optional() -> TestResult {
        let verdict: JudgeVerdict = serde_json::from_str(r#"{"passed": true, "comment": "ok"}"#)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(verdict.passed);
        assert_eq!(verdict.comment, "ok");
        assert!(verdict.missing.is_empty());
        Ok(())
    }

    #[test]
    fn test_all_met_and_verified_proposes_achieved_without_setting_status() -> TestResult {
        let mut fx = Fixture::new(goal_n(2), report(2, &[]));
        fx.workers = vec![worker("w1", &[0, 1], &["src/a"], Some(WorkerOutcome::Done))];
        fx.verification = VerificationState::Passed;

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::ProposeAchieved { evidence_summary } => {
                assert!(evidence_summary.contains("2 von 2"), "{evidence_summary}");
                assert!(
                    evidence_summary.contains("menschlicher Akteur"),
                    "{evidence_summary}"
                );
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert_eq!(fx.goal.status, GoalStatus::Active);
        Ok(())
    }

    #[test]
    fn test_all_met_but_unverified_verifies_first() -> TestResult {
        let mut fx = Fixture::new(goal_n(2), report(2, &[]));
        fx.workers = vec![worker("w1", &[0, 1], &["src/a"], Some(WorkerOutcome::Done))];

        let plan = fx.decide();

        assert!(
            matches!(only_step(&plan)?, WorkDriveStep::Verify { .. }),
            "{plan:?}"
        );
        Ok(())
    }

    #[test]
    fn test_limits_give_up() -> TestResult {
        type Tweak = fn(&mut Fixture);
        let cases: Vec<(GiveUpReason, Tweak)> = vec![
            (GiveUpReason::IterationLimit, |fx: &mut Fixture| {
                fx.iteration = fx.limits.max_iterations;
            }),
            (GiveUpReason::TokenBudget, |fx: &mut Fixture| {
                fx.usage.tokens_used = fx.limits.token_budget;
            }),
            (GiveUpReason::WallBudget, |fx: &mut Fixture| {
                fx.usage.started_at = now() - fx.limits.wall_budget;
            }),
            (GiveUpReason::Stalled, |fx: &mut Fixture| {
                fx.usage.iterations_without_progress = fx.limits.stall_iterations;
            }),
        ];
        for (expected, arrange) in cases {
            let mut fx = Fixture::new(goal_n(1), report(1, &[0]));
            arrange(&mut fx);
            let plan = fx.decide();
            match only_step(&plan)? {
                WorkDriveStep::GiveUp { reason, .. } => assert_eq!(*reason, expected),
                other => return Err(TestError::Unexpected(format!("{expected:?}: {other:?}"))),
            }
        }
        Ok(())
    }

    #[test]
    fn test_terminal_goal_yields_no_steps() {
        let mut fx = Fixture::new(goal_n(1), report(1, &[0]));
        fx.goal.status = GoalStatus::Achieved;

        let plan = fx.decide();

        assert!(plan.steps.is_empty(), "{plan:?}");
    }

    #[test]
    fn test_same_input_gives_same_plan_regardless_of_worker_order() {
        let mut fx = Fixture::new(goal_n(4), report(4, &[0, 1, 2, 3]));
        fx.workers = vec![
            worker("w2", &[1], &["src/b"], Some(WorkerOutcome::Partial)),
            worker("w1", &[0], &["src/a"], Some(WorkerOutcome::Partial)),
        ];
        fx.hints = vec![
            scope("h-3", &[3], &["src/d"]),
            scope("h-2", &[2], &["src/c"]),
        ];

        let first = fx.decide();
        let second = fx.decide();
        fx.workers.reverse();
        let reordered = fx.decide();

        assert_eq!(first, second);
        assert_eq!(first, reordered);
    }

    fn worker_report(status: WorkerReportStatus) -> WorkerReport {
        WorkerReport {
            status,
            criteria_addressed: vec![0],
            changed_paths: vec!["src/a.rs".to_owned()],
            summary: " a erledigt ".to_owned(),
            blockers: vec![],
        }
    }

    #[test]
    fn test_worker_report_status_maps_onto_outcome() {
        assert_eq!(
            worker_report(WorkerReportStatus::Done)
                .into_summary()
                .outcome,
            WorkerOutcome::Done
        );
        assert_eq!(
            worker_report(WorkerReportStatus::Partial)
                .into_summary()
                .outcome,
            WorkerOutcome::Partial
        );
        let blocked = WorkerReport {
            blockers: vec!["Frage A".to_owned(), " ".to_owned(), "Frage B".to_owned()],
            ..worker_report(WorkerReportStatus::Blocked)
        };
        assert_eq!(
            blocked.into_summary().outcome,
            WorkerOutcome::Blocked {
                reason: "Frage A; Frage B".to_owned()
            }
        );
        let failed = worker_report(WorkerReportStatus::Failed).into_summary();
        assert_eq!(
            failed.outcome,
            WorkerOutcome::Failed {
                reason: "a erledigt".to_owned()
            }
        );
        assert_eq!(failed.summary, "a erledigt");
        assert_eq!(failed.artifacts, vec!["src/a.rs".to_owned()]);
        assert_eq!(failed.suggested_next, None);
    }

    #[test]
    fn test_worker_report_validation_fails_closed() {
        assert_eq!(worker_report(WorkerReportStatus::Done).validate(1), Ok(()));
        assert_eq!(
            worker_report(WorkerReportStatus::Blocked).validate(1),
            Err(WorkerReportError::BlockedWithoutBlocker)
        );
        assert_eq!(
            worker_report(WorkerReportStatus::Done).validate(0),
            Err(WorkerReportError::CriterionOutOfRange { index: 0, total: 0 })
        );
        let empty = WorkerReport {
            summary: "  ".to_owned(),
            ..worker_report(WorkerReportStatus::Done)
        };
        assert_eq!(empty.validate(1), Err(WorkerReportError::EmptySummary));
        for bad in ["/etc/passwd", "../x", "src/../../x", "", "C:\\x"] {
            let with_path = WorkerReport {
                changed_paths: vec![bad.to_owned()],
                ..worker_report(WorkerReportStatus::Done)
            };
            assert!(
                matches!(
                    with_path.validate(1),
                    Err(WorkerReportError::InvalidPath { .. })
                ),
                "{bad:?} muss abgelehnt werden"
            );
        }
    }

    #[test]
    fn test_worker_report_wire_shape_and_unknown_fields() -> TestResult {
        let parsed: WorkerReport = serde_json::from_str(
            r#"{"status":"done","criteria_addressed":[0,2],"changed_paths":["src/a.rs"],"summary":"ok","blockers":[]}"#,
        )
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert_eq!(parsed.status, WorkerReportStatus::Done);
        assert_eq!(parsed.criteria_addressed, vec![0, 2]);
        let minimal: WorkerReport = serde_json::from_str(r#"{"status":"partial","summary":"s"}"#)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(minimal.changed_paths.is_empty() && minimal.blockers.is_empty());
        assert!(
            serde_json::from_str::<WorkerReport>(
                r#"{"status":"done","summary":"s","outcome":"done"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<WorkerReport>(r#"{"status":"finished","summary":"s"}"#).is_err()
        );
        let back =
            serde_json::to_value(&parsed).map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert_eq!(back["status"], serde_json::json!("done"));
        let schema = WorkerReport::input_schema();
        assert_eq!(schema["required"], serde_json::json!(["status", "summary"]));
        Ok(())
    }

    #[test]
    fn test_missing_report_is_partial_with_explicit_reason() {
        let summary = WorkerResultSummary::no_report("  ");
        assert_eq!(summary.outcome, WorkerOutcome::Partial);
        assert_eq!(summary.summary, NO_REPORT_REASON);
        let with_text = WorkerResultSummary::no_report("Status: done");
        assert_eq!(with_text.outcome, WorkerOutcome::Partial);
        assert_eq!(with_text.summary, "no report: Status: done");
        assert!(
            with_text
                .suggested_next
                .as_deref()
                .is_some_and(|next| next.contains(WORK_DRIVER_REPORT_TOOL))
        );
    }

    #[test]
    fn test_worker_report_addressed_criteria_are_sorted_into_the_summary() {
        let report = WorkerReport {
            criteria_addressed: vec![2, 0, 2],
            ..worker_report(WorkerReportStatus::Done)
        };
        assert_eq!(report.into_summary().criteria_addressed, vec![0, 2]);
        assert!(
            WorkerResultSummary::no_report("x")
                .criteria_addressed
                .is_empty()
        );
    }

    #[test]
    fn test_command_only_criteria_get_separate_workspace_scopes() -> TestResult {
        let fx = Fixture::new(goal_n(2), report(2, &[0, 1]));

        let plan = fx.decide();

        let scopes = delegates(&plan);
        assert_eq!(scopes.len(), 2, "{plan:?}");
        for scope in &scopes {
            assert_eq!(scope.owned_paths, vec![WORKSPACE_SCOPE.to_owned()]);
        }
        let task = plan
            .steps
            .iter()
            .find_map(|step| match step {
                WorkDriveStep::Delegate { task, .. } => Some(task.as_str()),
                _ => None,
            })
            .ok_or(TestError::Missing("delegate task"))?;
        assert!(task.contains("## Owned paths\nthe workspace"), "{task}");
        Ok(())
    }

    #[test]
    fn test_a_running_workspace_worker_does_not_defer_a_path_scope() {
        let mut fx = Fixture::new(goal_n(2), report(2, &[0, 1]));
        fx.workers = vec![worker("w1", &[0], &[WORKSPACE_SCOPE], None)];
        fx.hints = vec![scope("h-1", &[1], &["src/b.rs"])];

        let plan = fx.decide();

        let scopes = delegates(&plan);
        assert_eq!(scopes.len(), 1, "{plan:?}");
        assert_eq!(scopes[0].owned_paths, vec!["src/b.rs".to_owned()]);
    }

    #[test]
    fn test_failing_lines_never_match_the_workspace_scope_marker() {
        let wide = worker("w1", &[0], &[WORKSPACE_SCOPE], Some(WorkerOutcome::Done));
        let narrow = worker("w2", &[1], &["src/b.rs"], Some(WorkerOutcome::Done));
        let workers = [&wide, &narrow];
        let failing = vec!["src/b.rs: error".to_owned(), "linker failed.".to_owned()];
        assert_eq!(
            failing_lines_for(&wide, &workers, &failing),
            vec!["Fehlschlag: linker failed.".to_owned()]
        );
        assert_eq!(failing_lines_for(&narrow, &workers, &failing).len(), 2);
    }

    #[test]
    fn test_criteria_a_worker_reports_count_as_covered_and_get_its_feedback() -> TestResult {
        let mut fx = Fixture::new(goal_n(2), report(2, &[0, 1]));
        let mut claimer = worker("w1", &[0], &["src/a"], Some(WorkerOutcome::Partial));
        if let Some(result) = claimer.last_result.as_mut() {
            result.criteria_addressed = vec![1];
        }
        fx.workers = vec![claimer];

        let plan = fx.decide();

        match only_step(&plan)? {
            WorkDriveStep::Continue {
                worker_id,
                feedback,
            } => {
                assert_eq!(worker_id, "w1");
                assert_eq!(feedback, "Offen: [0] k0\nOffen: [1] k1");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_judge_followup_scope_may_write_the_workspace() -> TestResult {
        let mut fx = Fixture::new(goal(vec![manual_criterion("Doku")]), report(1, &[0]));
        fx.verification = VerificationState::Passed;
        fx.judge = Some(JudgeVerdict {
            passed: false,
            comment: "fehlt".to_owned(),
            missing: vec!["Abschnitt".to_owned()],
        });

        let plan = fx.decide();

        let scopes = delegates(&plan);
        let followup = scopes
            .iter()
            .find(|scope| scope.id == JUDGE_FOLLOWUP_SCOPE)
            .ok_or(TestError::Missing("judge-followup scope"))?;
        assert_eq!(followup.owned_paths, vec![WORKSPACE_SCOPE.to_owned()]);
        Ok(())
    }

    fn plan_node(id: &str, write_scope: Vec<PathOrSymbol>, criteria: Vec<Criterion>) -> PlanNode {
        PlanNode {
            id: harw_plan::TaskId::new(id),
            objective: format!("Knoten {id}"),
            dependencies: Vec::new(),
            input_contracts: Vec::new(),
            output_contracts: Vec::new(),
            read_scope: Vec::new(),
            write_scope,
            forbidden_scope: Vec::new(),
            acceptance_criteria: criteria,
            invalidation_conditions: Vec::new(),
            status: harw_plan::PlanNodeStatus::Ready,
            evidence: Vec::new(),
            kind: harw_plan::PlanNodeKind::default(),
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_scope_hints_from_plan_map_write_scopes_onto_goal_criteria() -> TestResult {
        let goal = goal_n(3);
        let by_description = Criterion {
            description: " k0 ".to_owned(),
            verification: Vec::new(),
        };
        let by_verification = Criterion {
            description: "anders formuliert".to_owned(),
            verification: vec![VerificationStep::Command {
                cmd: "check k1".to_owned(),
                expect_exit: 0,
            }],
        };
        let plan = Plan {
            id: harw_plan::PlanId::parse("p-drive")
                .map_err(|e| TestError::Unexpected(e.to_string()))?,
            revision: harw_plan::RevisionId::new(1),
            parent_revision: None,
            goal_statement: "Treiberziel".to_owned(),
            goal_id: Some("g-drive".to_owned()),
            nodes: vec![
                plan_node(
                    "n-a",
                    vec![
                        PathOrSymbol::new("./src/parser/"),
                        PathOrSymbol::new("src/**/*.rs"),
                        PathOrSymbol::symbol("src/lib.rs", "Parser"),
                    ],
                    vec![by_description.clone()],
                ),
                plan_node("n-b", Vec::new(), vec![by_description]),
                plan_node(
                    "n-c",
                    vec![PathOrSymbol::new("docs")],
                    vec![by_verification],
                ),
                plan_node(
                    "n-d",
                    vec![PathOrSymbol::new("src/**")],
                    vec![command_criterion("k2")],
                ),
            ],
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            tenant: None,
        };

        let hints = scope_hints_from_plan(&goal, &plan);

        assert_eq!(
            hints,
            vec![
                WorkScope {
                    id: "plan-n-a".to_owned(),
                    summary: "Knoten n-a".to_owned(),
                    owned_paths: vec!["src/lib.rs".to_owned(), "src/parser".to_owned()],
                    criteria: vec![0],
                },
                WorkScope {
                    id: "plan-n-c".to_owned(),
                    summary: "Knoten n-c".to_owned(),
                    owned_paths: vec!["docs".to_owned()],
                    criteria: vec![1],
                },
            ]
        );

        let mut fx = Fixture::new(goal, report(3, &[0, 1, 2]));
        fx.hints = hints;
        let plan = fx.decide();
        let ids: Vec<&str> = delegates(&plan).iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["plan-n-a", "plan-n-c", "criterion-2"], "{plan:?}");
        Ok(())
    }

    #[test]
    fn test_task_is_structured_and_asks_for_the_report_tool() -> TestResult {
        let mut criteria = vec![command_criterion("k0")];
        criteria.push(Criterion {
            description: "Datei da".to_owned(),
            verification: vec![VerificationStep::Artifact {
                path: "src/a.rs".to_owned(),
            }],
        });
        let fx = Fixture::new(goal(criteria), report(2, &[0, 1]));

        let plan = fx.decide();

        let tasks: Vec<&str> = plan
            .steps
            .iter()
            .filter_map(|step| match step {
                WorkDriveStep::Delegate { task, .. } => Some(task.as_str()),
                _ => None,
            })
            .collect();
        let artifact_task = tasks
            .iter()
            .find(|task| task.contains("[1] Datei da"))
            .ok_or(TestError::Missing("artifact task"))?;
        for needle in [
            "## Goal\nTreiberziel",
            "## Scope `criterion-1`",
            "## Acceptance criteria (index = position in the goal)\n- [1] Datei da\n  verified by: file `src/a.rs` exists",
            "## Owned paths\n- src/a.rs",
            "## Rules\n",
            WORK_DRIVER_REPORT_TOOL,
        ] {
            assert!(
                artifact_task.contains(needle),
                "{needle} missing in:\n{artifact_task}"
            );
        }
        let command_task = tasks
            .iter()
            .find(|task| task.contains("[0] k0"))
            .ok_or(TestError::Missing("command task"))?;
        assert!(
            command_task.contains("verified by: `check k0` exits 0"),
            "{command_task}"
        );
        assert!(
            !command_task.contains("Ziel:"),
            "no German free text: {command_task}"
        );
        assert!(TASK_RULES.contains(WORK_DRIVER_REPORT_TOOL));
        Ok(())
    }

    #[test]
    fn test_paths_overlap_respects_directory_boundaries() {
        assert!(paths_overlap("src/a", "src/a"));
        assert!(paths_overlap("src/a", "src/a/b.rs"));
        assert!(paths_overlap("src/a/b.rs", "src/a"));
        assert!(!paths_overlap("src/a", "src/ab.rs"));
        assert!(!paths_overlap("src/a.rs", "src/b.rs"));
    }
}
