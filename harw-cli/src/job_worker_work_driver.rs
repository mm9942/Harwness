//! Arbeitstreiber-Worker: ein `work_driver`-Job treibt ein Goal in Runden.
//!
//! # Ein Claim = der ganze Lauf, jede Runde durabel
//! `work_driver.enqueue` (`harw_ops::work_driver`) lässt den Job mit
//! `max_attempts = 1` zu, und `work_driver.status`/`work_driver.stop` sprechen
//! genau diese Job-Id an. `JobStore` kennt keinen Übergang „Running → Ready
//! ohne Versuch"; deshalb hält **ein** Claim den ganzen Lauf (der
//! `DurableJobRunner` verlängert die Lease), und jede Runde schreibt den
//! geteilten Zustand ([`WorkDriverState`], `<jobs>/work_driver/<work-id>.json`,
//! über `harw_ops::work_driver::write_state_sidecar`) atomar zurück. Ein
//! Neustart (Reclaim, `unblock` nach einer Eskalation) setzt an der
//! gespeicherten Runde fort. Runden verbrauchen kein Retry-Budget.
//!
//! # Eine Runde ([`drive_round`])
//! Goal und Plan (mandantengefiltert über `ScopedGoalStore`/`ScopedPlanStore`
//! mit `WorkDriverJobInput::tenant`, dem von `work_driver.enqueue` gestempelten
//! Mandanten des Aufrufers; `None` = ungefiltert, Einzelnutzer) → `evaluate_goal` mit der Goal-Evidenz
//! ([`goal_report`]) → [`WorkDriveInput`] (Scope-Hinweise aus dem Plan:
//! `harw_plan_bridge::scope_hints_from_plan`, `write_scope` der Knoten, die
//! ein Goal-Kriterium tragen) → [`WorkDriver::decide`] → Schritte:
//! - `Delegate`/`Continue`/`Respawn` bilden **eine Welle** und laufen
//!   gleichzeitig, höchstens [`RunMemory::parallel_now`] auf einmal
//!   ([`execute_wave`]); vor jedem Block wartet die Welle, solange der
//!   Provider eine Wartezeit meldet ([`ProviderPacing`]). `Continue` schickt
//!   nur das knappe Feedback an **dieselbe** durable Session (Cache-Regel);
//!   `Respawn` bekommt eine rein textuelle Übergabe.
//! - `Verify`: genau ein zentraler Lauf über `spec.verify` plus die
//!   `Command`- und `Artifact`-Schritte des Goals über `VerificationExecutor`;
//!   bestandene Nachweise gehen per `GoalAction::AttachEvidence` an das Goal
//!   (ein `Artifact`-Nachweis als `EvidenceKind::Diff` mit dem Pfad als
//!   Lokator, so wie `evaluate_goal` ihn zuordnet).
//! - `Judge`: ein eigener kleiner Worker ohne Werkzeuge (eigenes Modell über
//!   `InternalModelPoint::WorkDriverJudge`), dessen Gespräch über die Runden
//!   fortgeführt wird (stabiler Präfix `JUDGE_INSTRUCTION` + Kriterien, danach
//!   nur der variable Teil). Eine Antwort ohne auswertbares Urteil wird
//!   höchstens [`MAX_UNPARSABLE_VERDICTS`]-mal in Folge hingenommen (die
//!   nächste Runde fragt erneut, mit dem Hinweis auf das JSON-Format); danach
//!   eskaliert der Lauf mit ausdrücklichem Grund, statt als „nicht
//!   bestanden" weiterzulaufen.
//! - `Verify` an der Workspace-Sperre (`VerifyRunOutcome::Busy`): kein
//!   Fehlschlag; dieselbe Runde läuft nach einer Pause erneut.
//! - `Escalate`: Job endet `Blocked { work_driver:NeedsInput: … }`; nach
//!   `unblock` fließt die Freigabe-Notiz an die blockierten Worker.
//! - `ProposeAchieved`: Job endet `Succeeded` mit dem Vorschlag und dem
//!   Hinweis auf `/goal achieve` — der Status wird **nie** gesetzt.
//! - `GiveUp`: `Failed` mit Grenze und Messwerten.
//!
//! # Rückgabe der Worker (R18 D-E)
//! Jeder Worker-Turn bekommt das Werkzeug `work_driver.report`
//! (`harw_ops::work_driver::WorkerReportToolProvider`, über einen
//! [`ReportToolContributor`] nur in diese Montage gehängt); der letzte gültige
//! Bericht des Turns wird zu `WorkerReport::into_summary`. Fehlt er, gilt
//! `WorkerResultSummary::no_report` (`Partial`, „no report") — Freitext wird
//! nie gedeutet. Die gemeldeten `criteria_addressed` fließen ins Routing von
//! `decide`, nie in den Goal-Status.
//!
//! # Fortschritt (Stillstandsgrenze)
//! Eine Runde nach einer Verifikation zählt als Fortschritt, wenn mehr
//! Kriterien erfüllt sind als bisher bestenfalls **oder** weniger
//! Verifikationsschritte fehlschlagen als bisher bestenfalls
//! ([`RunMemory`]). Der erste Messwert je Claim setzt nur die Basis.
//!
//! # Rechte der Worker (verengt, nie erweitert)
//! - Kein Worker baut oder führt Prozesse aus: schreibende Worker laufen mit
//!   `RegistryProfile::WorkspaceEdit` (nur `fs.*`, `doc.*`, `explore.*`,
//!   Workspace-`deps.*`), lesende mit `ReadOnlyExplore`; die Sandbox trägt
//!   nie `ExecuteProcess` oder `NetworkAccess`.
//! - Die Rolle (`spec.worker_role`) bestimmt mit: eine lesende Rolle
//!   (`harw_ops::kanban::role_access`) schreibt nie; die Anweisungen ihrer
//!   Definition (`AgentRoster::instructions`) stehen im stabilen Präfix des
//!   Workers.
//! - `owned_paths` leer → nur lesen (`{ReadWorkspace}`).
//! - `owned_paths` = Workspace-Scope (`harw_plan_bridge::WORKSPACE_SCOPE`,
//!   Kriterium ohne Pfad) → schreiben überall, wo kein anderer Worker Pfade
//!   besitzt: höchstens ein solcher Worker je gleichzeitig laufendem Block,
//!   die Pfade aller anderen Worker sind für ihn verboten.
//! - `owned_paths` gesetzt → `{ReadWorkspace, WriteWorkspace}`. Die
//!   Rechte-/Sandbox-Schicht kennt **keine** pfadgenaue Schreibfreigabe;
//!   durchgesetzt wird der Schreibbereich deshalb mit dem vorhandenen
//!   Plan-Knoten-Mechanismus (Workspace-Schnappschuss vor/nach jedem
//!   gleichzeitig laufenden Block der Welle, `validate_patch` gegen die
//!   `owned_paths` **dieses Blocks** — nicht die der ganzen Welle, sonst
//!   deckte die erlaubte Vereinigung auch `owned_paths` ab, die erst in
//!   einem späteren oder einem schon abgeschlossenen Block derselben Welle
//!   laufen). Ein Verstoß blockiert die Worker der Welle und eskaliert an
//!   den Menschen (fail closed); Worker, die *gleichzeitig* im selben Block
//!   laufen, bleiben dabei nur über ihre deklarierten `owned_paths`
//!   unterscheidbar (s. o., keine pfadgenaue Sandbox).
//!
//! # Provider-Grenzen
//! Worker laufen im Prozess über **denselben** Provider wie der Rest des
//! Dienstes (gemeinsamer Rate-Limiter). Die Wellenbreite ist
//! `min(spec, Provider-max_concurrency / rate_limit.max_concurrent − 1 Platz
//! für Orchestrator/Bewerter)`, halbiert nach HTTP 429 und +1 nach einer
//! sauberen Welle ([`RunMemory`]); sie geht als `effective_parallel` an
//! `decide` und steht nach jeder Runde in `WorkDriverState::effective_parallel`
//! (`None`, solange sie nicht unter `spec.max_parallel_workers` liegt),
//! die Zahl der 429-Ereignisse (kumulativ) in `WorkDriverState::rate_limited`. Ein 429 ist kein Worker-Fehlschlag: Backoff (Retry-After,
//! exponentiell, gedeckelt), dann Fortsetzung derselben Session ohne
//! Versuchsverbrauch.
//!
//! # Modellunabhängig
//! Fortschritt misst der Treiber nur an generischen Artefakten: dem
//! strukturierten Bericht (`work_driver.report`), geänderten Dateien (aus dem
//! Dateisystem), Verifikation, Bewerterurteil. Andere Werkzeugnamen, -zahlen
//! oder Provider-Traces werden nie gelesen; der Verlauf einer Session bleibt
//! unberührt.
//!
//! # Testbarkeit
//! Die Runde hängt nur an kleinen Traits ([`GoalAccess`], [`WorkerSpawner`],
//! [`VerificationRunner`], [`Judge`], [`WriteScopeGuard`], [`Pacer`],
//! [`ProviderPacing`]); die Produktionsadapter stehen am Dateiende.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use harw_agent_dsl::ir_v2::WorkDriverSpec;
use harw_agent_dsl::roles::AgentRoleId;
use harw_core::{ModelProvider, StateStore, TurnInput, TurnOutcome, run_turn};
use harw_job_runtime::{JobClaim, JobKind, JobOutcome, WorkId};
use harw_ops::kanban::{RoleAccess, role_access};
use harw_ops::work_driver::{
    WORK_DRIVER_INPUT_SCHEMA_VERSION, WORK_DRIVER_STATE_SCHEMA_VERSION, WorkDriverJobInput,
    WorkDriverState, WorkDriverUsage, WorkerReportSlot, WorkerReportToolProvider,
    read_state_sidecar, write_state_sidecar,
};
use harw_plan::admission::{MutationContract, PathRule, RepoRevision, UnifiedDiff, validate_patch};
use harw_plan::goal::{GoalReport, evaluate_goal};
use harw_plan::tenant_scope::{ScopedGoalStore, ScopedPlanStore};
use harw_plan::{
    EvidenceRef, FileGoalStore, Goal, GoalAction, GoalStore, Plan, PlanId, PlanNode, PlanNodeKind,
    PlanNodeStatus, RevisionId, TaskId, VerificationStep,
};
use harw_plan_bridge::verify_exec::{
    NoSandboxRunner, RunVerdict, VerificationExecutor, VerifyConfig, VerifyOutcome, VerifyRun,
    VerifyRunOutcome, VerifyRunner, steps_from_commands,
};
use harw_plan_bridge::work_driver::JUDGE_INSTRUCTION;
use harw_plan_bridge::{
    BudgetUsageSnapshot, GiveUpReason, JudgeVerdict, VerificationState, WORK_DRIVER_REPORT_TOOL,
    WORKSPACE_SCOPE, WorkDriveInput, WorkDriveLimits, WorkDriveStep, WorkDriver, WorkScope,
    WorkerOutcome, WorkerReport, WorkerResultSummary, WorkerState, is_workspace_scope,
    offset_from_timestamp, scope_hints_from_plan,
};
use harw_registry_defaults::profile::{IdentityOverrides, RegistryProfile};
use harw_runtime::RuntimeNarrowing;
use harw_runtime::contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts};
use harw_runtime::job_ledger::WORK_DRIVER_JOB_KIND;
use harw_session_store::JobStore;
use harw_types::{ApprovalActor, ModelId, ProviderId, SessionId, TenantId};
use jiff::{SignedDuration, Timestamp};

use super::{
    BudgetedModelProvider, FileFingerprint, JobRuntimeRoot, JobWorkerContext, MISSING_RUNTIME_ROOT,
    PauseDisposition, PlanNodeServices, PromptTokenLedger, TurnSetup, WorkerExecutionControl,
    assemble_job_turn, assemble_job_turn_with, check_claim_fence, check_input_declared_scope,
    diff_snapshots, job_state_store, last_assistant_text, paused_turn_outcome, sanitize_failure,
    snapshot_workspace,
};
use crate::runtime_jobs::{JobAssemblyInputs, JobEntry, job_assembly, job_principal, job_sandbox};

// ──────────────────────────────────────────────────────────────────────────────
// Konstanten
// ──────────────────────────────────────────────────────────────────────────────

/// Akteur aller Goal-Mutationen (nur Evidenz) und Verifikationsnachweise.
const DRIVER_ACTOR: &str = "runtime:work-driver";

/// Präfix des `Blocked`-Grunds eines Treiber-Jobs.
pub(super) const BLOCK_REASON_PREFIX: &str = "work_driver:";

/// Höchstlänge eines `Blocked`/`Failed`-Grunds in Zeichen.
const MAX_OUTCOME_REASON_CHARS: usize = 600;

/// Höchstlänge einer Worker-Antwort in der Zusammenfassung.
const WORKER_TEXT_CLIP_CHARS: usize = 2_000;

/// Höchstzahl der `stderr`-Zeilen je fehlgeschlagenem Verifikationsbefehl.
const FAILING_TAIL_LINES: usize = 5;

/// Kennung des synthetischen Knotens, der Goal-Evidenz in `evaluate_goal` trägt.
const GOAL_EVIDENCE_NODE: &str = "work-driver-goal-evidence";

/// Plan-Id des leeren Auswertungsplans, wenn das Goal an keinen Plan gebunden ist.
const EVALUATION_PLAN_ID: &str = "work-driver";

/// Basis-Revision der Schreibbereichsprüfung (nur Vorher/Nachher derselben Welle).
const SCOPE_GUARD_REVISION: &str = "work-driver-wave";

/// Plätze, die bei einer Provider-Grenze für Orchestrator/Bewerter frei bleiben.
const RESERVED_PROVIDER_SLOTS: usize = 1;

/// Erneute Versuche einer Welle nach HTTP 429 innerhalb einer Runde.
const MAX_RATE_LIMIT_RETRIES: u32 = 4;

/// Höchstzahl erneuter Abfragen von [`ProviderPacing::pacing_wait`] vor
/// einem Block. Deckelt **nicht** die gemeldete Wartezeit selbst (die wird
/// ungekappt abgewartet), sondern nur die Zahl der Wiederholungen: bricht
/// `Pacer::pause` durch einen Abbruch vorzeitig ab (`CancellablePacer`),
/// meldet der Provider ohne echten Zeitablauf oft weiter eine Wartezeit,
/// sonst würde die Welle spinnen, statt den Abbruch beim nächsten
/// Worker-Start als `WorkerReply::Cancelled` bemerken zu lassen. Bei
/// normalem Zeitablauf (Sekunden bis Stunden pro Wartezeit) ist dieser
/// Deckel praktisch unerreichbar.
const MAX_PACING_POLLS: u32 = 1_000;

/// Basis des exponentiellen Backoffs nach HTTP 429.
const RATE_LIMIT_BASE_BACKOFF_SECS: u64 = 5;

/// Obergrenze eines einzelnen Backoffs nach HTTP 429 (`rate_limit_backoff`).
/// Gilt **nicht** für die proaktive Provider-Taktung ([`ProviderPacing`]):
/// deren gemeldete Wartezeit wird in `execute_wave` ungekappt abgewartet.
const RATE_LIMIT_MAX_BACKOFF_SECS: u64 = 300;

/// Wandzeit eines Laufs ohne eigenes Budget (wie `WorkDriveLimits::default`).
const DEFAULT_RUN_WALL: SignedDuration = SignedDuration::from_hours(4);

/// Grund, mit dem der Verifier ohne Sandbox-Backend jeden Befehl ablehnt.
const NO_VERIFY_SANDBOX: &str = "the job worker has no sandbox backend for verification commands \
(no job coordinator with a Landlock/bwrap executor is wired in); commands are never run unsandboxed";

/// Fester Rückgabevertrag am Ende jeder Erstaufgabe (Teil des stabilen
/// Präfixes, R18 D-E): das Ende jedes Turns ist ein Aufruf von
/// `work_driver.report`; Freitext wird nicht gelesen.
pub(super) const RETURN_CONTRACT: &str = "## Report\nEnd every turn by calling the tool \
`work_driver.report` with: `status` (`done` = scope finished and ready for the central \
verification, `partial` = continue later, `blocked` = a human must decide, `failed` = hard \
failure), `criteria_addressed` (the criterion indices above you worked on), `changed_paths` \
(workspace-relative files you changed), `summary` (short, not empty) and `blockers` (required \
for `blocked`: the concrete questions). An invalid report comes back as a tool error: fix it \
and call again; the last valid call counts. Without a report your turn counts as `partial` with \
the reason \"no report\".";

/// Fortsetzungstext nach einem Provider-Rate-Limit (Verlauf bleibt unberührt).
const RESUME_AFTER_RATE_LIMIT: &str = "Resume: your last attempt ended at a provider rate \
limit. Continue your task and finish again by calling `work_driver.report`.";

/// Aufeinanderfolgende Bewerterantworten ohne auswertbares Urteil, nach
/// denen der Lauf eskaliert (davor fragt die nächste Runde erneut).
const MAX_UNPARSABLE_VERDICTS: u32 = 3;

/// Zusatz der Bewerterfrage nach einer unauswertbaren Antwort.
const JUDGE_FORMAT_REMINDER: &str = "Your previous answer contained no JSON verdict. Answer ONLY \
with {\"passed\": bool, \"comment\": string, \"missing\": [string]}.";

/// Trenner zwischen stabilem Präfix und variablem Teil der Bewerterfrage.
const JUDGE_SEPARATOR: &str = "\n---\n";

/// Ausgabe-Deckel je Modellrunde des Bewerters: das kleinste gültige Urteil
/// ist `{"passed": false}`; mehr als eine knappe Begründung braucht es nie.
const JUDGE_MAX_OUTPUT_TOKENS: u32 = 256;

/// Wartezeit, bevor eine an der Workspace-Sperre gescheiterte Verifikation
/// (`VerifyRunOutcome::Busy`) in der nächsten Runde erneut versucht wird.
const VERIFY_BUSY_RETRY: Duration = Duration::from_secs(30);

// ──────────────────────────────────────────────────────────────────────────────
// Eingabe, Grenzen
// ──────────────────────────────────────────────────────────────────────────────

/// `true` für die Job-Art der Arbeitstreiber-Jobs.
pub(super) fn is_work_driver_kind(kind: &JobKind) -> bool {
    matches!(kind, JobKind::Custom(name) if name == WORK_DRIVER_JOB_KIND)
}

/// Prüft die typisierte Eingabe (`WorkDriverJobInput`) über das Schema hinaus.
///
/// # Errors
/// Falsche Schema-Version oder kein gültiges Goal.
pub(super) fn validate_input(input: &WorkDriverJobInput) -> Result<(), String> {
    if input.schema_version != WORK_DRIVER_INPUT_SCHEMA_VERSION {
        return Err(format!(
            "work-driver input schema {} is not supported (expected {WORK_DRIVER_INPUT_SCHEMA_VERSION})",
            input.schema_version
        ));
    }
    if input.goal_id.trim().is_empty() || input.goal_id.trim() != input.goal_id {
        return Err("work-driver input names no valid goal".to_owned());
    }
    Ok(())
}

/// Leitet die Grenzen des Treibers aus der Spezifikation ab.
///
/// # Description
/// Nicht gesetzte Budgets (`token_budget`, `wall_budget_secs`) fallen auf die
/// Sicherheits-Vorgaben von [`WorkDriveLimits::default`] zurück; der
/// Kontext-Schwellwert für Neustarts kommt immer von dort.
fn limits_from_spec(spec: &WorkDriverSpec) -> WorkDriveLimits {
    let defaults = WorkDriveLimits::default();
    let wall_end = spec
        .wall_budget_secs
        .and_then(|secs| i64::try_from(secs).ok())
        .and_then(|secs| {
            Timestamp::UNIX_EPOCH
                .checked_add(SignedDuration::from_secs(secs))
                .ok()
        });
    let wall_budget = match wall_end {
        Some(end) => offset_from_timestamp(end) - offset_from_timestamp(Timestamp::UNIX_EPOCH),
        None => defaults.wall_budget,
    };
    WorkDriveLimits {
        max_iterations: spec.max_iterations.max(1),
        max_attempts_per_worker: spec.max_attempts_per_worker.max(1),
        max_parallel_workers: spec_parallel(spec),
        token_budget: spec.token_budget.unwrap_or(defaults.token_budget),
        wall_budget,
        stall_iterations: spec.stall_iterations.max(1),
        respawn_context_tokens: defaults.respawn_context_tokens,
    }
}

fn spec_parallel(spec: &WorkDriverSpec) -> usize {
    usize::try_from(spec.max_parallel_workers.max(1)).unwrap_or(1)
}

// ──────────────────────────────────────────────────────────────────────────────
// Laufgedächtnis (nicht persistiert)
// ──────────────────────────────────────────────────────────────────────────────

/// Zustand eines Claims, der nicht im geteilten Sidecar steht.
///
/// # Description
/// Nach einem Neustart beginnt er frisch: die Wellenbreite wieder an der
/// Obergrenze, der Fortschrittsvergleich am aktuellen Stand (ein Neustart
/// zählt nicht als Runde ohne Fortschritt).
#[derive(Debug, Clone)]
pub(super) struct RunMemory {
    /// Start des Laufs (Einreichung des Jobs; Bezug des Wandzeit-Budgets).
    started_at: Timestamp,
    /// Obergrenze der Wellenbreite: `min(spec, Provider − Reserve)`.
    ceiling: usize,
    /// Aktuelle Wellenbreite (AIMD, `1..=ceiling`).
    current: usize,
    /// Größte gesehene Zahl erfüllter Kriterien (`None` bis zur ersten Runde).
    best_met: Option<usize>,
    /// Kleinste gesehene Zahl fehlgeschlagener Verifikationsschritte (`None`
    /// bis zur ersten ausgewerteten Verifikation dieses Claims).
    best_failed_steps: Option<usize>,
    /// Fehlgeschlagene Schritte der letzten Verifikation, noch nicht mit
    /// [`Self::best_failed_steps`] verglichen.
    last_failed_steps: Option<usize>,
    /// Nach einer Verifikation: in der nächsten Runde Fortschritt messen.
    progress_pending: bool,
    /// Bewerterantworten ohne auswertbares Urteil in Folge.
    unparsable_verdicts: u32,
    /// Operator-Antworten für die nächste Fortsetzung eines Workers.
    operator_notes: BTreeMap<String, String>,
    /// Worker, deren letzter Turn am Rate-Limit endete (kein Versuchsverbrauch).
    rate_limit_pending: BTreeSet<String>,
    /// Prompt-/Cache-Tokens je Worker seit Claim-Beginn.
    worker_tokens: BTreeMap<String, (u64, u64)>,
    /// Nächste Worker-Nummer (`None`: aus dem Zustand ableiten).
    next_worker: Option<u32>,
}

impl RunMemory {
    /// Frisches Gedächtnis.
    ///
    /// # Arguments
    /// - `spec`: die Grenzen des Laufs.
    /// - `provider_cap`: `max_concurrency`/`rate_limit.max_concurrent` des
    ///   Providers (das Minimum), `None` wenn nicht gesetzt.
    /// - `started_at`: Einreichung des Jobs.
    pub(super) fn new(
        spec: &WorkDriverSpec,
        provider_cap: Option<usize>,
        started_at: Timestamp,
    ) -> Self {
        let ceiling = parallel_ceiling(spec_parallel(spec), provider_cap);
        Self {
            started_at,
            ceiling,
            current: ceiling,
            best_met: None,
            best_failed_steps: None,
            last_failed_steps: None,
            progress_pending: false,
            unparsable_verdicts: 0,
            operator_notes: BTreeMap::new(),
            rate_limit_pending: BTreeSet::new(),
            worker_tokens: BTreeMap::new(),
            next_worker: None,
        }
    }

    /// Aktuelle Wellenbreite (mindestens 1).
    pub(super) fn parallel_now(&self) -> usize {
        self.current.max(1)
    }

    fn effective_parallel(&self) -> Option<NonZeroUsize> {
        NonZeroUsize::new(self.parallel_now())
    }

    /// Multiplikativ verringern (HTTP 429).
    fn on_rate_limited(&mut self) {
        self.current = (self.current / 2).max(1);
    }

    /// Wertet den Stand zu Rundenbeginn aus (`met` = erfüllte Kriterien).
    ///
    /// # Returns
    /// `Some(true)`/`Some(false)` (Fortschritt ja/nein), wenn seit der
    /// letzten Runde eine Verifikation lief und eine Basis besteht; sonst
    /// `None` (erste Runde des Claims, oder keine Verifikation dazwischen).
    /// Fortschritt ist ein neues Maximum erfüllter Kriterien oder ein neues
    /// Minimum fehlgeschlagener Verifikationsschritte.
    fn record_round(&mut self, met: usize) -> Option<bool> {
        let pending = std::mem::take(&mut self.progress_pending);
        let failed = self.last_failed_steps.take();
        let baseline = self.best_met.is_some();
        let met_improved = self.best_met.is_some_and(|best| met > best);
        self.best_met = Some(self.best_met.map_or(met, |best| best.max(met)));
        let verify_improved = match (self.best_failed_steps, failed) {
            (Some(best), Some(now)) => now < best,
            _ => false,
        };
        if let Some(now) = failed {
            self.best_failed_steps = Some(self.best_failed_steps.map_or(now, |best| best.min(now)));
        }
        (pending && baseline).then_some(met_improved || verify_improved)
    }

    /// Additiv erhöhen (saubere Welle), nie über die Obergrenze.
    fn on_clean_wave(&mut self) {
        self.current = self.current.saturating_add(1).min(self.ceiling);
    }

    /// Wellenbreite für den geteilten Zustand (`WorkDriverState::effective_parallel`):
    /// `Some(n)` nur, wenn Provider-Grenze oder AIMD sie unter
    /// `spec_limit` (`spec.max_parallel_workers`) gedrückt haben, sonst `None`.
    fn published_parallel(&self, spec_limit: usize) -> Option<u32> {
        let now = self.parallel_now();
        if now < spec_limit.max(1) {
            u32::try_from(now).ok()
        } else {
            None
        }
    }
}

/// `min(spec, cap − Reserve)`, mindestens 1; ohne Provider-Grenze nur die Spec.
fn parallel_ceiling(spec: usize, provider_cap: Option<usize>) -> usize {
    let spec = spec.max(1);
    match provider_cap {
        Some(cap) => spec.min(cap.saturating_sub(RESERVED_PROVIDER_SLOTS).max(1)),
        None => spec,
    }
}

/// Backoff nach HTTP 429: `max(Retry-After, Basis · 2^(n−1))`, gedeckelt.
fn rate_limit_backoff(retry_after_secs: u64, attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let exponential = RATE_LIMIT_BASE_BACKOFF_SECS.saturating_mul(1_u64 << exponent);
    Duration::from_secs(
        retry_after_secs
            .max(exponential)
            .min(RATE_LIMIT_MAX_BACKOFF_SECS),
    )
}

// ──────────────────────────────────────────────────────────────────────────────
// Ports (Traits) der Runde
// ──────────────────────────────────────────────────────────────────────────────

/// Ein geboxtes, sendbares Future (objektsichere Trait-Methoden).
pub(super) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Zugriff auf Goal und Plan (mandantengefiltert).
pub(super) trait GoalAccess: Send + Sync {
    /// Das Goal `goal_id`.
    fn goal(&self, goal_id: &str) -> Result<Goal, String>;
    /// Der Plan (`plan_id` wird geprüft, falls gesetzt); `None` ohne Plan.
    fn plan(&self, plan_id: Option<&str>) -> Result<Option<Plan>, String>;
    /// Hängt Nachweise an das Goal (`GoalAction::AttachEvidence`).
    fn attach_evidence(&self, goal_id: &str, evidence: &[EvidenceRef]) -> Result<(), String>;
}

/// Ein Auftrag an einen Worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerRequest {
    /// Worker-Kennung = Fortsetzungs-Handle (Session `work-driver-<id>`).
    pub(super) worker_id: String,
    /// Rolle des Workers.
    pub(super) role: String,
    /// Pfade, die der Worker ändern darf (leer: nur lesen;
    /// [`WORKSPACE_SCOPE`]: alles, was kein anderer Worker besitzt).
    pub(super) owned_paths: Vec<String>,
    /// Der anzuhängende Nutzertext.
    pub(super) text: String,
    /// `true`: Fortsetzung einer bestehenden Session (nur Feedback).
    pub(super) continuation: bool,
    /// Zahl der Akzeptanzkriterien des Goals (Prüfung von
    /// `criteria_addressed` in `work_driver.report`).
    pub(super) criteria_total: usize,
}

/// Wie ein Worker-Turn endete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkerReply {
    /// Abgeschlossen: letzter Assistententext und der letzte gültige
    /// `work_driver.report` des Turns (`None`: der Worker hat nicht
    /// berichtet).
    Completed {
        /// Letzter Assistententext (nur für den „no report"-Fall).
        text: String,
        /// Der Bericht, falls einer vorliegt.
        report: Option<WorkerReport>,
    },
    /// Pausiert (Freigabe/Handoff), im Job nicht fortsetzbar.
    Paused(String),
    /// Gescheitert.
    Failed(String),
    /// Provider-Rate-Limit (HTTP 429) — kein Fehlschlag des Workers.
    RateLimited {
        /// Empfohlene Wartezeit.
        retry_after_secs: u64,
    },
    /// Vom Supervisor abgebrochen.
    Cancelled,
}

/// Verbrauch eines Worker-Turns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct WorkerUsage {
    /// Input + Output über alle Runden.
    pub(super) tokens: u64,
    /// Prompt-Tokens über alle Runden (inkl. Cache).
    pub(super) prompt_tokens: u64,
    /// Davon aus dem Cache gelesen.
    pub(super) cached_tokens: u64,
    /// Kontextgröße der letzten Runde.
    pub(super) context_tokens: u64,
}

/// Ergebnis eines Worker-Turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerRun {
    /// Ausgang.
    pub(super) reply: WorkerReply,
    /// Verbrauch.
    pub(super) usage: WorkerUsage,
}

/// Startet oder setzt Worker fort.
pub(super) trait WorkerSpawner: Send + Sync {
    /// Führt genau einen Turn des Workers aus.
    fn run<'a>(&'a self, request: WorkerRequest) -> BoxFuture<'a, WorkerRun>;
}

/// Gesamturteil einer zentralen Verifikation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VerifyVerdict {
    /// Alles nachgewiesen.
    Passed,
    /// Mindestens ein Schritt widerlegt.
    Failed,
    /// Nichts widerlegt, aber nicht alles nachweisbar.
    Unverifiable,
    /// Eine andere Verifikation hielt die Workspace-Sperre; nichts lief.
    /// Kein Fehlschlag: in der nächsten Runde erneut.
    Busy,
}

/// Ergebnis einer zentralen Verifikation.
#[derive(Debug, Clone)]
pub(super) struct VerificationReport {
    /// Gesamturteil.
    pub(super) verdict: VerifyVerdict,
    /// Fehlschlagzeilen (werden Worker-Feedback).
    pub(super) failing: Vec<String>,
    /// Gründe nicht nachweisbarer Schritte.
    pub(super) unverifiable: Vec<String>,
    /// Erfüllungsnachweise (Lokator von `Command`-Schritten = Befehlstext,
    /// von `Artifact`-Schritten = Pfad).
    pub(super) evidence: Vec<EvidenceRef>,
    /// Zahl der fehlgeschlagenen Schritte (Fortschrittsmaß, siehe
    /// [`RunMemory::record_round`]); `failing` zählt dagegen Zeilen.
    pub(super) failed_steps: usize,
}

/// Führt die zentrale Verifikation aus (`harw_plan_bridge::verify_exec`).
pub(super) trait VerificationRunner: Send + Sync {
    /// Genau ein Lauf über `steps`.
    fn verify<'a>(&'a self, steps: &'a [VerificationStep]) -> BoxFuture<'a, VerificationReport>;
}

/// Eine Bewerterfrage, in stabilen Präfix und variablen Teil getrennt.
///
/// # Description
/// Der Bewerter führt über die Runden **ein** Gespräch: die erste Frage
/// sendet [`Self::first_message`] (Anweisung + Ziel + alle Kriterien +
/// variabler Teil), jede weitere nur [`Self::variable`] — der Präfix bleibt
/// unverändert im Verlauf und damit im Prompt-Cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JudgeRequest {
    /// Stabiler Präfix: `JUDGE_INSTRUCTION`, Rolle, Ziel, alle Kriterien in
    /// Indexreihenfolge.
    pub(super) prefix: String,
    /// Variabler Teil: Frage, Verifikation, Worker-Ergebnisse, Evidenz.
    pub(super) variable: String,
}

impl JudgeRequest {
    /// Die erste Nachricht eines Bewertergesprächs.
    pub(super) fn first_message(&self) -> String {
        format!("{}{JUDGE_SEPARATOR}{}", self.prefix, self.variable)
    }
}

/// Rohantwort des Bewerters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JudgeReply {
    /// Antworttext.
    pub(super) text: String,
    /// Verbrauchte Tokens.
    pub(super) tokens: u64,
}

/// Fehlschlag eines Bewerteraufrufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JudgeError {
    /// Provider-Rate-Limit: später erneut fragen.
    RateLimited {
        /// Empfohlene Wartezeit.
        retry_after_secs: u64,
    },
    /// Bewerter nicht verfügbar (eskaliert).
    Failed(String),
}

/// Der Bewerter.
pub(super) trait Judge: Send + Sync {
    /// Ein einzelner Aufruf.
    fn judge<'a>(
        &'a self,
        request: &'a JudgeRequest,
    ) -> BoxFuture<'a, Result<JudgeReply, JudgeError>>;
}

/// Geänderte Dateien einer Welle, dem Worker zugeordnet, plus Verstöße.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct WaveChanges {
    /// Geänderte Pfade je Worker (nur innerhalb seiner `owned_paths`).
    pub(super) changed: BTreeMap<String, Vec<String>>,
    /// Änderungen außerhalb aller `owned_paths` der Welle.
    pub(super) violations: Vec<String>,
}

/// Setzt den Schreibbereich der Worker einer Welle durch.
pub(super) trait WriteScopeGuard: Send + Sync {
    /// Vor dem Block (Schnappschuss).
    fn before_wave(&self);
    /// Nach dem Block: `owners` = (Worker, `owned_paths`) des gleichzeitig
    /// gelaufenen Blocks, `foreign` = `owned_paths` aller übrigen Worker
    /// des Laufs (für diesen Block verboten).
    fn after_wave(&self, owners: &[(String, Vec<String>)], foreign: &[String]) -> WaveChanges;
}

/// Wartet (Backoff nach Rate-Limit); injiziert, damit Tests nicht schlafen.
pub(super) trait Pacer: Send + Sync {
    /// Wartet `wait`.
    fn pause<'a>(&'a self, wait: Duration) -> BoxFuture<'a, ()>;
}

/// Fragt den Provider, wie lange vor der nächsten Anfrage zu warten ist
/// (`ModelProvider::pacing_wait`: gemeldete Limits, 429-Abkühlung,
/// TPM/RPM-Budgets); injiziert, damit Tests keinen Provider brauchen.
pub(super) trait ProviderPacing: Send + Sync {
    /// Wartezeit vor dem nächsten Start; `None`: sofort.
    fn pacing_wait(&self) -> Option<Duration>;
}

/// Die Ports einer Runde.
pub(super) struct DrivePorts<'a> {
    /// Goal und Plan.
    pub(super) goals: &'a dyn GoalAccess,
    /// Worker.
    pub(super) workers: &'a dyn WorkerSpawner,
    /// Zentrale Verifikation.
    pub(super) verifier: &'a dyn VerificationRunner,
    /// Bewerter.
    pub(super) judge: &'a dyn Judge,
    /// Schreibbereich.
    pub(super) scope_guard: &'a dyn WriteScopeGuard,
    /// Backoff.
    pub(super) pacer: &'a dyn Pacer,
    /// Provider-Taktung vor jedem Worker-Block.
    pub(super) pacing: &'a dyn ProviderPacing,
}

/// Ende einer Runde.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum RoundEnd {
    /// Weiter mit der nächsten Runde.
    Next,
    /// Dieselbe Runde erneut (Verifikation an der Workspace-Sperre); zählt
    /// weder als Runde noch als Runde ohne Fortschritt.
    Repeat,
    /// Der Job endet mit diesem Ausgang.
    Finish(JobOutcome),
}

/// Ende eines Claims.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum RoundsEnd {
    /// Der Lauf ist (vorerst) beendet.
    Finished(JobOutcome),
    /// Nur Tests: die erlaubte Rundenzahl ist erreicht.
    Yielded,
}

/// Wartet auf alle Futures (gleichzeitig gepollt) und liefert die Ergebnisse
/// in Eingabereihenfolge.
async fn join_all<'a, T>(futures: Vec<BoxFuture<'a, T>>) -> Vec<T> {
    let mut slots: Vec<Option<BoxFuture<'a, T>>> = futures.into_iter().map(Some).collect();
    let mut outputs: Vec<Option<T>> = slots.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (slot, output) in slots.iter_mut().zip(outputs.iter_mut()) {
            if let Some(future) = slot.as_mut() {
                match future.as_mut().poll(cx) {
                    Poll::Ready(value) => {
                        *output = Some(value);
                        *slot = None;
                    }
                    Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    outputs.into_iter().flatten().collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Der Lauf: Runden bis zum Ende
// ──────────────────────────────────────────────────────────────────────────────

/// Frischer Zustand eines Laufs.
fn new_state(work_id: &WorkId, now: Timestamp) -> WorkDriverState {
    WorkDriverState {
        schema_version: WORK_DRIVER_STATE_SCHEMA_VERSION,
        work_id: work_id.clone(),
        iteration: 0,
        workers: Vec::new(),
        verification: VerificationState::NotRun,
        usage: WorkDriverUsage::default(),
        last_judge: None,
        last_rationale: Vec::new(),
        effective_parallel: None,
        rate_limited: 0,
        updated_at: now,
    }
}

/// Fährt Runden, bis der Lauf endet (oder `max_rounds` erreicht ist — nur Tests).
///
/// # Description
/// Lädt den Sidecar (oder beginnt neu), wendet eine Operator-Antwort an
/// (Freigabe **nach** dem letzten Schreiben des Zustands = Antwort auf die
/// Eskalation, mit der der Job blockiert wurde), dann je Runde:
/// [`drive_round`], Zustand schreiben, weiter oder enden. Zwischen den
/// Runden wird der Abbruch geprüft.
pub(super) async fn run_rounds(
    job_store: &JobStore,
    work_id: &WorkId,
    input: &WorkDriverJobInput,
    ports: &DrivePorts<'_>,
    memory: &mut RunMemory,
    max_rounds: Option<u32>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> RoundsEnd {
    let loaded = match read_state_sidecar(job_store, work_id) {
        Ok(loaded) => loaded,
        Err(error) => {
            return RoundsEnd::Finished(JobOutcome::Failed {
                reason: format!("work-driver state unreadable: {error}"),
            });
        }
    };
    let resumed = loaded.is_some();
    let mut state = loaded.unwrap_or_else(|| new_state(work_id, Timestamp::now()));
    if state.schema_version != WORK_DRIVER_STATE_SCHEMA_VERSION || state.work_id != *work_id {
        return RoundsEnd::Finished(JobOutcome::Failed {
            reason: "work-driver state belongs to another schema or job".to_owned(),
        });
    }
    if resumed {
        match job_store.get_approval(work_id) {
            Ok(Some(approval)) if approval.approved_at > state.updated_at => {
                apply_operator_answer(&mut state, memory, approval.note);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(work_id = %work_id.as_str(), error = %error, "work-driver approval sidecar unreadable");
            }
        }
    }

    let mut rounds = 0_u32;
    loop {
        if cancelled() {
            return RoundsEnd::Finished(JobOutcome::Cancelled {
                reason: "cancelled by trusted supervisor".to_owned(),
            });
        }
        if max_rounds.is_some_and(|limit| rounds >= limit) {
            return RoundsEnd::Yielded;
        }
        let end = drive_round(input, &mut state, memory, ports, Timestamp::now()).await;
        state.updated_at = Timestamp::now();
        if let Err(error) = write_state_sidecar(job_store, &state) {
            tracing::error!(work_id = %work_id.as_str(), error = %error, "work-driver state could not be written");
            return RoundsEnd::Finished(JobOutcome::Failed {
                reason: format!("work-driver state not written: {error}"),
            });
        }
        rounds = rounds.saturating_add(1);
        match end {
            RoundEnd::Next | RoundEnd::Repeat => {}
            RoundEnd::Finish(outcome) => return RoundsEnd::Finished(outcome),
        }
    }
}

/// Antwort des Operators auf die Eskalation: blockierte Worker werden mit der
/// Notiz fortgesetzt, Verifikation und Urteil der Welle neu eingeholt.
fn apply_operator_answer(
    state: &mut WorkDriverState,
    memory: &mut RunMemory,
    note: Option<String>,
) {
    let note = note
        .filter(|note| !note.trim().is_empty())
        .unwrap_or_else(|| "(freigegeben ohne Notiz)".to_owned());
    for worker in &mut state.workers {
        if let Some(result) = worker.last_result.as_mut() {
            if matches!(result.outcome, WorkerOutcome::Blocked { .. }) {
                result.outcome = WorkerOutcome::Partial;
                memory
                    .operator_notes
                    .insert(worker.worker_id.clone(), note.clone());
            }
        }
    }
    state.verification = VerificationState::NotRun;
    state.last_judge = None;
    state
        .last_rationale
        .push(format!("Operator-Antwort übernommen: {note}"));
}

// ──────────────────────────────────────────────────────────────────────────────
// Eine Runde
// ──────────────────────────────────────────────────────────────────────────────

/// Führt genau eine Treiberrunde aus und aktualisiert `state`/`memory`.
///
/// # Description
/// Siehe Moduldoku. Bei [`RoundEnd::Next`] ist `state.iteration` bereits
/// weitergezählt; bei [`RoundEnd::Finish`] nicht (ein freigegebener
/// `Blocked`-Job führt dieselbe Runde erneut aus).
pub(super) async fn drive_round(
    input: &WorkDriverJobInput,
    state: &mut WorkDriverState,
    memory: &mut RunMemory,
    ports: &DrivePorts<'_>,
    now: Timestamp,
) -> RoundEnd {
    let goal = match ports.goals.goal(&input.goal_id) {
        Ok(goal) => goal,
        Err(reason) => return RoundEnd::Finish(JobOutcome::Failed { reason }),
    };
    let plan = match ports.goals.plan(input.plan_id.as_deref()) {
        Ok(plan) => plan,
        Err(reason) => return RoundEnd::Finish(JobOutcome::Failed { reason }),
    };
    let report = match goal_report(&goal, plan.as_ref()) {
        Ok(report) => report,
        Err(reason) => return RoundEnd::Finish(JobOutcome::Failed { reason }),
    };

    match memory.record_round(report.criteria_met.len()) {
        Some(true) => state.usage.iterations_without_progress = 0,
        Some(false) => {
            state.usage.iterations_without_progress =
                state.usage.iterations_without_progress.saturating_add(1);
        }
        None => {}
    }

    let scope_hints: Vec<WorkScope> = plan
        .as_ref()
        .map(|plan| scope_hints_from_plan(&goal, plan))
        .unwrap_or_default();
    let worker_role = Some(input.spec.worker_role.trim()).filter(|role| !role.is_empty());
    let decided = WorkDriver::decide(&WorkDriveInput {
        goal: &goal,
        report: &report,
        iteration: state.iteration,
        workers: &state.workers,
        scope_hints: &scope_hints,
        usage: BudgetUsageSnapshot {
            tokens_used: state.usage.tokens_used,
            started_at: offset_from_timestamp(memory.started_at),
            iterations_without_progress: state.usage.iterations_without_progress,
        },
        limits: limits_from_spec(&input.spec),
        verification: &state.verification,
        judge: state.last_judge.as_ref(),
        worker_role,
        effective_parallel: memory.effective_parallel(),
        now: offset_from_timestamp(now),
    });
    tracing::info!(
        work_id = %state.work_id.as_str(),
        iteration = state.iteration,
        steps = decided.steps.len(),
        "work-driver decided"
    );
    state.last_rationale = decided.rationale;

    let end = execute_steps(input, &goal, state, memory, ports, decided.steps).await;
    // AIMD-Stand nach der Runde; `rate_limited` zählt `execute_wave`/`run_judge`
    // direkt im Zustand (kumulativ über Neustarts).
    state.effective_parallel = memory.published_parallel(spec_parallel(&input.spec));
    if end == RoundEnd::Next {
        state.iteration = state.iteration.saturating_add(1);
    }
    end
}

/// Führt die Schritte einer Entscheidung aus (Reihenfolge wie `decide`).
async fn execute_steps(
    input: &WorkDriverJobInput,
    goal: &Goal,
    state: &mut WorkDriverState,
    memory: &mut RunMemory,
    ports: &DrivePorts<'_>,
    steps: Vec<WorkDriveStep>,
) -> RoundEnd {
    let questions: Vec<String> = steps
        .iter()
        .filter_map(|step| match step {
            WorkDriveStep::Escalate { question, .. } => Some(question.clone()),
            _ => None,
        })
        .collect();
    if !questions.is_empty() {
        return RoundEnd::Finish(needs_input(&questions.join(" / ")));
    }

    let criteria_total = goal.acceptance_criteria.len();
    let mut wave: Vec<WorkerRequest> = Vec::new();
    let mut after_wave: Vec<WorkDriveStep> = Vec::new();
    for step in steps {
        match step {
            WorkDriveStep::ProposeAchieved { evidence_summary } => {
                state
                    .last_rationale
                    .push(format!("Vorschlag: Ziel erreicht — {evidence_summary}"));
                return RoundEnd::Finish(JobOutcome::Succeeded {
                    result: serde_json::json!({
                        "source": "work-driver",
                        "job_id": state.work_id.as_str(),
                        "goal_id": input.goal_id,
                        "iteration": state.iteration,
                        "proposal": {
                            "status": "achieved",
                            "evidence_summary": evidence_summary,
                        },
                        "summary": format!(
                            "Das Ziel '{}' scheint erreicht. Bitte die Belege prüfen und mit \
                             /goal achieve bestätigen — der Treiber setzt den Status nie selbst.",
                            input.goal_id
                        ),
                    }),
                });
            }
            WorkDriveStep::GiveUp { reason, detail } => {
                return RoundEnd::Finish(JobOutcome::Failed {
                    reason: clip(
                        &format!("work-driver gave up ({}): {detail}", give_up_label(reason)),
                        MAX_OUTCOME_REASON_CHARS,
                    ),
                });
            }
            WorkDriveStep::Escalate { .. } => {}
            WorkDriveStep::Delegate { scope, role, task } => {
                let worker_id = allocate_worker_id(state, memory);
                let owned_paths = scope.owned_paths.clone();
                state.workers.push(WorkerState {
                    worker_id: worker_id.clone(),
                    scope,
                    attempts: 0,
                    last_result: None,
                    context_tokens_used: 0,
                    cache_hit_ratio: None,
                });
                wave.push(WorkerRequest {
                    worker_id,
                    role,
                    owned_paths,
                    text: first_task_text(&task, &input.spec.verify),
                    continuation: false,
                    criteria_total,
                });
            }
            WorkDriveStep::Continue {
                worker_id,
                feedback,
            } => {
                let note = memory.operator_notes.remove(&worker_id);
                let after_rate_limit = memory.rate_limit_pending.remove(&worker_id);
                let Some(worker) = state
                    .workers
                    .iter_mut()
                    .find(|worker| worker.worker_id == worker_id)
                else {
                    tracing::warn!(worker = %worker_id, "work-driver: continue for an unknown worker");
                    continue;
                };
                if !after_rate_limit {
                    worker.attempts = worker.attempts.saturating_add(1);
                }
                let mut text = continue_text(&feedback, note.as_deref());
                if after_rate_limit {
                    text = format!("{RESUME_AFTER_RATE_LIMIT}\n{text}");
                }
                wave.push(WorkerRequest {
                    worker_id,
                    role: worker_role_of(input),
                    owned_paths: worker.scope.owned_paths.clone(),
                    text,
                    continuation: true,
                    criteria_total,
                });
            }
            WorkDriveStep::Respawn {
                worker_id,
                reason,
                handoff,
            } => {
                let note = memory.operator_notes.remove(&worker_id);
                memory.rate_limit_pending.remove(&worker_id);
                let new_id = allocate_worker_id(state, memory);
                let verification = worker_verification_text(&state.verification);
                let Some(worker) = state
                    .workers
                    .iter_mut()
                    .find(|worker| worker.worker_id == worker_id)
                else {
                    tracing::warn!(worker = %worker_id, "work-driver: respawn for an unknown worker");
                    continue;
                };
                tracing::info!(old = %worker_id, new = %new_id, reason = ?reason, "work-driver respawns a worker");
                let scope = worker.scope.clone();
                *worker = WorkerState {
                    worker_id: new_id.clone(),
                    scope: scope.clone(),
                    attempts: 0,
                    last_result: None,
                    context_tokens_used: 0,
                    cache_hit_ratio: None,
                };
                // Reine Textübergabe: nie Provider-Traces des Vorgängers.
                let mut text = format!(
                    "## Handoff from your predecessor\n{handoff}\n\n## Last central verification\n{verification}"
                );
                if let Some(note) = note {
                    text.push_str(&format!("\n\n## Operator answer\n{note}"));
                }
                wave.push(WorkerRequest {
                    worker_id: new_id,
                    role: worker_role_of(input),
                    owned_paths: scope.owned_paths,
                    text: first_task_text(&text, &input.spec.verify),
                    continuation: false,
                    criteria_total,
                });
            }
            step @ (WorkDriveStep::Verify { .. } | WorkDriveStep::Judge { .. }) => {
                after_wave.push(step);
            }
        }
    }

    if let Some(end) = execute_wave(state, memory, ports, wave).await {
        return end;
    }

    for step in after_wave {
        match step {
            WorkDriveStep::Verify { workers } => {
                tracing::info!(
                    workers = workers.len(),
                    "work-driver runs the central verification"
                );
                if let Some(end) = run_verification(input, goal, state, memory, ports).await {
                    return end;
                }
            }
            WorkDriveStep::Judge { criteria, question } => {
                if let Some(end) =
                    run_judge(input, goal, &criteria, &question, state, memory, ports).await
                {
                    return end;
                }
            }
            _ => {}
        }
    }
    RoundEnd::Next
}

/// Nächste freie Worker-Kennung `<work-id>-w<n>` (nach einem Neustart aus den
/// vorhandenen Kennungen abgeleitet: neue Nummern sind immer größer).
fn allocate_worker_id(state: &WorkDriverState, memory: &mut RunMemory) -> String {
    let prefix = format!("{}-w", state.work_id.as_str());
    let next = *memory.next_worker.get_or_insert_with(|| {
        state
            .workers
            .iter()
            .filter_map(|worker| worker.worker_id.strip_prefix(prefix.as_str()))
            .filter_map(|number| number.parse::<u32>().ok())
            .max()
            .map_or(0, |max| max.saturating_add(1))
    });
    memory.next_worker = Some(next.saturating_add(1));
    format!("{prefix}{next}")
}

fn worker_role_of(input: &WorkDriverJobInput) -> String {
    let configured = input.spec.worker_role.trim();
    if configured.is_empty() {
        harw_plan_bridge::DEFAULT_WORKER_ROLE.to_owned()
    } else {
        configured.to_owned()
    }
}

/// Ende des nächsten gleichzeitig laufenden Blocks ab `start`: höchstens
/// `width` Aufträge und höchstens ein Workspace-Worker
/// ([`WORKSPACE_SCOPE`]) — zwei solche Worker gleichzeitig wären in der
/// Schreibbereichsprüfung nicht zu unterscheiden. Mindestens ein Auftrag.
fn chunk_end(pending: &[WorkerRequest], start: usize, width: usize) -> usize {
    let mut end = start;
    let mut has_workspace = false;
    for request in pending.iter().skip(start).take(width.max(1)) {
        let workspace = is_workspace_scope(&request.owned_paths);
        if workspace && has_workspace {
            break;
        }
        has_workspace |= workspace;
        end = end.saturating_add(1);
    }
    end.max(start.saturating_add(1)).min(pending.len())
}

/// `owned_paths` aller Worker des Laufs außerhalb des Blocks `owners` (ohne
/// den Workspace-Scope): für den Block verboten.
fn foreign_paths(state: &WorkDriverState, owners: &[(String, Vec<String>)]) -> Vec<String> {
    let mut foreign: Vec<String> = state
        .workers
        .iter()
        .filter(|worker| !owners.iter().any(|(id, _)| *id == worker.worker_id))
        .flat_map(|worker| worker.scope.owned_paths.iter().cloned())
        .filter(|path| path.trim() != WORKSPACE_SCOPE)
        .collect();
    foreign.sort();
    foreign.dedup();
    foreign
}

/// Führt die Worker-Turns einer Welle aus: gleichzeitig in Blöcken der
/// aktuellen Wellenbreite (höchstens ein Workspace-Worker je Block), 429 mit
/// Backoff und Fortsetzung, danach die Schreibbereichsprüfung je Block.
async fn execute_wave(
    state: &mut WorkDriverState,
    memory: &mut RunMemory,
    ports: &DrivePorts<'_>,
    requests: Vec<WorkerRequest>,
) -> Option<RoundEnd> {
    if requests.is_empty() {
        return None;
    }
    let wave_ids: Vec<String> = requests
        .iter()
        .map(|request| request.worker_id.clone())
        .collect();

    // Schreibbereich je gleichzeitig laufendem Block, nicht je Welle: sonst
    // deckt die erlaubte Vereinigung auch die `owned_paths` von Workern ab,
    // die in einem *anderen* Block derselben Welle laufen (vorher oder
    // nachher, nie gleichzeitig mit diesem) — ein Schreiben dorthin wäre dann
    // unsichtbar (kein Verstoß, stillschweigend dem falschen Besitzer
    // zugeschrieben). Innerhalb *desselben* gleichzeitig laufenden Blocks
    // bleibt eine Zuordnung nur über die deklarierten `owned_paths` möglich
    // (die Sandbox kennt keine pfadgenaue Schreibfreigabe, s. Moduldoku).
    let mut changed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut violations: Vec<String> = Vec::new();

    let mut pending = requests;
    let mut retries = 0_u32;
    let mut wave_rate_limited = false;
    loop {
        let width = memory.parallel_now();
        let mut limited: Vec<WorkerRequest> = Vec::new();
        let mut retry_after = 0_u64;
        let mut index = 0;
        while index < pending.len() {
            // Erst starten, wenn der Provider wieder Kapazität meldet; nach
            // jeder Wartezeit erneut abfragen (das Kontingent füllt sich mit
            // der Zeit weiter), statt die Wartezeit auf den Deckel des
            // reaktiven 429-Backoffs zu kappen — TPM-/RPM-Kontingente sind
            // vertraglich vereinbarte Grenzen (DEC-003), keine Näherung. Der
            // Iterationsdeckel greift nur, wenn `pause` durch einen Abbruch
            // ohne echten Zeitablauf immer wieder vorzeitig endet.
            let mut pacing_polls = 0_u32;
            while let Some(wait) = ports.pacing.pacing_wait().filter(|wait| !wait.is_zero()) {
                ports.pacer.pause(wait).await;
                pacing_polls = pacing_polls.saturating_add(1);
                if pacing_polls >= MAX_PACING_POLLS {
                    break;
                }
            }
            let end = chunk_end(&pending, index, width);
            let chunk = &pending[index..end];
            let chunk_owners: Vec<(String, Vec<String>)> = chunk
                .iter()
                .map(|request| (request.worker_id.clone(), request.owned_paths.clone()))
                .collect();
            let foreign = foreign_paths(state, &chunk_owners);
            ports.scope_guard.before_wave();
            let futures: Vec<BoxFuture<'_, WorkerRun>> = chunk
                .iter()
                .map(|request| ports.workers.run(request.clone()))
                .collect();
            let runs = join_all(futures).await;
            let mut chunk_limited = false;
            for (request, run) in chunk.iter().zip(runs) {
                account_usage(state, memory, &request.worker_id, run.usage);
                match run.reply {
                    WorkerReply::Cancelled => {
                        return Some(RoundEnd::Finish(JobOutcome::Cancelled {
                            reason: "cancelled by trusted supervisor".to_owned(),
                        }));
                    }
                    WorkerReply::RateLimited { retry_after_secs } => {
                        state.rate_limited = state.rate_limited.saturating_add(1);
                        retry_after = retry_after.max(retry_after_secs);
                        chunk_limited = true;
                        limited.push(request.clone());
                    }
                    WorkerReply::Completed { text, report } => {
                        let summary = match report {
                            Some(report) => report.into_summary(),
                            None => {
                                tracing::info!(worker = %request.worker_id, "work-driver worker ended without work_driver.report");
                                WorkerResultSummary::no_report(&clip(
                                    text.trim(),
                                    WORKER_TEXT_CLIP_CHARS,
                                ))
                            }
                        };
                        set_result(state, &request.worker_id, summary);
                    }
                    WorkerReply::Paused(reason) => set_result(
                        state,
                        &request.worker_id,
                        reason_summary(
                            WorkerOutcome::Blocked {
                                reason: reason.clone(),
                            },
                            &reason,
                        ),
                    ),
                    WorkerReply::Failed(reason) => set_result(
                        state,
                        &request.worker_id,
                        reason_summary(
                            WorkerOutcome::Failed {
                                reason: reason.clone(),
                            },
                            &reason,
                        ),
                    ),
                }
            }
            let chunk_changes = ports.scope_guard.after_wave(&chunk_owners, &foreign);
            for (worker_id, paths) in chunk_changes.changed {
                changed.entry(worker_id).or_default().extend(paths);
            }
            violations.extend(chunk_changes.violations);
            index = end;
            if chunk_limited && index < pending.len() {
                // Nicht in dasselbe Limit hineinstarten.
                ports
                    .pacer
                    .pause(rate_limit_backoff(retry_after, retries.saturating_add(1)))
                    .await;
            }
        }
        if limited.is_empty() {
            break;
        }
        wave_rate_limited = true;
        memory.on_rate_limited();
        retries = retries.saturating_add(1);
        if retries > MAX_RATE_LIMIT_RETRIES {
            for request in &limited {
                memory.rate_limit_pending.insert(request.worker_id.clone());
                set_result(
                    state,
                    &request.worker_id,
                    WorkerResultSummary {
                        outcome: WorkerOutcome::Partial,
                        summary: "Provider-Rate-Limit; wird in der nächsten Runde fortgesetzt."
                            .to_owned(),
                        artifacts: Vec::new(),
                        suggested_next: None,
                        criteria_addressed: Vec::new(),
                    },
                );
            }
            break;
        }
        ports
            .pacer
            .pause(rate_limit_backoff(retry_after, retries))
            .await;
        pending = limited
            .into_iter()
            .map(|request| WorkerRequest {
                text: RESUME_AFTER_RATE_LIMIT.to_owned(),
                continuation: true,
                ..request
            })
            .collect();
    }

    // Neue Worker-Arbeit macht Verifikation und Urteil der Welle ungültig.
    state.verification = VerificationState::NotRun;
    state.last_judge = None;

    for (worker_id, paths) in changed {
        if let Some(result) = state
            .workers
            .iter_mut()
            .find(|worker| worker.worker_id == worker_id)
            .and_then(|worker| worker.last_result.as_mut())
        {
            for path in paths {
                if !result.artifacts.contains(&path) {
                    result.artifacts.push(path);
                }
            }
        }
    }
    if !violations.is_empty() {
        let reason = clip(
            &format!(
                "Schreibbereich verletzt: {}. Bitte die Änderungen prüfen, dann freigeben.",
                violations.join("; ")
            ),
            MAX_OUTCOME_REASON_CHARS,
        );
        tracing::error!(
            violations = violations.len(),
            "work-driver wave wrote outside its owned paths"
        );
        for worker in &mut state.workers {
            if wave_ids.contains(&worker.worker_id) {
                let (summary, criteria_addressed) = worker
                    .last_result
                    .as_ref()
                    .map(|result| (result.summary.clone(), result.criteria_addressed.clone()))
                    .unwrap_or_default();
                worker.last_result = Some(WorkerResultSummary {
                    outcome: WorkerOutcome::Blocked {
                        reason: reason.clone(),
                    },
                    summary,
                    artifacts: Vec::new(),
                    suggested_next: None,
                    criteria_addressed,
                });
            }
        }
    }
    if !wave_rate_limited {
        memory.on_clean_wave();
    }
    None
}

/// Bucht Verbrauch und Cache-Quote eines Worker-Turns.
fn account_usage(
    state: &mut WorkDriverState,
    memory: &mut RunMemory,
    worker_id: &str,
    usage: WorkerUsage,
) {
    state.usage.tokens_used = state.usage.tokens_used.saturating_add(usage.tokens);
    let totals = memory
        .worker_tokens
        .entry(worker_id.to_owned())
        .or_insert((0, 0));
    totals.0 = totals.0.saturating_add(usage.prompt_tokens);
    totals.1 = totals.1.saturating_add(usage.cached_tokens);
    let ratio = cache_ratio(totals.1, totals.0);
    if let Some(worker) = state
        .workers
        .iter_mut()
        .find(|worker| worker.worker_id == worker_id)
    {
        worker.context_tokens_used = usage.context_tokens;
        if ratio.is_some() {
            worker.cache_hit_ratio = ratio;
        }
    }
    let (prompt, cached) = memory
        .worker_tokens
        .values()
        .fold((0_u64, 0_u64), |(p, c), (wp, wc)| {
            (p.saturating_add(*wp), c.saturating_add(*wc))
        });
    if let Some(ratio) = cache_ratio(cached, prompt) {
        state.usage.cache_hit_ratio = Some(ratio);
    }
}

fn set_result(state: &mut WorkDriverState, worker_id: &str, summary: WorkerResultSummary) {
    if let Some(worker) = state
        .workers
        .iter_mut()
        .find(|worker| worker.worker_id == worker_id)
    {
        worker.last_result = Some(summary);
    }
}

fn reason_summary(outcome: WorkerOutcome, reason: &str) -> WorkerResultSummary {
    WorkerResultSummary {
        outcome,
        summary: clip(reason, WORKER_TEXT_CLIP_CHARS),
        artifacts: Vec::new(),
        suggested_next: None,
        criteria_addressed: Vec::new(),
    }
}

/// Anteil gecachter Prompt-Tokens (`None` ohne Prompt-Tokens).
fn cache_ratio(cached: u64, prompt: u64) -> Option<f32> {
    if prompt == 0 {
        return None;
    }
    // Verhältnis zweier Zähler; Genauigkeitsverlust f64 → f32 ist gewollt.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let ratio = (cached.min(prompt) as f64 / prompt as f64) as f32;
    Some(ratio.clamp(0.0, 1.0))
}

/// Die zentrale Verifikation einer abgeschlossenen Welle (seriell, einmal).
async fn run_verification(
    input: &WorkDriverJobInput,
    goal: &Goal,
    state: &mut WorkDriverState,
    memory: &mut RunMemory,
    ports: &DrivePorts<'_>,
) -> Option<RoundEnd> {
    let steps = verification_steps(&input.spec, goal);
    memory.progress_pending = true;
    if steps.is_empty() {
        state.verification = VerificationState::Passed;
        return None;
    }
    let report = ports.verifier.verify(&steps).await;
    if report.verdict == VerifyVerdict::Busy {
        // Kein Fehlschlag, kein Stillstand: Stand bleibt `NotRun`, dieselbe
        // Runde läuft nach einer Pause erneut.
        memory.progress_pending = false;
        state.last_rationale.push(format!(
            "verification: busy ({}); erneuter Versuch in {} s",
            report.unverifiable.join("; "),
            VERIFY_BUSY_RETRY.as_secs()
        ));
        ports.pacer.pause(VERIFY_BUSY_RETRY).await;
        return Some(RoundEnd::Repeat);
    }
    let fresh: Vec<EvidenceRef> = report
        .evidence
        .iter()
        .filter(|candidate| {
            !goal.evidence.iter().any(|known| {
                known.kind == candidate.kind
                    && known.locator == candidate.locator
                    && known.digest == candidate.digest
            })
        })
        .cloned()
        .collect();
    if !fresh.is_empty() {
        if let Err(reason) = ports.goals.attach_evidence(&input.goal_id, &fresh) {
            // Fail closed wie jeder andere Verifikationsausgang dieser
            // Funktion: ohne die angehängte Evidenz bliebe das Kriterium in
            // `evaluate_goal` für immer offen und die Runde würde mit
            // identischem Feedback endlos wiederholt, statt den Menschen auf
            // den eigentlichen Fehler (Goal-Speicher) hinzuweisen.
            tracing::warn!(goal = %input.goal_id, reason = %reason, "work-driver could not attach verification evidence");
            state.last_rationale.push(format!(
                "verification: bestandene Nachweise konnten nicht am Goal hinterlegt werden ({reason})"
            ));
            return Some(RoundEnd::Finish(needs_input(&format!(
                "Die zentrale Verifikation ist bestanden, aber die Nachweise ließen sich nicht am \
                 Goal hinterlegen: {reason}. Nach Abhilfe freigeben, dann wird erneut verifiziert."
            ))));
        }
    }
    if matches!(
        report.verdict,
        VerifyVerdict::Passed | VerifyVerdict::Failed
    ) {
        memory.last_failed_steps = Some(report.failed_steps);
    }
    state.verification = match report.verdict {
        VerifyVerdict::Passed => VerificationState::Passed,
        VerifyVerdict::Failed => VerificationState::Failed {
            failing: report.failing,
        },
        VerifyVerdict::Unverifiable => {
            return Some(RoundEnd::Finish(needs_input(&format!(
                "Die zentrale Verifikation ließ sich nicht ausführen: {}. Nach Abhilfe \
                 freigeben, dann wird erneut verifiziert.",
                report.unverifiable.join("; ")
            ))));
        }
        VerifyVerdict::Busy => VerificationState::NotRun,
    };
    None
}

/// Ein Bewerteraufruf; 429 → Backoff und später erneut, Ausfall → Eskalation.
///
/// # Description
/// Eine Antwort ohne auswertbares Urteil ([`parse_verdict`] `None`) ist kein
/// „nicht bestanden": das Urteil bleibt offen, die nächste Runde fragt erneut
/// (mit [`JUDGE_FORMAT_REMINDER`]). Nach [`MAX_UNPARSABLE_VERDICTS`] solchen
/// Antworten in Folge eskaliert der Lauf mit ausdrücklichem Grund.
async fn run_judge(
    input: &WorkDriverJobInput,
    goal: &Goal,
    criteria: &[usize],
    question: &str,
    state: &mut WorkDriverState,
    memory: &mut RunMemory,
    ports: &DrivePorts<'_>,
) -> Option<RoundEnd> {
    let mut request = judge_request(&input.spec, goal, criteria, question, state);
    if memory.unparsable_verdicts > 0 {
        request.variable = format!("{JUDGE_FORMAT_REMINDER}\n{}", request.variable);
    }
    match ports.judge.judge(&request).await {
        Ok(reply) => {
            state.usage.tokens_used = state.usage.tokens_used.saturating_add(reply.tokens);
            match parse_verdict(&reply.text) {
                Some(verdict) => {
                    memory.unparsable_verdicts = 0;
                    state.last_judge = Some(verdict);
                    None
                }
                None => {
                    memory.unparsable_verdicts = memory.unparsable_verdicts.saturating_add(1);
                    state.last_judge = None;
                    let excerpt = clip(reply.text.trim(), MAX_OUTCOME_REASON_CHARS / 2);
                    state.last_rationale.push(format!(
                        "judge: kein auswertbares Urteil ({} von {MAX_UNPARSABLE_VERDICTS}): {excerpt}",
                        memory.unparsable_verdicts
                    ));
                    if memory.unparsable_verdicts >= MAX_UNPARSABLE_VERDICTS {
                        return Some(RoundEnd::Finish(needs_input(&format!(
                            "Der Bewerter lieferte {MAX_UNPARSABLE_VERDICTS}-mal in Folge kein \
                             auswertbares Urteil (erwartet: JSON mit `passed`); zuletzt: \
                             {excerpt}. Bewerter-Modell prüfen, dann freigeben."
                        ))));
                    }
                    None
                }
            }
        }
        Err(JudgeError::RateLimited { retry_after_secs }) => {
            state.rate_limited = state.rate_limited.saturating_add(1);
            memory.on_rate_limited();
            state.last_judge = None;
            ports
                .pacer
                .pause(rate_limit_backoff(retry_after_secs, 1))
                .await;
            None
        }
        Err(JudgeError::Failed(reason)) => Some(RoundEnd::Finish(needs_input(&format!(
            "Der Bewerter ist nicht verfügbar: {reason}"
        )))),
    }
}

/// `spec.verify` plus die `Command`- und `Artifact`-Schritte aller Kriterien
/// und Invarianten, in dieser Reihenfolge, ohne doppelte Befehle bzw. Pfade.
///
/// `TraceEvent`- und `Manual`-Schritte fehlen bewusst: der Executor meldet
/// sie immer als nicht nachweisbar (das eskalierte jeden Lauf); manuelle
/// Kriterien entscheidet der Bewerter.
pub(super) fn verification_steps(spec: &WorkDriverSpec, goal: &Goal) -> Vec<VerificationStep> {
    let mut steps: Vec<VerificationStep> = Vec::new();
    let from_goal = goal
        .acceptance_criteria
        .iter()
        .flat_map(|criterion| criterion.verification.iter())
        .chain(
            goal.invariants
                .iter()
                .flat_map(|invariant| invariant.verification.iter()),
        )
        .cloned();
    for step in steps_from_commands(&spec.verify)
        .into_iter()
        .chain(from_goal)
    {
        let known = match &step {
            VerificationStep::Command { cmd, .. } => steps.iter().any(|existing| {
                matches!(existing, VerificationStep::Command { cmd: other, .. } if other == cmd)
            }),
            VerificationStep::Artifact { path } => steps.iter().any(|existing| {
                matches!(existing, VerificationStep::Artifact { path: other } if other == path)
            }),
            VerificationStep::TraceEvent { .. } | VerificationStep::Manual { .. } => continue,
        };
        if !known {
            steps.push(step);
        }
    }
    steps
}

/// `evaluate_goal` über den Plan **und** die direkt am Goal hängende Evidenz.
///
/// # Description
/// `evaluate_goal` liest nur Evidenz abgeschlossener Plan-Knoten; die
/// Verifikationsnachweise des Treibers hängen am Goal. Sie werden deshalb als
/// synthetischer, abgeschlossener Knoten in eine Kopie des Plans (oder einen
/// leeren Auswertungsplan mit dem Mandanten des Goals) gelegt — die
/// Zuordnungsregeln bleiben exakt die von `evaluate_goal`.
///
/// # Errors
/// Nur, wenn der leere Auswertungsplan nicht gebaut werden kann.
pub(super) fn goal_report(goal: &Goal, plan: Option<&Plan>) -> Result<GoalReport, String> {
    let mut evaluated = match plan {
        Some(plan) => plan.clone(),
        None => Plan {
            id: PlanId::parse(EVALUATION_PLAN_ID).map_err(|error| error.to_string())?,
            revision: RevisionId::new(0),
            parent_revision: None,
            goal_statement: goal.statement.clone(),
            goal_id: Some(goal.id.as_str().to_owned()),
            nodes: Vec::new(),
            created_at: goal.created_at,
            updated_at: goal.updated_at,
            tenant: goal.tenant.clone(),
        },
    };
    if !goal.evidence.is_empty() {
        let stamp = evaluated.updated_at;
        evaluated.nodes.push(PlanNode {
            id: TaskId::new(GOAL_EVIDENCE_NODE),
            objective: "Evidenz direkt am Goal".to_owned(),
            dependencies: Vec::new(),
            input_contracts: Vec::new(),
            output_contracts: Vec::new(),
            read_scope: Vec::new(),
            write_scope: Vec::new(),
            forbidden_scope: Vec::new(),
            acceptance_criteria: Vec::new(),
            invalidation_conditions: Vec::new(),
            status: PlanNodeStatus::Completed,
            evidence: goal.evidence.clone(),
            kind: PlanNodeKind::default(),
            wave: None,
            assignment: None,
            parent: None,
            created_at: stamp,
            updated_at: stamp,
        });
    }
    Ok(evaluate_goal(goal, &evaluated))
}

// ──────────────────────────────────────────────────────────────────────────────
// Texte und Parser
// ──────────────────────────────────────────────────────────────────────────────

/// Erste Nachricht eines (neuen) Workers: Aufgabe, die zentralen
/// Verifikationsbefehle der Spec (falls vorhanden) und der feste
/// Rückgabevertrag.
fn first_task_text(task: &str, verify: &[String]) -> String {
    let commands: Vec<&str> = verify
        .iter()
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .collect();
    if commands.is_empty() {
        return format!("{task}\n\n{RETURN_CONTRACT}");
    }
    format!(
        "{task}\n\n## Central verification (the driver runs these, not you)\n- `{}`\n\n{RETURN_CONTRACT}",
        commands.join("`\n- `")
    )
}

/// Fortsetzung: **nur** Feedback (und ggf. die Operator-Antwort), nie das Ziel.
pub(super) fn continue_text(feedback: &str, note: Option<&str>) -> String {
    let mut text = format!("## Driver feedback\n{feedback}");
    if let Some(note) = note {
        text.push_str(&format!("\n\n## Operator answer\n{note}"));
    }
    text.push_str(&format!(
        "\n\nContinue within your scope and finish again by calling `{WORK_DRIVER_REPORT_TOOL}`."
    ));
    text
}

fn give_up_label(reason: GiveUpReason) -> &'static str {
    match reason {
        GiveUpReason::IterationLimit => "iteration limit",
        GiveUpReason::TokenBudget => "token budget",
        GiveUpReason::WallBudget => "wall-clock budget",
        GiveUpReason::Stalled => "stalled",
    }
}

/// `Blocked`-Ausgang „Eingabe nötig" mit der Frage.
fn needs_input(question: &str) -> JobOutcome {
    JobOutcome::Blocked {
        reason: clip(
            &format!("{BLOCK_REASON_PREFIX}NeedsInput: {question}"),
            MAX_OUTCOME_REASON_CHARS,
        ),
    }
}

/// Kürzt auf höchstens `max` Zeichen (mit `…`).
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// Baut die Bewerterfrage: stabiler Präfix (`JUDGE_INSTRUCTION`, Rolle, Ziel,
/// alle Kriterien in Indexreihenfolge) und variabler Teil (Frage ohne die
/// Anweisung, Verifikation, Worker-Ergebnisse, Evidenz).
pub(super) fn judge_request(
    spec: &WorkDriverSpec,
    goal: &Goal,
    criteria: &[usize],
    question: &str,
    state: &WorkDriverState,
) -> JudgeRequest {
    let role = spec.judge_role.as_deref().unwrap_or("judge");
    let mut prefix = format!(
        "{JUDGE_INSTRUCTION}\nRolle: {role}\nZiel: {}\nAkzeptanzkriterien:",
        goal.statement
    );
    for (idx, criterion) in goal.acceptance_criteria.iter().enumerate() {
        prefix.push_str(&format!("\n[{idx}] {}", criterion.description));
    }
    // `decide` stellt die Anweisung voran; sie steht bereits im Präfix.
    let question = question
        .strip_prefix(JUDGE_INSTRUCTION)
        .unwrap_or(question)
        .trim();
    let mut variable = format!("Zu bewerten: {criteria:?}\nFrage: {question}\n");
    variable.push_str(&format!(
        "Verifikation: {}\n",
        verification_label(&state.verification)
    ));
    variable.push_str("Worker-Ergebnisse:");
    for worker in &state.workers {
        if let Some(result) = &worker.last_result {
            variable.push_str(&format!(
                "\n- {} (Scope {}): {:?}; {}",
                worker.worker_id, worker.scope.id, result.outcome, result.summary
            ));
            if !result.artifacts.is_empty() {
                variable.push_str(&format!(
                    " Geänderte Dateien: {}",
                    result.artifacts.join(", ")
                ));
            }
        }
    }
    let evidence: Vec<String> = goal
        .evidence
        .iter()
        .map(|evidence| format!("{} {}", evidence.kind, evidence.locator))
        .collect();
    if !evidence.is_empty() {
        variable.push_str(&format!("\nEvidenz am Goal: {}", evidence.join("; ")));
    }
    JudgeRequest { prefix, variable }
}

/// Verifikationsstand für den Text an einen Worker (die Aufgabe ist englisch).
fn worker_verification_text(state: &VerificationState) -> String {
    match state {
        VerificationState::NotRun => "not run yet".to_owned(),
        VerificationState::Passed => "passed".to_owned(),
        VerificationState::Failed { failing } => format!("failed:\n- {}", failing.join("\n- ")),
    }
}

fn verification_label(state: &VerificationState) -> String {
    match state {
        VerificationState::NotRun => "nicht gelaufen".to_owned(),
        VerificationState::Passed => "grün".to_owned(),
        VerificationState::Failed { failing } => format!("rot: {}", failing.join("; ")),
    }
}

/// Alle JSON-Objekte im Text in Reihenfolge ihres Beginns (Klammern in
/// Zeichenketten zählen nicht).
fn json_objects(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut objects = Vec::new();
    let mut start = 0;
    while let Some(offset) = text.get(start..).and_then(|rest| rest.find('{')) {
        let open = start + offset;
        let mut depth = 0_usize;
        let mut in_string = false;
        let mut escaped = false;
        let mut close = None;
        for (index, byte) in bytes.iter().enumerate().skip(open) {
            if in_string {
                match byte {
                    _ if escaped => escaped = false,
                    b'\\' => escaped = true,
                    b'"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' => depth = depth.saturating_add(1),
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        close = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        match close {
            Some(close) => {
                if let Some(object) = text.get(open..=close) {
                    objects.push(object);
                }
                start = open + 1;
            }
            None => break,
        }
    }
    objects
}

/// Deutet ein JSON-Objekt als Urteil: `passed`, `met` oder `verified`
/// (bool), dazu `comment`/`rationale` und optional `missing`.
fn verdict_from_object(object: &str) -> Option<JudgeVerdict> {
    let value: serde_json::Value = serde_json::from_str(object).ok()?;
    let map = value.as_object()?;
    let passed = ["passed", "met", "verified"]
        .iter()
        .find_map(|key| map.get(*key).and_then(serde_json::Value::as_bool))?;
    let comment = ["comment", "rationale", "reason"]
        .iter()
        .find_map(|key| map.get(*key).and_then(serde_json::Value::as_str))
        .unwrap_or_default();
    let missing = map
        .get("missing")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(JudgeVerdict {
        passed,
        comment: clip(comment.trim(), MAX_OUTCOME_REASON_CHARS),
        missing,
    })
}

/// Parst das Urteil tolerant und modellunabhängig, fail closed.
///
/// # Description
/// Das erste JSON-Objekt mit `passed`/`met`/`verified` (bool) gewinnt. Ohne
/// ein solches Objekt `None`: kein Urteil — weder „bestanden" noch still
/// „nicht bestanden" ([`run_judge`] fragt begrenzt erneut, dann Eskalation).
/// Ein Klartext-Marker-Scan auf bloße Wörter wie „PASSED"/„FAILED" träfe
/// auch verneinte Formulierungen (z. B. „hat … noch nicht PASSED") und wird
/// deshalb nicht versucht.
pub(super) fn parse_verdict(text: &str) -> Option<JudgeVerdict> {
    json_objects(text).into_iter().find_map(verdict_from_object)
}

/// Übersetzt einen Verifikationslauf in einen [`VerificationReport`].
///
/// # Description
/// Nachweise bestandener Schritte werden so gestellt, wie `evaluate_goal`
/// sie zuordnet: `Command` mit dem Befehlstext als Lokator (der Executor
/// vergibt `verify-command:<cmd>` bzw. den Runner-Lokator), `Artifact` als
/// `EvidenceKind::Diff` mit dem Pfad des
/// Schritts als Lokator (der Executor vergibt `Other` und
/// `workspace:<pfad>`); der Inhalts-Digest bleibt erhalten.
pub(super) fn report_from_run(run: &VerifyRun) -> VerificationReport {
    let mut failing = Vec::new();
    let mut unverifiable = Vec::new();
    let mut evidence = Vec::new();
    let mut failed_steps = 0_usize;
    for report in &run.steps {
        let label = step_label(&report.step);
        match &report.outcome {
            VerifyOutcome::Passed { evidence: found } => {
                let mut found = found.clone();
                match &report.step {
                    VerificationStep::Command { cmd, .. } => found.locator = cmd.clone(),
                    VerificationStep::Artifact { path } => {
                        found.kind = harw_plan::EvidenceKind::Diff;
                        found.locator = path.clone();
                    }
                    VerificationStep::TraceEvent { .. } | VerificationStep::Manual { .. } => {}
                }
                evidence.push(found);
            }
            VerifyOutcome::Failed { failure, .. } => {
                failed_steps = failed_steps.saturating_add(1);
                failing.push(format!("{label}: {failure}"));
                if let Some(trace) = &report.trace {
                    let tail = String::from_utf8_lossy(&trace.stderr_tail);
                    let lines: Vec<&str> = tail
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect();
                    let skip = lines.len().saturating_sub(FAILING_TAIL_LINES);
                    failing.extend(
                        lines
                            .into_iter()
                            .skip(skip)
                            .map(|line| format!("{label}: {line}")),
                    );
                }
            }
            VerifyOutcome::Unverifiable { reason } => {
                unverifiable.push(format!("{label}: {reason}"));
            }
        }
    }
    if run.skipped > 0 {
        unverifiable.push(format!(
            "{} Schritte nach frühem Abbruch übersprungen",
            run.skipped
        ));
    }
    let verdict = match run.verdict() {
        RunVerdict::Passed => VerifyVerdict::Passed,
        RunVerdict::Failed => VerifyVerdict::Failed,
        RunVerdict::Unverifiable => VerifyVerdict::Unverifiable,
    };
    VerificationReport {
        verdict,
        failing,
        unverifiable,
        evidence,
        failed_steps,
    }
}

fn step_label(step: &VerificationStep) -> String {
    match step {
        VerificationStep::Command { cmd, .. } => cmd.clone(),
        VerificationStep::Artifact { path } => format!("artifact {path}"),
        VerificationStep::TraceEvent { name } => format!("trace {name}"),
        VerificationStep::Manual { note } => format!("manual {note}"),
    }
}

/// Regeln für die `owned_paths` eines Scopes: die Datei selbst oder alles
/// darunter; der Workspace-Scope ([`WORKSPACE_SCOPE`]) erlaubt alles
/// (`**`) — was davon anderen Workern gehört, verbietet
/// [`attribute_changes`] über `foreign`.
fn owned_rules(owned_paths: &[String]) -> Vec<PathRule> {
    owned_paths
        .iter()
        .map(|path| path.trim())
        .flat_map(|path| {
            if path == WORKSPACE_SCOPE {
                return vec![PathRule::Glob("**".to_owned())];
            }
            let path = path.trim_start_matches("./").trim_end_matches('/');
            if path.is_empty() {
                return Vec::new();
            }
            vec![
                PathRule::Exact(path.to_owned()),
                PathRule::DirectoryPrefix(path.to_owned()),
            ]
        })
        .collect()
}

/// Ordnet geänderte Pfade den Workern eines gleichzeitig gelaufenen Blocks
/// zu und meldet Verstöße.
///
/// # Description
/// - Ein Pfad in den konkreten `owned_paths` genau eines Workers gehört ihm;
///   trifft er mehrere, ist das fail closed ein Verstoß (`owned_paths` sollen
///   laut Vertrag disjunkt sein) statt einer stillen Mehrfachzuschreibung.
/// - Ein Pfad außerhalb aller konkreten `owned_paths` gehört dem einen
///   Workspace-Worker des Blocks ([`WORKSPACE_SCOPE`]); gibt es mehrere, ist
///   er nicht zuzuordnen (Verstoß).
/// - Verstöße prüft `harw_plan::admission::validate_patch`: erlaubt ist die
///   Vereinigung der Block-Regeln, verboten sind die `owned_paths` aller
///   übrigen Worker des Laufs (`foreign`, Vorrang vor „erlaubt").
pub(super) fn attribute_changes(
    diff: &UnifiedDiff,
    owners: &[(String, Vec<String>)],
    foreign: &[String],
) -> WaveChanges {
    let mut changed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ambiguous: Vec<String> = Vec::new();
    let concrete = |owned: &[String]| -> Vec<String> {
        owned
            .iter()
            .filter(|path| path.trim() != WORKSPACE_SCOPE)
            .cloned()
            .collect()
    };
    let workspace_owners: Vec<String> = owners
        .iter()
        .filter(|(_, owned)| is_workspace_scope(owned))
        .map(|(worker_id, _)| worker_id.clone())
        .collect();
    for file in &diff.files {
        let matched: Vec<String> = owners
            .iter()
            .filter(|(_, owned)| {
                owned_rules(&concrete(owned.as_slice()))
                    .iter()
                    .any(|rule| rule.matches(&file.path))
            })
            .map(|(worker_id, _)| worker_id.clone())
            .collect();
        let candidates: Vec<String> = if matched.is_empty() {
            workspace_owners.clone()
        } else {
            matched
        };
        match candidates.as_slice() {
            [] => {}
            [worker_id] => {
                changed
                    .entry(worker_id.clone())
                    .or_default()
                    .push(file.path.clone());
            }
            many => {
                ambiguous.push(format!(
                    "'{}' liegt in mehreren owned_paths der Welle ({})",
                    file.path,
                    many.join(", ")
                ));
            }
        }
    }
    let contract = MutationContract {
        base_revision: diff.base_revision.clone(),
        plan_revision: RevisionId::new(0),
        task_id: TaskId::new(SCOPE_GUARD_REVISION),
        allowed_paths: owners
            .iter()
            .flat_map(|(_, owned)| owned_rules(owned))
            .collect(),
        forbidden_paths: owned_rules(&concrete(foreign)),
    };
    let report = validate_patch(&contract, diff);
    let mut violations: Vec<String> = report
        .violations
        .iter()
        .map(|violation| match violation {
            harw_plan::admission::ScopeViolation::OutsideAllowed { path } => {
                format!("'{path}' liegt außerhalb der owned_paths der Welle")
            }
            harw_plan::admission::ScopeViolation::Forbidden { path, .. } => {
                format!("'{path}' gehört einem anderen Worker des Laufs")
            }
            harw_plan::admission::ScopeViolation::BaseRevisionMismatch { .. } => {
                "Schnappschüsse passen nicht zusammen".to_owned()
            }
        })
        .collect();
    violations.extend(ambiguous);
    // Ein verbotener Pfad ist als Verstoß gemeldet und wird niemandem als
    // Artefakt zugeschrieben (er fiele sonst an den Workspace-Worker).
    for paths in changed.values_mut() {
        paths.retain(|path| {
            !report.violations.iter().any(|violation| {
                matches!(violation, harw_plan::admission::ScopeViolation::Forbidden { path: bad, .. } if bad == path)
            })
        });
    }
    changed.retain(|_, paths| !paths.is_empty());
    WaveChanges {
        changed,
        violations,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Produktions-Einstieg
// ──────────────────────────────────────────────────────────────────────────────

/// Führt einen geclaimten `work_driver`-Job aus (siehe Moduldoku).
pub(super) async fn execute_work_driver_claim(
    claim: JobClaim,
    input: serde_json::Value,
    provider: Arc<dyn ModelProvider>,
    job_store: Arc<JobStore>,
    plan_services: Option<Arc<PlanNodeServices>>,
    control: Arc<WorkerExecutionControl>,
    context: &JobWorkerContext,
) -> JobOutcome {
    let work_id = claim.job.id.clone();
    if let Err(reason) =
        check_claim_fence(&claim).and_then(|()| check_input_declared_scope(&claim.scope, &input))
    {
        tracing::warn!(work_id = %work_id.as_str(), reason = %reason, "work-driver job rejected before any model call");
        return JobOutcome::Failed { reason };
    }
    let record = match job_store.get(&work_id) {
        Ok(record) => record,
        Err(error) => {
            return JobOutcome::Failed {
                reason: sanitize_failure(&format!("work-driver job unreadable: {error}")),
            };
        }
    };
    let driver_input = match WorkDriverJobInput::from_job(&record)
        .map_err(|_| "work-driver input is malformed".to_owned())
        .and_then(|driver_input| validate_input(&driver_input).map(|()| driver_input))
    {
        Ok(driver_input) => driver_input,
        Err(reason) => {
            tracing::warn!(work_id = %work_id.as_str(), reason = %reason, "work-driver input rejected");
            return JobOutcome::Failed { reason };
        }
    };
    let Some(runtime_root) = context.runtime_root.as_ref() else {
        return JobOutcome::Blocked {
            reason: MISSING_RUNTIME_ROOT.to_owned(),
        };
    };
    let goals = match open_goal_store(&runtime_root.cwd, &driver_input.goal_id) {
        Ok(goals) => goals,
        Err(reason) => {
            tracing::warn!(work_id = %work_id.as_str(), reason = %reason, "work-driver goal store unavailable");
            return JobOutcome::Blocked {
                reason: clip(
                    &format!("{BLOCK_REASON_PREFIX}{reason}"),
                    MAX_OUTCOME_REASON_CHARS,
                ),
            };
        }
    };
    let submitter = match claim.scope.submitter() {
        ApprovalActor::Operator { id } if super::is_scope_identifier(id) => id.clone(),
        _ => "work-driver".to_owned(),
    };

    // Budgets: Token-Ledger des Jobs (von `work_driver.enqueue` aus der Spec
    // gespiegelt) für alle Modellrunden des Laufs; die Wandzeit bindet jeden
    // Worker-Turn an das Laufende.
    let ledger = PromptTokenLedger::new(claim.job.budget.clone(), claim.job.usage.clone());
    let budgeted: Arc<dyn ModelProvider> = Arc::new(BudgetedModelProvider {
        inner: Arc::clone(&provider),
        ledger,
    });
    let run_wall = claim.job.budget.max_wall.unwrap_or(DEFAULT_RUN_WALL);
    let elapsed = Timestamp::now().duration_since(record.submitted_at);
    let remaining =
        std::time::Duration::try_from(run_wall.saturating_sub(elapsed)).unwrap_or_default();
    let started = tokio::time::Instant::now();
    let deadline = started
        .checked_add(remaining)
        .or_else(|| {
            started.checked_add(Duration::from_secs(
                DEFAULT_RUN_WALL.as_secs().unsigned_abs(),
            ))
        })
        .unwrap_or(started);

    let config = load_run_config(
        context,
        runtime_root,
        &job_store,
        &provider,
        &submitter,
        &work_id,
        &worker_role_of(&driver_input),
    );
    let mut memory = RunMemory::new(&driver_input.spec, config.provider_cap, record.submitted_at);
    let guard = match WorkspaceScopeGuard::new(runtime_root, context, &job_store) {
        Ok(guard) => guard,
        Err(reason) => {
            return JobOutcome::Blocked {
                reason: clip(
                    &format!("{BLOCK_REASON_PREFIX}{reason}"),
                    MAX_OUTCOME_REASON_CHARS,
                ),
            };
        }
    };
    let goal_access = StoreGoalAccess::for_input(goals, plan_services, &driver_input);
    let spawner = JobTurnSpawner {
        context,
        runtime_root,
        job_store: Arc::clone(&job_store),
        provider: Arc::clone(&budgeted),
        submitter: submitter.clone(),
        control: Arc::clone(&control),
        deadline,
        role_instructions: config.worker_instructions.clone(),
    };
    let sandboxed = context
        .verify_runner
        .as_ref()
        .map(|runner| ExecutorVerifier {
            executor: VerificationExecutor::new(
                runner.clone(),
                VerifyConfig::new(runtime_root.cwd.clone(), DRIVER_ACTOR),
            ),
        });
    let fallback;
    let verifier: &dyn VerificationRunner = match sandboxed.as_ref() {
        Some(sandboxed) => sandboxed,
        None => {
            fallback = fallback_verifier(runtime_root, &work_id);
            &fallback
        }
    };
    let judge_model: Arc<dyn ModelProvider> = match config.judge {
        Some((provider_id, model)) => Arc::new(harw_core::PinnedModelProvider::new(
            Arc::clone(&budgeted),
            provider_id.as_deref().map(ProviderId::from),
            Some(ModelId::from(model.as_str())),
        )),
        None => Arc::clone(&budgeted),
    };
    let judge_provider: Arc<dyn ModelProvider> = Arc::new(OutputCappedProvider {
        inner: judge_model,
        max_output_tokens: JUDGE_MAX_OUTPUT_TOKENS,
    });
    let judge = ConversationJudge {
        context,
        runtime_root,
        job_store: Arc::clone(&job_store),
        provider: judge_provider,
        submitter: submitter.clone(),
        session_id: SessionId::from_str(format!("work-driver-{}-judge", work_id.as_str())),
        control: Arc::clone(&control),
        deadline,
    };
    let pacer = CancellablePacer {
        control: Arc::clone(&control),
    };
    // Derselbe Provider wie die Worker (die Budget-Hülle reicht durch).
    let pacing = ModelPacing(Arc::clone(&budgeted));
    let ports = DrivePorts {
        goals: &goal_access,
        workers: &spawner,
        verifier,
        judge: &judge,
        scope_guard: &guard,
        pacer: &pacer,
        pacing: &pacing,
    };
    let cancellation = control.cancellation();
    let cancelled = move || *cancellation.borrow();
    match run_rounds(
        &job_store,
        &work_id,
        &driver_input,
        &ports,
        &mut memory,
        None,
        &cancelled,
    )
    .await
    {
        RoundsEnd::Finished(outcome) => outcome,
        RoundsEnd::Yielded => JobOutcome::Failed {
            reason: "work-driver stopped without an outcome".to_owned(),
        },
    }
}

/// Was der Lauf aus der Konfiguration braucht.
struct RunConfig {
    /// Eigene Provider-/Modell-Wahl des Bewerters
    /// (`InternalModelPoint::WorkDriverJudge`); `None`: Hauptmodell.
    judge: Option<(Option<String>, String)>,
    /// Minimum aus `max_concurrency` und `rate_limit.max_concurrent` des
    /// Providers, über den die Worker laufen.
    provider_cap: Option<usize>,
    /// Anweisungen der Worker-Rolle aus ihrer Definition; `None` ohne
    /// Definition bzw. ohne Anweisungen.
    worker_instructions: Option<String>,
}

/// Liest Bewerter-Modell und Provider-Grenze aus einer werkzeuglosen
/// Prompt-Montage (dieselbe Konfiguration, die jeder Job-Turn sieht).
///
/// Alle Worker laufen über den Provider des Dienstes (`ModelSource::Override`);
/// seine Grenzen gelten, nicht die einer Rollen-Modell-Wahl. Aus derselben
/// Montage kommen die Anweisungen der Worker-Rolle (`worker_role`).
fn load_run_config(
    context: &JobWorkerContext,
    runtime_root: &JobRuntimeRoot,
    job_store: &Arc<JobStore>,
    provider: &Arc<dyn ModelProvider>,
    submitter: &str,
    work_id: &WorkId,
    worker_role: &str,
) -> RunConfig {
    let assembly = job_assembly(JobAssemblyInputs {
        entry: JobEntry::Prompt,
        home: &runtime_root.home,
        cwd: &runtime_root.cwd,
        principal: job_principal(submitter),
        session_id: SessionId::from_str(format!("work-driver-{}-config", work_id.as_str())),
        state_store: job_state_store(&context.transcript_root),
        job_store: Arc::clone(job_store),
        model: Arc::clone(provider),
        narrowing: None,
    });
    let (config, worker_instructions) = match assembly {
        Ok(assembly) => (
            Arc::clone(assembly.config()),
            assembly
                .agent_roster()
                .instructions(worker_role)
                .map(str::to_owned),
        ),
        Err(error) => {
            tracing::warn!(error = %error, "work-driver could not read the configuration");
            return RunConfig {
                judge: None,
                provider_cap: None,
                worker_instructions: None,
            };
        }
    };
    let provider_id = provider
        .pinned_provider_id()
        .or_else(|| config.harness.default_provider.clone());
    let provider_cap = provider_id
        .as_deref()
        .and_then(|id| config.providers.get(id))
        .and_then(|toml| {
            let concurrent = toml
                .rate_limit
                .as_ref()
                .and_then(|limit| limit.max_concurrent)
                .and_then(|value| usize::try_from(value).ok());
            match (toml.max_concurrency, concurrent) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        });
    let resolved = harw_config::resolve_internal_model(
        &config,
        harw_config::InternalModelPoint::WorkDriverJudge,
    );
    let judge = match (resolved.is_main_model(), resolved.model) {
        (false, Some(model)) => Some((resolved.provider, model)),
        _ => None,
    };
    RunConfig {
        judge,
        provider_cap,
        worker_instructions,
    }
}

/// Öffnet den Goal-Store des Projekts unter `cwd`: `goals/<goal_id>`, falls
/// vorhanden, sonst der Standardbereich (`goals/default`).
fn open_goal_store(cwd: &Path, goal_id: &str) -> Result<Arc<dyn GoalStore>, String> {
    let project = harw_home::project::discover_project(cwd, &[])
        .map_err(|error| format!("no project for the goal store: {error}"))?;
    let goals_dir = harw_home::project::ProjectHome::at(&project).goals_dir();
    let safe_id = !goal_id.is_empty()
        && goal_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    let dedicated = goals_dir.join(goal_id);
    let root = if safe_id && dedicated.is_dir() {
        dedicated
    } else {
        goals_dir.join(crate::DEFAULT_GOAL_SPACE)
    };
    let store =
        FileGoalStore::new(&root).map_err(|error| format!("goal store unusable: {error}"))?;
    Ok(Arc::new(store))
}

/// [`GoalAccess`] über Goal-Store und (optional) Plan-Store, beide nur durch
/// die Mandanten-Sicht des Aufrufers (`ScopedGoalStore`/`ScopedPlanStore`).
struct StoreGoalAccess {
    goals: Arc<dyn GoalStore>,
    plan: Option<Arc<PlanNodeServices>>,
    /// Sicht: `WorkDriverJobInput::tenant`; `None` = ungefiltert (Einzelnutzer,
    /// mandantenlose Goals bleiben sichtbar).
    tenant: Option<TenantId>,
}

impl StoreGoalAccess {
    /// Sicht mit dem Mandanten, den `work_driver.enqueue` vom Aufrufer in die
    /// Eingabe gestempelt hat — nie der Mandant des Job-Scopes.
    fn for_input(
        goals: Arc<dyn GoalStore>,
        plan: Option<Arc<PlanNodeServices>>,
        input: &WorkDriverJobInput,
    ) -> Self {
        Self {
            goals,
            plan,
            tenant: input.tenant.clone(),
        }
    }
}

impl GoalAccess for StoreGoalAccess {
    fn goal(&self, goal_id: &str) -> Result<Goal, String> {
        let scoped = ScopedGoalStore::new(self.goals.as_ref(), self.tenant.clone());
        let goal = scoped
            .current()
            .map_err(|error| format!("work-driver: goal unavailable: {error}"))?;
        if goal.id.as_str() != goal_id {
            return Err(format!(
                "work-driver: the goal store holds goal '{}', not '{goal_id}'",
                goal.id
            ));
        }
        Ok(goal)
    }

    fn plan(&self, plan_id: Option<&str>) -> Result<Option<Plan>, String> {
        let Some(services) = self.plan.as_ref() else {
            return match plan_id {
                Some(id) => Err(format!(
                    "work-driver: plan '{id}' requested, but this worker has no plan store"
                )),
                None => Ok(None),
            };
        };
        let scoped = ScopedPlanStore::new(services.plan(), self.tenant.clone());
        match (harw_plan::PlanStore::current(&scoped), plan_id) {
            (Ok(plan), Some(id)) if plan.id.as_str() != id => Err(format!(
                "work-driver: the plan store holds plan '{}', not '{id}'",
                plan.id.as_str()
            )),
            (Ok(plan), _) => Ok(Some(plan)),
            (Err(_), None) => Ok(None),
            (Err(error), Some(id)) => Err(format!("work-driver: plan '{id}' unavailable: {error}")),
        }
    }

    fn attach_evidence(&self, _goal_id: &str, evidence: &[EvidenceRef]) -> Result<(), String> {
        let scoped = ScopedGoalStore::new(self.goals.as_ref(), self.tenant.clone());
        for item in evidence {
            scoped
                .apply(
                    GoalAction::AttachEvidence {
                        evidence: item.clone(),
                    },
                    DRIVER_ACTOR,
                )
                .map_err(|error| format!("evidence not attached: {error}"))?;
        }
        Ok(())
    }
}

/// Zählt Token-Nutzung je Modellrunde mit (inkl. Cache-Tokens) und merkt sich
/// ein Provider-Rate-Limit (HTTP 429).
#[derive(Debug, Default, Clone, Copy)]
struct UsageMeter {
    tokens: u64,
    prompt_tokens: u64,
    cached_tokens: u64,
    last_prompt_tokens: u64,
    rate_limited: Option<u64>,
}

/// Ein [`ModelProvider`], der die gemeldete Usage jeder Runde mitschreibt.
struct MeteredModelProvider {
    inner: Arc<dyn ModelProvider>,
    meter: Arc<Mutex<UsageMeter>>,
}

impl MeteredModelProvider {
    fn new(inner: Arc<dyn ModelProvider>) -> (Self, Arc<Mutex<UsageMeter>>) {
        let meter = Arc::new(Mutex::new(UsageMeter::default()));
        (
            Self {
                inner,
                meter: Arc::clone(&meter),
            },
            meter,
        )
    }
}

fn meter_snapshot(meter: &Mutex<UsageMeter>) -> UsageMeter {
    match meter.lock() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

impl ModelProvider for MeteredModelProvider {
    fn respond<'a>(&'a self, request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
        Box::pin(async move {
            let result = self.inner.respond(request).await;
            let mut guard = match self.meter.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match &result {
                Ok(response) => {
                    let usage = &response.usage;
                    guard.tokens = guard.tokens.saturating_add(usage.total());
                    guard.prompt_tokens = guard.prompt_tokens.saturating_add(usage.prompt_tokens());
                    guard.cached_tokens = guard
                        .cached_tokens
                        .saturating_add(usage.cached_tokens.unwrap_or(0));
                    guard.last_prompt_tokens = usage.prompt_tokens();
                }
                Err(harw_core::ModelError::RateLimited {
                    retry_after_secs, ..
                }) => {
                    guard.rate_limited =
                        Some(guard.rate_limited.unwrap_or(0).max(*retry_after_secs));
                }
                Err(_) => {}
            }
            drop(guard);
            result
        })
    }

    fn pinned_model_id(&self) -> Option<String> {
        self.inner.pinned_model_id()
    }

    fn pinned_provider_id(&self) -> Option<String> {
        self.inner.pinned_provider_id()
    }

    fn pacing_wait(&self) -> Option<Duration> {
        self.inner.pacing_wait()
    }
}

/// Ein [`ModelProvider`], der jede Anfrage auf `max_output_tokens` deckelt
/// (Bewerter: Lesen ist Cache, nur Ausgabe kostet).
struct OutputCappedProvider {
    inner: Arc<dyn ModelProvider>,
    max_output_tokens: u32,
}

impl ModelProvider for OutputCappedProvider {
    fn respond<'a>(&'a self, request: harw_core::ModelRequest) -> harw_core::ModelFuture<'a> {
        self.inner
            .respond(request.with_max_output_tokens(Some(self.max_output_tokens)))
    }

    fn pinned_model_id(&self) -> Option<String> {
        self.inner.pinned_model_id()
    }

    fn pinned_provider_id(&self) -> Option<String> {
        self.inner.pinned_provider_id()
    }

    fn pacing_wait(&self) -> Option<Duration> {
        self.inner.pacing_wait()
    }
}

/// Session-Id eines Workers (Fortsetzungs-Handle).
fn worker_session_id(worker_id: &str) -> SessionId {
    SessionId::from_str(format!("work-driver-{worker_id}"))
}

/// Bricht den Turn-Task ab, wenn der Claim-Future fallen gelassen wird.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// [`WorkerSpawner`] über durable Job-Sessions im Prozess (gemeinsamer
/// Provider und damit gemeinsamer Rate-Limiter).
struct JobTurnSpawner<'a> {
    context: &'a JobWorkerContext,
    runtime_root: &'a JobRuntimeRoot,
    job_store: Arc<JobStore>,
    provider: Arc<dyn ModelProvider>,
    submitter: String,
    control: Arc<WorkerExecutionControl>,
    deadline: tokio::time::Instant,
    /// Anweisungen der Worker-Rolle aus ihrer Definition
    /// (`AgentRoster::instructions`), falls vorhanden.
    role_instructions: Option<String>,
}

impl WorkerSpawner for JobTurnSpawner<'_> {
    fn run<'a>(&'a self, request: WorkerRequest) -> BoxFuture<'a, WorkerRun> {
        Box::pin(self.run_turn(request))
    }
}

impl JobTurnSpawner<'_> {
    async fn run_turn(&self, request: WorkerRequest) -> WorkerRun {
        tracing::debug!(
            worker = %request.worker_id,
            continuation = request.continuation,
            writes = !request.owned_paths.is_empty(),
            "work-driver worker turn"
        );
        let (metered, meter) = MeteredModelProvider::new(Arc::clone(&self.provider));
        // Eine Ablage je Turn: nur ein Bericht *dieses* Turns zählt.
        let slot = Arc::new(WorkerReportSlot::new());
        let reply = match self.prepare(&request, Arc::new(metered), &slot) {
            Ok((setup, model)) => {
                drive_worker_turn(
                    setup,
                    worker_session_id(&request.worker_id),
                    request.text,
                    model,
                    &self.control,
                    self.deadline,
                )
                .await
            }
            Err(reason) => WorkerReply::Failed(reason),
        };
        let reply = match reply {
            WorkerReply::Completed { text, .. } => WorkerReply::Completed {
                text,
                report: slot.take(),
            },
            other => other,
        };
        let usage = meter_snapshot(&meter);
        let reply = match (reply, usage.rate_limited) {
            (WorkerReply::Failed(_), Some(retry_after_secs)) => {
                WorkerReply::RateLimited { retry_after_secs }
            }
            (reply, _) => reply,
        };
        WorkerRun {
            reply,
            usage: WorkerUsage {
                tokens: usage.tokens,
                prompt_tokens: usage.prompt_tokens,
                cached_tokens: usage.cached_tokens,
                context_tokens: usage.last_prompt_tokens,
            },
        }
    }

    /// Montiert die Runtime des Worker-Turns, verengt auf Lesen bzw.
    /// Lesen + Schreiben (nur mit `owned_paths` und schreibender Rolle), ohne
    /// Prozess-, Netz- oder Host-Rechte, plus `work_driver.report` über
    /// `slot` ([`ReportToolContributor`], nur in dieser Montage).
    fn prepare(
        &self,
        request: &WorkerRequest,
        model: Arc<dyn ModelProvider>,
        slot: &Arc<WorkerReportSlot>,
    ) -> Result<(TurnSetup, Arc<dyn ModelProvider>), String> {
        let writes = !request.owned_paths.is_empty()
            && !matches!(role_access(&request.role), RoleAccess::ReadOnly);
        let (entry, profile) = if writes {
            (
                JobEntry::PlanNode {
                    kind: PlanNodeKind::Coding,
                    may_write: true,
                },
                RegistryProfile::WorkspaceEdit,
            )
        } else {
            (
                JobEntry::PlanNode {
                    kind: PlanNodeKind::Explore,
                    may_write: false,
                },
                RegistryProfile::ReadOnlyExplore,
            )
        };
        let sandbox = job_sandbox(entry, &self.runtime_root.cwd)?;
        let writable: &[String] = if writes { &request.owned_paths } else { &[] };
        let narrowing = RuntimeNarrowing {
            registry_profile: profile,
            identity: worker_identity(
                &request.role,
                profile,
                writable,
                self.role_instructions.as_deref(),
            ),
            permissions: sandbox.permissions().clone(),
            workspace_root: Some(sandbox.workspace().canonical_root().to_path_buf()),
        };
        let report_tool: Arc<dyn AssemblyContributor> = Arc::new(ReportToolContributor {
            provider: Arc::new(WorkerReportToolProvider::new(
                Arc::clone(slot),
                request.criteria_total,
            )),
        });
        assemble_job_turn_with(
            JobAssemblyInputs {
                entry,
                home: &self.runtime_root.home,
                cwd: sandbox.workspace().canonical_root(),
                principal: job_principal(&self.submitter),
                session_id: worker_session_id(&request.worker_id),
                state_store: job_state_store(&self.context.transcript_root),
                job_store: Arc::clone(&self.job_store),
                model,
                narrowing: Some(narrowing),
            },
            PauseDisposition::Blocked,
            Some(&sandbox),
            vec![report_tool],
        )
    }
}

/// Hängt `work_driver.report` in die Montage **eines** WorkDriver-Worker-Turns
/// (R18 D-E). Kein Registry-Profil und keine andere Job-Art bekommt das
/// Werkzeug; der Beitrag vergibt keine Rechte (das Werkzeug schreibt nur in
/// seine [`WorkerReportSlot`]).
struct ReportToolContributor {
    provider: Arc<WorkerReportToolProvider>,
}

impl AssemblyContributor for ReportToolContributor {
    fn contribute(
        &self,
        _inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> harw_runtime::RuntimeResult<()> {
        let concrete = Arc::clone(&self.provider);
        let provider: Arc<dyn harw_extension_api::ToolProvider> = concrete;
        parts.registry = std::mem::take(&mut parts.registry).tool_provider(provider);
        Ok(())
    }
}

/// Identität eines Worker-Agenten; stabil je Rolle und Scope (Cache-Präfix).
///
/// # Arguments
/// - `owned_paths`: der wirksame Schreibbereich (leer: nur lesen).
/// - `instructions`: Anweisungen der Rollendefinition; stehen vorn im
///   stabilen Präfix.
fn worker_identity(
    role: &str,
    profile: RegistryProfile,
    owned_paths: &[String],
    instructions: Option<&str>,
) -> IdentityOverrides {
    let write_rule = if owned_paths.is_empty() {
        "You may only read the workspace; do not change any file.".to_owned()
    } else if is_workspace_scope(owned_paths) {
        "You may change files anywhere in the workspace except paths other workers own; every          change is checked after the wave and a change in a foreign path blocks the whole run."
            .to_owned()
    } else {
        format!(
            "Change only these paths: {}. Changes elsewhere block the whole run.",
            owned_paths.join(", ")
        )
    };
    let mut extra_context: Vec<String> = instructions
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("Instructions of your role '{role}':\n{text}"))
        .into_iter()
        .collect();
    extra_context.extend([
        "You run unattended inside a durable job. There is no user: an approval request ends your turn as blocked."
            .to_owned(),
        write_rule,
        "Never build or run tests: one central verification runs per wave over the combined state."
            .to_owned(),
        format!(
            "Finish every turn by calling the tool `{WORK_DRIVER_REPORT_TOOL}`; a status line in your text is not read."
        ),
    ]);
    IdentityOverrides {
        agent_name: Some(format!("work-driver-{role}")),
        role_description: Some(format!(
            "{} working one scope of a work-driver run as role '{role}'",
            profile.role_description()
        )),
        extra_context,
        organizational_role: Some(AgentRoleId::Worker),
    }
}

/// Wartet, bis die Abbruch-Anforderung gesetzt ist (nie, wenn der Sender fehlt).
async fn cancellation_requested(mut cancellation: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *cancellation.borrow() {
            return;
        }
        if cancellation.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Ein Worker-Turn in seiner durablen Session, mit Wanduhr und Abbruch. Der
/// Verlauf der Session wird nie gelesen oder verändert, nur der letzte
/// Assistententext.
async fn drive_worker_turn(
    setup: TurnSetup,
    session_id: SessionId,
    prompt: String,
    model: Arc<dyn ModelProvider>,
    control: &Arc<WorkerExecutionControl>,
    deadline: tokio::time::Instant,
) -> WorkerReply {
    let TurnSetup {
        mut session,
        state_store,
        pause,
    } = setup;
    let durable_store: Arc<dyn StateStore> = Arc::clone(&state_store);
    let mut turn = tokio::spawn(async move {
        run_turn(
            &mut session,
            model.as_ref(),
            state_store.as_ref(),
            TurnInput::user(prompt),
        )
        .await
    });
    let _abort = AbortOnDrop(turn.abort_handle());
    let joined = tokio::select! {
        joined = &mut turn => Some(joined),
        () = tokio::time::sleep_until(deadline) => None,
        () = cancellation_requested(control.cancellation()) => {
            turn.abort();
            let _ = turn.await;
            return WorkerReply::Cancelled;
        }
    };
    let Some(joined) = joined else {
        turn.abort();
        let _ = turn.await;
        return WorkerReply::Failed("budget: wall-clock limit of the run exceeded".to_owned());
    };
    match joined {
        Ok(Ok(TurnOutcome::Completed)) => match durable_store.load_history(&session_id).await {
            Ok(history) => WorkerReply::Completed {
                text: last_assistant_text(&history),
                report: None,
            },
            Err(error) => WorkerReply::Failed(sanitize_failure(&error.to_string())),
        },
        Ok(Ok(paused)) => match paused_turn_outcome(&paused, pause) {
            JobOutcome::Blocked { reason } | JobOutcome::Failed { reason } => {
                WorkerReply::Paused(reason)
            }
            JobOutcome::Cancelled { .. } => WorkerReply::Cancelled,
            JobOutcome::Succeeded { .. } => {
                WorkerReply::Failed("worker turn paused without a reason".to_owned())
            }
        },
        Ok(Err(error)) => WorkerReply::Failed(sanitize_failure(&error.to_string())),
        Err(error) if error.is_cancelled() => WorkerReply::Cancelled,
        Err(error) => WorkerReply::Failed(sanitize_failure(&error.to_string())),
    }
}

/// Der Verifier eines Laufs, wenn dem Job-Worker kein Sandbox-Backend
/// übergeben wurde (`JobWorkerContext::verify_runner` ist `None`).
///
/// # Description
/// Mit Backend verifiziert `execute_work_driver_claim` über
/// `ExecutorVerifier { executor: VerificationExecutor::new(runner,
/// VerifyConfig::new(cwd, DRIVER_ACTOR)) }` mit dem
/// `crate::verify_sandbox::SandboxVerifier` des Kontexts (Workspace-Wurzel =
/// `runtime_root.cwd`). Ohne Backend läuft hier `NoSandboxRunner`: jeder
/// `Command`-Schritt ist Unverifiable (nie unsandboxed auf dem Host), nur
/// `Artifact`-Schritte werden geprüft; ein Lauf mit Befehlen eskaliert mit
/// [`NO_VERIFY_SANDBOX`] an den Menschen.
fn fallback_verifier(
    runtime_root: &JobRuntimeRoot,
    work_id: &WorkId,
) -> ExecutorVerifier<NoSandboxRunner> {
    tracing::warn!(
        work_id = %work_id.as_str(),
        workspace = %runtime_root.cwd.display(),
        "work-driver: no sandbox backend for verification commands is wired into the job worker; \
         falling back to NoSandboxRunner — every Command verification step is unverifiable \
         and the run escalates at its first Verify"
    );
    ExecutorVerifier {
        executor: VerificationExecutor::new(
            NoSandboxRunner::with_reason(NO_VERIFY_SANDBOX),
            VerifyConfig::new(runtime_root.cwd.clone(), DRIVER_ACTOR),
        ),
    }
}

/// [`VerificationRunner`] über `harw_plan_bridge::verify_exec` (dort liegt
/// auch die workspace-weite Serialisierung der Builds).
struct ExecutorVerifier<R> {
    executor: VerificationExecutor<R>,
}

impl<R: VerifyRunner + Send + Sync> VerificationRunner for ExecutorVerifier<R> {
    fn verify<'a>(&'a self, steps: &'a [VerificationStep]) -> BoxFuture<'a, VerificationReport> {
        Box::pin(async move {
            match self
                .executor
                .run(steps, offset_from_timestamp(Timestamp::now()))
                .await
            {
                VerifyRunOutcome::Completed(run) => report_from_run(&run),
                VerifyRunOutcome::Busy { lock_path, waited } => VerificationReport {
                    verdict: VerifyVerdict::Busy,
                    failing: Vec::new(),
                    unverifiable: vec![format!(
                        "workspace lock {} held for {} s",
                        lock_path.display(),
                        waited.as_secs()
                    )],
                    evidence: Vec::new(),
                    failed_steps: 0,
                },
            }
        })
    }
}

/// [`Judge`] als eigener kleiner interner Worker: ein Turn **ohne Werkzeuge**
/// (`JobEntry::Prompt`) in einem über die Runden fortgeführten Gespräch
/// (Session `work-driver-<job>-judge`), mit eigenem Modell/Provider
/// (`InternalModelPoint::WorkDriverJudge`, gepinnt über den Dienst-Provider).
///
/// Die erste Frage trägt den stabilen Präfix, jede weitere nur den variablen
/// Teil — der Präfix bleibt unverändert im Verlauf (Prompt-Cache).
struct ConversationJudge<'a> {
    context: &'a JobWorkerContext,
    runtime_root: &'a JobRuntimeRoot,
    job_store: Arc<JobStore>,
    provider: Arc<dyn ModelProvider>,
    submitter: String,
    session_id: SessionId,
    control: Arc<WorkerExecutionControl>,
    deadline: tokio::time::Instant,
}

impl Judge for ConversationJudge<'_> {
    fn judge<'a>(
        &'a self,
        request: &'a JudgeRequest,
    ) -> BoxFuture<'a, Result<JudgeReply, JudgeError>> {
        Box::pin(async move {
            let state_store = job_state_store(&self.context.transcript_root);
            let first = match state_store.load_history(&self.session_id).await {
                Ok(history) => history.is_empty(),
                Err(_) => true,
            };
            let text = if first {
                request.first_message()
            } else {
                request.variable.clone()
            };
            let (metered, meter) = MeteredModelProvider::new(Arc::clone(&self.provider));
            let assembled = assemble_job_turn(
                JobAssemblyInputs {
                    entry: JobEntry::Prompt,
                    home: &self.runtime_root.home,
                    cwd: &self.runtime_root.cwd,
                    principal: job_principal(&self.submitter),
                    session_id: self.session_id.clone(),
                    state_store,
                    job_store: Arc::clone(&self.job_store),
                    model: Arc::new(metered),
                    narrowing: None,
                },
                PauseDisposition::Blocked,
                None,
            );
            let reply = match assembled {
                Ok((setup, model)) => {
                    drive_worker_turn(
                        setup,
                        self.session_id.clone(),
                        text,
                        model,
                        &self.control,
                        self.deadline,
                    )
                    .await
                }
                Err(reason) => WorkerReply::Failed(reason),
            };
            let usage = meter_snapshot(&meter);
            match (reply, usage.rate_limited) {
                (WorkerReply::Completed { text, .. }, _) => Ok(JudgeReply {
                    text,
                    tokens: usage.tokens,
                }),
                (_, Some(retry_after_secs)) => Err(JudgeError::RateLimited { retry_after_secs }),
                (WorkerReply::Cancelled, None) => {
                    Err(JudgeError::Failed("judge turn cancelled".to_owned()))
                }
                (WorkerReply::Failed(reason) | WorkerReply::Paused(reason), None) => {
                    Err(JudgeError::Failed(reason))
                }
                (WorkerReply::RateLimited { retry_after_secs }, None) => {
                    Err(JudgeError::RateLimited { retry_after_secs })
                }
            }
        })
    }
}

/// [`ProviderPacing`] über den Provider des Laufs.
struct ModelPacing(Arc<dyn ModelProvider>);

impl ProviderPacing for ModelPacing {
    fn pacing_wait(&self) -> Option<Duration> {
        self.0.pacing_wait()
    }
}

/// [`Pacer`] mit `tokio::time::sleep`, vorzeitig beendet durch einen Abbruch.
struct CancellablePacer {
    control: Arc<WorkerExecutionControl>,
}

impl Pacer for CancellablePacer {
    fn pause<'a>(&'a self, wait: Duration) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = cancellation_requested(self.control.cancellation()) => {}
            }
        })
    }
}

/// [`WriteScopeGuard`] über den Workspace-Schnappschuss des Plan-Knoten-Pfads
/// (`snapshot_workspace`/`diff_snapshots`) und `validate_patch`.
///
/// # Description
/// Ausgenommen sind `target/` (Build-Artefakte der zentralen Verifikation
/// anderer Läufe), `.harw/` (Verifikationssperre) und die Verzeichnisse des
/// Dienstes selbst (Home, Transkripte, Job-Speicher), falls sie im Workspace
/// liegen.
struct WorkspaceScopeGuard {
    root: PathBuf,
    excluded: Vec<String>,
    before: Mutex<Option<BTreeMap<String, FileFingerprint>>>,
}

impl WorkspaceScopeGuard {
    fn new(
        runtime_root: &JobRuntimeRoot,
        context: &JobWorkerContext,
        job_store: &JobStore,
    ) -> Result<Self, String> {
        let sandbox = job_sandbox(
            JobEntry::PlanNode {
                kind: PlanNodeKind::Explore,
                may_write: false,
            },
            &runtime_root.cwd,
        )?;
        let root = sandbox.workspace().canonical_root().to_path_buf();
        // `.harw/` trägt u. a. die Verifikationssperre anderer Läufe und ist für
        // `fs.write` ohnehin geschützt.
        let mut excluded = vec!["target".to_owned(), ".harw".to_owned()];
        for path in [
            runtime_root.home.as_path(),
            context.transcript_root.as_path(),
            job_store.root(),
        ] {
            let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            if let Ok(relative) = canonical.strip_prefix(&root) {
                if let Some(relative) = relative.to_str() {
                    let relative = relative.replace('\\', "/");
                    if !relative.is_empty() {
                        excluded.push(relative);
                    }
                }
            }
        }
        Ok(Self {
            root,
            excluded,
            before: Mutex::new(None),
        })
    }

    fn is_excluded(&self, path: &str) -> bool {
        self.excluded.iter().any(|prefix| {
            path == prefix
                || path
                    .strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }
}

impl WriteScopeGuard for WorkspaceScopeGuard {
    fn before_wave(&self) {
        let snapshot = snapshot_workspace(&self.root);
        if let Ok(mut slot) = self.before.lock() {
            *slot = Some(snapshot);
        }
    }

    fn after_wave(&self, owners: &[(String, Vec<String>)], foreign: &[String]) -> WaveChanges {
        let before = match self.before.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(before) = before else {
            return WaveChanges {
                changed: BTreeMap::new(),
                violations: vec!["kein Schnappschuss vor der Welle".to_owned()],
            };
        };
        let after = snapshot_workspace(&self.root);
        let mut diff = diff_snapshots(
            RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
            &before,
            &after,
        );
        diff.files.retain(|file| !self.is_excluded(&file.path));
        attribute_changes(&diff, owners, foreign)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx, some_or};
    use harw_job_runtime::{
        Budget, EnforcementState, Job, JobScope, RetryPolicy, SandboxReport, StoredJob,
    };
    use harw_plan::admission::{FileChange, PatchFile};
    use harw_plan::{Criterion, EvidenceKind, GoalId, GoalStatus, InMemoryGoalStore};
    use harw_plan_bridge::WorkerReportStatus;
    use harw_plan_bridge::verify_exec::{CommandExit, CommandRequest, CommandRun, RunnerError};
    use harw_session_store::{ClaimRequest, CompleteRequest};
    use harw_types::WorkspaceId;
    use std::collections::VecDeque;

    // ── Fakes ─────────────────────────────────────────────────────────────

    struct FakeGoals {
        goal: Mutex<Goal>,
        /// Gesetzt: der nächste `attach_evidence` schlägt mit dieser
        /// Fehlermeldung fehl (Goal-Speicher nicht erreichbar o. Ä.).
        fail_attach: Mutex<Option<String>>,
    }

    impl FakeGoals {
        fn new(goal: Goal) -> Self {
            Self {
                goal: Mutex::new(goal),
                fail_attach: Mutex::new(None),
            }
        }

        fn snapshot(&self) -> TestResult<Goal> {
            self.goal
                .lock()
                .map(|goal| goal.clone())
                .map_err(|_| TestError::Unexpected("goal lock poisoned".to_owned()))
        }

        /// Lässt jeden folgenden `attach_evidence`-Aufruf mit `reason` fehlschlagen.
        fn fail_attach_evidence(&self, reason: &str) {
            if let Ok(mut fail_attach) = self.fail_attach.lock() {
                *fail_attach = Some(reason.to_owned());
            }
        }
    }

    impl GoalAccess for FakeGoals {
        fn goal(&self, goal_id: &str) -> Result<Goal, String> {
            let goal = self.goal.lock().map_err(|_| "poisoned".to_owned())?.clone();
            if goal.id.as_str() == goal_id {
                Ok(goal)
            } else {
                Err("unknown goal".to_owned())
            }
        }

        fn plan(&self, _plan_id: Option<&str>) -> Result<Option<Plan>, String> {
            Ok(None)
        }

        fn attach_evidence(&self, _goal_id: &str, evidence: &[EvidenceRef]) -> Result<(), String> {
            if let Some(reason) = self
                .fail_attach
                .lock()
                .map_err(|_| "poisoned".to_owned())?
                .clone()
            {
                return Err(reason);
            }
            let mut goal = self.goal.lock().map_err(|_| "poisoned".to_owned())?;
            goal.evidence.extend(evidence.iter().cloned());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeSpawner {
        replies: Mutex<VecDeque<WorkerReply>>,
        calls: Mutex<Vec<WorkerRequest>>,
    }

    impl FakeSpawner {
        fn with_replies(replies: Vec<WorkerReply>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> TestResult<Vec<WorkerRequest>> {
            self.calls
                .lock()
                .map(|calls| calls.clone())
                .map_err(|_| TestError::Unexpected("calls lock poisoned".to_owned()))
        }
    }

    impl WorkerSpawner for FakeSpawner {
        fn run<'a>(&'a self, request: WorkerRequest) -> BoxFuture<'a, WorkerRun> {
            Box::pin(async move {
                if let Ok(mut calls) = self.calls.lock() {
                    calls.push(request);
                }
                let reply = self
                    .replies
                    .lock()
                    .ok()
                    .and_then(|mut replies| replies.pop_front())
                    .unwrap_or_else(|| done_reply("fertig"));
                WorkerRun {
                    reply,
                    usage: WorkerUsage {
                        tokens: 100,
                        prompt_tokens: 80,
                        cached_tokens: 60,
                        context_tokens: 1_000,
                    },
                }
            })
        }
    }

    struct FakeVerifier {
        reports: Mutex<VecDeque<VerificationReport>>,
        calls: Mutex<usize>,
    }

    impl FakeVerifier {
        fn with_reports(reports: Vec<VerificationReport>) -> Self {
            Self {
                reports: Mutex::new(reports.into()),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.lock().map(|calls| *calls).unwrap_or(usize::MAX)
        }
    }

    impl VerificationRunner for FakeVerifier {
        fn verify<'a>(
            &'a self,
            _steps: &'a [VerificationStep],
        ) -> BoxFuture<'a, VerificationReport> {
            Box::pin(async move {
                if let Ok(mut calls) = self.calls.lock() {
                    *calls += 1;
                }
                self.reports
                    .lock()
                    .ok()
                    .and_then(|mut reports| reports.pop_front())
                    .unwrap_or_else(passed_report)
            })
        }
    }

    struct FakeJudge {
        reply: String,
        requests: Mutex<Vec<JudgeRequest>>,
    }

    impl FakeJudge {
        fn replying(reply: &str) -> Self {
            Self {
                reply: reply.to_owned(),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> TestResult<Vec<JudgeRequest>> {
            self.requests
                .lock()
                .map(|requests| requests.clone())
                .map_err(|_| TestError::Unexpected("judge lock poisoned".to_owned()))
        }
    }

    impl Judge for FakeJudge {
        fn judge<'a>(
            &'a self,
            request: &'a JudgeRequest,
        ) -> BoxFuture<'a, Result<JudgeReply, JudgeError>> {
            Box::pin(async move {
                if let Ok(mut requests) = self.requests.lock() {
                    requests.push(request.clone());
                }
                Ok(JudgeReply {
                    text: self.reply.clone(),
                    tokens: 10,
                })
            })
        }
    }

    /// One recorded wave: (worker id, changed paths) per worker.
    type Wave = Vec<(String, Vec<String>)>;

    struct FakeGuard {
        violations: Mutex<Vec<String>>,
        waves: Mutex<Vec<Wave>>,
        /// Ein `UnifiedDiff` je `after_wave`-Aufruf (FIFO); erschöpft: leerer
        /// Diff (keine Änderungen). Läuft durch die echte
        /// `attribute_changes`, damit Tests reale Block-Grenzen prüfen
        /// können, statt nur die übergebenen `owners` mitzuschreiben.
        diffs: Mutex<VecDeque<UnifiedDiff>>,
    }

    impl FakeGuard {
        fn violating(violation: &str) -> Self {
            Self {
                violations: Mutex::new(vec![violation.to_owned()]),
                waves: Mutex::new(Vec::new()),
                diffs: Mutex::new(VecDeque::new()),
            }
        }

        fn with_diffs(diffs: Vec<UnifiedDiff>) -> Self {
            Self {
                violations: Mutex::new(Vec::new()),
                waves: Mutex::new(Vec::new()),
                diffs: Mutex::new(diffs.into()),
            }
        }
    }

    impl Default for FakeGuard {
        fn default() -> Self {
            Self {
                violations: Mutex::new(Vec::new()),
                waves: Mutex::new(Vec::new()),
                diffs: Mutex::new(VecDeque::new()),
            }
        }
    }

    impl WriteScopeGuard for FakeGuard {
        fn before_wave(&self) {}

        fn after_wave(&self, owners: &[(String, Vec<String>)], foreign: &[String]) -> WaveChanges {
            if let Ok(mut waves) = self.waves.lock() {
                waves.push(owners.to_vec());
            }
            let preset = self
                .violations
                .lock()
                .map(|mut violations| std::mem::take(&mut *violations))
                .unwrap_or_default();
            if !preset.is_empty() {
                return WaveChanges {
                    changed: BTreeMap::new(),
                    violations: preset,
                };
            }
            let diff = self
                .diffs
                .lock()
                .ok()
                .and_then(|mut diffs| diffs.pop_front())
                .unwrap_or_else(|| UnifiedDiff {
                    base_revision: RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
                    files: Vec::new(),
                });
            attribute_changes(&diff, owners, foreign)
        }
    }

    #[derive(Default)]
    struct FakePacer {
        pauses: Mutex<Vec<Duration>>,
    }

    impl FakePacer {
        fn pauses(&self) -> Vec<Duration> {
            self.pauses
                .lock()
                .map(|pauses| pauses.clone())
                .unwrap_or_default()
        }
    }

    impl Pacer for FakePacer {
        fn pause<'a>(&'a self, wait: Duration) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                if let Ok(mut pauses) = self.pauses.lock() {
                    pauses.push(wait);
                }
            })
        }
    }

    /// Provider-Taktung: liefert `wait` bei der ersten Abfrage, danach `None`
    /// (das Kontingent gilt als aufgefüllt) — bildet nach, dass ein
    /// TPM-/RPM-Kontingent mit der Zeit von selbst wieder frei wird, ohne
    /// dass ein Fake eine echte Uhr bräuchte. Standard: keine Wartezeit.
    #[derive(Default)]
    struct FakePacing {
        wait: Mutex<Option<Duration>>,
        polls: Mutex<u32>,
    }

    impl FakePacing {
        fn once(wait: Option<Duration>) -> Self {
            Self {
                wait: Mutex::new(wait),
                polls: Mutex::new(0),
            }
        }

        fn polls(&self) -> u32 {
            self.polls.lock().map(|polls| *polls).unwrap_or(0)
        }
    }

    impl ProviderPacing for FakePacing {
        fn pacing_wait(&self) -> Option<Duration> {
            if let Ok(mut polls) = self.polls.lock() {
                *polls = polls.saturating_add(1);
            }
            self.wait.lock().ok().and_then(|mut wait| wait.take())
        }
    }

    /// Spawner, der je Start die Zahl der bis dahin gemachten Pausen merkt.
    struct PauseAwareSpawner<'a> {
        pacer: &'a FakePacer,
        pauses_at_start: Mutex<Vec<usize>>,
    }

    impl<'a> PauseAwareSpawner<'a> {
        fn new(pacer: &'a FakePacer) -> Self {
            Self {
                pacer,
                pauses_at_start: Mutex::new(Vec::new()),
            }
        }

        fn pauses_at_start(&self) -> TestResult<Vec<usize>> {
            self.pauses_at_start
                .lock()
                .map(|seen| seen.clone())
                .map_err(|_| TestError::Unexpected("spawner lock poisoned".to_owned()))
        }
    }

    impl WorkerSpawner for PauseAwareSpawner<'_> {
        fn run<'a>(&'a self, _request: WorkerRequest) -> BoxFuture<'a, WorkerRun> {
            Box::pin(async move {
                if let Ok(mut seen) = self.pauses_at_start.lock() {
                    seen.push(self.pacer.pauses().len());
                }
                WorkerRun {
                    reply: done_reply("fertig"),
                    usage: WorkerUsage::default(),
                }
            })
        }
    }

    /// Skriptbarer Verifikations-Runner (voll sandboxed, Exit 0); merkt sich
    /// jeden Befehlstext.
    #[derive(Default)]
    struct PassingRunner {
        seen: Mutex<Vec<String>>,
    }

    impl PassingRunner {
        fn seen(&self) -> TestResult<Vec<String>> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .map_err(|_| TestError::Unexpected("runner lock poisoned".to_owned()))
        }
    }

    impl VerifyRunner for PassingRunner {
        async fn run(&self, request: &CommandRequest) -> Result<CommandRun, RunnerError> {
            self.seen
                .lock()
                .map_err(|_| RunnerError::Backend {
                    reason: "fake runner lock poisoned".to_owned(),
                })?
                .push(request.raw.clone());
            Ok(CommandRun {
                exit: CommandExit::Exited(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
                upstream_truncated: false,
                sandbox: SandboxReport::uniform(EnforcementState::Enforced),
                locator: None,
            })
        }
    }

    /// Alle Fakes einer Testrunde.
    struct Fakes {
        goals: FakeGoals,
        spawner: FakeSpawner,
        verifier: FakeVerifier,
        judge: FakeJudge,
        guard: FakeGuard,
        pacer: FakePacer,
        pacing: FakePacing,
    }

    impl Fakes {
        fn new(goal: Goal) -> Self {
            Self {
                goals: FakeGoals::new(goal),
                spawner: FakeSpawner::default(),
                verifier: FakeVerifier::with_reports(Vec::new()),
                judge: FakeJudge::replying("{}"),
                guard: FakeGuard::default(),
                pacer: FakePacer::default(),
                pacing: FakePacing::default(),
            }
        }

        fn ports(&self) -> DrivePorts<'_> {
            DrivePorts {
                goals: &self.goals,
                workers: &self.spawner,
                verifier: &self.verifier,
                judge: &self.judge,
                scope_guard: &self.guard,
                pacer: &self.pacer,
                pacing: &self.pacing,
            }
        }
    }

    // ── Fixtures ──────────────────────────────────────────────────────────

    const STATEMENT: &str = "Der Parser akzeptiert UTF-8 vollständig";
    const JOB: &str = "wd-run";

    /// Ein abgeschlossener Turn mit einem `work_driver.report` dieses Status.
    fn reported(status: WorkerReportStatus, summary: &str, blockers: Vec<String>) -> WorkerReply {
        WorkerReply::Completed {
            text: format!("{summary} (Freitext, wird nicht gelesen)"),
            report: Some(WorkerReport {
                status,
                criteria_addressed: Vec::new(),
                changed_paths: Vec::new(),
                summary: summary.to_owned(),
                blockers,
            }),
        }
    }

    fn done_reply(summary: &str) -> WorkerReply {
        reported(WorkerReportStatus::Done, summary, Vec::new())
    }

    fn partial_reply() -> WorkerReply {
        reported(WorkerReportStatus::Partial, "Halb fertig.", Vec::new())
    }

    fn blocked_reply(question: &str) -> WorkerReply {
        reported(
            WorkerReportStatus::Blocked,
            "Ich komme nicht weiter.",
            vec![question.to_owned()],
        )
    }

    fn passed_report() -> VerificationReport {
        VerificationReport {
            verdict: VerifyVerdict::Passed,
            failing: Vec::new(),
            unverifiable: Vec::new(),
            evidence: Vec::new(),
            failed_steps: 0,
        }
    }

    fn failed_report(steps: usize) -> VerificationReport {
        VerificationReport {
            verdict: VerifyVerdict::Failed,
            failing: (0..steps)
                .map(|step| format!("cargo test t{step}: exit code 1, expected 0"))
                .collect(),
            unverifiable: Vec::new(),
            evidence: Vec::new(),
            failed_steps: steps,
        }
    }

    fn evidence(cmd: &str) -> EvidenceRef {
        EvidenceRef {
            kind: EvidenceKind::CargoTest,
            locator: cmd.to_owned(),
            attached_at: offset_from_timestamp(Timestamp::UNIX_EPOCH),
            actor: DRIVER_ACTOR.to_owned(),
            digest: None,
        }
    }

    fn command_criterion(description: &str, cmd: &str) -> Criterion {
        Criterion {
            description: description.to_owned(),
            verification: vec![VerificationStep::Command {
                cmd: cmd.to_owned(),
                expect_exit: 0,
            }],
        }
    }

    fn artifact_criterion(description: &str, path: &str) -> Criterion {
        Criterion {
            description: description.to_owned(),
            verification: vec![VerificationStep::Artifact {
                path: path.to_owned(),
            }],
        }
    }

    fn manual_criterion(description: &str) -> Criterion {
        Criterion {
            description: description.to_owned(),
            verification: vec![VerificationStep::Manual {
                note: description.to_owned(),
            }],
        }
    }

    fn goal(criteria: Vec<Criterion>) -> Goal {
        Goal {
            id: GoalId::new("g-utf8"),
            revision: 1,
            statement: STATEMENT.to_owned(),
            non_goals: Vec::new(),
            invariants: Vec::new(),
            acceptance_criteria: criteria,
            constraints: Vec::new(),
            open_questions: Vec::new(),
            status: GoalStatus::Active,
            plan_id: None,
            plan_revision: None,
            evidence: Vec::new(),
            created_at: offset_from_timestamp(Timestamp::UNIX_EPOCH),
            updated_at: offset_from_timestamp(Timestamp::UNIX_EPOCH),
            tenant: None,
        }
    }

    fn spec(max_iterations: u32) -> WorkDriverSpec {
        WorkDriverSpec {
            max_iterations,
            max_parallel_workers: 4,
            max_attempts_per_worker: 4,
            stall_iterations: 3,
            worker_role: "implementer".to_owned(),
            judge_role: Some("reviewer".to_owned()),
            verify: vec!["cargo clippy --workspace".to_owned()],
            token_budget: None,
            wall_budget_secs: None,
        }
    }

    fn input(max_iterations: u32) -> WorkDriverJobInput {
        WorkDriverJobInput {
            schema_version: WORK_DRIVER_INPUT_SCHEMA_VERSION,
            goal_id: "g-utf8".to_owned(),
            plan_id: None,
            orchestrator_role: "orchestrator".to_owned(),
            requested_by: "operator".to_owned(),
            spec: spec(max_iterations),
            tenant: None,
        }
    }

    fn memory_for(input: &WorkDriverJobInput, provider_cap: Option<usize>) -> RunMemory {
        RunMemory::new(&input.spec, provider_cap, Timestamp::now())
    }

    /// Minimaler Worker-Zustand mit `owned_paths` (sonst leer/frisch).
    fn worker_state(worker_id: &str, owned_paths: Vec<String>) -> WorkerState {
        WorkerState {
            worker_id: worker_id.to_owned(),
            scope: WorkScope {
                id: worker_id.to_owned(),
                summary: String::new(),
                owned_paths,
                criteria: Vec::new(),
            },
            attempts: 0,
            last_result: None,
            context_tokens_used: 0,
            cache_hit_ratio: None,
        }
    }

    /// Ein frischer, nicht fortgesetzter Auftrag an `worker_id`.
    fn worker_request(worker_id: &str, owned_paths: Vec<String>) -> WorkerRequest {
        WorkerRequest {
            worker_id: worker_id.to_owned(),
            role: "implementer".to_owned(),
            owned_paths,
            text: "tu was".to_owned(),
            continuation: false,
            criteria_total: 1,
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        store: JobStore,
    }

    fn harness() -> TestResult<Harness> {
        let dir = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let store = JobStore::new(dir.path());
        Ok(Harness { _dir: dir, store })
    }

    fn work_id() -> WorkId {
        WorkId::from_str(JOB)
    }

    fn not_cancelled() -> bool {
        false
    }

    /// `n` Runden in einem Claim mit `memory`.
    async fn step(
        harness: &Harness,
        input: &WorkDriverJobInput,
        ports: &DrivePorts<'_>,
        memory: &mut RunMemory,
        rounds: u32,
    ) -> RoundsEnd {
        run_rounds(
            &harness.store,
            &work_id(),
            input,
            ports,
            memory,
            Some(rounds),
            &not_cancelled,
        )
        .await
    }

    fn state_of(harness: &Harness) -> TestResult<WorkDriverState> {
        some_or(
            read_state_sidecar(&harness.store, &work_id()).map_err(ctx("read sidecar"))?,
            "driver state",
        )
    }

    fn outcome_of(end: RoundsEnd) -> TestResult<JobOutcome> {
        match end {
            RoundsEnd::Finished(outcome) => Ok(outcome),
            RoundsEnd::Yielded => Err(TestError::Unexpected("run yielded".to_owned())),
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[test]
    fn test_work_driver_kind_is_supported_by_the_worker() {
        let kind = JobKind::Custom(WORK_DRIVER_JOB_KIND.to_owned());
        assert!(is_work_driver_kind(&kind));
        assert!(super::super::is_supported_kind(&kind));
        assert!(!is_work_driver_kind(&JobKind::Worker));
        assert_eq!(
            WORK_DRIVER_JOB_KIND,
            harw_ops::work_driver::WORK_DRIVER_JOB_KIND
        );
    }

    #[test]
    fn test_input_validation_rejects_a_foreign_schema_and_an_empty_goal() {
        let mut wrong = input(5);
        wrong.schema_version = 99;
        assert!(validate_input(&wrong).is_err());
        let mut empty = input(5);
        empty.goal_id = " ".to_owned();
        assert!(validate_input(&empty).is_err());
        assert!(validate_input(&input(5)).is_ok());
    }

    #[tokio::test]
    async fn test_first_round_delegates_in_parallel_and_persists_the_state() -> TestResult {
        let harness = harness()?;
        let fakes = Fakes::new(goal(vec![
            command_criterion("ASCII bleibt", "cargo test -p parser ascii"),
            command_criterion("Umlaute gehen", "cargo test -p parser umlaut"),
        ]));
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let end = step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
        assert_eq!(end, RoundsEnd::Yielded);

        let calls = fakes.spawner.calls()?;
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| !call.continuation));
        assert!(calls.iter().all(|call| call.role == "implementer"));
        assert!(
            calls
                .iter()
                .all(|call| call.text.ends_with(RETURN_CONTRACT))
        );
        assert_eq!(calls[0].worker_id, "wd-run-w0");
        assert_eq!(calls[1].worker_id, "wd-run-w1");
        assert_eq!(fakes.verifier.calls(), 0);

        let state = state_of(&harness)?;
        assert_eq!(state.iteration, 1);
        assert_eq!(state.workers.len(), 2);
        assert_eq!(state.usage.tokens_used, 200);
        assert_eq!(state.usage.cache_hit_ratio, Some(0.75));
        assert_eq!(
            state.effective_parallel, None,
            "width 4 = spec, not narrowed"
        );
        assert_eq!(state.rate_limited, 0);
        assert!(
            !state
                .last_rationale
                .iter()
                .any(|line| line.starts_with("effective_parallel=")
                    || line.starts_with("rate_limited=")),
            "AIMD state lives in its own fields, not in the rationale"
        );
        let worker = &state.workers[0];
        assert_eq!(worker.cache_hit_ratio, Some(0.75));
        assert_eq!(worker.context_tokens_used, 1_000);
        assert!(matches!(
            worker.last_result.as_ref().map(|result| &result.outcome),
            Some(WorkerOutcome::Done)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_continue_reuses_the_same_worker_and_sends_only_feedback() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![command_criterion(
            "Umlaute gehen",
            "cargo test -p parser umlaut",
        )]));
        fakes.spawner = FakeSpawner::with_replies(vec![partial_reply(), done_reply("fertig")]);
        let input = input(10);
        let mut memory = memory_for(&input, None);
        step(&harness, &input, &fakes.ports(), &mut memory, 2).await;

        let calls = fakes.spawner.calls()?;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].worker_id, calls[1].worker_id);
        assert!(!calls[0].continuation);
        assert!(calls[1].continuation);
        assert!(calls[0].text.contains(STATEMENT));
        // Cache-Regel: die Fortsetzung schickt weder Ziel noch Vertrag erneut.
        assert!(!calls[1].text.contains(STATEMENT));
        assert!(!calls[1].text.contains(RETURN_CONTRACT));
        assert!(calls[1].text.contains("Offen: [0] Umlaute gehen"));

        let state = state_of(&harness)?;
        assert_eq!(state.workers.len(), 1);
        assert_eq!(state.workers[0].attempts, 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_runs_once_per_settled_wave() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![
            command_criterion("ASCII bleibt", "cargo test -p parser ascii"),
            command_criterion("Umlaute gehen", "cargo test -p parser umlaut"),
        ]));
        fakes.verifier = FakeVerifier::with_reports(vec![VerificationReport {
            verdict: VerifyVerdict::Failed,
            failing: vec!["cargo clippy --workspace: exit code 1, expected 0".to_owned()],
            unverifiable: Vec::new(),
            evidence: Vec::new(),
            failed_steps: 1,
        }]);
        let input = input(10);
        let mut memory = memory_for(&input, None);
        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
        assert_eq!(fakes.verifier.calls(), 0);
        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
        assert_eq!(fakes.verifier.calls(), 1);
        assert_eq!(fakes.spawner.calls()?.len(), 2);
        assert!(matches!(
            state_of(&harness)?.verification,
            VerificationState::Failed { .. }
        ));

        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
        // Rote Verifikation: beide Worker bekommen die Fehlschläge, kein
        // zweiter Verifikationslauf in derselben Welle.
        assert_eq!(fakes.verifier.calls(), 1);
        let calls = fakes.spawner.calls()?;
        assert_eq!(calls.len(), 4);
        assert!(calls[2..].iter().all(|call| call.continuation));
        assert!(
            calls[2]
                .text
                .contains("Fehlschlag: cargo clippy --workspace")
        );
        assert_eq!(state_of(&harness)?.verification, VerificationState::NotRun);
        Ok(())
    }

    #[tokio::test]
    async fn test_sandboxed_executor_verifier_passes_commands_with_evidence() -> TestResult {
        let harness = harness()?;
        let workspace = tempfile::tempdir().map_err(ctx("workspace dir"))?;
        let cmd = "cargo test -p parser umlaut";
        let fakes = Fakes::new(goal(vec![command_criterion("Umlaute gehen", cmd)]));
        let verifier = ExecutorVerifier {
            executor: VerificationExecutor::new(
                PassingRunner::default(),
                VerifyConfig::new(workspace.path(), DRIVER_ACTOR),
            ),
        };
        let ports = DrivePorts {
            verifier: &verifier,
            ..fakes.ports()
        };
        let input = input(10);
        let mut memory = memory_for(&input, None);
        // Runde 1: Welle; Runde 2: zentrale Verifikation über den Executor.
        let outcome = outcome_of(
            run_rounds(
                &harness.store,
                &work_id(),
                &input,
                &ports,
                &mut memory,
                None,
                &not_cancelled,
            )
            .await,
        )?;
        // Mit Sandbox-Runner keine Eskalation mangels Backend.
        assert!(
            matches!(outcome, JobOutcome::Succeeded { .. }),
            "expected success, got {outcome:?}"
        );

        let seen = verifier.executor.runner().seen()?;
        assert!(seen.iter().any(|raw| raw == cmd), "ran: {seen:?}");
        assert!(seen.iter().any(|raw| raw == "cargo clippy --workspace"));
        assert_eq!(state_of(&harness)?.verification, VerificationState::Passed);
        let goal = fakes.goals.snapshot()?;
        assert!(
            goal.evidence
                .iter()
                .any(|found| found.locator == cmd && found.actor == DRIVER_ACTOR),
            "evidence: {:?}",
            goal.evidence
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_provider_pacing_pauses_before_the_first_worker_starts() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![
            command_criterion("ASCII bleibt", "cargo test -p parser ascii"),
            command_criterion("Umlaute gehen", "cargo test -p parser umlaut"),
        ]));
        fakes.pacing = FakePacing::once(Some(Duration::from_secs(9)));
        let spawner = PauseAwareSpawner::new(&fakes.pacer);
        let ports = DrivePorts {
            workers: &spawner,
            ..fakes.ports()
        };
        let input = input(10);
        let mut memory = memory_for(&input, None);
        step(&harness, &input, &ports, &mut memory, 1).await;

        // Zwei Workspace-Worker (reine `Command`-Kriterien) laufen in zwei
        // Blöcken: genau eine Pause, vor jedem Start (jede weitere Abfrage
        // meldet keine Wartezeit mehr).
        assert_eq!(fakes.pacer.pauses(), vec![Duration::from_secs(9)]);
        assert_eq!(spawner.pauses_at_start()?, vec![1, 1]);
        Ok(())
    }

    #[tokio::test]
    async fn test_provider_pacing_waits_the_full_reported_duration_and_ignores_zero_or_none()
    -> TestResult {
        let input = input(10);
        for (wait, expected) in [
            (None, Vec::new()),
            (Some(Duration::ZERO), Vec::new()),
            // Ein volles TPM-/RPM-Kontingentfenster (1 h, wie
            // `budget::MAX_REPORTED_WAIT` bei harw-provider-http) wird in
            // voller Länge abgewartet, nicht auf den 429-Backoff-Deckel
            // (`RATE_LIMIT_MAX_BACKOFF_SECS` = 300 s) gekappt.
            (
                Some(Duration::from_secs(3_600)),
                vec![Duration::from_secs(3_600)],
            ),
        ] {
            let harness = harness()?;
            let mut fakes = Fakes::new(goal(vec![command_criterion(
                "Umlaute gehen",
                "cargo test -p parser umlaut",
            )]));
            fakes.pacing = FakePacing::once(wait);
            let mut memory = memory_for(&input, None);
            step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
            assert_eq!(fakes.pacer.pauses(), expected, "pacing {wait:?}");
            assert_eq!(fakes.spawner.calls()?.len(), 1);
            // Nach der Wartezeit fragt die Welle erneut ab (mindestens ein
            // zweiter Poll), statt nach einer einzigen, gekappten Pause
            // blind zu starten.
            if wait.filter(|wait| !wait.is_zero()).is_some() {
                assert!(fakes.pacing.polls() >= 2, "polls {wait:?}");
            }
        }
        Ok(())
    }

    /// Meldet immer eine Wartezeit; bildet nach, dass `Pacer::pause` (etwa
    /// nach einem Abbruch) ohne echten Zeitablauf vorzeitig endet und der
    /// Provider deshalb nie „aufgefüllt" erscheint.
    struct AlwaysWaitingPacing;

    impl ProviderPacing for AlwaysWaitingPacing {
        fn pacing_wait(&self) -> Option<Duration> {
            Some(Duration::from_millis(1))
        }
    }

    #[tokio::test]
    async fn test_pacing_poll_cap_stops_a_spin_and_dispatches_anyway() -> TestResult {
        // Ohne echten Zeitablauf (Fake-Pacer wartet nicht) meldet
        // `AlwaysWaitingPacing` endlos eine Wartezeit — ohne Deckel würde die
        // Welle hier für immer spinnen, statt einen Abbruch beim
        // Worker-Start (`WorkerReply::Cancelled`) überhaupt bemerken zu
        // können.
        let mut state = new_state(&work_id(), Timestamp::now());
        state.workers.push(worker_state("w0", Vec::new()));
        let requests = vec![worker_request("w0", Vec::new())];
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let spawner = FakeSpawner::with_replies(vec![done_reply("fertig")]);
        let pacer = FakePacer::default();
        let pacing = AlwaysWaitingPacing;
        let guard = FakeGuard::default();
        let judge = FakeJudge::replying("{}");
        let goals = FakeGoals::new(goal(Vec::new()));
        let verifier = FakeVerifier::with_reports(Vec::new());
        let ports = DrivePorts {
            goals: &goals,
            workers: &spawner,
            verifier: &verifier,
            judge: &judge,
            scope_guard: &guard,
            pacer: &pacer,
            pacing: &pacing,
        };
        let end = execute_wave(&mut state, &mut memory, &ports, requests).await;
        assert_eq!(end, None);
        assert_eq!(pacer.pauses().len(), MAX_PACING_POLLS as usize);
        assert_eq!(spawner.calls()?.len(), 1, "dispatches despite the cap");
        Ok(())
    }

    #[tokio::test]
    async fn test_judge_unmet_continues_with_the_missing_items() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![manual_criterion("Doku erklärt BOM-Verhalten")]));
        fakes.judge = FakeJudge::replying(
            r#"{"passed": false, "comment": "Doku fehlt", "missing": ["README-Abschnitt zu BOM"]}"#,
        );
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let end = step(&harness, &input, &fakes.ports(), &mut memory, 4).await;
        assert_eq!(end, RoundsEnd::Yielded);
        assert_eq!(fakes.verifier.calls(), 1);
        let requests = fakes.judge.requests()?;
        assert_eq!(requests.len(), 1);
        // Stabiler Präfix (Anweisung, Ziel, Kriterien), variable Evidenz danach.
        let request = &requests[0];
        assert!(request.prefix.starts_with(JUDGE_INSTRUCTION));
        assert!(request.prefix.contains(&format!("Ziel: {STATEMENT}")));
        assert!(!request.prefix.contains("Worker-Ergebnisse"));
        assert!(request.variable.contains("Worker-Ergebnisse"));
        assert!(!request.variable.contains(JUDGE_INSTRUCTION));
        let first = request.first_message();
        let separator = some_or(first.find(JUDGE_SEPARATOR), "separator")?;
        let results = some_or(first.find("Worker-Ergebnisse"), "worker results")?;
        assert!(separator < results);
        let state = state_of(&harness)?;
        assert_eq!(
            state.last_judge.map(|verdict| verdict.passed),
            None,
            "new worker work clears the verdict of the wave"
        );

        let calls = fakes.spawner.calls()?;
        assert_eq!(calls.len(), 2);
        assert!(calls[1].continuation);
        assert_eq!(calls[0].worker_id, calls[1].worker_id);
        assert!(
            calls[1]
                .text
                .contains("Bewerter vermisst: README-Abschnitt zu BOM")
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_propose_achieved_succeeds_and_never_sets_the_goal_status() -> TestResult {
        let harness = harness()?;
        let cmd = "cargo test -p parser umlaut";
        let mut fakes = Fakes::new(goal(vec![command_criterion("Umlaute gehen", cmd)]));
        fakes.verifier = FakeVerifier::with_reports(vec![VerificationReport {
            verdict: VerifyVerdict::Passed,
            failing: Vec::new(),
            unverifiable: Vec::new(),
            evidence: vec![evidence(cmd)],
            failed_steps: 0,
        }]);
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let outcome = outcome_of(
            run_rounds(
                &harness.store,
                &work_id(),
                &input,
                &fakes.ports(),
                &mut memory,
                None,
                &not_cancelled,
            )
            .await,
        )?;
        let JobOutcome::Succeeded { result } = outcome else {
            return Err(TestError::Unexpected(format!(
                "expected success, got {outcome:?}"
            )));
        };
        assert_eq!(
            result.pointer("/proposal/status"),
            Some(&serde_json::json!("achieved"))
        );
        let summary = result
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(summary.contains("/goal achieve"));

        let goal = fakes.goals.snapshot()?;
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(goal.evidence.len(), 1);
        let state = state_of(&harness)?;
        assert!(
            state
                .last_rationale
                .iter()
                .any(|line| line.starts_with("Vorschlag: Ziel erreicht"))
        );
        assert_eq!(state.iteration, 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_give_up_at_the_iteration_limit_fails_the_job() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![command_criterion("Umlaute gehen", "cargo test")]));
        fakes.spawner = FakeSpawner::with_replies(vec![partial_reply()]);
        let input = input(1);
        let mut memory = memory_for(&input, None);
        let outcome = outcome_of(
            run_rounds(
                &harness.store,
                &work_id(),
                &input,
                &fakes.ports(),
                &mut memory,
                None,
                &not_cancelled,
            )
            .await,
        )?;
        let JobOutcome::Failed { reason } = outcome else {
            return Err(TestError::Unexpected(format!(
                "expected failure, got {outcome:?}"
            )));
        };
        assert!(reason.contains("iteration limit"), "{reason}");
        assert_eq!(fakes.spawner.calls()?.len(), 1);
        Ok(())
    }

    /// Legt den Job an, claimt ihn und schließt ihn mit `outcome` ab.
    fn park_job(harness: &Harness, outcome: JobOutcome) -> TestResult {
        let now = Timestamp::now();
        let mut job = Job::new(
            work_id(),
            JobKind::Custom(WORK_DRIVER_JOB_KIND.to_owned()),
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 1,
                base_delay: SignedDuration::from_secs(1),
                factor: 1.0,
                max_delay: SignedDuration::from_secs(1),
                jitter: 0.0,
            },
            now,
        );
        job.mark_ready(now).map_err(ctx("mark ready"))?;
        let record = StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("tenant"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
            ),
            input: serde_json::to_value(input(10)).map_err(ctx("input json"))?,
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        };
        harness.store.admit(&record).map_err(ctx("admit"))?;
        let claim = harness
            .store
            .claim(
                &work_id(),
                &ClaimRequest {
                    worker_id: "test".to_owned(),
                    lease_ttl: SignedDuration::from_secs(60),
                    now,
                },
            )
            .map_err(ctx("claim"))?;
        harness
            .store
            .complete(
                &work_id(),
                &CompleteRequest {
                    token: claim.token,
                    completed_at: Timestamp::now(),
                    outcome,
                },
            )
            .map_err(ctx("complete"))?;
        Ok(())
    }

    #[tokio::test]
    async fn test_escalation_blocks_and_the_unblock_note_continues_the_worker() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![command_criterion("Umlaute gehen", "cargo test")]));
        fakes.spawner = FakeSpawner::with_replies(vec![blocked_reply("Welche Unicode-Version?")]);
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let outcome = outcome_of(
            run_rounds(
                &harness.store,
                &work_id(),
                &input,
                &fakes.ports(),
                &mut memory,
                None,
                &not_cancelled,
            )
            .await,
        )?;
        let JobOutcome::Blocked { reason } = outcome.clone() else {
            return Err(TestError::Unexpected(format!(
                "expected blocked, got {outcome:?}"
            )));
        };
        assert!(reason.starts_with("work_driver:NeedsInput"), "{reason}");
        assert!(reason.contains("Welche Unicode-Version?"));
        assert_eq!(fakes.spawner.calls()?.len(), 1);

        // Ohne Freigabe (Reclaim) bleibt der Job blockiert, ohne Worker-Aufruf.
        let mut restarted = memory_for(&input, None);
        let again = outcome_of(step(&harness, &input, &fakes.ports(), &mut restarted, 1).await)?;
        assert!(matches!(again, JobOutcome::Blocked { .. }));
        assert_eq!(fakes.spawner.calls()?.len(), 1);

        // Mit Freigabe (unblock + Notiz): derselbe Worker läuft weiter.
        park_job(&harness, outcome)?;
        harness
            .store
            .unblock(
                &work_id(),
                Timestamp::now(),
                ApprovalActor::Operator {
                    id: "operator".to_owned(),
                },
                Some("Unicode 15".to_owned()),
            )
            .map_err(ctx("unblock"))?;
        let mut resumed = memory_for(&input, None);
        let end = step(&harness, &input, &fakes.ports(), &mut resumed, 1).await;
        assert_eq!(end, RoundsEnd::Yielded);
        let calls = fakes.spawner.calls()?;
        assert_eq!(calls.len(), 2);
        assert!(calls[1].continuation);
        assert_eq!(calls[0].worker_id, calls[1].worker_id);
        assert!(calls[1].text.contains("## Operator answer\nUnicode 15"));
        Ok(())
    }

    #[tokio::test]
    async fn test_state_survives_a_restart_between_rounds() -> TestResult {
        let harness = harness()?;
        let make_goal = || {
            goal(vec![command_criterion(
                "Umlaute gehen",
                "cargo test -p parser umlaut",
            )])
        };
        let input = input(10);
        let first_worker = {
            let mut fakes = Fakes::new(make_goal());
            fakes.spawner = FakeSpawner::with_replies(vec![partial_reply()]);
            let mut memory = memory_for(&input, None);
            step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
            fakes.spawner.calls()?[0].worker_id.clone()
        };

        // „Neustart": frische Ports und frisches Gedächtnis; nur der Sidecar bleibt.
        let fakes = Fakes::new(make_goal());
        let mut memory = memory_for(&input, None);
        let end = step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
        assert_eq!(end, RoundsEnd::Yielded);
        let calls = fakes.spawner.calls()?;
        assert_eq!(calls.len(), 1);
        assert!(calls[0].continuation);
        assert_eq!(calls[0].worker_id, first_worker);
        assert_eq!(state_of(&harness)?.iteration, 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_rate_limit_backs_off_keeps_the_attempt_and_halves_parallelism() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![command_criterion("Umlaute gehen", "cargo test")]));
        fakes.spawner = FakeSpawner::with_replies(vec![
            WorkerReply::RateLimited {
                retry_after_secs: 7,
            },
            done_reply("fertig"),
        ]);
        let input = input(10);
        let mut memory = memory_for(&input, None);
        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;

        let calls = fakes.spawner.calls()?;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].worker_id, calls[1].worker_id);
        assert!(calls[1].continuation);
        assert_eq!(calls[1].text, RESUME_AFTER_RATE_LIMIT);
        assert_eq!(fakes.pacer.pauses(), vec![Duration::from_secs(7)]);
        assert_eq!(memory.parallel_now(), 2);
        let state = state_of(&harness)?;
        assert_eq!(state.workers[0].attempts, 0);
        assert!(matches!(
            state.workers[0]
                .last_result
                .as_ref()
                .map(|result| &result.outcome),
            Some(WorkerOutcome::Done)
        ));
        assert_eq!(state.rate_limited, 1);
        assert_eq!(state.effective_parallel, Some(2));
        Ok(())
    }

    #[tokio::test]
    async fn test_busy_verification_repeats_the_round_without_counting_it() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![command_criterion("a", "cargo test a")]));
        fakes.verifier = FakeVerifier::with_reports(vec![
            VerificationReport {
                verdict: VerifyVerdict::Busy,
                failing: Vec::new(),
                unverifiable: vec!["workspace lock .harw/verify.lock held for 600 s".to_owned()],
                evidence: Vec::new(),
                failed_steps: 0,
            },
            passed_report(),
        ]);
        let input = input(10);
        let mut memory = memory_for(&input, None);
        step(&harness, &input, &fakes.ports(), &mut memory, 2).await;
        let state = state_of(&harness)?;
        assert_eq!(state.iteration, 1, "the busy round is not counted");
        assert_eq!(state.verification, VerificationState::NotRun);
        assert!(
            state
                .last_rationale
                .iter()
                .any(|line| line.starts_with("verification: busy"))
        );
        assert_eq!(fakes.pacer.pauses(), vec![VERIFY_BUSY_RETRY]);

        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
        let state = state_of(&harness)?;
        assert_eq!(fakes.verifier.calls(), 2);
        assert_eq!(state.iteration, 2);
        assert_eq!(state.verification, VerificationState::Passed);
        assert_eq!(state.usage.iterations_without_progress, 0);
        assert_eq!(fakes.spawner.calls()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_a_failed_attach_evidence_after_a_green_run_escalates_instead_of_looping()
    -> TestResult {
        let harness = harness()?;
        let cmd = "cargo test -p parser umlaut";
        let mut fakes = Fakes::new(goal(vec![command_criterion("Umlaute gehen", cmd)]));
        fakes.verifier = FakeVerifier::with_reports(vec![VerificationReport {
            verdict: VerifyVerdict::Passed,
            failing: Vec::new(),
            unverifiable: Vec::new(),
            evidence: vec![evidence(cmd)],
            failed_steps: 0,
        }]);
        fakes.goals.fail_attach_evidence("store unavailable");
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let outcome = outcome_of(
            run_rounds(
                &harness.store,
                &work_id(),
                &input,
                &fakes.ports(),
                &mut memory,
                None,
                &not_cancelled,
            )
            .await,
        )?;
        // Fail closed wie jeder andere Verifikationsausgang dieser Funktion:
        // ohne die angehängte Evidenz bliebe das Kriterium für immer offen
        // und die Runde würde sonst mit identischem Feedback endlos laufen.
        let JobOutcome::Blocked { reason } = outcome else {
            return Err(TestError::Unexpected(format!(
                "expected blocked, got {outcome:?}"
            )));
        };
        assert!(reason.contains("store unavailable"), "{reason}");
        let state = state_of(&harness)?;
        assert_ne!(state.verification, VerificationState::Passed);
        assert!(
            state
                .last_rationale
                .iter()
                .any(|line| line.contains("store unavailable")),
            "{:?}",
            state.last_rationale
        );
        // Die Evidenz selbst wurde nicht angehängt.
        let goal = fakes.goals.snapshot()?;
        assert!(goal.evidence.is_empty());
        Ok(())
    }

    #[test]
    fn test_parallelism_follows_the_provider_cap_and_aimd() {
        let input = input(10);
        let mut memory = memory_for(&input, Some(3));
        // Provider-Grenze 3, ein Platz für Orchestrator/Bewerter reserviert.
        assert_eq!(memory.parallel_now(), 2);
        memory.on_rate_limited();
        assert_eq!(memory.parallel_now(), 1);
        memory.on_rate_limited();
        assert_eq!(memory.parallel_now(), 1);
        memory.on_clean_wave();
        memory.on_clean_wave();
        memory.on_clean_wave();
        assert_eq!(memory.parallel_now(), 2);
        assert_eq!(memory_for(&input, None).parallel_now(), 4);
        assert_eq!(memory_for(&input, Some(1)).parallel_now(), 1);
        assert_eq!(rate_limit_backoff(0, 1), Duration::from_secs(5));
        assert_eq!(rate_limit_backoff(0, 3), Duration::from_secs(20));
        assert_eq!(rate_limit_backoff(900, 1), Duration::from_secs(300));
    }

    #[tokio::test]
    async fn test_provider_cap_limits_the_first_wave() -> TestResult {
        let harness = harness()?;
        let fakes = Fakes::new(goal(vec![
            command_criterion("a", "cargo test a"),
            command_criterion("b", "cargo test b"),
            command_criterion("c", "cargo test c"),
        ]));
        let input = input(10);
        let mut memory = memory_for(&input, Some(3));
        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
        assert_eq!(fakes.spawner.calls()?.len(), 2);
        // Provider-Grenze 3 − 1 Reserve drückt die Breite unter die Spec (4).
        assert_eq!(state_of(&harness)?.effective_parallel, Some(2));
        Ok(())
    }

    #[tokio::test]
    async fn test_a_write_outside_the_owned_paths_blocks_the_run() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![artifact_criterion(
            "Parser-Datei",
            "src/parser.rs",
        )]));
        fakes.guard =
            FakeGuard::violating("'Cargo.toml' liegt außerhalb der owned_paths der Welle");
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let outcome = outcome_of(
            run_rounds(
                &harness.store,
                &work_id(),
                &input,
                &fakes.ports(),
                &mut memory,
                None,
                &not_cancelled,
            )
            .await,
        )?;
        let JobOutcome::Blocked { reason } = outcome else {
            return Err(TestError::Unexpected(format!(
                "expected blocked, got {outcome:?}"
            )));
        };
        assert!(reason.contains("Schreibbereich verletzt"), "{reason}");
        let calls = fakes.spawner.calls()?;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].owned_paths, vec!["src/parser.rs".to_owned()]);
        let waves = fakes
            .guard
            .waves
            .lock()
            .map(|waves| waves.clone())
            .map_err(|_| TestError::Unexpected("guard lock".to_owned()))?;
        assert_eq!(
            waves,
            vec![vec![(
                "wd-run-w0".to_owned(),
                vec!["src/parser.rs".to_owned()]
            )]]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_a_write_into_a_different_blocks_owned_path_is_caught_across_blocks() -> TestResult
    {
        // Wellenbreite 1: w0 und w1 laufen in getrennten, nacheinander
        // ausgeführten Blöcken derselben Welle, nie gleichzeitig.
        let mut state = new_state(&work_id(), Timestamp::now());
        state.workers.push(worker_state("w0", vec!["a".to_owned()]));
        state.workers.push(worker_state("w1", vec!["b".to_owned()]));
        let requests = vec![
            worker_request("w0", vec!["a".to_owned()]),
            worker_request("w1", vec!["b".to_owned()]),
        ];
        let spec = WorkDriverSpec {
            max_parallel_workers: 1,
            ..spec(10)
        };
        let mut memory = RunMemory::new(&spec, None, Timestamp::now());
        let spawner =
            FakeSpawner::with_replies(vec![done_reply("a erledigt"), done_reply("b erledigt")]);
        // Block 1 (nur w0 läuft) verändert tatsächlich `b/x` — eine
        // Scope-Flucht in den Bereich von w1, der in diesem Block noch nicht
        // einmal gestartet ist. Die alte, wellenweite Vereinigung hätte das
        // klaglos w1 zugeschrieben (siehe Kontrollrechnung unten); die
        // block-genaue Prüfung erkennt es als Verstoß.
        let guard = FakeGuard::with_diffs(vec![
            UnifiedDiff {
                base_revision: RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
                files: vec![PatchFile {
                    path: "b/x".to_owned(),
                    source_path: None,
                    change: FileChange::Modified,
                }],
            },
            UnifiedDiff {
                base_revision: RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
                files: Vec::new(),
            },
        ]);
        let pacer = FakePacer::default();
        let pacing = FakePacing::default();
        let judge = FakeJudge::replying("{}");
        let goals = FakeGoals::new(goal(Vec::new()));
        let verifier = FakeVerifier::with_reports(Vec::new());
        let ports = DrivePorts {
            goals: &goals,
            workers: &spawner,
            verifier: &verifier,
            judge: &judge,
            scope_guard: &guard,
            pacer: &pacer,
            pacing: &pacing,
        };
        let requests_len = requests.len();
        let end = execute_wave(&mut state, &mut memory, &ports, requests).await;
        assert_eq!(end, None);
        assert_eq!(spawner.calls()?.len(), requests_len);
        for worker_id in ["w0", "w1"] {
            let outcome = state
                .workers
                .iter()
                .find(|worker| worker.worker_id == worker_id)
                .and_then(|worker| worker.last_result.as_ref())
                .map(|result| &result.outcome);
            assert!(
                matches!(outcome, Some(WorkerOutcome::Blocked { .. })),
                "{worker_id}: {outcome:?}"
            );
        }

        // Kontrollrechnung: dieselbe Änderung gegen die alte, wellenweite
        // Vereinigung (beide Besitzer gleichzeitig erlaubt) zeigt keinen
        // Verstoß und schreibt die Datei stillschweigend w1 zu — genau die
        // Lücke, die die block-genaue Prüfung oben schließt.
        let wide_owners = vec![
            ("w0".to_owned(), vec!["a".to_owned()]),
            ("w1".to_owned(), vec!["b".to_owned()]),
        ];
        let wide_diff = UnifiedDiff {
            base_revision: RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
            files: vec![PatchFile {
                path: "b/x".to_owned(),
                source_path: None,
                change: FileChange::Modified,
            }],
        };
        let wide = attribute_changes(&wide_diff, &wide_owners, &[]);
        assert!(wide.violations.is_empty());
        assert_eq!(wide.changed.get("w1"), Some(&vec!["b/x".to_owned()]));
        Ok(())
    }

    #[tokio::test]
    async fn test_cancellation_between_rounds_stops_without_a_worker_call() -> TestResult {
        let harness = harness()?;
        let fakes = Fakes::new(goal(vec![command_criterion("a", "cargo test a")]));
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let cancelled = || true;
        let end = run_rounds(
            &harness.store,
            &work_id(),
            &input,
            &fakes.ports(),
            &mut memory,
            None,
            &cancelled,
        )
        .await;
        assert!(matches!(
            end,
            RoundsEnd::Finished(JobOutcome::Cancelled { .. })
        ));
        assert!(fakes.spawner.calls()?.is_empty());
        Ok(())
    }

    #[test]
    fn test_attribute_changes_maps_paths_to_owners_and_reports_the_rest() {
        let diff = UnifiedDiff {
            base_revision: RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
            files: vec![
                PatchFile {
                    path: "src/parser/mod.rs".to_owned(),
                    source_path: None,
                    change: FileChange::Modified,
                },
                PatchFile {
                    path: "docs/a.md".to_owned(),
                    source_path: None,
                    change: FileChange::Added,
                },
                PatchFile {
                    path: "Cargo.toml".to_owned(),
                    source_path: None,
                    change: FileChange::Modified,
                },
            ],
        };
        let owners = vec![
            ("w0".to_owned(), vec!["src/parser".to_owned()]),
            ("w1".to_owned(), vec!["docs/a.md".to_owned()]),
            ("w2".to_owned(), Vec::new()),
        ];
        let changes = attribute_changes(&diff, &owners, &[]);
        assert_eq!(
            changes.changed.get("w0"),
            Some(&vec!["src/parser/mod.rs".to_owned()])
        );
        assert_eq!(
            changes.changed.get("w1"),
            Some(&vec!["docs/a.md".to_owned()])
        );
        assert!(!changes.changed.contains_key("w2"));
        assert_eq!(changes.violations.len(), 1);
        assert!(changes.violations[0].contains("Cargo.toml"));
    }

    #[test]
    fn test_attribute_changes_treats_overlapping_owned_paths_as_a_violation() {
        // `owned_paths` sollen laut Vertrag disjunkt sein; überlappen sie
        // sich dennoch, ist eine stillschweigende Mehrfachzuschreibung fail
        // closed keine Option.
        let diff = UnifiedDiff {
            base_revision: RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
            files: vec![PatchFile {
                path: "src/parser/mod.rs".to_owned(),
                source_path: None,
                change: FileChange::Modified,
            }],
        };
        let owners = vec![
            ("w0".to_owned(), vec!["src/parser".to_owned()]),
            ("w1".to_owned(), vec!["src/parser".to_owned()]),
        ];
        let changes = attribute_changes(&diff, &owners, &[]);
        assert!(changes.changed.is_empty());
        assert_eq!(changes.violations.len(), 1);
        assert!(changes.violations[0].contains("src/parser/mod.rs"));
    }

    fn input_for_tenant(tenant: Option<&str>) -> WorkDriverJobInput {
        let mut input = input(5);
        input.tenant = tenant.map(TenantId::from_str);
        input
    }

    #[test]
    fn test_goal_access_sees_only_the_goal_of_the_caller_tenant() -> TestResult {
        let store = Arc::new(InMemoryGoalStore::new());
        let mut owned = goal(vec![command_criterion("a", "cargo test")]);
        owned.tenant = Some(TenantId::from_str("tenant"));
        store
            .apply(GoalAction::Set { goal: owned }, "human:alice")
            .map_err(ctx("set goal"))?;
        let own = StoreGoalAccess::for_input(
            Arc::clone(&store) as Arc<dyn GoalStore>,
            None,
            &input_for_tenant(Some("tenant")),
        );
        assert!(own.goal("g-utf8").is_ok());
        let foreign = StoreGoalAccess::for_input(
            Arc::clone(&store) as Arc<dyn GoalStore>,
            None,
            &input_for_tenant(Some("other")),
        );
        assert!(foreign.goal("g-utf8").is_err());
        // Ohne Mandant (Einzelnutzer) ungefiltert: auch ein fremdes Goal ist sichtbar.
        let unscoped =
            StoreGoalAccess::for_input(store as Arc<dyn GoalStore>, None, &input_for_tenant(None));
        assert!(unscoped.goal("g-utf8").is_ok());
        Ok(())
    }

    #[test]
    fn test_goal_access_without_a_tenant_sees_an_untenanted_goal() -> TestResult {
        let store = Arc::new(InMemoryGoalStore::new());
        let cmd = "cargo test -p parser";
        store
            .apply(
                GoalAction::Set {
                    goal: goal(vec![command_criterion("a", cmd)]),
                },
                "human:alice",
            )
            .map_err(ctx("set goal"))?;
        let unscoped = StoreGoalAccess::for_input(
            Arc::clone(&store) as Arc<dyn GoalStore>,
            None,
            &input_for_tenant(None),
        );
        let seen = unscoped.goal("g-utf8").map_err(TestError::Unexpected)?;
        assert_eq!(seen.tenant, None);
        assert!(
            unscoped
                .plan(None)
                .map_err(TestError::Unexpected)?
                .is_none()
        );
        unscoped
            .attach_evidence("g-utf8", &[evidence(cmd)])
            .map_err(TestError::Unexpected)?;
        assert_eq!(
            unscoped
                .goal("g-utf8")
                .map_err(TestError::Unexpected)?
                .evidence
                .len(),
            1
        );
        // Ein gescopter Aufrufer sieht das mandantenlose Goal dagegen nicht.
        let scoped = StoreGoalAccess::for_input(
            store as Arc<dyn GoalStore>,
            None,
            &input_for_tenant(Some("tenant")),
        );
        assert!(scoped.goal("g-utf8").is_err());
        Ok(())
    }

    #[test]
    fn test_parse_verdict_is_tolerant_and_fails_closed() -> TestResult {
        let passed = some_or(
            parse_verdict(r#"{"passed": true, "comment": "belegt"}"#),
            "passed verdict",
        )?;
        assert!(passed.passed);
        assert_eq!(passed.comment, "belegt");
        assert!(passed.missing.is_empty());
        let minimal = some_or(parse_verdict(r#"{"passed": false}"#), "minimal verdict")?;
        assert!(!minimal.passed);
        assert!(minimal.comment.is_empty());
        assert!(minimal.missing.is_empty());
        let old = some_or(
            parse_verdict(r#"{"met": false, "rationale": "r", "missing": ["a"]}"#),
            "old field names",
        )?;
        assert!(!old.passed);
        assert_eq!(old.comment, "r");
        assert_eq!(old.missing, vec!["a".to_owned()]);
        let prose = some_or(
            parse_verdict("Hier mein Urteil: {\"verified\": true, \"comment\": \"ok {x}\"} Ende."),
            "verdict in prose",
        )?;
        assert!(prose.passed);
        assert_eq!(prose.comment, "ok {x}");
        let first_bool_wins = some_or(
            parse_verdict(r#"{"note": "x"} und dann {"passed": false, "comment": "c"}"#),
            "first object with a bool",
        )?;
        assert!(!first_bool_wins.passed);
        // Kein JSON-Urteil: kein Urteil — weder bestanden noch still „nicht
        // bestanden", auch wenn das bloße Wort "PASSED" vorkommt (kein
        // Klartext-Marker-Scan).
        for text in [
            "Ergebnis: PASSED",
            "passed? no — FAILED",
            "This has not passed review yet; more tests are needed.",
            "Ich kann das nicht beurteilen.",
            "{}",
            r#"{"passed": "yes"}"#,
        ] {
            assert_eq!(parse_verdict(text), None, "{text}");
        }
        Ok(())
    }

    #[test]
    fn test_judge_prefix_is_stable_across_rounds() {
        let goal = goal(vec![manual_criterion("a"), manual_criterion("b")]);
        let mut state = new_state(&work_id(), Timestamp::now());
        let first = judge_request(&spec(5), &goal, &[0], "Frage 1", &state);
        state.verification = VerificationState::Passed;
        let second = judge_request(
            &spec(5),
            &goal,
            &[0, 1],
            &format!("{JUDGE_INSTRUCTION}\n\nFrage 2"),
            &state,
        );
        assert_eq!(first.prefix, second.prefix);
        assert_ne!(first.variable, second.variable);
        assert!(second.variable.contains("Frage: Frage 2"));
    }

    #[test]
    fn test_verification_steps_merge_spec_and_goal_commands_without_duplicates() {
        let goal = goal(vec![
            command_criterion("a", "cargo clippy --workspace"),
            command_criterion("b", "cargo test -p parser"),
            manual_criterion("c"),
        ]);
        let steps = verification_steps(&spec(5), &goal);
        let commands: Vec<String> = steps
            .iter()
            .filter_map(|step| match step {
                VerificationStep::Command { cmd, .. } => Some(cmd.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            commands,
            vec![
                "cargo clippy --workspace".to_owned(),
                "cargo test -p parser".to_owned()
            ]
        );
    }

    #[test]
    fn test_goal_report_counts_evidence_attached_to_the_goal() -> TestResult {
        let cmd = "cargo test -p parser";
        let mut goal = goal(vec![command_criterion("a", cmd)]);
        let open = goal_report(&goal, None).map_err(TestError::Unexpected)?;
        assert_eq!(open.criteria_open, vec![0]);
        goal.evidence.push(evidence(cmd));
        let met = goal_report(&goal, None).map_err(TestError::Unexpected)?;
        assert_eq!(met.criteria_met, vec![0]);
        Ok(())
    }

    #[test]
    fn test_report_from_run_uses_the_command_as_locator() {
        use harw_plan_bridge::verify_exec::StepReport;
        let cmd = "cargo test -p parser";
        let mut found = evidence(cmd);
        found.locator = format!("verify-command:{cmd}");
        let run = VerifyRun {
            steps: vec![StepReport {
                index: 0,
                step: VerificationStep::Command {
                    cmd: cmd.to_owned(),
                    expect_exit: 0,
                },
                outcome: VerifyOutcome::Passed { evidence: found },
                trace: None,
            }],
            skipped: 0,
        };
        let report = report_from_run(&run);
        assert_eq!(report.verdict, VerifyVerdict::Passed);
        assert_eq!(report.evidence.len(), 1);
        assert_eq!(report.evidence[0].locator, cmd);
    }

    #[test]
    fn test_limits_follow_the_spec() {
        let mut spec = spec(7);
        spec.wall_budget_secs = Some(90);
        spec.token_budget = Some(5_000);
        let limits = limits_from_spec(&spec);
        assert_eq!(limits.max_iterations, 7);
        assert_eq!(limits.token_budget, 5_000);
        assert_eq!(limits.wall_budget.whole_seconds(), 90);
        assert_eq!(limits.max_parallel_workers, 4);
    }

    // ── R18 P4: report tool, scopes, verification, stall, judge ───────────

    /// WD-01: a turn that called `work_driver.report` ends with exactly
    /// `WorkerReport::into_summary`; its free text is not read.
    #[tokio::test]
    async fn test_wd01_a_reported_turn_becomes_the_report_summary() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![
            command_criterion("a", "cargo test a"),
            command_criterion("b", "cargo test b"),
        ]));
        let report = WorkerReport {
            status: WorkerReportStatus::Blocked,
            criteria_addressed: vec![1, 0],
            changed_paths: vec!["src/x.rs".to_owned()],
            summary: "halb erledigt".to_owned(),
            blockers: vec!["Welche API?".to_owned()],
        };
        fakes.spawner = FakeSpawner::with_replies(vec![WorkerReply::Completed {
            text: "Status: done".to_owned(),
            report: Some(report.clone()),
        }]);
        let input = input(10);
        let mut memory = memory_for(&input, None);
        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;

        let calls = fakes.spawner.calls()?;
        assert!(calls.iter().all(|call| call.criteria_total == 2));
        let state = state_of(&harness)?;
        assert_eq!(
            state.workers[0].last_result.as_ref(),
            Some(&report.into_summary())
        );
        Ok(())
    }

    /// WD-02: a turn without a report is `Partial` with the explicit reason
    /// "no report" — never read as `Done` from a status line.
    #[tokio::test]
    async fn test_wd02_a_turn_without_report_is_partial_with_no_report() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![command_criterion("a", "cargo test a")]));
        fakes.spawner = FakeSpawner::with_replies(vec![WorkerReply::Completed {
            text: "Fertig.\nStatus: done".to_owned(),
            report: None,
        }]);
        let input = input(10);
        let mut memory = memory_for(&input, None);
        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;

        let state = state_of(&harness)?;
        let result = some_or(state.workers[0].last_result.clone(), "worker result")?;
        assert_eq!(result.outcome, WorkerOutcome::Partial);
        assert!(
            result
                .summary
                .starts_with(harw_plan_bridge::NO_REPORT_REASON),
            "{}",
            result.summary
        );
        assert!(result.summary.contains("Status: done"));
        assert!(
            result
                .suggested_next
                .as_deref()
                .is_some_and(|next| next.contains(WORK_DRIVER_REPORT_TOOL))
        );
        // Die nächste Runde setzt denselben Worker fort, statt zu verifizieren.
        step(&harness, &input, &fakes.ports(), &mut memory, 1).await;
        assert_eq!(fakes.verifier.calls(), 0);
        assert!(fakes.spawner.calls()?[1].continuation);
        Ok(())
    }

    #[test]
    fn test_task_texts_carry_the_verification_and_the_report_contract() {
        let text = first_task_text(
            "## Goal\nx",
            &[
                "cargo test".to_owned(),
                " ".to_owned(),
                "cargo clippy".to_owned(),
            ],
        );
        assert!(
            text.contains(
                "## Central verification (the driver runs these, not you)\n- `cargo test`\n- `cargo clippy`\n\n## Report"
            ),
            "{text}"
        );
        assert!(text.ends_with(RETURN_CONTRACT));
        assert!(RETURN_CONTRACT.contains(WORK_DRIVER_REPORT_TOOL));
        assert!(!RETURN_CONTRACT.contains("Status:"));
        assert_eq!(first_task_text("t", &[]), format!("t\n\n{RETURN_CONTRACT}"));

        let feedback = continue_text("Offen: [0] a", Some("ja"));
        assert!(feedback.starts_with("## Driver feedback\nOffen: [0] a"));
        assert!(feedback.contains("## Operator answer\nja"));
        assert!(feedback.contains(WORK_DRIVER_REPORT_TOOL));
        assert!(RESUME_AFTER_RATE_LIMIT.contains(WORK_DRIVER_REPORT_TOOL));
    }

    #[test]
    fn test_worker_identity_uses_the_role_instructions_and_the_write_scope() {
        let wide = worker_identity(
            "implementer",
            RegistryProfile::WorkspaceEdit,
            &[WORKSPACE_SCOPE.to_owned()],
            Some(" Arbeite sorgfältig. "),
        );
        assert_eq!(
            wide.extra_context.first().map(String::as_str),
            Some("Instructions of your role 'implementer':\nArbeite sorgfältig.")
        );
        assert!(
            wide.extra_context
                .iter()
                .any(|line| line
                    .contains("anywhere in the workspace except paths other workers own"))
        );
        assert!(
            wide.extra_context
                .last()
                .is_some_and(|line| line.contains(WORK_DRIVER_REPORT_TOOL))
        );
        let reader = worker_identity("explorer", RegistryProfile::ReadOnlyExplore, &[], None);
        assert!(
            reader
                .extra_context
                .first()
                .is_some_and(|line| line.starts_with("You run unattended"))
        );
        assert!(
            reader
                .extra_context
                .iter()
                .any(|line| line.contains("only read the workspace"))
        );
        let narrow = worker_identity(
            "implementer",
            RegistryProfile::WorkspaceEdit,
            &["src/a".to_owned()],
            None,
        );
        assert!(
            narrow
                .extra_context
                .iter()
                .any(|line| line.contains("Change only these paths: src/a."))
        );
    }

    #[test]
    fn test_blocks_hold_at_most_one_workspace_worker() {
        let wide = |id: &str| worker_request(id, vec![WORKSPACE_SCOPE.to_owned()]);
        let pending = vec![
            wide("w0"),
            worker_request("w1", vec!["a".to_owned()]),
            wide("w2"),
            wide("w3"),
        ];
        assert_eq!(chunk_end(&pending, 0, 4), 2);
        assert_eq!(chunk_end(&pending, 2, 4), 3);
        assert_eq!(chunk_end(&pending, 3, 4), 4);
        assert_eq!(chunk_end(&pending, 0, 1), 1);
        assert_eq!(
            chunk_end(&pending, 1, 0),
            2,
            "at least one request per block"
        );
    }

    #[tokio::test]
    async fn test_two_workspace_workers_never_run_in_the_same_block() -> TestResult {
        let mut state = new_state(&work_id(), Timestamp::now());
        state
            .workers
            .push(worker_state("w0", vec![WORKSPACE_SCOPE.to_owned()]));
        state
            .workers
            .push(worker_state("w1", vec![WORKSPACE_SCOPE.to_owned()]));
        state
            .workers
            .push(worker_state("w2", vec!["docs".to_owned()]));
        let requests = vec![
            worker_request("w0", vec![WORKSPACE_SCOPE.to_owned()]),
            worker_request("w1", vec![WORKSPACE_SCOPE.to_owned()]),
        ];
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let fakes = Fakes::new(goal(Vec::new()));
        let end = execute_wave(&mut state, &mut memory, &fakes.ports(), requests).await;
        assert_eq!(end, None);
        let waves = fakes
            .guard
            .waves
            .lock()
            .map(|waves| waves.clone())
            .map_err(|_| TestError::Unexpected("guard lock".to_owned()))?;
        assert_eq!(
            waves,
            vec![
                vec![("w0".to_owned(), vec![WORKSPACE_SCOPE.to_owned()])],
                vec![("w1".to_owned(), vec![WORKSPACE_SCOPE.to_owned()])],
            ]
        );
        let foreign = foreign_paths(&state, &waves[0]);
        assert_eq!(foreign, vec!["docs".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_workspace_worker_owns_the_rest_but_never_a_foreign_path() {
        let file = |path: &str| PatchFile {
            path: path.to_owned(),
            source_path: None,
            change: FileChange::Modified,
        };
        let diff = UnifiedDiff {
            base_revision: RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
            files: vec![file("src/a/x.rs"), file("README.md"), file("docs/y.md")],
        };
        let owners = vec![
            ("w0".to_owned(), vec![WORKSPACE_SCOPE.to_owned()]),
            ("w1".to_owned(), vec!["src/a".to_owned()]),
        ];
        let changes = attribute_changes(&diff, &owners, &["docs".to_owned()]);
        assert_eq!(
            changes.changed.get("w0"),
            Some(&vec!["README.md".to_owned()])
        );
        assert_eq!(
            changes.changed.get("w1"),
            Some(&vec!["src/a/x.rs".to_owned()])
        );
        assert_eq!(changes.violations.len(), 1, "{:?}", changes.violations);
        assert!(changes.violations[0].contains("docs/y.md"));
        assert!(changes.violations[0].contains("anderen Worker"));

        // Zwei Workspace-Worker in einem Block: nicht zuzuordnen, fail closed.
        let two = vec![
            ("w0".to_owned(), vec![WORKSPACE_SCOPE.to_owned()]),
            ("w1".to_owned(), vec![WORKSPACE_SCOPE.to_owned()]),
        ];
        let readme = UnifiedDiff {
            base_revision: RepoRevision(SCOPE_GUARD_REVISION.to_owned()),
            files: vec![file("README.md")],
        };
        let ambiguous = attribute_changes(&readme, &two, &[]);
        assert!(ambiguous.changed.is_empty());
        assert_eq!(ambiguous.violations.len(), 1);
    }

    #[test]
    fn test_verification_steps_include_artifacts_but_not_manual_steps() {
        let goal = goal(vec![
            artifact_criterion("a", "src/a.rs"),
            artifact_criterion("b", "src/a.rs"),
            command_criterion("c", "cargo test -p parser"),
            manual_criterion("d"),
        ]);
        let steps = verification_steps(&spec(5), &goal);
        let artifacts: Vec<&str> = steps
            .iter()
            .filter_map(|step| match step {
                VerificationStep::Artifact { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(artifacts, vec!["src/a.rs"]);
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, VerificationStep::Manual { .. }))
        );
        assert_eq!(steps.len(), 3, "{steps:?}");
    }

    #[test]
    fn test_report_from_run_maps_artifact_evidence_the_way_evaluate_goal_reads_it() -> TestResult {
        use harw_plan_bridge::verify_exec::StepReport;
        let mut found = evidence("x");
        found.kind = EvidenceKind::Other;
        found.locator = "workspace:src/a.rs".to_owned();
        let run = VerifyRun {
            steps: vec![StepReport {
                index: 0,
                step: VerificationStep::Artifact {
                    path: "src/a.rs".to_owned(),
                },
                outcome: VerifyOutcome::Passed { evidence: found },
                trace: None,
            }],
            skipped: 0,
        };
        let report = report_from_run(&run);
        assert_eq!(report.verdict, VerifyVerdict::Passed);
        assert_eq!(report.failed_steps, 0);
        let mut goal = goal(vec![artifact_criterion("a", "src/a.rs")]);
        goal.evidence = report.evidence;
        let evaluated = goal_report(&goal, None).map_err(TestError::Unexpected)?;
        assert_eq!(evaluated.criteria_met, vec![0]);
        Ok(())
    }

    #[tokio::test]
    async fn test_an_artifact_criterion_is_verified_centrally_and_proposed() -> TestResult {
        let harness = harness()?;
        let workspace = tempfile::tempdir().map_err(ctx("workspace dir"))?;
        std::fs::create_dir_all(workspace.path().join("src")).map_err(ctx("src dir"))?;
        std::fs::write(workspace.path().join("src/parser.rs"), "fn parse() {}")
            .map_err(ctx("artifact"))?;
        let fakes = Fakes::new(goal(vec![artifact_criterion(
            "Parser-Datei",
            "src/parser.rs",
        )]));
        let verifier = ExecutorVerifier {
            executor: VerificationExecutor::new(
                PassingRunner::default(),
                VerifyConfig::new(workspace.path(), DRIVER_ACTOR),
            ),
        };
        let ports = DrivePorts {
            verifier: &verifier,
            ..fakes.ports()
        };
        let input = input(10);
        let mut memory = memory_for(&input, None);
        let outcome = outcome_of(
            run_rounds(
                &harness.store,
                &work_id(),
                &input,
                &ports,
                &mut memory,
                None,
                &not_cancelled,
            )
            .await,
        )?;
        assert!(
            matches!(outcome, JobOutcome::Succeeded { .. }),
            "expected success, got {outcome:?}"
        );
        let goal = fakes.goals.snapshot()?;
        assert!(
            goal.evidence
                .iter()
                .any(|found| found.kind == EvidenceKind::Diff && found.locator == "src/parser.rs"),
            "evidence: {:?}",
            goal.evidence
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fewer_failing_verification_steps_count_as_progress() -> TestResult {
        for (second, expected) in [(1, 0), (3, 2)] {
            let harness = harness()?;
            let mut fakes = Fakes::new(goal(vec![command_criterion("a", "cargo test a")]));
            fakes.verifier =
                FakeVerifier::with_reports(vec![failed_report(3), failed_report(second)]);
            let input = input(20);
            let mut memory = memory_for(&input, None);
            // R1 Welle, R2 Verify (3 rot), R3 Basis + Fortsetzung, R4 Verify.
            step(&harness, &input, &fakes.ports(), &mut memory, 3).await;
            assert_eq!(state_of(&harness)?.usage.iterations_without_progress, 1);
            step(&harness, &input, &fakes.ports(), &mut memory, 2).await;
            assert_eq!(fakes.verifier.calls(), 2);
            assert_eq!(
                state_of(&harness)?.usage.iterations_without_progress,
                expected,
                "second verification with {second} failing steps"
            );
        }
        Ok(())
    }

    #[test]
    fn test_record_round_sets_baselines_before_it_counts() {
        let input = input(10);
        let mut memory = memory_for(&input, None);
        assert_eq!(memory.record_round(0), None, "first round: baseline only");
        memory.progress_pending = true;
        memory.last_failed_steps = Some(4);
        assert_eq!(
            memory.record_round(0),
            Some(false),
            "first verification: baseline"
        );
        memory.progress_pending = true;
        memory.last_failed_steps = Some(4);
        assert_eq!(memory.record_round(1), Some(true), "a newly met criterion");
        memory.progress_pending = true;
        memory.last_failed_steps = Some(2);
        assert_eq!(memory.record_round(1), Some(true), "fewer failing steps");
        memory.progress_pending = true;
        memory.last_failed_steps = Some(3);
        assert_eq!(memory.record_round(1), Some(false), "worse than the best");
        assert_eq!(memory.record_round(1), None, "no verification in between");
    }

    #[tokio::test]
    async fn test_an_unparsable_judge_is_asked_again_then_escalates() -> TestResult {
        let harness = harness()?;
        let mut fakes = Fakes::new(goal(vec![manual_criterion("Doku erklärt BOM-Verhalten")]));
        fakes.judge = FakeJudge::replying("Das kann ich nicht beurteilen.");
        let input = input(20);
        let mut memory = memory_for(&input, None);
        let outcome = outcome_of(
            run_rounds(
                &harness.store,
                &work_id(),
                &input,
                &fakes.ports(),
                &mut memory,
                None,
                &not_cancelled,
            )
            .await,
        )?;
        let JobOutcome::Blocked { reason } = outcome else {
            return Err(TestError::Unexpected(format!(
                "expected blocked, got {outcome:?}"
            )));
        };
        assert!(reason.starts_with("work_driver:NeedsInput"), "{reason}");
        assert!(reason.contains("kein auswertbares Urteil"), "{reason}");
        let requests = fakes.judge.requests()?;
        assert_eq!(requests.len(), MAX_UNPARSABLE_VERDICTS as usize);
        assert!(!requests[0].variable.starts_with(JUDGE_FORMAT_REMINDER));
        assert!(
            requests[1..]
                .iter()
                .all(|request| request.variable.starts_with(JUDGE_FORMAT_REMINDER))
        );
        let state = state_of(&harness)?;
        assert_eq!(state.last_judge, None, "no verdict was invented");
        assert_eq!(fakes.spawner.calls()?.len(), 1, "no worker was re-driven");
        Ok(())
    }

    #[tokio::test]
    async fn test_join_all_keeps_the_input_order() {
        let futures: Vec<BoxFuture<'_, u32>> = vec![
            Box::pin(async {
                tokio::task::yield_now().await;
                1
            }),
            Box::pin(async { 2 }),
        ];
        assert_eq!(join_all(futures).await, vec![1, 2]);
    }
}
