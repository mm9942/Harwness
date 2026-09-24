//! Plan-Katalog, Bestätigung und Schritt-Verfolgung der `plan`-Operation
//! (Runde 5, Teil P).
//!
//! # Verantwortungsbereich
//! Die `plan`-Operation ([`crate::plan`]) ruft hier ein; dieses Modul hält
//! die Logik, damit `plan.rs` nur Einhängepunkte trägt:
//!
//! | Subcommand | Funktion | Wirkung |
//! |---|---|---|
//! | `list` (Befehl: `/plan plans`) | [`render_list`] | alle Pläne mit Freigabe, Fortschritt, aktiv/archiviert |
//! | `switch <id>` | [`switch`] | Plan aktiv machen (holt ihn aus dem Archiv) |
//! | `archive <id>` | [`archive`] | Plan ausblenden, nichts löschen |
//! | `inspect [id]` | [`inspect_status_lines`] | Freigabe, Fortschrittsbalken, aktueller Schritt |
//! | `create` | [`after_create`] | Modell-Fläche: Plan ist ein **Vorschlag** |
//! | `submit [id]` | [`submit`] | Vorschlag zur Bestätigung vorlegen |
//! | `step <id> <status> [beleg…]` | [`step`] | Fortschritt melden, `done` nur mit Beleg |
//! | Mutationen | [`gate_change`] | Rückfrage bei wesentlichen Änderungen |
//!
//! # Freigabe (proposed → confirmed)
//! - **Befehlsfläche (Mensch):** ein mit `/plan create` angelegter Plan ist
//!   sofort bestätigt; `/plan submit` bestätigt einen Vorschlag direkt.
//! - **Modell-Fläche:** `create` legt einen **Vorschlag** an
//!   (`PlanApproval::Proposed`). Der Agent baut ihn ohne Rückfragen aus
//!   (`add`, `dep`, `patch` …) und legt ihn mit `submit` vor. In der TUI
//!   öffnet das Freigabefenster in der Variante „Plan bestätigen“
//!   ([`harw_tool_plan::PlanConfirmChannel`]) und zeigt den gerenderten Plan
//!   (Ziel, Schritte mit Abhängigkeiten, Wellen, Lese-/Schreibbereiche,
//!   Verifikation). Erst „bestätigen“ macht den Plan aktiv, bindet ihn an
//!   ein Goal und erlaubt die Umsetzung (`status … in_progress`, `step …`).
//!   „ablehnen“ gibt die Rückmeldung an den Agenten zurück; der Plan bleibt
//!   Vorschlag.
//! - **Wesentliche Änderungen** an einem bestätigten Plan über die
//!   Modell-Fläche (Knoten hinzufügen, zerlegen, verdichten, ablösen,
//!   invalidieren, Plan-Revision ablösen) fragen in der TUI ebenfalls; erst
//!   nach „bestätigen“ wird die Änderung übernommen.
//! - **Ohne TUI** (Gateway, One-Shot, Jobs) gibt es kein Fenster: `create`
//!   legt einen Vorschlag an und meldet das, Änderungen wirken wie bisher
//!   sofort, die Umsetzung eines Vorschlags wird nicht gesperrt.
//! - **Automatisch erzeugte Pläne** (`/analyze` → `plan-analyze`) laufen
//!   nicht über diese Operation und fragen nie; sie gelten als bestätigt.
//!
//! # Goal-Verfolgung
//! Ein bestätigter Plan wird an ein Goal gebunden ([`bind_goal`]): ein
//! passendes, nicht abgeschlossenes Goal wird wiederverwendet, sonst entsteht
//! aus dem Plan-Ziel `goal-<plan-id>`. Die Knoten sind die Schritte; ihr
//! Fortschritt steht in `plan inspect`, `/goal show` und der TUI-Marke.
//!
//! # Nebenläufigkeit
//! Zustandslos; die Serialisierung liegt bei den Stores.

use harw_operations::{OpContext, OpError};
use harw_plan::actions::PlanAction;
use harw_plan::error::PlanError;
use harw_plan::goal::{Goal, GoalAction, GoalId, GoalStatus, GoalStore};
use harw_plan::ids::{PlanId, TaskId};
use harw_plan::types::{EvidenceKind, EvidenceRef, Plan, PlanNode, PlanNodeStatus};
use harw_plan::{
    PlanApproval, PlanStore, current_step, graph, has_blocked_step, plan_progress, progress_bar,
};
use harw_plan_bridge::OpContextPlanExt;
use harw_tool_plan::{PlanConfirmChannel, PlanConfirmKind, PlanConfirmOutcome};
use time::OffsetDateTime;

use crate::plan::{
    CallSurface, join_scope, kind_label, map_plan_error, parse_evidence_kind, parse_plan_id,
    require_arg, status_label,
};

/// Breite des Fortschrittsbalkens in Textausgaben.
const BAR_WIDTH: usize = 10;

/// Der Bestätigungskanal der TUI, falls diese Montage einen hat.
fn confirm_channel(ctx: &OpContext) -> Option<&PlanConfirmChannel> {
    ctx.service::<PlanConfirmChannel>()
}

/// Liest einen Plan per ID oder den aktiven.
pub(crate) fn plan_or_active(store: &dyn PlanStore, id: Option<String>) -> Result<Plan, OpError> {
    match id.filter(|raw| !raw.trim().is_empty()) {
        Some(raw) => store
            .plan_by_id(&parse_plan_id(raw.trim())?)
            .map_err(map_plan_error),
        None => store.current().map_err(map_plan_error),
    }
}

// ── Katalog ─────────────────────────────────────────────────────────────────

/// `plan list` — alle Pläne des Stores.
///
/// # Errors
/// [`OpError::Execution`], wenn der Store nicht lesbar ist.
pub(crate) fn render_list(store: &dyn PlanStore) -> Result<String, OpError> {
    let plans = store.list_plans().map_err(map_plan_error)?;
    if plans.is_empty() {
        return Ok(
            "Keine Pläne im Store. Lege einen mit `plan create <plan-id> <ziel…>` an.".to_owned(),
        );
    }
    let mut lines = vec![format!("{} Plan/Pläne im Store:", plans.len())];
    for summary in &plans {
        let mut marks = vec![summary.meta.approval.label()];
        if summary.active {
            marks.insert(0, "aktiv");
        }
        if summary.meta.archived {
            marks.push("archiviert");
        }
        let goal = summary
            .goal_id
            .as_deref()
            .map(|goal| format!(" · Goal {goal}"))
            .unwrap_or_default();
        lines.push(format!(
            "{} {} [{}] Rev {} · {}/{} erledigt{goal} — {}",
            if summary.active { "*" } else { "-" },
            summary.id,
            marks.join(", "),
            summary.revision,
            summary.completed,
            summary.node_count,
            summary.goal_statement
        ));
    }
    lines.push(
        "`plan switch <id>` wechselt den aktiven Plan, `plan archive <id>` blendet einen aus."
            .to_owned(),
    );
    Ok(lines.join("\n"))
}

/// `plan switch <id>` — macht einen Plan aktiv.
///
/// # Errors
/// [`OpError::InvalidArguments`] ohne/mit ungültiger ID,
/// [`OpError::Execution`] bei unbekannter ID oder Schreibfehler.
pub(crate) fn switch(
    store: &dyn PlanStore,
    id: Option<String>,
    actor: &str,
) -> Result<String, OpError> {
    let id = parse_plan_id(&require_arg(id, "plan switch <plan-id>")?)?;
    let plan = store.switch_plan(&id, actor).map_err(map_plan_error)?;
    let approval = store
        .plan_meta(&id)
        .map(|meta| meta.approval)
        .unwrap_or_default();
    let (done, total) = plan_progress(&plan);
    Ok(format!(
        "Aktiver Plan ist jetzt '{id}' ({}) — Ziel: {}\nFortschritt {}",
        approval.label(),
        plan.goal_statement,
        progress_bar(done, total, BAR_WIDTH)
    ))
}

/// `plan archive <id>` — blendet einen Plan aus, ohne ihn zu löschen.
///
/// # Errors
/// Wie [`switch`].
pub(crate) fn archive(
    store: &dyn PlanStore,
    id: Option<String>,
    actor: &str,
) -> Result<String, OpError> {
    let id = parse_plan_id(&require_arg(id, "plan archive <plan-id>")?)?;
    let was_active = store.current().is_ok_and(|plan| plan.id == id);
    store.archive_plan(&id, actor).map_err(map_plan_error)?;
    let tail = if was_active {
        " Er war aktiv — jetzt ist kein Plan aktiv; `plan switch <id>` oder `plan create …` \
         wählt einen."
    } else {
        ""
    };
    Ok(format!(
        "Plan '{id}' archiviert (nicht gelöscht; `plan switch {id}` holt ihn zurück).{tail}"
    ))
}

/// Zusatzzeilen für `plan inspect`: Freigabe, Fortschritt, aktueller Schritt.
#[must_use]
pub(crate) fn inspect_status_lines(store: &dyn PlanStore, plan: &Plan) -> String {
    let meta = store.plan_meta(&plan.id).unwrap_or_default();
    let active = store.current().is_ok_and(|current| current.id == plan.id);
    let (done, total) = plan_progress(plan);
    let mut lines = vec![format!(
        "Freigabe: {}{}{}",
        meta.approval.label(),
        if active {
            " · aktiv"
        } else {
            " · nicht aktiv"
        },
        if meta.archived { " · archiviert" } else { "" }
    )];
    let current = current_step(plan)
        .map(|node| {
            format!(
                " · aktueller Schritt: {} ({}) {}",
                node.id,
                step_label(node.status),
                node.objective
            )
        })
        .unwrap_or_default();
    lines.push(format!(
        "Fortschritt {}{current}",
        progress_bar(done, total, BAR_WIDTH)
    ));
    if has_blocked_step(plan) {
        lines.push("⚠ mindestens ein Schritt ist blockiert".to_owned());
    }
    if meta.approval == PlanApproval::Proposed {
        lines.push(
            "Vorschlag — noch nicht bestätigt. Mit `plan submit` zur Bestätigung vorlegen."
                .to_owned(),
        );
    }
    lines.join("\n")
}

/// Schritt-Status in Nutzersprache.
fn step_label(status: PlanNodeStatus) -> &'static str {
    match status {
        PlanNodeStatus::Draft | PlanNodeStatus::Ready => "offen",
        PlanNodeStatus::InProgress => "läuft",
        PlanNodeStatus::Blocked => "blockiert",
        PlanNodeStatus::Completed => "erledigt",
        PlanNodeStatus::Superseded => "abgelöst",
        PlanNodeStatus::Invalidated => "invalidiert",
    }
}

// ── Freigabe ────────────────────────────────────────────────────────────────

/// Nach einem erfolgreichen `create`: Freigabestand setzen und melden.
///
/// # Beschreibung
/// Befehlsfläche (Mensch): bestätigt, nichts weiter. Modell-Fläche: der Plan
/// wird Vorschlag; der Hinweis nennt den nächsten Schritt (`submit`) bzw.
/// ohne TUI, dass niemand bestätigen kann.
///
/// # Errors
/// [`OpError::Execution`], wenn der Freigabestand nicht gespeichert werden kann.
pub(crate) fn after_create(
    ctx: &OpContext,
    store: &dyn PlanStore,
    plan_id: &PlanId,
    surface: CallSurface,
    actor: &str,
) -> Result<String, OpError> {
    if surface == CallSurface::Command {
        return Ok("Der Plan ist aktiv und bestätigt.".to_owned());
    }
    match store.set_approval(plan_id, PlanApproval::Proposed, actor) {
        Ok(()) => {}
        // Einzelplan-Stores (Test-/Fremd-Implementierungen) kennen keinen
        // Freigabestand: der Plan bleibt dann wie bisher sofort verbindlich.
        Err(PlanError::CatalogUnsupported { .. }) => {
            return Ok(
                "Dieser Plan-Store kennt keinen Vorschlagsstatus; der Plan ist sofort \
                       verbindlich."
                    .to_owned(),
            );
        }
        Err(error) => return Err(map_plan_error(error)),
    }
    Ok(if confirm_channel(ctx).is_some() {
        format!(
            "Der Plan ist ein VORSCHLAG (proposed) und noch nicht bestätigt. Baue ihn aus \
             (`plan add`, `plan dep`, `plan patch` — ohne Rückfragen) und lege ihn dann mit \
             `plan submit` vor; die Nutzerin sieht ihn und bestätigt oder lehnt mit Rückmeldung \
             ab. Umsetzen (`plan step`, `plan status … in_progress`) geht erst nach der \
             Bestätigung. Ein früherer Plan bleibt erhalten (`plan list`, `plan switch`). \
             Plan-ID: {plan_id}"
        )
    } else {
        format!(
            "Der Plan ist als VORSCHLAG (proposed) angelegt. In diesem Einstieg gibt es kein \
             Bestätigungsfenster; die Nutzerin kann ihn in der TUI mit `/plan submit {plan_id}` \
             bestätigen."
        )
    })
}

/// `plan submit [id]` — legt einen Vorschlag zur Bestätigung vor.
///
/// # Beschreibung
/// Befehlsfläche: die Nutzerin bestätigt selbst — sofort bestätigt.
/// Modell-Fläche mit TUI: Freigabefenster „Plan bestätigen“ mit dem
/// gerenderten Plan; nur „bestätigen“ macht ihn aktiv und bindet ein Goal.
/// Modell-Fläche ohne TUI: bleibt Vorschlag, mit Hinweis.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: kein Plan, leerer Plan.
/// - [`OpError::Execution`]: Store-Fehler oder Turn-Abbruch.
pub(crate) async fn submit(
    ctx: &OpContext,
    store: &dyn PlanStore,
    id: Option<String>,
    surface: CallSurface,
    actor: &str,
) -> Result<String, OpError> {
    let plan = plan_or_active(store, id)?;
    let meta = store.plan_meta(&plan.id).map_err(map_plan_error)?;
    if surface == CallSurface::Command {
        return confirm_and_track(ctx, store, &plan.id, actor);
    }
    if meta.approval == PlanApproval::Confirmed {
        return Ok(format!(
            "Plan '{}' ist bereits bestätigt — setze ihn um und melde den Fortschritt mit \
             `plan step <task-id> <running|done|blocked> [beleg…]`.",
            plan.id
        ));
    }
    if plan.nodes.is_empty() {
        return Err(OpError::InvalidArguments(format!(
            "Plan '{}' hat noch keine Schritte — erst `plan add <task-id> <kind> <ziel…>`, dann \
             `plan submit`",
            plan.id
        )));
    }
    let Some(channel) = confirm_channel(ctx) else {
        return Ok(format!(
            "Plan '{}' bleibt ein Vorschlag: in diesem Einstieg gibt es kein Bestätigungsfenster. \
             Die Nutzerin bestätigt ihn in der TUI mit `/plan submit {}`.",
            plan.id, plan.id
        ));
    };
    let summary = format!(
        "{} Schritt(e) · Ziel: {}",
        plan.nodes.len(),
        plan.goal_statement
    );
    let content = render_confirm_markdown(&plan, None);
    match channel
        .ask(
            plan.id.as_str(),
            PlanConfirmKind::NewPlan,
            &summary,
            content,
            ctx.cancel_token(),
        )
        .await
    {
        PlanConfirmOutcome::Confirmed => confirm_and_track(ctx, store, &plan.id, actor),
        PlanConfirmOutcome::Rejected { feedback } => Ok(format!(
            "Plan '{}' wurde NICHT bestätigt und bleibt ein Vorschlag. Rückmeldung der Nutzerin: \
             {}\nArbeite sie ein (`plan add`/`plan patch`/`plan dep` …) und lege den Plan erneut \
             mit `plan submit` vor.",
            plan.id,
            feedback_text(&feedback)
        )),
        PlanConfirmOutcome::NoAnswer => Ok(format!(
            "Keine Entscheidung zu Plan '{}' (Fenster geschlossen oder Zeitablauf) — er bleibt \
             ein Vorschlag. Frage die Nutzerin, wie es weitergehen soll.",
            plan.id
        )),
        PlanConfirmOutcome::Cancelled => Err(OpError::Execution(
            "Turn abgebrochen, bevor der Plan bestätigt wurde".to_owned(),
        )),
    }
}

/// Rückmeldung für das Modell (leer → Hinweis).
fn feedback_text(feedback: &str) -> String {
    if feedback.trim().is_empty() {
        "(keine Rückmeldung eingegeben)".to_owned()
    } else {
        feedback.trim().to_owned()
    }
}

/// Bestätigt einen Plan: Freigabe setzen, aktiv schalten, an ein Goal binden.
///
/// # Errors
/// [`OpError::Execution`] bei Store-Fehlern (die Goal-Bindung selbst ist
/// best effort und meldet Probleme nur im Text).
fn confirm_and_track(
    ctx: &OpContext,
    store: &dyn PlanStore,
    plan_id: &PlanId,
    actor: &str,
) -> Result<String, OpError> {
    store
        .set_approval(plan_id, PlanApproval::Confirmed, actor)
        .map_err(map_plan_error)?;
    let plan = store.switch_plan(plan_id, actor).map_err(map_plan_error)?;
    let goal_line = match ctx.goal_store() {
        Some(goals) => bind_goal(goals.as_ref(), store, &plan, actor),
        None => "Goal-Bindung: keine (kein Goal-Store in diesem Einstieg).".to_owned(),
    };
    let plan = store.current().unwrap_or(plan);
    let (done, total) = plan_progress(&plan);
    Ok(format!(
        "Plan '{plan_id}' ist bestätigt und aktiv.\n{goal_line}\nFortschritt {}\nSetze ihn \
         Schritt für Schritt um und melde jeden Schritt mit `plan step <task-id> \
         <running|done|blocked> [beleg…]`; `done` zählt nur mit Beleg (geänderte Dateien, \
         Testergebnis).",
        progress_bar(done, total, BAR_WIDTH)
    ))
}

/// Bindet einen bestätigten Plan an ein Goal.
///
/// # Beschreibung
/// - Ein nicht abgeschlossenes Goal wird wiederverwendet, wenn es zu diesem
///   Plan passt (bereits gebunden, noch ohne Plan, oder der Plan verweist
///   darauf).
/// - Ein nicht abgeschlossenes Goal eines **anderen** Plans wird als
///   `Superseded` abgelöst — die Nutzerin hat gerade einen neuen Plan
///   bestätigt; der Goal-Store hält genau ein Goal.
/// - Sonst entsteht `goal-<plan-id>` aus dem Plan-Ziel (Status `Active`).
///
/// Danach zeigen Plan (`BindGoal`) und Goal (`BindPlan`) aufeinander.
///
/// # Returns
/// Eine Zeile für den Bericht; Fehler werden dort gemeldet, nicht geworfen.
pub(crate) fn bind_goal(
    goals: &dyn GoalStore,
    store: &dyn PlanStore,
    plan: &Plan,
    actor: &str,
) -> String {
    let existing = goals
        .current()
        .ok()
        .filter(|goal| !is_terminal(goal.status));
    let fits = |goal: &Goal| {
        goal.plan_id.as_ref().is_none_or(|bound| bound == &plan.id)
            || plan.goal_id.as_deref() == Some(goal.id.as_str())
    };
    let (goal_id, reused) = match existing {
        Some(goal) if fits(&goal) => (goal.id.clone(), true),
        other => {
            if let Some(previous) = other {
                let reason = format!("abgelöst durch den bestätigten Plan '{}'", plan.id);
                if let Err(error) = goals.apply(
                    GoalAction::SetStatus {
                        status: GoalStatus::Superseded,
                        reason: Some(reason),
                    },
                    actor,
                ) {
                    return format!(
                        "Goal-Bindung übersprungen: das laufende Goal '{}' ließ sich nicht \
                         ablösen ({error}).",
                        previous.id
                    );
                }
            }
            let goal = new_goal(plan);
            let id = goal.id.clone();
            if let Err(error) = goals.apply(GoalAction::Set { goal }, actor) {
                return format!("Goal-Bindung übersprungen: {error}");
            }
            (id, false)
        }
    };
    if plan.goal_id.as_deref() != Some(goal_id.as_str())
        && let Err(error) = store.apply(
            PlanAction::BindGoal {
                goal_id: goal_id.as_str().to_owned(),
            },
            actor,
        )
    {
        return format!("Goal-Bindung unvollständig (Plan → Goal): {error}");
    }
    if let Err(error) = goals.apply(
        GoalAction::BindPlan {
            plan_id: plan.id.clone(),
            revision: store.revision(),
        },
        actor,
    ) {
        return format!("Goal-Bindung unvollständig (Goal → Plan): {error}");
    }
    if reused {
        format!("Goal-Bindung: bestehendes Goal '{goal_id}' verfolgt jetzt diesen Plan.")
    } else {
        format!(
            "Goal-Bindung: neues Goal '{goal_id}' aus dem Plan-Ziel angelegt; `/goal show` zeigt \
             den Fortschritt."
        )
    }
}

/// `true` für abgeschlossene Goal-Zustände.
fn is_terminal(status: GoalStatus) -> bool {
    matches!(
        status,
        GoalStatus::Achieved | GoalStatus::Abandoned | GoalStatus::Superseded
    )
}

/// Baut das Goal zu einem bestätigten Plan.
fn new_goal(plan: &Plan) -> Goal {
    Goal {
        id: GoalId::new(format!("goal-{}", plan.id)),
        revision: 0,
        statement: plan.goal_statement.clone(),
        non_goals: Vec::new(),
        invariants: Vec::new(),
        acceptance_criteria: Vec::new(),
        constraints: Vec::new(),
        open_questions: Vec::new(),
        status: GoalStatus::Active,
        plan_id: Some(plan.id.clone()),
        plan_revision: Some(plan.revision),
        evidence: Vec::new(),
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
    }
}

// ── Rückfrage bei Änderungen ────────────────────────────────────────────────

/// Ergebnis von [`gate_change`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChangeGate {
    /// Weiter wie gewohnt.
    Proceed,
    /// Nicht anwenden; der Text geht an den Aufrufer (kein Fehler).
    Refused(String),
}

/// `true` für Mutationen, die den Plan wesentlich verändern
/// (Knoten hinzufügen/entfernen, Struktur umbauen).
fn is_significant(action: &PlanAction) -> bool {
    matches!(
        action,
        PlanAction::AddNode { .. }
            | PlanAction::Expand { .. }
            | PlanAction::Condense { .. }
            | PlanAction::Supersede { .. }
            | PlanAction::Invalidate { .. }
            | PlanAction::SetStatus {
                status: PlanNodeStatus::Superseded | PlanNodeStatus::Invalidated,
                ..
            }
    )
}

/// `true` für Mutationen, die einen Plan **umsetzen** (Schritt starten oder
/// abschließen).
fn is_execution(action: &PlanAction) -> bool {
    matches!(
        action,
        PlanAction::SetStatus {
            status: PlanNodeStatus::Ready | PlanNodeStatus::InProgress | PlanNodeStatus::Completed,
            ..
        }
    )
}

/// Prüft eine Mutation der Modell-Fläche vor dem Anwenden.
///
/// # Beschreibung
/// - Umsetzung eines **Vorschlags** in der TUI → abgelehnt mit Hinweis auf
///   `submit` (erst nach Bestätigung aktiv).
/// - Wesentliche Änderung eines **bestätigten** Plans in der TUI → Rückfrage
///   mit dem gerenderten Plan nach der Änderung; nur „bestätigen“ lässt sie
///   durch.
/// - Befehlsfläche, Vorschlag im Entwurf, ohne TUI → unverändert durch.
///
/// # Errors
/// [`OpError::Execution`] bei Turn-Abbruch während der Rückfrage.
pub(crate) async fn gate_change(
    ctx: &OpContext,
    store: &dyn PlanStore,
    surface: CallSurface,
    action: &PlanAction,
    label: &str,
) -> Result<ChangeGate, OpError> {
    if surface == CallSurface::Command {
        return Ok(ChangeGate::Proceed);
    }
    let Some(channel) = confirm_channel(ctx) else {
        return Ok(ChangeGate::Proceed);
    };
    let Ok(plan) = store.current() else {
        // Kein aktiver Plan: der Store meldet das gleich selbst.
        return Ok(ChangeGate::Proceed);
    };
    let approval = store
        .plan_meta(&plan.id)
        .map(|meta| meta.approval)
        .unwrap_or_default();
    if approval == PlanApproval::Proposed {
        if is_execution(action) {
            return Ok(ChangeGate::Refused(format!(
                "Plan '{}' ist nur ein Vorschlag und noch nicht bestätigt — Schritte starten oder \
                 abschließen geht erst danach. Lege ihn mit `plan submit` zur Bestätigung vor.",
                plan.id
            )));
        }
        return Ok(ChangeGate::Proceed);
    }
    if !is_significant(action) {
        return Ok(ChangeGate::Proceed);
    }
    let preview = preview_plan(&plan, action);
    let content = render_confirm_markdown(&preview, Some(label));
    match channel
        .ask(
            plan.id.as_str(),
            PlanConfirmKind::Change,
            label,
            content,
            ctx.cancel_token(),
        )
        .await
    {
        PlanConfirmOutcome::Confirmed => Ok(ChangeGate::Proceed),
        PlanConfirmOutcome::Rejected { feedback } => Ok(ChangeGate::Refused(format!(
            "Änderung an Plan '{}' NICHT übernommen ({label}). Rückmeldung der Nutzerin: {}",
            plan.id,
            feedback_text(&feedback)
        ))),
        PlanConfirmOutcome::NoAnswer => Ok(ChangeGate::Refused(format!(
            "Keine Entscheidung zur Änderung an Plan '{}' ({label}) — nicht übernommen.",
            plan.id
        ))),
        PlanConfirmOutcome::Cancelled => Err(OpError::Execution(
            "Turn abgebrochen, bevor die Änderung bestätigt wurde".to_owned(),
        )),
    }
}

/// Der Plan, wie er nach `action` aussähe (nur für die Anzeige).
///
/// Neue Knoten (`AddNode`, `Expand`) werden angehängt; alle anderen
/// Änderungen stehen als Zeile „Änderung“ über dem unveränderten Plan.
fn preview_plan(plan: &Plan, action: &PlanAction) -> Plan {
    let mut preview = plan.clone();
    match action {
        PlanAction::AddNode { node } => preview.nodes.push(node.clone()),
        PlanAction::Expand { children, .. } => preview.nodes.extend(children.iter().cloned()),
        _ => {}
    }
    preview
}

// ── Darstellung ─────────────────────────────────────────────────────────────

/// Rendert einen Plan als Markdown für das Freigabefenster.
///
/// # Beschreibung
/// Ziel, optional die Änderung, alle Schritte mit Art, Status,
/// Abhängigkeiten, Lese-/Schreibbereichen und Verifikation
/// (Akzeptanzkriterien), danach die topologischen Wellen.
#[must_use]
pub(crate) fn render_confirm_markdown(plan: &Plan, change: Option<&str>) -> String {
    let mut out = format!(
        "# Plan `{}`\n\n**Ziel:** {}\n",
        plan.id, plan.goal_statement
    );
    if let Some(change) = change {
        out.push_str(&format!("\n**Änderung:** {change}\n"));
    }
    let steps: Vec<&PlanNode> = plan
        .nodes
        .iter()
        .filter(|node| {
            !matches!(
                node.status,
                PlanNodeStatus::Superseded | PlanNodeStatus::Invalidated
            )
        })
        .collect();
    out.push_str(&format!("\n## Schritte ({})\n\n", steps.len()));
    if steps.is_empty() {
        out.push_str("_noch keine Schritte_\n");
    }
    for (index, node) in steps.iter().enumerate() {
        out.push_str(&format!(
            "{}. **{}** · {} · {} — {}\n",
            index.saturating_add(1),
            node.id,
            kind_label(node.kind),
            status_label(node.status),
            node.objective
        ));
        if !node.dependencies.is_empty() {
            let deps = node
                .dependencies
                .iter()
                .map(TaskId::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("   - nach: {deps}\n"));
        }
        if !node.read_scope.is_empty() {
            out.push_str(&format!("   - liest: {}\n", join_scope(&node.read_scope)));
        }
        if !node.write_scope.is_empty() {
            out.push_str(&format!(
                "   - schreibt: {}\n",
                join_scope(&node.write_scope)
            ));
        }
        let verification = node
            .acceptance_criteria
            .iter()
            .map(|criterion| criterion.description.as_str())
            .collect::<Vec<_>>();
        if verification.is_empty() {
            out.push_str("   - Verifikation: —\n");
        } else {
            out.push_str(&format!("   - Verifikation: {}\n", verification.join("; ")));
        }
    }
    match graph::topological_waves(plan) {
        Ok(waves) if !waves.is_empty() => {
            out.push_str("\n## Wellen\n\n");
            for (index, wave) in waves.iter().enumerate() {
                let ids = wave
                    .iter()
                    .map(TaskId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("- Welle {}: {ids}\n", index.saturating_add(1)));
            }
        }
        Ok(_) => {}
        Err(error) => out.push_str(&format!("\n⚠ Wellen nicht bestimmbar: {error}\n")),
    }
    out
}

// ── Schritt-Verfolgung ──────────────────────────────────────────────────────

/// Gewünschter Schritt-Status von `plan step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    /// Offen (bereit).
    Open,
    /// Läuft.
    Running,
    /// Erledigt — nur mit Beleg.
    Done,
    /// Blockiert.
    Blocked,
}

/// Parst den Schritt-Status (deutsch oder englisch).
///
/// # Errors
/// [`OpError::InvalidArguments`] mit der Werteliste.
pub(crate) fn parse_step_state(raw: &str) -> Result<StepState, OpError> {
    match raw.trim().to_lowercase().replace('-', "_").as_str() {
        "open" | "offen" | "ready" => Ok(StepState::Open),
        "running" | "läuft" | "laeuft" | "in_progress" | "start" | "started" => {
            Ok(StepState::Running)
        }
        "done" | "erledigt" | "completed" | "fertig" => Ok(StepState::Done),
        "blocked" | "blockiert" => Ok(StepState::Blocked),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter Schritt-Status '{other}'; erwartet eines von: open, running, done, \
             blocked (deutsch: offen, läuft, erledigt, blockiert)"
        ))),
    }
}

/// Liest einen Beleg: optional `evidence=`/`beleg=`-Präfix, optional
/// `<art>:`-Präfix (finding, cargo_test, clippy, diff, trace_span, manual,
/// job, other); sonst `manual`.
fn parse_evidence(raw: &str) -> Option<(EvidenceKind, String)> {
    let mut text = raw.trim();
    for prefix in ["evidence=", "beleg=", "evidenz="] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
        }
    }
    let text = text.trim_matches('"').trim();
    if text.is_empty() {
        return None;
    }
    if let Some((kind, locator)) = text.split_once(':')
        && !kind.contains(char::is_whitespace)
        && let Ok(kind) = parse_evidence_kind(kind)
        && !locator.trim().is_empty()
    {
        return Some((kind, locator.trim().to_owned()));
    }
    Some((EvidenceKind::Manual, text.to_owned()))
}

/// Die Statusfolge von `from` bis `InProgress` (leer, wenn schon dort).
///
/// # Errors
/// [`OpError::InvalidArguments`] für abgeschlossene Schritte.
fn path_to_running(id: &TaskId, from: PlanNodeStatus) -> Result<Vec<PlanNodeStatus>, OpError> {
    match from {
        PlanNodeStatus::Draft | PlanNodeStatus::Blocked => {
            Ok(vec![PlanNodeStatus::Ready, PlanNodeStatus::InProgress])
        }
        PlanNodeStatus::Ready => Ok(vec![PlanNodeStatus::InProgress]),
        PlanNodeStatus::InProgress => Ok(Vec::new()),
        terminal => Err(OpError::InvalidArguments(format!(
            "Schritt '{id}' ist bereits {} — nichts zu tun",
            step_label(terminal)
        ))),
    }
}

/// Baut eine `SetStatus`-Aktion.
fn set_status(id: &TaskId, status: PlanNodeStatus, reason: Option<String>) -> PlanAction {
    PlanAction::SetStatus {
        id: id.clone(),
        status,
        reason,
    }
}

/// `plan step <task-id> <open|running|done|blocked> [beleg…]` — Fortschritt
/// eines Schritts melden.
///
/// # Beschreibung
/// Übersetzt den Wunsch in die legalen Statusübergänge des Plans und wendet
/// sie **atomar** an (`apply_batch`): `running` geht bei Bedarf über `ready`,
/// `done` über `running`, hängt den Beleg an und schließt ab. Ohne Beleg
/// (weder im Aufruf noch am Knoten) bleibt der Schritt „läuft“ und die
/// Antwort sagt, was fehlt. `blocked` nimmt den Rest als Grund.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: Aufrufform, unbekannter Knoten/Status.
/// - [`OpError::Execution`]: der Store lehnt einen Übergang ab (etwa eine
///   unerledigte Abhängigkeit).
pub(crate) fn step(
    store: &dyn PlanStore,
    id: Option<String>,
    state: Option<String>,
    evidence: Option<String>,
    actor: &str,
) -> Result<String, OpError> {
    let usage = "plan step <task-id> <open|running|done|blocked> [beleg…]";
    let id = TaskId::new(require_arg(id, usage)?.as_str());
    let state = parse_step_state(&require_arg(state, usage)?)?;
    let plan = store.current().map_err(map_plan_error)?;
    let node = plan
        .nodes
        .iter()
        .find(|node| node.id == id)
        .ok_or_else(|| OpError::InvalidArguments(format!("Plan-Knoten '{id}' existiert nicht")))?;
    let evidence = evidence.as_deref().and_then(parse_evidence);

    let mut actions: Vec<PlanAction> = Vec::new();
    let mut missing_evidence = false;
    match state {
        StepState::Running => {
            for status in path_to_running(&id, node.status)? {
                actions.push(set_status(&id, status, None));
            }
        }
        StepState::Done => {
            for status in path_to_running(&id, node.status)? {
                actions.push(set_status(&id, status, None));
            }
            if let Some((kind, locator)) = &evidence {
                actions.push(PlanAction::AttachEvidence {
                    id: id.clone(),
                    evidence: EvidenceRef {
                        kind: *kind,
                        locator: locator.clone(),
                        // Runtime-eigenes Feld: der Store setzt seine Uhr.
                        attached_at: OffsetDateTime::UNIX_EPOCH,
                        actor: actor.to_owned(),
                        digest: None,
                    },
                });
            }
            if evidence.is_some() || !node.evidence.is_empty() {
                actions.push(set_status(&id, PlanNodeStatus::Completed, None));
            } else {
                missing_evidence = true;
            }
        }
        StepState::Blocked => match node.status {
            PlanNodeStatus::Blocked => {}
            PlanNodeStatus::Draft | PlanNodeStatus::Ready | PlanNodeStatus::InProgress => {
                let reason = evidence.as_ref().map(|(_, text)| text.clone());
                actions.push(set_status(&id, PlanNodeStatus::Blocked, reason));
            }
            terminal => {
                return Err(OpError::InvalidArguments(format!(
                    "Schritt '{id}' ist bereits {} und kann nicht blockiert werden",
                    step_label(terminal)
                )));
            }
        },
        StepState::Open => match node.status {
            PlanNodeStatus::Draft | PlanNodeStatus::Blocked => {
                actions.push(set_status(&id, PlanNodeStatus::Ready, None));
            }
            PlanNodeStatus::Ready => {}
            other => {
                return Err(OpError::InvalidArguments(format!(
                    "Schritt '{id}' ist {} und kann nicht wieder geöffnet werden (für \
                     invalidierte Schritte: `plan reopen <id> <grund>`)",
                    step_label(other)
                )));
            }
        },
    }

    let revision = if actions.is_empty() {
        plan.revision
    } else {
        store
            .apply_batch(&plan.id, actions, actor, plan.revision)
            .map_err(map_plan_error)?
            .revision
    };
    let updated = store.current().unwrap_or(plan);
    let status = updated
        .nodes
        .iter()
        .find(|node| node.id == id)
        .map_or(PlanNodeStatus::Draft, |node| node.status);
    let (done, total) = plan_progress(&updated);
    let mut text = format!(
        "Schritt '{id}' → {} (Revision {revision}). Fortschritt {}",
        step_label(status),
        progress_bar(done, total, BAR_WIDTH)
    );
    if missing_evidence {
        text.push_str(
            "\nOhne Beleg bleibt der Schritt „läuft“: melde `done` mit Beleg, z. B. \
             `plan step <id> done cargo_test:cargo test -p <crate>` oder \
             `plan step <id> done diff:src/datei.rs`.",
        );
    }
    if let Some(next) = current_step(&updated).filter(|next| next.id != id) {
        text.push_str(&format!(
            "\nNächster Schritt: {} ({}) {}",
            next.id,
            step_label(next.status),
            next.objective
        ));
    }
    Ok(text)
}

// ── Anzeige in `/goal show` ─────────────────────────────────────────────────

/// Fortschrittszeile des an `goal` gebundenen Plans für `/goal show`.
///
/// # Returns
/// `None` ohne Plan-Store oder ohne auffindbaren Plan.
pub(crate) fn goal_progress_line(ctx: &OpContext, goal: &Goal) -> Option<String> {
    let store = ctx.plan_store()?;
    let plan = match &goal.plan_id {
        Some(id) => store.plan_by_id(id).ok()?,
        None => store.current().ok()?,
    };
    let (done, total) = plan_progress(&plan);
    let current = current_step(&plan)
        .map(|node| {
            format!(
                " · aktueller Schritt: {} ({}) {}",
                node.id,
                step_label(node.status),
                node.objective
            )
        })
        .unwrap_or_default();
    let warning = if has_blocked_step(&plan) {
        " · ⚠ blockiert"
    } else {
        ""
    };
    Some(format!(
        "Fortschritt (Plan {}): {}{current}{warning}",
        plan.id,
        progress_bar(done, total, BAR_WIDTH)
    ))
}
