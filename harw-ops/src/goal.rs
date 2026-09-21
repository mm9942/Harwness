//! `/goal` — Ziel-Operation über `harw-plan`.
//!
//! # Verantwortungsbereich
//! Exponiert den in [`OpContext`] registrierten `GoalStore` als `/goal`-Command
//! (`channel_parity`) und als Modell-Tool. Ein Goal beschreibt den **gewünschten
//! Endzustand** — was am Ende wahr sein muss — und überlebt Plan-Revisionen,
//! Context-Compacts und Modellwechsel (philosophy.md §5). Der Plan ist nur die
//! aktuell gewählte Strategie.
//!
//! # Die zentrale Regel: `achieve` und `abandon` sind Command-only
//! Ein Modell darf ein Ziel schärfen, Kriterien ergänzen und es gegen den Plan
//! bewerten. Es darf ein Ziel **nicht** für erreicht oder für aufgegeben
//! erklären. Diese Grenze wird hier **zweifach und unabhängig** gezogen — das
//! ist Absicht, nicht Redundanz:
//!
//! 1. **Flächen-Grenze (in diesem Modul).** Kommt der Aufruf über die
//!    Modell-Tool- oder Agent-Tool-Fläche ([`CallSurface::Model`]) und lautet
//!    die Aktion `achieve` oder `abandon`, endet die Operation vor jedem
//!    Store-Zugriff mit [`OpError::NotAvailable`]. Der Store sieht die Aktion
//!    nie.
//! 2. **Akteur-Grenze (in `harw-plan`).** Selbst wenn Grenze 1 umgangen würde,
//!    prüft `harw_plan::goal::validate_goal_action` das `actor`-Präfix und
//!    weist jeden Akteur mit `"model:"` mit `PlanError::ActorNotAuthorized`
//!    zurück. Das Präfix stammt aus derselben Fläche
//!    ([`crate::plan::require_actor`]), wird aber in einer anderen Crate, an einer
//!    anderen Stelle und mit einem anderen Mechanismus geprüft.
//!
//! Bewusst **nicht** gewählt wurde der bequeme Weg, die Modell-Tool-Fläche
//! einfach wegzulassen: `show`, `check`, `refine`, `criteria`, `invariant`,
//! `constraint`, `question` und `bind` sind genau die Operationen, die ein
//! Modell braucht, um überhaupt zielgerichtet zu arbeiten. Eine Fläche
//! wegzunehmen, um eine einzelne Aktion zu schützen, würde das Werkzeug
//! entwerten statt es zu sichern.
//!
//! # Warum `NotAvailable` und nicht `PermissionDenied`
//! [`harw_operations::OpError`] hat genau drei Varianten: `InvalidArguments`,
//! `Execution`, `NotAvailable`. Eine `PermissionDenied`-Variante existiert
//! nicht. Die Doku von `NotAvailable` nennt „unzureichende Berechtigungen"
//! ausdrücklich als ihren Anwendungsfall — sie ist damit die passende
//! vorhandene Variante, nicht ein Notbehelf. Die Begründung im Text macht
//! unmissverständlich, dass es sich um eine Autoritätsgrenze handelt.
//!
//! # Approval-Politik
//! Wie bei [`crate::plan`]: deklariert ist `model_tool(approval = "always")`,
//! weil `map_approval` in `harw-macros/src/operation.rs` nur `"none"` und
//! `"always"` kennt. Gewollt wäre
//! [`harw_operations::ApprovalPolicy::RequireForScope`] — `show` und `check`
//! sind reine Lesezugriffe. Die nötige Makro-Erweiterung ist im Modulkopf von
//! [`crate::plan`] beschrieben.
//!
//! # Schlüsseltypen
//! - [`GoalArgs`] — Subcommand-Enum für beide Flächen.
//! - [`GoalCall`] — Hülle aus [`CallSurface`] + [`GoalArgs`]; wie in
//!   [`crate::plan`] leitet sie die Aufruf-Fläche aus dem Parse-Pfad des
//!   `#[operation]`-Makros ab.
//! - `GoalOperation` — vom `#[operation]`-Makro erzeugt.
//!
//! # Nebenläufigkeit
//! `GoalOperation` ist ein Unit-Struct ohne inneren Zustand (`Send + Sync`);
//! die Serialisierung liegt beim `GoalStore`.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Goal-Store, Werkzeug deaktiviert, oder
//!   ein Modell versucht `achieve`/`abandon`.
//! - [`OpError::InvalidArguments`] — Argumentgrammatik verletzt, noch kein Ziel
//!   gesetzt, unbekannte Constraint-Art.
//! - [`OpError::Execution`] — `harw-plan` lehnt die Mutation ab.

use harw_macros::operation;
use harw_operations::require_service;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_plan::error::PlanError;
use harw_plan::goal::{
    Constraint, ConstraintKind, Goal, GoalAction, GoalId, GoalPatch, GoalReport, GoalStatus,
    GoalStore, Invariant, evaluate_goal,
};
use harw_plan::ids::RevisionId;
use harw_plan::types::{Criterion, Plan, VerificationStep};
use harw_plan_bridge::OpContextPlanExt;
use time::OffsetDateTime;

use crate::plan::CallSurface;

// ── Argumente ────────────────────────────────────────────────────────────────

/// Subcommands der `/goal`-Operation.
///
/// # Beschreibung
/// Ein Enum für beide Flächen — `#[raw(subcommand)]` für die Command-Zeile,
/// `#[serde(tag = "action")]` für die Modell-Tool-Fläche,
/// [`harw_macros::OpArgs`] für das JSON-Schema. Jedes Feld trägt
/// `#[serde(default)]`, damit die Tool-Fläche Optionales weglassen darf.
///
/// # Spec-Referenz
/// AP W4-09; `harw_plan::goal` (Goal ≠ Plan, philosophy.md §5).
#[derive(Debug, Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[raw(subcommand)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum GoalArgs {
    /// `goal set <goal-id> <statement…>` — legt das Ziel an.
    Set {
        /// Bezeichner des Ziels.
        #[serde(default)]
        #[raw(nth = 0)]
        id: Option<String>,
        /// Was am Ende wahr sein muss.
        #[serde(default)]
        #[raw(join_from = 1)]
        statement: Option<String>,
    },
    /// `goal show` — Statement, Kriterien, Status, Coverage.
    #[default]
    #[raw(default_subcommand)]
    Show,
    /// `goal refine <statement…>` — Formulierung schärfen (Kriterien bleiben unberührt).
    Refine {
        /// Neue Formulierung des Ziel-Statements.
        #[serde(default)]
        #[raw(join_from = 0)]
        statement: Option<String>,
    },
    /// `goal criteria <beschreibung…>` — Akzeptanzkriterium ergänzen.
    Criteria {
        /// Beschreibung des Akzeptanzkriteriums.
        #[serde(default)]
        #[raw(join_from = 0)]
        description: Option<String>,
    },
    /// `goal invariant <beschreibung…>` — Invariante ergänzen.
    Invariant {
        /// Beschreibung der Invariante.
        #[serde(default)]
        #[raw(join_from = 0)]
        description: Option<String>,
    },
    /// `goal constraint <art> <beschreibung…>` — Rahmenbedingung ergänzen.
    Constraint {
        /// Art: budget, scope, policy, time, dependency.
        #[serde(default)]
        #[raw(nth = 0)]
        kind: Option<String>,
        /// Beschreibung der Rahmenbedingung.
        #[serde(default)]
        #[raw(join_from = 1)]
        description: Option<String>,
    },
    /// `goal question <text…>` — offene Frage vermerken.
    Question {
        /// Die offene Frage.
        #[serde(default)]
        #[raw(join_from = 0)]
        text: Option<String>,
    },
    /// `goal check` — Bewertung gegen den gebundenen Plan.
    Check,
    /// `goal bind <plan-id>` — Plan binden.
    Bind {
        /// Bezeichner des zu bindenden Plans.
        #[serde(default)]
        #[raw(nth = 0)]
        plan_id: Option<String>,
    },
    /// `goal achieve <begründung…>` — nur per Command, nie durch das Modell.
    Achieve {
        /// Begründung, warum das Ziel erreicht ist.
        #[serde(default)]
        #[raw(join_from = 0)]
        reason: Option<String>,
    },
    /// `goal abandon <begründung…>` — nur per Command.
    Abandon {
        /// Begründung, warum das Ziel aufgegeben wird.
        #[serde(default)]
        #[raw(join_from = 0)]
        reason: Option<String>,
    },
}

impl GoalArgs {
    /// `true`, wenn diese Aktion einem menschlichen Akteur vorbehalten ist.
    ///
    /// # Beschreibung
    /// Nur `achieve` und `abandon` erklären einen Endzustand des Ziels. Alles
    /// andere ist Vorschlag, Ergänzung oder Lesezugriff — dafür ist ein Modell
    /// ausdrücklich zuständig.
    #[must_use]
    pub fn is_human_only(&self) -> bool {
        matches!(self, Self::Achieve { .. } | Self::Abandon { .. })
    }

    /// Der kanonische Subcommand-Name (identisch zum `action`-Tag der JSON-Fläche).
    #[must_use]
    pub fn action_name(&self) -> &'static str {
        match self {
            Self::Set { .. } => "set",
            Self::Show => "show",
            Self::Refine { .. } => "refine",
            Self::Criteria { .. } => "criteria",
            Self::Invariant { .. } => "invariant",
            Self::Constraint { .. } => "constraint",
            Self::Question { .. } => "question",
            Self::Check => "check",
            Self::Bind { .. } => "bind",
            Self::Achieve { .. } => "achieve",
            Self::Abandon { .. } => "abandon",
        }
    }
}

/// Argumenttyp der `/goal`-Operation: Subcommand **plus** Aufruf-Fläche.
///
/// # Beschreibung
/// Identisches Muster wie [`crate::plan::PlanCall`] — die Fläche wird aus dem
/// Parse-Pfad des `#[operation]`-Makros abgeleitet:
/// `FromRawArgs` ⇒ [`CallSurface::Command`], `Deserialize`/`Default` ⇒
/// [`CallSurface::Model`]. Hier ist das keine Bequemlichkeit, sondern die
/// Grundlage der ersten Autoritätsgrenze (siehe Modulkopf).
#[derive(Debug, serde::Deserialize)]
#[serde(from = "GoalArgs")]
pub struct GoalCall {
    /// Fläche, über die der Aufruf kam.
    pub surface: CallSurface,
    /// Der gewählte Subcommand mit seinen Argumenten.
    pub args: GoalArgs,
}

impl GoalCall {
    /// Baut einen Aufruf der Command-Fläche (menschlicher Akteur).
    #[must_use]
    pub fn from_command(args: GoalArgs) -> Self {
        Self {
            surface: CallSurface::Command,
            args,
        }
    }

    /// Baut einen Aufruf der Modell-Tool-Fläche (Modell-Akteur).
    #[must_use]
    pub fn from_model(args: GoalArgs) -> Self {
        Self {
            surface: CallSurface::Model,
            args,
        }
    }
}

impl From<GoalArgs> for GoalCall {
    /// Serde-Pfad: erreicht nur aus `OpInvocation::ModelTool`/`AgentTool`.
    fn from(args: GoalArgs) -> Self {
        Self::from_model(args)
    }
}

impl Default for GoalCall {
    /// Default-Pfad: das Makro ruft `Default::default()` ausschließlich für die
    /// Tool-Flächen mit `Null`-Argumenten auf.
    fn default() -> Self {
        Self::from_model(GoalArgs::default())
    }
}

impl harw_operations::FromRawArgs for GoalCall {
    /// Command-Pfad: das Makro ruft `from_raw_args` **nur** für
    /// `OpInvocation::Command` auf — auch bei leerem Token-Slice.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self::from_command(
            <GoalArgs as harw_operations::FromRawArgs>::from_raw_args(tokens)?,
        ))
    }
}

impl harw_operations::OpArgsSchema for GoalCall {
    /// Reicht das von [`GoalArgs`] abgeleitete Subcommand-Schema durch.
    fn json_schema() -> harw_tools::JsonSchema {
        <GoalArgs as harw_operations::OpArgsSchema>::json_schema()
    }
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Führt einen `/goal`-Subcommand gegen den registrierten Goal-Store aus.
///
/// # Beschreibung
/// Reihenfolge (fail-closed):
/// 1. [`OpContextPlanExt::plan_config`] + [`harw_plan::PlanToolConfig::require_enabled`] —
///    `[tools.plan]` regiert die gesamte Planungsfläche; Plan- und Goal-Store
///    werden von `harw_plan_bridge::register_plan_services` als ein Bündel
///    eingetragen.
/// 2. **Flächen-Grenze**: `achieve`/`abandon` über eine Modell-Fläche enden
///    hier — vor jedem Store-Zugriff.
/// 3. [`OpContextPlanExt::goal_store`] — ohne Store keine Aktion.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext; liefert Session-Identität und
///   die Plan-Dienste.
/// - `call` (`GoalCall`): Subcommand plus Aufruf-Fläche.
///
/// # Rückgabe
/// `Ok(OpOutput::from(text))` mit kompaktem, für ein Modell lesbarem Text.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Goal-Store, Werkzeug deaktiviert, oder ein
///   Modell versucht `achieve`/`abandon`.
/// - [`OpError::InvalidArguments`]: fehlende Argumente, kein Ziel gesetzt.
/// - [`OpError::Execution`]: `harw-plan` lehnt die Mutation ab (z. B. weil der
///   Statusübergang nicht in der Goal-Status-Matrix steht).
///
/// # Nebenläufigkeit
/// Zustandslos; die Serialisierung liegt beim `GoalStore`.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über `Operation::run` — direkte Nutzung nur im Test.
/// ```
#[operation(
    name = "goal",
    summary = "Ziel verwalten: setzen, schärfen, Kriterien und Invarianten ergänzen, gegen den Plan bewerten.",
    domain = "execution",
    permission = "operator",
    command(path = "/goal", visibility = "channel_parity"),
    model_tool(approval = "always"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: aus demselben
    // Grund wie `/plan` (siehe dortiger Kommentar) — gemischte Lese-/
    // Schreib-Sub-Kommandos über einen Aufrufpfad, `approval = "always"`
    // behandelt jeden Aufruf konservativ als bestätigungspflichtig.
    web(path = "/api/goal", method = "post", approval = "always")
)]
async fn goal(ctx: &OpContext, call: GoalCall) -> Result<OpOutput, OpError> {
    let config = require_service!(ctx.plan_config(), "Goal-Store");
    config.require_enabled().map_err(|error| {
        OpError::NotAvailable(format!(
            "Ziel-Werkzeug ist nicht nutzbar ({error}); aktiviere es über `[tools.plan] enabled = true`"
        ))
    })?;

    // ── Erste Autoritätsgrenze: die Aufruf-Fläche ───────────────────────────
    // Vor jedem Store-Zugriff, damit ein Modell nicht einmal einen Versuch in
    // der Goal-History hinterlässt.
    if call.surface.is_model() && call.args.is_human_only() {
        return Err(OpError::NotAvailable(format!(
            "`goal {}` ist der Command-Fläche vorbehalten: nur ein menschlicher Akteur darf ein \
             Ziel für erreicht oder für aufgegeben erklären. Ein Modell schlägt vor, die Runtime \
             entscheidet. Nutze `goal check`, um den Belegstand zu berichten.",
            call.args.action_name()
        )));
    }

    let store_handle = require_service!(ctx.goal_store(), "Goal-Store");
    let store: &dyn GoalStore = store_handle.as_ref();
    let actor = crate::plan::require_actor(ctx, call.surface)?;

    let text = match call.args {
        GoalArgs::Show => render_show(store, ctx)?,
        GoalArgs::Check => render_check(store, ctx)?,

        GoalArgs::Set { id, statement } => {
            let usage = "goal set <goal-id> <statement…>";
            let id = require_arg(id, usage)?;
            let statement = require_arg(statement, usage)?;
            guard_replacement(store)?;
            let event = apply(
                store,
                GoalAction::Set {
                    goal: new_goal(&id, statement),
                },
                &actor,
            )?;
            format!("Ziel '{id}' gesetzt (Revision {event}).")
        }

        GoalArgs::Refine { statement } => {
            let statement = require_arg(statement, "goal refine <statement…>")?;
            let event = apply(
                store,
                GoalAction::Refine {
                    patch: GoalPatch {
                        statement: Some(statement),
                        ..GoalPatch::default()
                    },
                },
                &actor,
            )?;
            format!(
                "Statement geschärft (Revision {event}). Kriterien und Invarianten sind unberührt \
                 — der Typ `GoalPatch` erreicht sie gar nicht."
            )
        }

        GoalArgs::Criteria { description } => {
            let description = require_arg(description, "goal criteria <beschreibung…>")?;
            let event = apply(
                store,
                GoalAction::AddCriterion {
                    criterion: new_criterion(&description),
                },
                &actor,
            )?;
            let count = current(store)?.acceptance_criteria.len();
            format!(
                "Kriterium {count} ergänzt (Revision {event}): {description}\n\
                 Verifikation: manual '{description}' — belegbar mit \
                 `plan evidence <task-id> manual {description}`."
            )
        }

        GoalArgs::Invariant { description } => {
            let description = require_arg(description, "goal invariant <beschreibung…>")?;
            let goal = current(store)?;
            let invariant = new_invariant(goal.invariants.len(), &description);
            let id = invariant.id.clone();
            let event = apply(store, GoalAction::AddInvariant { invariant }, &actor)?;
            format!(
                "Invariante '{id}' ergänzt (Revision {event}): {description}\n\
                 Verifikation: manual '{description}'."
            )
        }

        GoalArgs::Constraint { kind, description } => {
            let usage = "goal constraint <budget|scope|policy|time|dependency> <beschreibung…>";
            let kind = parse_constraint_kind(&require_arg(kind, usage)?)?;
            let description = require_arg(description, usage)?;
            let mut constraints = current(store)?.constraints;
            constraints.push(Constraint {
                kind,
                statement: description.clone(),
            });
            let count = constraints.len();
            let event = apply(
                store,
                GoalAction::Refine {
                    patch: GoalPatch {
                        constraints: Some(constraints),
                        ..GoalPatch::default()
                    },
                },
                &actor,
            )?;
            format!(
                "Rahmenbedingung [{}] ergänzt (Revision {event}); {count} insgesamt: {description}",
                constraint_label(kind)
            )
        }

        GoalArgs::Question { text } => {
            let text = require_arg(text, "goal question <text…>")?;
            let mut questions = current(store)?.open_questions;
            questions.push(text.clone());
            let count = questions.len();
            let event = apply(
                store,
                GoalAction::Refine {
                    patch: GoalPatch {
                        open_questions: Some(questions),
                        ..GoalPatch::default()
                    },
                },
                &actor,
            )?;
            format!("Offene Frage {count} vermerkt (Revision {event}): {text}")
        }

        GoalArgs::Bind { plan_id } => {
            let plan_id = require_arg(plan_id, "goal bind <plan-id>")?;
            let (revision, note) = plan_revision_for(ctx, &plan_id);
            let event = apply(
                store,
                GoalAction::BindPlan {
                    plan_id: harw_plan::ids::PlanId::new(plan_id.as_str()),
                    revision,
                },
                &actor,
            )?;
            format!(
                "Ziel an Plan '{plan_id}' @ Revision {revision} gebunden (Revision {event}).{note}"
            )
        }

        GoalArgs::Achieve { reason } => {
            let reason = require_arg(reason, "goal achieve <begründung…>")?;
            set_status(store, GoalStatus::Achieved, reason, &actor)?
        }

        GoalArgs::Abandon { reason } => {
            let reason = require_arg(reason, "goal abandon <begründung…>")?;
            set_status(store, GoalStatus::Abandoned, reason, &actor)?
        }
    };

    Ok(OpOutput::from(text))
}

// ── Store-Zugriff ────────────────────────────────────────────────────────────

/// Liest das aktuelle Ziel.
fn current(store: &dyn GoalStore) -> Result<Goal, OpError> {
    store.current().map_err(map_goal_error)
}

/// Wendet eine Goal-Aktion an und liefert die neue Revision.
fn apply(store: &dyn GoalStore, action: GoalAction, actor: &str) -> Result<u64, OpError> {
    store
        .apply(action, actor)
        .map(|event| event.revision)
        .map_err(map_goal_error)
}

/// Setzt einen terminalen Zielstatus und nennt Akteur und Begründung im Bericht.
///
/// # Beschreibung
/// Beide terminalen Übergänge laufen über dieselbe Stelle. `Achieved` verlangt
/// laut `validate_goal_action` mindestens ein Akzeptanzkriterium und einen
/// Ausgangsstatus, der laut Goal-Status-Matrix dorthin führen darf — beides
/// prüft `harw-plan`, nicht diese Operation.
///
/// # Errors
/// - [`OpError::NotAvailable`]: der Akteur trägt ein `"model:"`-Präfix (zweite
///   Autoritätsgrenze, siehe Modulkopf).
/// - [`OpError::Execution`]: der Übergang ist in der Status-Matrix nicht
///   vorgesehen oder es fehlt ein Akzeptanzkriterium.
fn set_status(
    store: &dyn GoalStore,
    status: GoalStatus,
    reason: String,
    actor: &str,
) -> Result<String, OpError> {
    let event = apply(
        store,
        GoalAction::SetStatus {
            status,
            reason: Some(reason.clone()),
        },
        actor,
    )?;
    Ok(format!(
        "Ziel ist jetzt {} (Revision {event}), erklärt von '{actor}'.\nBegründung: {reason}",
        status_label(status)
    ))
}

/// Verhindert, dass ein laufendes Ziel durch `goal set` still ersetzt wird.
///
/// # Beschreibung
/// [`GoalAction::Set`] ersetzt das Ziel **vollständig** — Kriterien und
/// Invarianten inklusive. Genau das ist die Schrumpfung, die philosophy.md §5
/// verbietet. Ein bereits terminales Ziel (`Achieved`, `Abandoned`,
/// `Superseded`) darf dagegen von einem neuen abgelöst werden: dort ist nichts
/// mehr zu verlieren.
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn ein nicht-terminales Ziel existiert.
fn guard_replacement(store: &dyn GoalStore) -> Result<(), OpError> {
    let Ok(existing) = store.current() else {
        // Kein Ziel vorhanden — `Set` ist der reguläre Weg.
        return Ok(());
    };
    if matches!(
        existing.status,
        GoalStatus::Achieved | GoalStatus::Abandoned | GoalStatus::Superseded
    ) {
        return Ok(());
    }
    Err(OpError::InvalidArguments(format!(
        "es existiert bereits das Ziel '{}' ({}); `goal set` würde seine {} Kriterien und {} \
         Invarianten ersatzlos verwerfen. Schärfe es mit `goal refine <statement…>` oder schließe \
         es zuerst mit `goal achieve`/`goal abandon` ab.",
        existing.id,
        status_label(existing.status),
        existing.acceptance_criteria.len(),
        existing.invariants.len()
    )))
}

/// Übersetzt einen [`PlanError`] aus dem Goal-Pfad in einen [`OpError`].
///
/// # Beschreibung
/// - `GoalNotFound` ist keine Störung, sondern ein Zustand: es gibt noch kein
///   Ziel. Das gehört zu `InvalidArguments`, damit ein Modell den nächsten
///   Schritt (`goal set`) ableiten kann.
/// - `ActorNotAuthorized` ist die **zweite Autoritätsgrenze**. Sie darf nicht
///   als beliebiger Ausführungsfehler durchgereicht werden, sondern wird als
///   `NotAvailable` gemeldet — mit derselben Aussage wie die erste Grenze.
fn map_goal_error(error: PlanError) -> OpError {
    match error {
        PlanError::GoalNotFound => OpError::InvalidArguments(
            "es existiert noch kein Ziel; lege es mit `goal set <goal-id> <statement…>` an"
                .to_owned(),
        ),
        PlanError::ActorNotAuthorized { action, actor } => OpError::NotAvailable(format!(
            "Akteur '{actor}' darf '{action}' nicht ausführen: nur ein menschlicher Akteur darf \
             ein Ziel für erreicht oder für aufgegeben erklären"
        )),
        other => OpError::Execution(format!("Goal-Store lehnt ab: {other}")),
    }
}

// ── Argument-Helfer ──────────────────────────────────────────────────────────

/// Fordert ein Pflichtargument oder meldet die Aufrufform.
fn require_arg(value: Option<String>, usage: &str) -> Result<String, OpError> {
    match value {
        Some(raw) if !raw.trim().is_empty() => Ok(raw.trim().to_owned()),
        _ => Err(OpError::InvalidArguments(format!(
            "fehlendes Argument; Aufruf: {usage}"
        ))),
    }
}

/// Parst eine Constraint-Art.
///
/// # Errors
/// [`OpError::InvalidArguments`] mit der vollständigen Werteliste.
fn parse_constraint_kind(raw: &str) -> Result<ConstraintKind, OpError> {
    match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "budget" => Ok(ConstraintKind::Budget),
        "scope" => Ok(ConstraintKind::Scope),
        "policy" => Ok(ConstraintKind::Policy),
        "time" => Ok(ConstraintKind::Time),
        "dependency" => Ok(ConstraintKind::Dependency),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannte Constraint-Art '{other}'; erwartet eines von: budget, scope, policy, \
             time, dependency"
        ))),
    }
}

/// Baut ein frisches Ziel im Status [`GoalStatus::Active`].
///
/// # Beschreibung
/// `Active` und nicht `Draft`: die Goal-Status-Matrix kennt keinen Übergang
/// `Draft → Achieved`, und diese Operation bietet bewusst kein `activate`
/// an — ein mit `goal set` erklärtes Ziel wird verfolgt, sonst hätte man es
/// nicht gesetzt. `revision`, `created_at` und `updated_at` sind Platzhalter:
/// sie gehören dem Store.
fn new_goal(id: &str, statement: String) -> Goal {
    Goal {
        id: GoalId::new(id),
        revision: 0,
        statement,
        non_goals: Vec::new(),
        invariants: Vec::new(),
        acceptance_criteria: Vec::new(),
        constraints: Vec::new(),
        open_questions: Vec::new(),
        status: GoalStatus::Active,
        plan_id: None,
        plan_revision: None,
        evidence: Vec::new(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

/// Baut ein Akzeptanzkriterium aus seiner Beschreibung.
///
/// # Beschreibung
/// Ein Kriterium **ohne** Verifikationsschritt gilt in
/// `harw_plan::goal::evaluate_goal` niemals als erfüllt — eine leere Liste ist
/// dort bewusst „nicht belegt". Ein von der Kommandozeile ergänztes Kriterium
/// bekommt deshalb einen [`VerificationStep::Manual`], dessen `note` die
/// Beschreibung ist. Damit belegt genau ein `EvidenceRef` der Art `manual` mit
/// demselben Lokator dieses Kriterium — siehe
/// `harw_plan::goal::verification_satisfied_by`.
fn new_criterion(description: &str) -> Criterion {
    Criterion {
        description: description.to_owned(),
        verification: vec![VerificationStep::Manual {
            note: description.to_owned(),
        }],
    }
}

/// Baut eine Invariante mit fortlaufender, deterministischer ID.
fn new_invariant(existing: usize, statement: &str) -> Invariant {
    Invariant {
        id: format!("inv-{}", existing.saturating_add(1)),
        statement: statement.to_owned(),
        verification: vec![VerificationStep::Manual {
            note: statement.to_owned(),
        }],
    }
}

/// Ermittelt die Revision, mit der ein Plan gebunden wird.
///
/// # Beschreibung
/// Bindet an die Revision des tatsächlich geladenen Plans, sofern dessen ID zum
/// Argument passt. Passt sie nicht (oder fehlt der Plan-Store), wird
/// `RevisionId::new(0)` gebunden und der Rückgabetext sagt das ausdrücklich —
/// eine erfundene Revision wäre schlimmer als eine sichtbar unbekannte.
///
/// # Rückgabe
/// `(RevisionId, Hinweistext)`; der Hinweistext ist leer, wenn die Revision aus
/// dem Store stammt.
fn plan_revision_for(ctx: &OpContext, plan_id: &str) -> (RevisionId, String) {
    let Some(store) = ctx.plan_store() else {
        return (
            RevisionId::new(0),
            " Hinweis: kein Plan-Store verfügbar — Revision 0 gebunden.".to_owned(),
        );
    };
    match store.current() {
        Ok(plan) if plan.id.as_str() == plan_id => (plan.revision, String::new()),
        Ok(plan) => (
            RevisionId::new(0),
            format!(
                " Hinweis: der geladene Plan heißt '{}' — Revision 0 gebunden.",
                plan.id
            ),
        ),
        Err(_) => (
            RevisionId::new(0),
            " Hinweis: noch kein Plan angelegt — Revision 0 gebunden.".to_owned(),
        ),
    }
}

// ── Ausgabe ──────────────────────────────────────────────────────────────────

/// Kanonischer Name eines Zielstatus (identisch zur Serde-Repräsentation).
fn status_label(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Draft => "draft",
        GoalStatus::Active => "active",
        GoalStatus::Blocked => "blocked",
        GoalStatus::Achieved => "achieved",
        GoalStatus::Abandoned => "abandoned",
        GoalStatus::Superseded => "superseded",
    }
}

/// Kanonischer Name einer Constraint-Art.
fn constraint_label(kind: ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::Budget => "budget",
        ConstraintKind::Scope => "scope",
        ConstraintKind::Policy => "policy",
        ConstraintKind::Time => "time",
        ConstraintKind::Dependency => "dependency",
    }
}

/// Kurzform eines Verifikationsschrittes.
fn verification_label(step: &VerificationStep) -> String {
    match step {
        VerificationStep::Command { cmd, expect_exit } => {
            format!("command '{cmd}' (exit {expect_exit})")
        }
        VerificationStep::Artifact { path } => format!("artifact '{path}'"),
        VerificationStep::TraceEvent { name } => format!("trace '{name}'"),
        VerificationStep::Manual { note } => format!("manual '{note}'"),
    }
}

/// Verbindet die Verifikationsschritte eines Kriteriums zu einer Zeile.
fn verification_line(steps: &[VerificationStep]) -> String {
    if steps.is_empty() {
        return "keine (kann nie als erfüllt gelten)".to_owned();
    }
    steps
        .iter()
        .map(verification_label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formatiert eine Coverage als ganzzahligen Prozentwert.
fn coverage_line(report: &GoalReport, total: usize) -> String {
    format!(
        "Coverage: {:.0} % ({} von {total} Kriterien belegt)",
        report.coverage * 100.0,
        report.criteria_met.len()
    )
}

/// Liest den aktuellen Plan-Snapshot, falls einer verfügbar ist.
fn current_plan(ctx: &OpContext) -> Option<Plan> {
    ctx.plan_store().and_then(|store| store.current().ok())
}

/// `goal show` — Statement, Kriterien, Invarianten, Constraints, Status, Coverage.
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn noch kein Ziel gesetzt wurde.
fn render_show(store: &dyn GoalStore, ctx: &OpContext) -> Result<String, OpError> {
    let goal = current(store)?;
    let binding = match (&goal.plan_id, goal.plan_revision) {
        (Some(plan_id), Some(revision)) => format!("{plan_id} @ Revision {revision}"),
        (Some(plan_id), None) => plan_id.to_string(),
        _ => "keine".to_owned(),
    };

    let mut lines = vec![
        format!(
            "Ziel {} [{}] (Revision {})",
            goal.id,
            status_label(goal.status),
            goal.revision
        ),
        format!("Statement: {}", goal.statement),
        format!("Plan-Bindung: {binding}"),
    ];

    if !goal.non_goals.is_empty() {
        lines.push(format!("Non-Goals: {}", goal.non_goals.join("; ")));
    }

    lines.push(format!("Kriterien ({}):", goal.acceptance_criteria.len()));
    for (index, criterion) in goal.acceptance_criteria.iter().enumerate() {
        lines.push(format!(
            "  {}. {}",
            index.saturating_add(1),
            criterion.description
        ));
        lines.push(format!(
            "     Verifikation: {}",
            verification_line(&criterion.verification)
        ));
    }

    lines.push(format!("Invarianten ({}):", goal.invariants.len()));
    for invariant in &goal.invariants {
        lines.push(format!("  {}: {}", invariant.id, invariant.statement));
    }

    lines.push(format!("Constraints ({}):", goal.constraints.len()));
    for constraint in &goal.constraints {
        lines.push(format!(
            "  [{}] {}",
            constraint_label(constraint.kind),
            constraint.statement
        ));
    }

    lines.push(format!("Offene Fragen ({}):", goal.open_questions.len()));
    for (index, question) in goal.open_questions.iter().enumerate() {
        lines.push(format!("  {}. {question}", index.saturating_add(1)));
    }

    lines.push(match current_plan(ctx) {
        Some(plan) => coverage_line(&evaluate_goal(&goal, &plan), goal.acceptance_criteria.len()),
        None => "Coverage: nicht bewertbar (kein Plan verfügbar)".to_owned(),
    });

    Ok(lines.join("\n"))
}

/// `goal check` — vollständige Bewertung gegen den gebundenen Plan.
///
/// # Beschreibung
/// Ruft `harw_plan::goal::evaluate_goal` und rendert den [`GoalReport`]:
/// erfüllte und offene Kriterien mit **1-basierter** Nummer und Text, verletzte
/// Invarianten, Coverage in Prozent, blockierende Knoten und die vorgeschlagenen
/// nächsten Schritte.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: kein Ziel gesetzt oder kein Plan angelegt.
/// - [`OpError::NotAvailable`]: kein Plan-Store konfiguriert.
fn render_check(store: &dyn GoalStore, ctx: &OpContext) -> Result<String, OpError> {
    let goal = current(store)?;
    let plan_store = ctx.plan_store().ok_or_else(|| {
        OpError::NotAvailable(
            "kein Plan-Store in diesem Kontext konfiguriert; ohne Plan gibt es keine Evidenz, \
             gegen die ein Ziel bewertet werden könnte"
                .to_owned(),
        )
    })?;
    let plan = plan_store.current().map_err(|error| match error {
        PlanError::PlanNotFound => OpError::InvalidArguments(
            "es existiert noch kein Plan; lege ihn mit `plan create <plan-id> <ziel…>` an"
                .to_owned(),
        ),
        other => OpError::Execution(format!("Plan-Store lehnt ab: {other}")),
    })?;

    let report = evaluate_goal(&goal, &plan);
    let total = goal.acceptance_criteria.len();

    let mut lines = vec![
        format!(
            "Zielbewertung {} [{}] gegen Plan {} (Revision {})",
            goal.id,
            status_label(goal.status),
            plan.id,
            plan.revision
        ),
        coverage_line(&report, total),
    ];

    lines.push(format!("Erfüllt ({}):", report.criteria_met.len()));
    for &index in &report.criteria_met {
        lines.push(format!(
            "  {}. {}",
            index.saturating_add(1),
            goal.acceptance_criteria[index].description
        ));
    }

    lines.push(format!("Offen ({}):", report.criteria_open.len()));
    for &index in &report.criteria_open {
        lines.push(format!(
            "  {}. {}",
            index.saturating_add(1),
            goal.acceptance_criteria[index].description
        ));
    }

    lines.push(format!(
        "Verletzte Invarianten ({}):",
        report.invariants_violated.len()
    ));
    for id in &report.invariants_violated {
        let statement = goal
            .invariants
            .iter()
            .find(|invariant| &invariant.id == id)
            .map_or("(unbekannt)", |invariant| invariant.statement.as_str());
        lines.push(format!("  {id}: {statement}"));
    }

    if report.blocking_nodes.is_empty() {
        lines.push("Blockierende Knoten: keine".to_owned());
    } else {
        let ids = report
            .blocking_nodes
            .iter()
            .map(harw_plan::ids::TaskId::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "Blockierende Knoten ({}): {ids}",
            report.blocking_nodes.len()
        ));
    }

    lines.push(format!("Nächste Schritte ({}):", report.next_actions.len()));
    for action in &report.next_actions {
        lines.push(format!("  - {action}"));
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::{GoalArgs, GoalCall};
    use crate::plan::CallSurface;
    use crate::testutil::toks;
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError};
    use harw_plan::actions::PlanAction;
    use harw_plan::error::PlanError;
    use harw_plan::goal::{GoalAction, GoalStatus, GoalStore};
    use harw_plan::ids::{PlanId, TaskId};
    use harw_plan::types::{EvidenceKind, EvidenceRef, PlanNode, PlanNodeKind, PlanNodeStatus};
    use harw_plan::{InMemoryGoalStore, InMemoryPlanStore, PlanStore, PlanToolConfig};
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{
        IngressSurface, PermissionTier, Principal, PrincipalKind, SessionId, TenantId, TurnId,
        WorkspaceId,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use time::OffsetDateTime;

    // ── Goal-Store: echte Implementierung ────────────────────────────────────
    //
    // Bis AP W1-08b lieferte `harw-plan` noch keine `GoalStore`-Implementierung,
    // und dieses Modul baute sich dafür ein eigenes `TestGoalStore`-Double, das
    // `validate_goal_action`/`apply_goal_action` nachbaute (Locking, Revisions-
    // und Zeitstempelvergabe, History). Seit `harw_plan::InMemoryGoalStore`
    // existiert, verwenden die Tests dieses Moduls sie direkt — kein zweiter Ort
    // mehr für dieselbe Store-Mechanik.

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// Menschlicher Test-Principal, wie ihn `local_principal(Cli)` baut.
    fn human_principal() -> Principal {
        Principal::trusted_ingress(
            PrincipalKind::Human,
            "uid:1000",
            IngressSurface::Cli,
            PermissionTier::Operator,
        )
    }

    /// Baut einen `OpContext` mit temporärem Workspace und den übergebenen
    /// Diensten; ergänzt einen menschlichen Principal, falls keiner darin
    /// liegt. Seit `require_actor`/`require_principal` an der Eingangsgrenze
    /// einen authentifizierten Principal verlangen (siehe `plan.rs`), bräuchte
    /// jeder Testfall sonst individuell einen — dieselbe Konvention wie
    /// `plan::tests::context_with`.
    fn context_with(mut services: ServiceMap) -> (OpContext, std::path::PathBuf) {
        if services.get::<Principal>().is_none() {
            services.insert(human_principal());
        }
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("harw-goal-op-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).expect("Test-Workspace anlegen");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("WorkspaceRegistry bauen");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("Workspace auflösen");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        )
    }

    /// Kontext mit aktiviertem Werkzeug, Goal-Store und Plan-Store.
    fn context_with_stores() -> (
        OpContext,
        Arc<dyn GoalStore>,
        Arc<dyn PlanStore>,
        std::path::PathBuf,
    ) {
        let goal_store: Arc<dyn GoalStore> = Arc::new(InMemoryGoalStore::default());
        let plan_store: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&goal_store));
        services.insert(Arc::clone(&plan_store));
        services.insert(PlanToolConfig::enabled_defaults());
        let (ctx, root) = context_with(services);
        (ctx, goal_store, plan_store, root)
    }

    fn cleanup(root: std::path::PathBuf) {
        std::fs::remove_dir_all(root).ok();
    }

    async fn run_command(ctx: &OpContext, tokens: &[&str]) -> Result<String, OpError> {
        let call = GoalCall::from_raw_args(&toks(tokens))?;
        super::goal(ctx, call).await.map(|output| output.text)
    }

    /// Baut einen minimalen Plan-Knoten im Status `Draft` ohne Scopes.
    fn draft_node(id: &str) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "arbeiten".to_owned(),
            dependencies: Vec::new(),
            input_contracts: Vec::new(),
            output_contracts: Vec::new(),
            read_scope: Vec::new(),
            write_scope: Vec::new(),
            forbidden_scope: Vec::new(),
            acceptance_criteria: Vec::new(),
            invalidation_conditions: Vec::new(),
            status: PlanNodeStatus::Draft,
            evidence: Vec::new(),
            kind: PlanNodeKind::Coding,
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Legt einen Plan mit einem abgeschlossenen Knoten an, der genau einen
    /// `manual`-Nachweis mit dem übergebenen Lokator trägt.
    fn seed_completed_plan(store: &dyn PlanStore, locator: &str) {
        let seeded = store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new("p-1"),
                    goal: "Belegter Plan".to_owned(),
                },
                "test",
            )
            .and_then(|_| {
                store.apply(
                    PlanAction::AddNode {
                        node: draft_node("t-1"),
                    },
                    "test",
                )
            })
            .and_then(|_| {
                store.apply(
                    PlanAction::AttachEvidence {
                        id: TaskId::new("t-1"),
                        evidence: EvidenceRef {
                            kind: EvidenceKind::Manual,
                            locator: locator.to_owned(),
                            attached_at: OffsetDateTime::UNIX_EPOCH,
                            actor: "test".to_owned(),
                            digest: None,
                        },
                    },
                    "test",
                )
            })
            .and_then(|_| {
                store.apply(
                    PlanAction::SetStatus {
                        id: TaskId::new("t-1"),
                        status: PlanNodeStatus::Ready,
                        reason: None,
                    },
                    "test",
                )
            })
            .and_then(|_| {
                store.apply(
                    PlanAction::SetStatus {
                        id: TaskId::new("t-1"),
                        status: PlanNodeStatus::InProgress,
                        reason: None,
                    },
                    "test",
                )
            })
            .and_then(|_| {
                store.apply(
                    PlanAction::SetStatus {
                        id: TaskId::new("t-1"),
                        status: PlanNodeStatus::Completed,
                        reason: None,
                    },
                    "test",
                )
            });
        if let Err(error) = seeded {
            panic!("Plan-Fixture schlug fehl: {error}");
        }
    }

    // ── Argument-Parsing ─────────────────────────────────────────────────────

    #[test]
    fn from_raw_args_without_tokens_uses_default_subcommand_show() {
        match GoalCall::from_raw_args(&toks(&[])) {
            Ok(call) => {
                assert_eq!(call.surface, CallSurface::Command);
                assert!(matches!(call.args, GoalArgs::Show));
            }
            Err(error) => panic!("leerer Input muss den Default liefern: {error}"),
        }
    }

    #[test]
    fn from_raw_args_unknown_subcommand_is_invalid_arguments() {
        assert!(matches!(
            GoalCall::from_raw_args(&toks(&["wünschen"])),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn from_raw_args_set_takes_id_and_joined_statement() {
        match GoalCall::from_raw_args(&toks(&["set", "g-1", "Alles", "grün"])) {
            Ok(GoalCall {
                args: GoalArgs::Set { id, statement },
                surface,
            }) => {
                assert_eq!(surface, CallSurface::Command);
                assert_eq!(id.as_deref(), Some("g-1"));
                assert_eq!(statement.as_deref(), Some("Alles grün"));
            }
            other => panic!("erwartet Set, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_joined_single_argument_subcommands_parse() {
        match GoalCall::from_raw_args(&toks(&["refine", "Neuer", "Wortlaut"])).map(|c| c.args) {
            Ok(GoalArgs::Refine { statement }) => {
                assert_eq!(statement.as_deref(), Some("Neuer Wortlaut"));
            }
            other => panic!("erwartet Refine, war: {other:?}"),
        }
        match GoalCall::from_raw_args(&toks(&["criteria", "Tests", "grün"])).map(|c| c.args) {
            Ok(GoalArgs::Criteria { description }) => {
                assert_eq!(description.as_deref(), Some("Tests grün"));
            }
            other => panic!("erwartet Criteria, war: {other:?}"),
        }
        match GoalCall::from_raw_args(&toks(&["invariant", "keine", "Downtime"])).map(|c| c.args) {
            Ok(GoalArgs::Invariant { description }) => {
                assert_eq!(description.as_deref(), Some("keine Downtime"));
            }
            other => panic!("erwartet Invariant, war: {other:?}"),
        }
        match GoalCall::from_raw_args(&toks(&["question", "Wie", "testen?"])).map(|c| c.args) {
            Ok(GoalArgs::Question { text }) => {
                assert_eq!(text.as_deref(), Some("Wie testen?"));
            }
            other => panic!("erwartet Question, war: {other:?}"),
        }
        match GoalCall::from_raw_args(&toks(&["achieve", "alles", "belegt"])).map(|c| c.args) {
            Ok(GoalArgs::Achieve { reason }) => {
                assert_eq!(reason.as_deref(), Some("alles belegt"));
            }
            other => panic!("erwartet Achieve, war: {other:?}"),
        }
        match GoalCall::from_raw_args(&toks(&["abandon", "nicht", "mehr", "nötig"])).map(|c| c.args)
        {
            Ok(GoalArgs::Abandon { reason }) => {
                assert_eq!(reason.as_deref(), Some("nicht mehr nötig"));
            }
            other => panic!("erwartet Abandon, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_constraint_takes_kind_and_joined_description() {
        match GoalCall::from_raw_args(&toks(&["constraint", "scope", "nur", "harw-ops"]))
            .map(|c| c.args)
        {
            Ok(GoalArgs::Constraint { kind, description }) => {
                assert_eq!(kind.as_deref(), Some("scope"));
                assert_eq!(description.as_deref(), Some("nur harw-ops"));
            }
            other => panic!("erwartet Constraint, war: {other:?}"),
        }
    }

    #[test]
    fn from_raw_args_bind_and_unit_subcommands_parse() {
        match GoalCall::from_raw_args(&toks(&["bind", "p-1"])).map(|c| c.args) {
            Ok(GoalArgs::Bind { plan_id }) => assert_eq!(plan_id.as_deref(), Some("p-1")),
            other => panic!("erwartet Bind, war: {other:?}"),
        }
        assert!(matches!(
            GoalCall::from_raw_args(&toks(&["check"])).map(|c| c.args),
            Ok(GoalArgs::Check)
        ));
        assert!(matches!(
            GoalCall::from_raw_args(&toks(&["show"])).map(|c| c.args),
            Ok(GoalArgs::Show)
        ));
    }

    #[test]
    fn json_surface_deserializes_to_model_surface() {
        let value = serde_json::json!({ "action": "criteria", "description": "Tests grün" });
        match serde_json::from_value::<GoalCall>(value) {
            Ok(call) => {
                assert_eq!(call.surface, CallSurface::Model);
                assert!(matches!(call.args, GoalArgs::Criteria { .. }));
            }
            Err(error) => panic!("JSON-Deserialisierung schlug fehl: {error}"),
        }
    }

    #[test]
    fn human_only_actions_are_exactly_achieve_and_abandon() {
        assert!(GoalArgs::Achieve { reason: None }.is_human_only());
        assert!(GoalArgs::Abandon { reason: None }.is_human_only());
        assert!(!GoalArgs::Show.is_human_only());
        assert!(!GoalArgs::Check.is_human_only());
        assert!(!GoalArgs::Refine { statement: None }.is_human_only());
        assert!(!GoalArgs::Criteria { description: None }.is_human_only());
    }

    // ── Verfügbarkeit ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn missing_goal_store_is_not_available() {
        let mut services = ServiceMap::new();
        services.insert(PlanToolConfig::enabled_defaults());
        let (ctx, root) = context_with(services);

        let result = super::goal(&ctx, GoalCall::from_command(GoalArgs::Show)).await;
        cleanup(root);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("kein Goal-Store"), "war: {message}");
            }
            other => panic!("erwartet NotAvailable, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn disabled_config_is_not_available_and_names_the_switch() {
        let goal_store: Arc<dyn GoalStore> = Arc::new(InMemoryGoalStore::default());
        let mut services = ServiceMap::new();
        services.insert(goal_store);
        services.insert(PlanToolConfig::default());
        let (ctx, root) = context_with(services);

        let result = super::goal(&ctx, GoalCall::from_command(GoalArgs::Show)).await;
        cleanup(root);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("[tools.plan] enabled"), "war: {message}");
            }
            other => panic!("erwartet NotAvailable, war: {other:?}"),
        }
    }

    // ── Autoritätsgrenze ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn achieve_is_rejected_on_the_model_tool_surface_and_allowed_on_the_command_surface() {
        let (ctx, store, _plan, root) = context_with_stores();

        let set = run_command(&ctx, &["set", "g-1", "Autorität", "prüfen"]).await;
        let criterion = run_command(&ctx, &["criteria", "Tests", "grün"]).await;

        let via_model = super::goal(
            &ctx,
            GoalCall::from_model(GoalArgs::Achieve {
                reason: Some("ich finde es fertig".to_owned()),
            }),
        )
        .await;
        let status_after_model = store.current().map(|goal| goal.status);
        let history_after_model = store.history(None).map(|events| events.len());

        let via_command = run_command(&ctx, &["achieve", "alle", "Kriterien", "belegt"]).await;
        let status_after_command = store.current().map(|goal| goal.status);
        cleanup(root);

        assert!(set.is_ok(), "set schlug fehl: {set:?}");
        assert!(criterion.is_ok(), "criteria schlug fehl: {criterion:?}");

        match via_model {
            Err(OpError::NotAvailable(message)) => {
                assert!(
                    message.contains("menschlicher Akteur"),
                    "die Begründung muss die Autoritätsgrenze nennen: {message}"
                );
                assert!(message.contains("goal achieve"), "war: {message}");
            }
            other => panic!("Modell-Fläche muss abgelehnt werden, war: {other:?}"),
        }
        assert_eq!(
            status_after_model.ok(),
            Some(GoalStatus::Active),
            "der abgelehnte Versuch darf den Status nicht verändern"
        );
        assert_eq!(
            history_after_model.ok(),
            Some(2),
            "der abgelehnte Versuch darf keinen Eintrag in der Goal-History hinterlassen"
        );

        assert!(via_command.is_ok(), "Command-Fläche: {via_command:?}");
        assert_eq!(status_after_command.ok(), Some(GoalStatus::Achieved));
    }

    #[tokio::test]
    async fn abandon_is_rejected_on_the_model_tool_surface() {
        let (ctx, _store, _plan, root) = context_with_stores();
        let set = run_command(&ctx, &["set", "g-1", "Autorität", "prüfen"]).await;
        let result = super::goal(
            &ctx,
            GoalCall::from_model(GoalArgs::Abandon {
                reason: Some("lohnt nicht".to_owned()),
            }),
        )
        .await;
        cleanup(root);

        assert!(set.is_ok(), "set schlug fehl: {set:?}");
        assert!(matches!(result, Err(OpError::NotAvailable(_))));
    }

    #[tokio::test]
    async fn the_second_boundary_rejects_a_model_actor_inside_harw_plan() {
        // Defense in depth: dieselbe Aktion direkt am Store, mit einem
        // `model:`-Akteur — die Flächen-Grenze dieses Moduls wird bewusst
        // umgangen, `validate_goal_action` muss trotzdem ablehnen.
        let store = InMemoryGoalStore::default();
        let seeded = store.apply(
            GoalAction::Set {
                goal: super::new_goal("g-1", "Zweite Grenze".to_owned()),
            },
            "human:tester",
        );
        let with_criterion = store.apply(
            GoalAction::AddCriterion {
                criterion: super::new_criterion("Tests grün"),
            },
            "human:tester",
        );
        let result = store.apply(
            GoalAction::SetStatus {
                status: GoalStatus::Achieved,
                reason: None,
            },
            "model:opus",
        );

        assert!(seeded.is_ok(), "Fixture schlug fehl: {seeded:?}");
        assert!(with_criterion.is_ok(), "Fixture: {with_criterion:?}");
        assert!(
            matches!(result, Err(PlanError::ActorNotAuthorized { .. })),
            "harw-plan muss den model:-Akteur ablehnen, war: {result:?}"
        );
    }

    // ── Bewertung ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn check_reports_fifty_percent_when_one_of_two_criteria_is_covered() {
        let (ctx, _store, plan_store, root) = context_with_stores();
        seed_completed_plan(plan_store.as_ref(), "Tests grün");

        let set = run_command(&ctx, &["set", "g-1", "Halbe", "Strecke"]).await;
        let first = run_command(&ctx, &["criteria", "Tests", "grün"]).await;
        let second = run_command(&ctx, &["criteria", "Doku", "vollständig"]).await;
        let check = run_command(&ctx, &["check"]).await;
        cleanup(root);

        assert!(set.is_ok(), "set schlug fehl: {set:?}");
        assert!(first.is_ok(), "erstes Kriterium: {first:?}");
        assert!(second.is_ok(), "zweites Kriterium: {second:?}");

        match check {
            Ok(text) => {
                assert!(
                    text.contains("Coverage: 50 % (1 von 2 Kriterien belegt)"),
                    "erwartet 50 %, war:\n{text}"
                );
                assert!(text.contains("Erfüllt (1):"), "war:\n{text}");
                assert!(text.contains("1. Tests grün"), "war:\n{text}");
                assert!(text.contains("Offen (1):"), "war:\n{text}");
                assert!(text.contains("2. Doku vollständig"), "war:\n{text}");
            }
            Err(error) => panic!("check schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn show_lists_statement_criteria_status_and_coverage() {
        let (ctx, _store, plan_store, root) = context_with_stores();
        seed_completed_plan(plan_store.as_ref(), "Tests grün");

        let set = run_command(&ctx, &["set", "g-1", "Sichtbar", "machen"]).await;
        let criterion = run_command(&ctx, &["criteria", "Tests", "grün"]).await;
        let constraint = run_command(&ctx, &["constraint", "scope", "nur", "harw-ops"]).await;
        let question = run_command(&ctx, &["question", "Wer", "prüft?"]).await;
        let show = run_command(&ctx, &["show"]).await;
        cleanup(root);

        assert!(set.is_ok(), "set: {set:?}");
        assert!(criterion.is_ok(), "criteria: {criterion:?}");
        assert!(constraint.is_ok(), "constraint: {constraint:?}");
        assert!(question.is_ok(), "question: {question:?}");

        match show {
            Ok(text) => {
                assert!(text.contains("Ziel g-1 [active]"), "war:\n{text}");
                assert!(text.contains("Statement: Sichtbar machen"), "war:\n{text}");
                assert!(text.contains("Kriterien (1):"), "war:\n{text}");
                assert!(text.contains("[scope] nur harw-ops"), "war:\n{text}");
                assert!(text.contains("1. Wer prüft?"), "war:\n{text}");
                assert!(
                    text.contains("Coverage: 100 % (1 von 1 Kriterien belegt)"),
                    "war:\n{text}"
                );
            }
            Err(error) => panic!("show schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn show_without_a_goal_points_at_goal_set() {
        let (ctx, _store, _plan, root) = context_with_stores();
        let result = super::goal(&ctx, GoalCall::from_command(GoalArgs::Show)).await;
        cleanup(root);

        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("goal set"), "war: {message}");
            }
            other => panic!("erwartet InvalidArguments, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_refuses_to_replace_a_live_goal_and_names_what_would_be_lost() {
        let (ctx, _store, _plan, root) = context_with_stores();
        let set = run_command(&ctx, &["set", "g-1", "Erstes", "Ziel"]).await;
        let criterion = run_command(&ctx, &["criteria", "Tests", "grün"]).await;
        let replaced = run_command(&ctx, &["set", "g-2", "Zweites", "Ziel"]).await;
        cleanup(root);

        assert!(set.is_ok(), "set: {set:?}");
        assert!(criterion.is_ok(), "criteria: {criterion:?}");
        match replaced {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("g-1"), "war: {message}");
                assert!(message.contains("goal refine"), "war: {message}");
            }
            other => panic!("erwartet InvalidArguments, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn refine_changes_the_statement_and_keeps_the_criteria() {
        let (ctx, store, _plan, root) = context_with_stores();
        let set = run_command(&ctx, &["set", "g-1", "Alter", "Wortlaut"]).await;
        let criterion = run_command(&ctx, &["criteria", "Tests", "grün"]).await;
        let refined = run_command(&ctx, &["refine", "Neuer", "Wortlaut"]).await;
        let goal = store.current();
        cleanup(root);

        assert!(set.is_ok(), "set: {set:?}");
        assert!(criterion.is_ok(), "criteria: {criterion:?}");
        assert!(refined.is_ok(), "refine: {refined:?}");
        match goal {
            Ok(goal) => {
                assert_eq!(goal.statement, "Neuer Wortlaut");
                assert_eq!(goal.acceptance_criteria.len(), 1);
            }
            Err(error) => panic!("Ziel lesen schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn bind_uses_the_revision_of_the_loaded_plan() {
        let (ctx, store, plan_store, root) = context_with_stores();
        seed_completed_plan(plan_store.as_ref(), "Tests grün");
        let expected = plan_store.revision();

        let set = run_command(&ctx, &["set", "g-1", "Binden"]).await;
        let bound = run_command(&ctx, &["bind", "p-1"]).await;
        let goal = store.current();
        cleanup(root);

        assert!(set.is_ok(), "set: {set:?}");
        assert!(bound.is_ok(), "bind: {bound:?}");
        match goal {
            Ok(goal) => {
                assert_eq!(goal.plan_id, Some(PlanId::new("p-1")));
                assert_eq!(goal.plan_revision, Some(expected));
            }
            Err(error) => panic!("Ziel lesen schlug fehl: {error}"),
        }
    }

    #[tokio::test]
    async fn unknown_constraint_kind_is_rejected_with_the_full_value_list() {
        let (ctx, _store, _plan, root) = context_with_stores();
        let set = run_command(&ctx, &["set", "g-1", "Rahmen"]).await;
        let result = run_command(&ctx, &["constraint", "wetter", "sonnig"]).await;
        cleanup(root);

        assert!(set.is_ok(), "set: {set:?}");
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("wetter"), "war: {message}");
                assert!(message.contains("policy"), "Werteliste fehlt: {message}");
            }
            other => panic!("erwartet InvalidArguments, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_arguments_report_the_usage_line() {
        let (ctx, _store, _plan, root) = context_with_stores();
        let result = super::goal(
            &ctx,
            GoalCall::from_command(GoalArgs::Set {
                id: None,
                statement: None,
            }),
        )
        .await;
        cleanup(root);

        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("goal set"), "war: {message}");
            }
            other => panic!("erwartet InvalidArguments, war: {other:?}"),
        }
    }
}
