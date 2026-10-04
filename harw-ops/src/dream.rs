//! `/dream` — Traumläufe und Traumberichte des Wissensspeichers
//! (`docs/design/knowledge-surfaces.md` §4, `interaction-contract.md` §2.2,
//! Plan D5).
//!
//! # Subcommands
//! - (bare) / `list` — alle sichtbaren Traumberichte, neueste zuerst;
//!   `OpOutput::data` = `{"reports":[{"id","date","proposals"}]}`.
//!   `proposals` ist die Liste der Vorschläge des Berichts
//!   (`[{"section","target","summary"}]`); die TUI zeigt ihre Anzahl.
//! - `show <id>` — ein Bericht; `OpOutput::data` =
//!   `{"report":{"id","date","body","proposals"}}`.
//! - `run` — startet einen Traumlauf über den gemeinsamen Kern
//!   ([`crate::dream_run::run_dream`]) — denselben Weg wie der
//!   Gateway-Scheduler. Braucht einen `Arc<dyn DreamLauncher>` im Kontext
//!   (von der Laufzeit bereitgestellt); `data` =
//!   `{"run":{"work_id","report_id","structured","suggestions","in_ledger","maintenance"}}`.
//! - `status` — Scheduler-Zustand aus `dreams/state.json` und `[dream]`:
//!   aktiv?, Auslösung, letzter Lauf, läuft gerade?, nächster Lauf, Ledger-
//!   Zustand des letzten Jobs, offene Vorschläge; `data` = `{"status":{…}}`.
//! - `review [<id>]` — offene Vorschläge aller bzw. eines Berichts;
//!   `data` = `{"reviews":[{"id","date","pending":[…]}]}` bzw.
//!   `{"report":{"id","date","suggestions":[…]}}`.
//! - `review <id> accept|reject <p-id> [grund…]` — entscheidet einen
//!   Vorschlag. `accept` führt den Schreibpfad aus (siehe unten) und setzt
//!   dann den Status (`harw_knowledge::dream::decide_suggestion`, mit dem
//!   Ergebnis bzw. Grund als Notiz); `data` =
//!   `{"decision":{"report","suggestion","status","note"}}`.
//!
//! Liegt neben dem Bericht die strukturierte Seitendatei
//! (`harw_knowledge::dream::read_report_data`), tragen `list`/`show` je
//! Bericht zusätzlich `"suggestions":[{"id","kind","target","text","status"}]`
//! (`DreamSuggestion`); `proposals` bleibt für Altbestand aus dem Markdown. `<id>` darf die volle
//!   Artefakt-Id (`dream/<YYYY-MM-DD>/<work-id>`), `<YYYY-MM-DD>/<work-id>`
//!   oder die bloße Work-Id sein (muss dann eindeutig sein).
//!
//! # Schreibpfade beim Annehmen
//! | Art | Wirkung |
//! |---|---|
//! | `topic` | `harw_knowledge::memory::topic::propose_topic` → `topics/<slug>.md` **provisional** (Herkunft `dream`); `established` erst über `/palace promote` |
//! | `palace` | bestehendes Ziel: nur Hinweis auf `/palace promote <ziel>`; sonst wie `topic` |
//! | `diary_reflection` | `harw_knowledge::diary::record_dream_reflection` (Agent = Ziel, sonst `root`) |
//! | `skill_idea` / `agent_idea` | Lern-Vorschlag über den `/learn`-Weg (Skill: zusätzlich `SkillProposalStore`); nie live |
//! | `follow_up` / `maintenance` | nur Status (nichts wird gelöscht) |
//!
//! Nach jeder Entscheidung und nach `run` meldet die Op
//! `AgentEventKind::Knowledge` (`publish_knowledge`, Fläche `dream` mit der
//! Berichts-Id; bei Schreibpfaden zusätzlich `palace` bzw. `diary`).
//!
//! # Lesepfad und Sichtbarkeit
//! `harw-knowledge` hat keine eigene Listen-API für Traumberichte; gelesen
//! wird wie bei `/palace` über `KnowledgeIndex::rebuild` (der die
//! `dreams/`-Fläche als `ArtifactKind::DreamReport` mit Id
//! `dream/<datum>/<work-id>` einliest) und
//! `harw_knowledge::memory::recall::search_with` mit der Sicht des echten
//! Aufrufers ([`crate::knowledge_common::KnowledgeCaller`]) — dieselbe
//! fail-closed-Disziplin wie `/palace`. Die Seitendatei wird nur für einen
//! bereits als sichtbar bestätigten Bericht gelesen; `review` entscheidet
//! nur über sichtbare Berichte.
//!
//! # Fläche
//! Nur `Surface::Command` (`channel_reduced`), kein Modell-Werkzeug:
//! Traumberichte sind review-gated und für den Operator bestimmt.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store bzw. (für `run`) kein
//!   Traum-Starter im Kontext.
//! - [`OpError::InvalidArguments`] — Grammatik, unbekannte/mehrdeutige Id,
//!   bereits entschiedener Vorschlag, Konflikt beim Schreibpfad.
//! - [`OpError::Execution`] — Index-/Ein-/Ausgabefehler, gescheiterter oder
//!   bereits laufender Traum.

use std::sync::Arc;

use harw_config::ResolvedConfig;
use harw_knowledge::artifact::{ArtifactId, ArtifactKind, KnowledgeArtifact, RecallQuery};
use harw_knowledge::dream::{
    DreamReportData, DreamSuggestion, DreamSuggestionKind, DreamSuggestionStatus,
    decide_suggestion, read_report_data, read_scheduler_state, split_report_id,
};
use harw_knowledge::index::KnowledgeIndex;
use harw_knowledge::memory::recall::{ListAllRanker, search_with};
use harw_knowledge::memory::topic::{TopicOrigin, TopicProposal, propose_topic};
use harw_knowledge::{AgentId, KnowledgeStore, VisibilityScope};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_registry_defaults::skill_proposal_tools::SkillProposalStore;
use harw_session_store::JobStore;
use jiff::Timestamp;

use crate::dream_run::{
    DreamLauncher, DreamRunError, DreamSettings, DreamTrigger, SchedulerDecision,
    scheduler_decision,
};
use crate::knowledge_args::KnowledgeArgs;
use crate::knowledge_common::{
    AREA_DIARY, AREA_DREAM, AREA_PALACE, AREA_WORKBENCH, KnowledgeCaller, caller_agent,
    knowledge_store, map_knowledge_error, publish_knowledge,
};
use crate::learn::{LearnProposalStore, LearnTarget, file_external_proposal};

/// Diary-Agent einer angenommenen Reflexion ohne (gültiges) Ziel — derselbe
/// Vorgabe-Agent wie die Diary-Verdrahtung der Laufzeit.
pub const DEFAULT_REFLECTION_AGENT: &str = "root";

/// Id-Präfix, unter dem der Index Traumberichte führt.
const DREAM_ID_PREFIX: &str = "dream/";

/// Überschriften-Präfix der Vorschlagsabschnitte im Berichts-Body
/// (`harw_knowledge::dream::DreamReport::render_body`).
const PROPOSAL_HEADING_PREFIX: &str = "## Vorgeschlagene ";

/// Argument-Container für `/dream`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct DreamArgs {
    /// Alle Tokens nach `/dream`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl harw_operations::FromRawArgs for DreamArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Führt `/dream` aus.
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "dream",
    summary = "Traumläufe und -berichte: list, show <id>, run, status, review [<id>] [accept|reject <p-id> [grund]].",
    domain = "knowledge",
    permission = "operator",
    command(
        path = "/dream",
        visibility = "channel_reduced",
        channel_subcommands = "-,list,show,status,review",
        busy_subcommands = "-=immediate, list=immediate, show=immediate, status=immediate"
    )
)]
async fn dream(ctx: &OpContext, args: DreamArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    let caller = KnowledgeCaller::from_context(ctx);
    let parsed = KnowledgeArgs::parse(&args.tokens, &[])?;
    match parsed.subcommand() {
        Some("run") => run(ctx).await,
        Some("status") => {
            let settings = ctx
                .service::<Arc<ResolvedConfig>>()
                .map(|config| DreamSettings::from_config(config))
                .unwrap_or_default();
            let jobs = ctx.service::<Arc<JobStore>>().cloned();
            run_status(
                &store,
                &caller.viewer,
                &settings,
                jobs.as_deref(),
                Timestamp::now(),
            )
        }
        Some("review") => {
            let learn = || crate::learn::learn_store(ctx);
            let skills = || crate::learn::skill_store(ctx);
            let env = DreamReviewEnv {
                author: caller_agent(ctx),
                learn: &learn,
                skills: &skills,
                now: Timestamp::now(),
            };
            let reviewed = run_review_as(&store, &caller.viewer, parsed.rest(), &env)?;
            for (area, id) in &reviewed.touched {
                publish_knowledge(ctx, area, id.clone());
            }
            Ok(reviewed.output)
        }
        _ => run_dream_as(&store, &caller.viewer, &args.tokens),
    }
}

/// Der reine Kern von `/dream` aus Operator-Sicht (testbar ohne
/// `OpContext`; ohne `run`/`review`-Entscheidungen, die Kontextdienste
/// brauchen).
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_dream(store: &KnowledgeStore, tokens: &[String]) -> Result<OpOutput, OpError> {
    run_dream_as(store, &VisibilityScope::OperatorOnly, tokens)
}

/// Der reine Kern von `/dream list|show|status` mit Lesesicht `viewer`.
///
/// # Beschreibung
/// `status` nutzt hier die Vorgabe-Einstellungen und kein Ledger; `run`
/// und `review` brauchen den Op-Kontext und sind hier
/// [`OpError::NotAvailable`].
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_dream_as(
    store: &KnowledgeStore,
    viewer: &VisibilityScope,
    tokens: &[String],
) -> Result<OpOutput, OpError> {
    let args = KnowledgeArgs::parse(tokens, &[])?;
    let tail = args.rest();
    match args.subcommand() {
        None | Some("list") => list(store, viewer),
        Some("show") => show(store, viewer, tail),
        Some("status") => run_status(
            store,
            viewer,
            &DreamSettings::default(),
            None,
            Timestamp::now(),
        ),
        Some(sub @ ("run" | "review")) => Err(OpError::NotAvailable(format!(
            "/dream {sub} braucht den Op-Kontext einer Sitzung"
        ))),
        Some(other) => Err(OpError::InvalidArguments(format!(
            "unbekannter /dream-Subcommand: {other} (list, show, run, status, review)"
        ))),
    }
}

/// Alle für `viewer` sichtbaren Traumberichte, neueste zuerst.
///
/// # Rückgabe
/// `(berichte, gekürzt)`; `gekürzt` ist `true`, wenn die Recall-Obergrenze
/// gegriffen hat.
fn visible_reports(
    store: &KnowledgeStore,
    viewer: &VisibilityScope,
) -> Result<(Vec<KnowledgeArtifact>, bool), OpError> {
    let index = KnowledgeIndex::rebuild(store).map_err(|error| {
        OpError::Execution(format!("Knowledge-Index-Aufbau fehlgeschlagen: {error}"))
    })?;
    let mut query = RecallQuery::new(String::new(), viewer.clone());
    query.kinds = vec![ArtifactKind::DreamReport];
    query.max_hops = 0;
    let result = search_with(&index, &query, &ListAllRanker).map_err(map_knowledge_error)?;
    let mut reports: Vec<KnowledgeArtifact> = result
        .hits
        .iter()
        .filter_map(|hit| index.get(&hit.artifact.id).cloned())
        .collect();
    reports.sort_by(|left, right| {
        report_date(right)
            .cmp(&report_date(left))
            .then_with(|| right.id.as_str().cmp(left.id.as_str()))
    });
    Ok((reports, result.truncated))
}

/// Datum eines Berichts: das Datumssegment der Id, sonst `created_at` (UTC).
fn report_date(report: &KnowledgeArtifact) -> String {
    report
        .id
        .as_str()
        .strip_prefix(DREAM_ID_PREFIX)
        .and_then(|rest| rest.split_once('/'))
        .map(|(date, _)| date.to_owned())
        .unwrap_or_else(|| {
            report
                .frontmatter
                .created_at
                .strftime("%Y-%m-%d")
                .to_string()
        })
}

/// Die Vorschläge eines Berichts aus seinen `## Vorgeschlagene …`-Abschnitten.
///
/// # Rückgabe
/// `[{"section","target","summary"}]` in Body-Reihenfolge; `section` ist der
/// Rest der Überschrift (etwa `Topic-Updates`).
fn proposals(body: &str) -> Vec<serde_json::Value> {
    let mut section: Option<&str> = None;
    let mut found = Vec::new();
    for line in body.lines() {
        if line.starts_with("## ") {
            // Jede Abschnittsüberschrift beendet den vorigen Abschnitt; nur
            // Vorschlagsabschnitte öffnen einen neuen.
            section = line.strip_prefix(PROPOSAL_HEADING_PREFIX).map(str::trim);
            continue;
        }
        let Some(current) = section else {
            continue;
        };
        let Some(entry) = line.strip_prefix("- `") else {
            continue;
        };
        let (target, summary) = match entry.split_once("`:") {
            Some((target, summary)) => (target.trim(), summary.trim()),
            None => (entry.trim_end_matches('`').trim(), ""),
        };
        found.push(serde_json::json!({
            "section": current,
            "target": target,
            "summary": summary,
        }));
    }
    found
}

/// Kopf-Nutzlast eines Berichts (ohne Body); mit Seitendatei zusätzlich
/// `suggestions` (strukturierte Vorschläge samt Review-Status).
fn report_summary(store: &KnowledgeStore, report: &KnowledgeArtifact) -> serde_json::Value {
    let mut summary = serde_json::json!({
        "id": report.id.as_str(),
        "date": report_date(report),
        "proposals": proposals(&report.body),
    });
    let structured = split_report_id(&report.id)
        .and_then(|(date, work_id)| read_report_data(store, &date, &work_id).ok().flatten());
    if let (Some(data), Some(fields)) = (structured, summary.as_object_mut()) {
        fields.insert(
            "suggestions".to_owned(),
            serde_json::to_value(&data.suggestions).unwrap_or_default(),
        );
    }
    summary
}

fn list(store: &KnowledgeStore, viewer: &VisibilityScope) -> Result<OpOutput, OpError> {
    let (reports, truncated) = visible_reports(store, viewer)?;
    let summaries: Vec<serde_json::Value> = reports
        .iter()
        .map(|report| report_summary(store, report))
        .collect();
    let data = serde_json::json!({ "reports": summaries });
    if reports.is_empty() {
        return Ok(OpOutput {
            text: "Keine sichtbaren Traumberichte.".to_owned(),
            data: Some(data),
        });
    }
    let mut out = format!("{} Traumberichte:\n", reports.len());
    for report in &reports {
        out.push_str(&format!(
            "· {} — {} ({} Vorschläge)\n",
            report_date(report),
            report.id,
            proposals(&report.body).len()
        ));
    }
    if truncated {
        out.push_str("(gekürzt — ältere Berichte nicht aufgeführt)\n");
    }
    Ok(OpOutput {
        text: out,
        data: Some(data),
    })
}

/// Löst eine `show`-Referenz gegen die sichtbaren Berichte auf.
///
/// # Fehler
/// [`OpError::InvalidArguments`] bei unbekannter oder mehrdeutiger Id.
fn resolve(reports: Vec<KnowledgeArtifact>, raw: &str) -> Result<KnowledgeArtifact, OpError> {
    let raw = raw.trim();
    let exact = if raw.starts_with(DREAM_ID_PREFIX) {
        Some(raw.to_owned())
    } else if raw.contains('/') {
        Some(format!("{DREAM_ID_PREFIX}{raw}"))
    } else {
        None
    };
    let suffix = format!("/{raw}");
    let mut matches: Vec<KnowledgeArtifact> = reports
        .into_iter()
        .filter(|report| match &exact {
            Some(id) => report.id.as_str() == id,
            None => report.id.as_str().ends_with(&suffix),
        })
        .collect();
    match matches.len() {
        0 => Err(OpError::InvalidArguments(format!(
            "kein sichtbarer Traumbericht '{raw}'"
        ))),
        1 => matches.pop().ok_or_else(|| {
            OpError::InvalidArguments(format!("kein sichtbarer Traumbericht '{raw}'"))
        }),
        _ => {
            let ids: Vec<&str> = matches.iter().map(|report| report.id.as_str()).collect();
            Err(OpError::InvalidArguments(format!(
                "Traumbericht '{raw}' ist mehrdeutig: {}",
                ids.join(", ")
            )))
        }
    }
}

fn show(
    store: &KnowledgeStore,
    viewer: &VisibilityScope,
    tail: &[String],
) -> Result<OpOutput, OpError> {
    let raw = tail
        .first()
        .ok_or_else(|| OpError::InvalidArguments("Aufruf: /dream show <id>".to_owned()))?;
    let (reports, _) = visible_reports(store, viewer)?;
    let report = resolve(reports, raw)?;
    let date = report_date(&report);
    let body = report.body.trim();
    let mut data = report_summary(store, &report);
    if let Some(fields) = data.as_object_mut() {
        fields.insert("body".to_owned(), serde_json::json!(body));
    }
    Ok(OpOutput {
        text: format!("Traumbericht {date} — {}\n\n{body}\n", report.id),
        data: Some(serde_json::json!({ "report": data })),
    })
}

// ---------------------------------------------------------------------------
// /dream run
// ---------------------------------------------------------------------------

/// `/dream run`: ein Lauf über den [`DreamLauncher`] des Kontexts.
async fn run(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let launcher = ctx
        .service::<Arc<dyn DreamLauncher>>()
        .cloned()
        .ok_or_else(|| {
            OpError::NotAvailable(
                "kein Traum-Starter in diesem Kontext — /dream run braucht eine Sitzung mit Modell"
                    .to_owned(),
            )
        })?;
    let outcome = launcher
        .launch(DreamTrigger::Manual)
        .await
        .map_err(|error| match error {
            DreamRunError::Busy => OpError::Execution(
                "ein Traumlauf läuft bereits — später erneut versuchen (/dream status)".to_owned(),
            ),
            DreamRunError::Failed(message) => {
                OpError::Execution(format!("Traumlauf fehlgeschlagen: {message}"))
            }
        })?;
    publish_knowledge(ctx, AREA_DREAM, Some(outcome.report_id.clone()));
    if outcome.maintenance.diary_rolled_up_days > 0 {
        publish_knowledge(ctx, AREA_DIARY, None);
    }
    if outcome.maintenance.workbench_removed > 0 {
        publish_knowledge(ctx, AREA_WORKBENCH, None);
    }
    Ok(run_output(&outcome))
}

/// Ausgabe eines abgeschlossenen Laufs.
fn run_output(outcome: &crate::dream_run::DreamRunOutcome) -> OpOutput {
    let pending = outcome.data.suggestions.len();
    let mode = if outcome.structured {
        "strukturiert"
    } else {
        "nur Fließtext"
    };
    let maintenance = &outcome.maintenance;
    let text = format!(
        "Traumlauf {work} abgeschlossen → {report} ({mode}).\n\
         {pending} Vorschlag/Vorschläge zur Prüfung · Wissenspflege: {diary} Diary-Tag(e) ins \
         Rollup, {bench} Workbench-Scope(s) entfernt, {stale} Veraltungskandidat(en).\n\
         Prüfen: /dream review {work}\n",
        work = outcome.work_id,
        report = outcome.report_id,
        diary = maintenance.diary_rolled_up_days,
        bench = maintenance.workbench_removed,
        stale = maintenance.stale_candidates,
    );
    OpOutput {
        text,
        data: Some(serde_json::json!({
            "run": {
                "work_id": outcome.work_id,
                "report_id": outcome.report_id,
                "structured": outcome.structured,
                "suggestions": pending,
                "in_ledger": outcome.in_ledger,
                "maintenance": {
                    "diary_rolled_up_days": maintenance.diary_rolled_up_days,
                    "workbench_removed": maintenance.workbench_removed,
                    "stale_candidates": maintenance.stale_candidates,
                    "errors": maintenance.errors,
                },
            }
        })),
    }
}

// ---------------------------------------------------------------------------
// /dream status
// ---------------------------------------------------------------------------

/// `/dream status`: Scheduler-Zustand, letzter/nächster Lauf, Ledger.
///
/// # Fehler
/// [`OpError::Execution`] bei unlesbarem Zustand oder Index.
pub fn run_status(
    store: &KnowledgeStore,
    viewer: &VisibilityScope,
    settings: &DreamSettings,
    jobs: Option<&JobStore>,
    now: Timestamp,
) -> Result<OpOutput, OpError> {
    let state = read_scheduler_state(store).map_err(|error| {
        OpError::Execution(format!(
            "Traum-Zustand (dreams/state.json) nicht lesbar: {error}"
        ))
    })?;
    let running = state.running_marker(now, settings.running_stale_after());
    let decision = scheduler_decision(settings, &state, None, now);
    let (reports, _) = visible_reports(store, viewer)?;
    let pending: usize = reports
        .iter()
        .filter_map(|report| sidecar(store, report))
        .map(|data| pending_of(&data).len())
        .sum();
    let ledger_state = state.last_work_id.as_deref().and_then(|work_id| {
        jobs.and_then(|jobs| {
            jobs.get(&harw_job_runtime::WorkId::from_str(work_id))
                .ok()
                .map(|record| format!("{:?}", record.job.state).to_lowercase())
        })
    });

    let mode = match &settings.schedule {
        Some(expression) => format!("Zeitplan „{expression}“ (UTC)"),
        None => format!("Leerlauf ≥ {} min im Gateway", settings.idle.as_secs() / 60),
    };
    let mut text = format!(
        "Traum-Scheduler: {} · Auslösung: {mode} · Cooldown: {} min · Budget: {} Token
",
        if settings.enabled {
            "aktiv"
        } else {
            "deaktiviert ([dream] enabled = false)"
        },
        settings.cooldown.as_secs() / 60,
        settings.budget_tokens,
    );
    match running {
        Some(marker) => text.push_str(&format!(
            "Läuft gerade: ja — {} seit {} ({})
",
            marker.work_id,
            marker.started_at.strftime("%Y-%m-%d %H:%M:%S UTC"),
            marker.trigger
        )),
        None => text.push_str(
            "Läuft gerade: nein
",
        ),
    }
    match (&state.last_run_at, &state.last_work_id) {
        (Some(at), Some(work_id)) => {
            let status = match (state.last_status, &state.last_error) {
                (Some(harw_knowledge::DreamRunStatus::Failed), Some(error)) => {
                    format!("fehlgeschlagen: {error}")
                }
                (Some(status), _) => status.label().to_owned(),
                (None, _) => "unbekannt".to_owned(),
            };
            text.push_str(&format!(
                "Letzter Lauf: {} — {work_id} ({}) — {status}{}
",
                at.strftime("%Y-%m-%d %H:%M:%S UTC"),
                state.last_trigger.as_deref().unwrap_or("?"),
                state
                    .last_report_id
                    .as_deref()
                    .map(|id| format!(" → {id}"))
                    .unwrap_or_default(),
            ));
            if let Some(ledger) = &ledger_state {
                text.push_str(&format!(
                    "Ledger: Job {work_id} ist {ledger}
"
                ));
            }
        }
        _ => text.push_str(
            "Letzter Lauf: noch keiner
",
        ),
    }
    let (next_kind, next_at, next_text) = describe_next(&decision, settings);
    text.push_str(&format!(
        "Nächster Lauf: {next_text}
"
    ));
    text.push_str(&format!(
        "Offene Vorschläge: {pending} (/dream review)\n\
         Hinweis: Der Scheduler läuft im Gateway (`harw gateway`); /dream run startet sofort.\n"
    ));

    let data = serde_json::json!({
        "status": {
            "enabled": settings.enabled,
            "schedule": settings.schedule,
            "idle_minutes": settings.idle.as_secs() / 60,
            "cooldown_minutes": settings.cooldown.as_secs() / 60,
            "budget": settings.budget_tokens,
            "running": running.map(|marker| serde_json::json!({
                "work_id": marker.work_id,
                "started_at": marker.started_at.to_string(),
                "trigger": marker.trigger,
            })),
            "last_run": state.last_run_at.map(|at| serde_json::json!({
                "at": at.to_string(),
                "work_id": state.last_work_id,
                "status": state.last_status.map(|status| status.label()),
                "trigger": state.last_trigger,
                "error": state.last_error,
                "report_id": state.last_report_id,
                "ledger_state": ledger_state,
            })),
            "next": { "kind": next_kind, "at": next_at },
            "runs": state.runs,
            "pending_suggestions": pending,
        }
    });
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

/// `(art, zeitpunkt, text)` des nächsten Laufs.
fn describe_next(
    decision: &SchedulerDecision,
    settings: &DreamSettings,
) -> (&'static str, Option<String>, String) {
    match decision {
        SchedulerDecision::Disabled => (
            "disabled",
            None,
            "kein automatischer Lauf (deaktiviert)".to_owned(),
        ),
        SchedulerDecision::Running { work_id } => {
            ("running", None, format!("läuft gerade ({work_id})"))
        }
        SchedulerDecision::CoolingDown { until } => (
            "cooldown",
            Some(until.to_string()),
            format!(
                "frühestens {} (Cooldown)",
                until.strftime("%Y-%m-%d %H:%M UTC")
            ),
        ),
        SchedulerDecision::AwaitingIdle { .. } => (
            "idle",
            None,
            format!(
                "sobald das Gateway {} min Leerlauf sieht",
                settings.idle.as_secs() / 60
            ),
        ),
        SchedulerDecision::NotDue { next } => (
            "schedule",
            Some(next.to_string()),
            next.strftime("%Y-%m-%d %H:%M UTC").to_string(),
        ),
        SchedulerDecision::Never => (
            "never",
            None,
            "nie (der Zeitplan feuert nicht mehr)".to_owned(),
        ),
        SchedulerDecision::InvalidSchedule(error) => ("invalid", None, error.clone()),
        SchedulerDecision::Due(_) => (
            "due",
            None,
            "fällig (beim nächsten Scheduler-Takt im Gateway)".to_owned(),
        ),
    }
}

// ---------------------------------------------------------------------------
// /dream review
// ---------------------------------------------------------------------------

/// Dienste der Review-Entscheidung (injizierbar für Tests).
pub struct DreamReviewEnv<'a> {
    /// Autor der Schreibpfade (Topic-Frontmatter).
    pub author: AgentId,
    /// Lern-Vorschlagsablage (für `skill_idea`/`agent_idea`).
    pub learn: &'a dyn Fn() -> Result<Arc<LearnProposalStore>, OpError>,
    /// Skill-Vorschlagsablage (für `skill_idea`).
    pub skills: &'a dyn Fn() -> Result<Arc<SkillProposalStore>, OpError>,
    /// Entscheidungszeitpunkt.
    pub now: Timestamp,
}

/// Berührte Wissensflächen `(fläche, id)` für `publish_knowledge`.
pub type Touched = Vec<(&'static str, Option<String>)>;

/// Ergebnis von [`run_review_as`]: Ausgabe plus berührte Flächen für
/// `publish_knowledge`.
#[derive(Debug)]
pub struct ReviewOutcome {
    /// Die Op-Ausgabe.
    pub output: OpOutput,
    /// `(fläche, id)` je Schreibvorgang; leer bei reinen Lesezugriffen.
    pub touched: Touched,
}

/// `/dream review …` (Tokens nach `review`).
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_review_as(
    store: &KnowledgeStore,
    viewer: &VisibilityScope,
    tail: &[String],
    env: &DreamReviewEnv<'_>,
) -> Result<ReviewOutcome, OpError> {
    const USAGE: &str = "/dream review [<id>] | /dream review <id> accept|reject <p-id> [grund]";
    let read_only = |output| ReviewOutcome {
        output,
        touched: Vec::new(),
    };
    match tail {
        [] => review_all(store, viewer).map(read_only),
        [id] => review_one(store, viewer, id).map(read_only),
        [id, verb, suggestion_id, reason @ ..] if verb == "accept" || verb == "reject" => {
            let (reports, _) = visible_reports(store, viewer)?;
            let report = resolve(reports, id)?;
            let (date, work_id) = split_report_id(&report.id).ok_or_else(|| {
                OpError::InvalidArguments(format!("'{}' ist keine Traumbericht-Id", report.id))
            })?;
            let data = read_report_data(store, &date, &work_id)
                .map_err(map_knowledge_error)?
                .ok_or_else(|| {
                    OpError::InvalidArguments(format!(
                        "Traumbericht {} hat keine strukturierten Vorschläge",
                        report.id
                    ))
                })?;
            let suggestion = data
                .suggestions
                .iter()
                .find(|suggestion| suggestion.id == *suggestion_id)
                .cloned()
                .ok_or_else(|| {
                    OpError::InvalidArguments(format!(
                        "kein Vorschlag '{suggestion_id}' in {}",
                        report.id
                    ))
                })?;
            if suggestion.status != DreamSuggestionStatus::Pending {
                return Err(OpError::InvalidArguments(format!(
                    "Vorschlag {suggestion_id} ist bereits {}",
                    suggestion.status.label()
                )));
            }
            let reason = reason.join(" ");
            let (status, note, mut touched) = if verb == "accept" {
                let (note, touched) = accept(store, env, &work_id, &date, &suggestion)?;
                (DreamSuggestionStatus::Accepted, note, touched)
            } else {
                let note = if reason.trim().is_empty() {
                    "abgelehnt".to_owned()
                } else {
                    reason.trim().to_owned()
                };
                (DreamSuggestionStatus::Rejected, note, Vec::new())
            };
            let decided =
                decide_suggestion(store, &date, &work_id, &suggestion.id, status, Some(&note))
                    .map_err(map_knowledge_error)?;
            touched.push((AREA_DREAM, Some(report.id.as_str().to_owned())));
            let text = format!(
                "{} {} in {}: {}
",
                decided.id,
                match status {
                    DreamSuggestionStatus::Accepted => "angenommen",
                    _ => "abgelehnt",
                },
                report.id,
                note
            );
            Ok(ReviewOutcome {
                output: OpOutput {
                    text,
                    data: Some(serde_json::json!({
                        "decision": {
                            "report": report.id.as_str(),
                            "suggestion": decided.id,
                            "kind": decided.kind.label(),
                            "status": decided.status.label(),
                            "note": decided.decision_note,
                        }
                    })),
                },
                touched,
            })
        }
        _ => Err(OpError::InvalidArguments(format!("Aufruf: {USAGE}"))),
    }
}

/// Führt den Schreibpfad eines angenommenen Vorschlags aus.
///
/// # Rückgabe
/// `(notiz, berührte flächen)`.
fn accept(
    store: &KnowledgeStore,
    env: &DreamReviewEnv<'_>,
    work_id: &str,
    date: &str,
    suggestion: &DreamSuggestion,
) -> Result<(String, Touched), OpError> {
    let source = format!("dream:{work_id}/{}", suggestion.id);
    match suggestion.kind {
        DreamSuggestionKind::Topic => {
            propose_from_suggestion(store, env, work_id, date, suggestion)
        }
        DreamSuggestionKind::Palace => {
            let existing = suggestion.target.as_deref().and_then(|target| {
                KnowledgeIndex::rebuild(store).ok().and_then(|index| {
                    index
                        .get(&ArtifactId::new(target))
                        .map(|_| target.to_owned())
                })
            });
            match existing {
                Some(target) => Ok((
                    format!(
                        "bestehender Eintrag {target} — nichts geschrieben; übernehmen mit /palace promote {target}"
                    ),
                    Vec::new(),
                )),
                None => propose_from_suggestion(store, env, work_id, date, suggestion),
            }
        }
        DreamSuggestionKind::DiaryReflection => {
            let agent = suggestion
                .target
                .as_deref()
                .map(AgentId::new)
                .filter(|agent| harw_knowledge::diary::validate_agent_id(agent).is_ok())
                .unwrap_or_else(|| AgentId::new(DEFAULT_REFLECTION_AGENT));
            harw_knowledge::diary::record_dream_reflection(
                store,
                &agent,
                &suggestion.text,
                env.now,
            )
            .map_err(map_knowledge_error)?;
            Ok((
                format!("Diary-Eintrag (dream-reflection) für {}", agent.as_str()),
                vec![(AREA_DIARY, Some(agent.as_str().to_owned()))],
            ))
        }
        DreamSuggestionKind::SkillIdea | DreamSuggestionKind::AgentIdea => {
            let target = if suggestion.kind == DreamSuggestionKind::SkillIdea {
                LearnTarget::Skill
            } else {
                LearnTarget::Agent
            };
            let learn = (env.learn)()?;
            let (proposal, created) =
                file_external_proposal(&learn, env.skills, target, &suggestion.text, &source)?;
            let next = match &proposal.skill_proposal_id {
                Some(skill_id) => format!("prüfen: /skills review {skill_id}"),
                None => format!("prüfen: /learn show {}", proposal.id),
            };
            Ok((
                format!(
                    "Lern-Vorschlag {} {} (nichts übernommen; {next})",
                    proposal.id,
                    if created {
                        "angelegt"
                    } else {
                        "bestand bereits"
                    }
                ),
                Vec::new(),
            ))
        }
        DreamSuggestionKind::FollowUp | DreamSuggestionKind::Maintenance => Ok((
            "vermerkt (kein Schreibpfad; nichts wurde geändert)".to_owned(),
            Vec::new(),
        )),
    }
}

/// Legt einen Topic-/Palace-Vorschlag als `provisional` Thema an.
fn propose_from_suggestion(
    store: &KnowledgeStore,
    env: &DreamReviewEnv<'_>,
    work_id: &str,
    date: &str,
    suggestion: &DreamSuggestion,
) -> Result<(String, Touched), OpError> {
    let slug = suggestion.target.as_deref().map(|target| {
        target
            .strip_prefix("topic/")
            .or_else(|| target.strip_prefix("palace/"))
            .unwrap_or(target)
            .to_owned()
    });
    let title: String = suggestion
        .text
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect();
    let proposal = TopicProposal {
        slug,
        title,
        body: suggestion.text.clone(),
        tags: vec!["dream".to_owned()],
        origin: TopicOrigin {
            kind: "dream".to_owned(),
            id: format!("{work_id}/{}", suggestion.id),
            detail: Some(date.to_owned()),
        },
    };
    let slug = propose_topic(
        store,
        &proposal,
        &env.author,
        VisibilityScope::OperatorOnly,
        env.now,
    )
    .map_err(|error| match &error {
        harw_knowledge::KnowledgeError::Io(io)
            if io.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            OpError::InvalidArguments(format!("{error} — Vorschlag bleibt offen"))
        }
        _ => map_knowledge_error(error),
    })?;
    Ok((
        format!(
            "topic/{slug} als provisional angelegt — übernehmen mit /palace promote topic/{slug}"
        ),
        vec![(AREA_PALACE, Some(format!("topic/{slug}")))],
    ))
}

/// Die offenen Vorschläge einer Seitendatei.
fn pending_of(data: &DreamReportData) -> Vec<&DreamSuggestion> {
    data.suggestions
        .iter()
        .filter(|suggestion| suggestion.status == DreamSuggestionStatus::Pending)
        .collect()
}

/// Die Seitendatei eines (sichtbaren) Berichts, falls vorhanden.
fn sidecar(store: &KnowledgeStore, report: &KnowledgeArtifact) -> Option<DreamReportData> {
    split_report_id(&report.id)
        .and_then(|(date, work_id)| read_report_data(store, &date, &work_id).ok().flatten())
}

/// Eine Vorschlagszeile.
fn suggestion_line(suggestion: &DreamSuggestion) -> String {
    let target = suggestion
        .target
        .as_deref()
        .map(|target| format!(" → {target}"))
        .unwrap_or_default();
    let note = suggestion
        .decision_note
        .as_deref()
        .map(|note| format!(" — {note}"))
        .unwrap_or_default();
    format!(
        "  {} [{}{target}] ({}) {}{note}\n",
        suggestion.id,
        suggestion.kind.label(),
        suggestion.status.label(),
        suggestion.text.replace('\n', " ")
    )
}

fn review_all(store: &KnowledgeStore, viewer: &VisibilityScope) -> Result<OpOutput, OpError> {
    let (reports, _) = visible_reports(store, viewer)?;
    let mut text = String::new();
    let mut reviews = Vec::new();
    for report in &reports {
        let Some(data) = sidecar(store, report) else {
            continue;
        };
        let pending = pending_of(&data);
        if pending.is_empty() {
            continue;
        }
        text.push_str(&format!("{} — {} offen:\n", report.id, pending.len()));
        for suggestion in &pending {
            text.push_str(&suggestion_line(suggestion));
        }
        reviews.push(serde_json::json!({
            "id": report.id.as_str(),
            "date": report_date(report),
            "pending": pending,
        }));
    }
    if reviews.is_empty() {
        text = "Keine offenen Traumvorschläge.\n".to_owned();
    } else {
        text.push_str("Entscheiden: /dream review <id> accept|reject <p-id> [grund]\n");
    }
    Ok(OpOutput {
        text,
        data: Some(serde_json::json!({ "reviews": reviews })),
    })
}

fn review_one(
    store: &KnowledgeStore,
    viewer: &VisibilityScope,
    raw: &str,
) -> Result<OpOutput, OpError> {
    let (reports, _) = visible_reports(store, viewer)?;
    let report = resolve(reports, raw)?;
    let data = sidecar(store, &report).ok_or_else(|| {
        OpError::InvalidArguments(format!(
            "Traumbericht {} hat keine strukturierten Vorschläge",
            report.id
        ))
    })?;
    let mut text = format!(
        "{} — {} Vorschläge, {} offen:\n",
        report.id,
        data.suggestions.len(),
        pending_of(&data).len()
    );
    for suggestion in &data.suggestions {
        text.push_str(&suggestion_line(suggestion));
    }
    Ok(OpOutput {
        text,
        data: Some(serde_json::json!({
            "report": {
                "id": report.id.as_str(),
                "date": report_date(&report),
                "suggestions": data.suggestions,
            }
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::{DreamArgs, proposals, run_dream};
    use crate::knowledge_test_support::temporary_store;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_job_runtime::WorkId;
    use harw_knowledge::KnowledgeStore;
    use harw_knowledge::artifact::ArtifactId;
    use harw_knowledge::dream::{DreamProposal, DreamReport};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpError, Operation};

    fn report(work_id: &str, second: i64, topics: usize) -> TestResult<DreamReport> {
        let created_at = jiff::Timestamp::from_second(second).map_err(ctx("valid timestamp"))?;
        Ok(DreamReport {
            work_id: WorkId::from_str(work_id),
            created_at,
            summary: "Zusammenfassung".to_owned(),
            proposed_topic_updates: (0..topics)
                .map(|index| DreamProposal {
                    artifact_id: ArtifactId::new(format!("topic/t{index}")),
                    summary: format!("Thema {index} ergänzen"),
                })
                .collect(),
            proposed_palace_promotions: vec![DreamProposal {
                artifact_id: ArtifactId::new("palace/p"),
                summary: "Knoten anlegen".to_owned(),
            }],
            follow_ups: vec!["- `kein/vorschlag`: nur ein Faden".to_owned()],
        })
    }

    fn run(store: &KnowledgeStore, tokens: &[&str]) -> Result<harw_operations::OpOutput, OpError> {
        run_dream(store, &toks(tokens))
    }

    #[test]
    fn dream_is_a_command_without_a_model_tool_surface() {
        let meta = super::DreamOperation.meta();
        assert_eq!(meta.name, "dream");
        assert_eq!(meta.permission, harw_operations::PermissionTier::Operator);
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Command {
                path: "/dream",
                visibility: CommandVisibility::ChannelReduced,
            }
        )));
    }

    #[test]
    fn from_raw_args_keeps_every_token() -> TestResult {
        let args = DreamArgs::from_raw_args(&toks(&["show", "x"])).map_err(ctx("parse"))?;
        assert_eq!(args.tokens, toks(&["show", "x"]));
        Ok(())
    }

    #[test]
    fn an_empty_store_lists_no_reports() -> TestResult {
        let store = temporary_store("empty")?;
        let output = run(&store, &[]).map_err(ctx("bare succeeds"))?;
        assert_eq!(output.text, "Keine sichtbaren Traumberichte.");
        let data = output.data.ok_or(TestError::Missing("list data"))?;
        assert_eq!(data["reports"], serde_json::json!([]));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn list_carries_newest_first_with_proposals() -> TestResult {
        let store = temporary_store("list")?;
        store
            .write_dream_report(&report("dream-old", 0, 1)?)
            .map_err(ctx("write old"))?;
        store
            .write_dream_report(&report("dream-new", 1_758_715_200, 2)?)
            .map_err(ctx("write new"))?;

        let output = run(&store, &["list"]).map_err(ctx("list succeeds"))?;
        let data = output.data.ok_or(TestError::Missing("list data"))?;
        assert_eq!(data["reports"][0]["id"], "dream/2025-09-24/dream-new");
        assert_eq!(data["reports"][0]["date"], "2025-09-24");
        let newest = data["reports"][0]["proposals"]
            .as_array()
            .ok_or(TestError::Missing("proposals array"))?;
        assert_eq!(newest.len(), 3, "{data}");
        assert_eq!(newest[0]["target"], "topic/t0");
        assert_eq!(newest[0]["summary"], "Thema 0 ergänzen");
        assert_eq!(newest[2]["section"], "Palace-Promotionen");
        assert_eq!(data["reports"][1]["id"], "dream/1970-01-01/dream-old");
        let suggestions = data["reports"][0]["suggestions"]
            .as_array()
            .ok_or(TestError::Missing("structured suggestions"))?;
        // Zwei Topic-Updates, eine Palace-Promotion, ein offener Faden.
        assert_eq!(suggestions.len(), 4, "{data}");
        assert_eq!(suggestions[0]["kind"], "topic");
        assert_eq!(suggestions[0]["status"], "pending");
        assert_eq!(suggestions[2]["kind"], "palace");
        assert_eq!(suggestions[3]["kind"], "follow_up");
        assert!(
            output.text.starts_with("2 Traumberichte:"),
            "{}",
            output.text
        );
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn show_resolves_full_dated_and_bare_ids() -> TestResult {
        let store = temporary_store("show")?;
        store
            .write_dream_report(&report("dream-a", 0, 1)?)
            .map_err(ctx("write"))?;
        for reference in ["dream/1970-01-01/dream-a", "1970-01-01/dream-a", "dream-a"] {
            let output = run(&store, &["show", reference]).map_err(ctx("show succeeds"))?;
            let data = output.data.ok_or(TestError::Missing("show data"))?;
            assert_eq!(data["report"]["id"], "dream/1970-01-01/dream-a");
            assert_eq!(data["report"]["date"], "1970-01-01");
            let body = data["report"]["body"]
                .as_str()
                .ok_or(TestError::Missing("body"))?;
            assert!(body.contains("# Traumbericht dream-a"), "{body}");
            assert!(output.text.contains("Zusammenfassung"), "{}", output.text);
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn a_bare_id_shared_by_two_days_is_ambiguous() -> TestResult {
        let store = temporary_store("ambiguous")?;
        store
            .write_dream_report(&report("dream-x", 0, 0)?)
            .map_err(ctx("write day 1"))?;
        store
            .write_dream_report(&report("dream-x", 86_400, 0)?)
            .map_err(ctx("write day 2"))?;
        match run(&store, &["show", "dream-x"]) {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("mehrdeutig"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "mehrdeutige Id muss InvalidArguments sein, war {other:?}"
                )));
            }
        }
        run(&store, &["show", "1970-01-02/dream-x"]).map_err(ctx("dated id is unique"))?;
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn grammar_errors_are_invalid_arguments() -> TestResult {
        let store = temporary_store("grammar")?;
        for tokens in [
            vec!["bogus"],
            vec!["show"],
            vec!["show", "unbekannt"],
            vec!["show", "../../etc/passwd"],
        ] {
            match run(&store, &tokens) {
                Err(OpError::InvalidArguments(_)) => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?} must be InvalidArguments, got {other:?}"
                    )));
                }
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn run_and_review_need_the_op_context_in_the_pure_core() -> TestResult {
        let store = temporary_store("pure-run")?;
        for tokens in [vec!["run"], vec!["review"]] {
            match run(&store, &tokens) {
                Err(OpError::NotAvailable(_)) => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?} must be NotAvailable, got {other:?}"
                    )));
                }
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    // ── run / status / review ──────────────────────────────────────────────

    use super::{DreamReviewEnv, ReviewOutcome, run_review_as, run_status};
    use crate::dream_run::{
        DreamFuture, DreamLauncher, DreamReasoner, DreamRunError, DreamRunOutcome, DreamRunRequest,
        DreamSettings, DreamTrigger, run_dream as run_core,
    };
    use crate::learn::LearnProposalStore;
    use harw_knowledge::dream::{DreamSuggestionStatus, read_report_data};
    use harw_knowledge::{AgentId, VisibilityScope};
    use harw_registry_defaults::skill_proposal_tools::SkillProposalStore;
    use std::sync::Arc;

    const MODEL_REPLY: &str = r#"{"summary":"Reflexion.","suggestions":[
        {"kind":"topic","text":"Traumläufe sind review-gated","target":"topic/traeume"},
        {"kind":"palace","text":"Knoten ergänzen","target":"palace/neu"},
        {"kind":"diary_reflection","text":"Ruhiger Tag.","target":"worker-1"},
        {"kind":"skill_idea","text":"Jedes Mal wenn ein Deploy ansteht, Checkliste abarbeiten"},
        {"kind":"agent_idea","text":"Ein Agent, der Releases vorbereitet"},
        {"kind":"follow_up","text":"Deploy prüfen"}]}"#;

    struct Fixed(&'static str);

    impl DreamReasoner for Fixed {
        fn reflect<'a>(
            &'a self,
            _label: &'a str,
            _prompt: &'a str,
        ) -> DreamFuture<'a, Result<String, String>> {
            let reply = self.0.to_owned();
            Box::pin(async move { Ok(reply) })
        }
    }

    /// Launcher über den echten Kern mit festem Modell.
    struct TestLauncher {
        store: KnowledgeStore,
        now: jiff::Timestamp,
    }

    impl DreamLauncher for TestLauncher {
        fn launch(
            &self,
            trigger: DreamTrigger,
        ) -> DreamFuture<'_, Result<DreamRunOutcome, DreamRunError>> {
            Box::pin(async move {
                let settings = DreamSettings::default();
                run_core(
                    &Fixed(MODEL_REPLY),
                    DreamRunRequest {
                        knowledge: &self.store,
                        jobs: None,
                        transcript_context: "",
                        settings: &settings,
                        trigger,
                        now: self.now,
                    },
                )
                .await
            })
        }
    }

    fn at(second: i64) -> TestResult<jiff::Timestamp> {
        jiff::Timestamp::from_second(second).map_err(ctx("valid timestamp"))
    }

    async fn dreamed(store: &KnowledgeStore) -> TestResult<DreamRunOutcome> {
        let launcher = TestLauncher {
            store: store.clone(),
            now: at(1_758_715_200)?,
        };
        launcher
            .launch(DreamTrigger::Manual)
            .await
            .map_err(|error| TestError::Unexpected(error.to_string()))
    }

    struct Env {
        _temp: tempfile::TempDir,
        learn: Arc<LearnProposalStore>,
        skills: Arc<SkillProposalStore>,
    }

    impl Env {
        fn new() -> TestResult<Self> {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            Ok(Self {
                learn: Arc::new(LearnProposalStore::new(temp.path().join("learn/proposals"))),
                skills: Arc::new(SkillProposalStore::new(temp.path().join("skills"))),
                _temp: temp,
            })
        }

        fn review(
            &self,
            store: &KnowledgeStore,
            tokens: &[&str],
        ) -> Result<ReviewOutcome, OpError> {
            let learn = || Ok(Arc::clone(&self.learn));
            let skills = || Ok(Arc::clone(&self.skills));
            let env = DreamReviewEnv {
                author: AgentId::new("operator"),
                learn: &learn,
                skills: &skills,
                now: jiff::Timestamp::from_second(1_758_720_000)
                    .map_err(|error| OpError::Execution(error.to_string()))?,
            };
            run_review_as(store, &VisibilityScope::OperatorOnly, &toks(tokens), &env)
        }
    }

    #[tokio::test]
    async fn review_lists_pending_and_accept_runs_each_write_path() -> TestResult {
        let store = temporary_store("review")?;
        let outcome = dreamed(&store).await?;
        let work = outcome.work_id.clone();
        let env = Env::new()?;

        let listed = env.review(&store, &[]).map_err(ctx("review all"))?;
        let data = listed.output.data.ok_or(TestError::Missing("reviews"))?;
        assert_eq!(data["reviews"][0]["id"], outcome.report_id.as_str());
        assert_eq!(
            data["reviews"][0]["pending"].as_array().map(Vec::len),
            Some(6)
        );
        assert!(listed.touched.is_empty());

        // p1 topic → provisional Thema.
        let accepted = env
            .review(&store, &[&work, "accept", "p1"])
            .map_err(ctx("accept topic"))?;
        assert!(store.topic_path("traeume").is_file());
        assert!(accepted.touched.iter().any(|(area, _)| *area == "palace"));
        assert!(accepted.touched.iter().any(|(area, id)| {
            *area == "dream" && id.as_deref() == Some(outcome.report_id.as_str())
        }));
        let topic =
            harw_knowledge::memory::topic::read(&store, "traeume").map_err(ctx("read topic"))?;
        assert_eq!(
            harw_knowledge::memory::palace::artifact_status(&topic),
            harw_knowledge::memory::palace::PalaceStatus::Provisional
        );
        assert_eq!(topic.frontmatter.extra["origin"]["kind"], "dream");

        // p2 palace ohne bestehendes Ziel → ebenfalls Thema.
        env.review(&store, &[&work, "accept", "p2"])
            .map_err(ctx("accept palace"))?;
        assert!(store.topic_path("neu").is_file());

        // p3 diary_reflection → Diary des Ziel-Agenten.
        let diary = env
            .review(&store, &[&work, "accept", "p3"])
            .map_err(ctx("accept diary"))?;
        assert!(diary.touched.iter().any(|(area, _)| *area == "diary"));
        assert_eq!(
            harw_knowledge::diary::list_agents(&store).map_err(ctx("agents"))?,
            vec![AgentId::new("worker-1")]
        );

        // p4 skill_idea → Lern- und Skill-Vorschlag, nie live.
        let skill = env
            .review(&store, &[&work, "accept", "p4"])
            .map_err(ctx("accept skill"))?;
        assert!(
            skill.output.text.contains("/skills review"),
            "{}",
            skill.output.text
        );
        assert_eq!(env.learn.list().map_err(ctx("learn list"))?.len(), 1);

        // p5 agent_idea → Lern-Vorschlag.
        env.review(&store, &[&work, "accept", "p5"])
            .map_err(ctx("accept agent"))?;
        assert_eq!(env.learn.list().map_err(ctx("learn list"))?.len(), 2);

        // p6 follow_up → abgelehnt mit Grund.
        let rejected = env
            .review(&store, &[&work, "reject", "p6", "schon", "erledigt"])
            .map_err(ctx("reject"))?;
        let decision = rejected.output.data.ok_or(TestError::Missing("decision"))?;
        assert_eq!(decision["decision"]["status"], "rejected");
        assert_eq!(decision["decision"]["note"], "schon erledigt");

        let data = read_report_data(&store, "2025-09-24", &work)
            .map_err(ctx("sidecar"))?
            .ok_or(TestError::Missing("sidecar"))?;
        let statuses: Vec<DreamSuggestionStatus> =
            data.suggestions.iter().map(|s| s.status).collect();
        assert_eq!(
            statuses,
            vec![
                DreamSuggestionStatus::Accepted,
                DreamSuggestionStatus::Accepted,
                DreamSuggestionStatus::Accepted,
                DreamSuggestionStatus::Accepted,
                DreamSuggestionStatus::Accepted,
                DreamSuggestionStatus::Rejected,
            ]
        );
        assert!(
            data.suggestions[0]
                .decision_note
                .as_deref()
                .is_some_and(|note| note.contains("/palace promote topic/traeume"))
        );

        // Bereits entschieden und leere Liste.
        assert!(matches!(
            env.review(&store, &[&work, "accept", "p1"]),
            Err(OpError::InvalidArguments(_))
        ));
        let empty = env.review(&store, &[]).map_err(ctx("review empty"))?;
        assert_eq!(empty.output.text, "Keine offenen Traumvorschläge.\n");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[tokio::test]
    async fn a_topic_conflict_keeps_the_suggestion_pending() -> TestResult {
        let store = temporary_store("review-conflict")?;
        let outcome = dreamed(&store).await?;
        let env = Env::new()?;
        std::fs::create_dir_all(store.root().join("topics")).map_err(ctx("topics dir"))?;
        std::fs::write(store.topic_path("traeume"), "---\n---\nalt\n").map_err(ctx("seed"))?;
        assert!(matches!(
            env.review(&store, &[&outcome.work_id, "accept", "p1"]),
            Err(OpError::InvalidArguments(message)) if message.contains("offen")
        ));
        let one = env
            .review(&store, &[&outcome.work_id])
            .map_err(ctx("review one"))?;
        let data = one.output.data.ok_or(TestError::Missing("report"))?;
        assert_eq!(data["report"]["suggestions"][0]["status"], "pending");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn review_grammar_errors_are_invalid_arguments() -> TestResult {
        let store = temporary_store("review-grammar")?;
        let env = Env::new()?;
        for tokens in [
            vec!["a", "b"],
            vec!["unbekannt"],
            vec!["x", "maybe", "p1"],
            vec!["unbekannt", "accept", "p1"],
        ] {
            match env.review(&store, &tokens) {
                Err(OpError::InvalidArguments(_)) => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?} must be InvalidArguments, got {other:?}"
                    )));
                }
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[tokio::test]
    async fn status_reports_last_run_next_run_and_pending() -> TestResult {
        let store = temporary_store("status")?;
        let fresh = run_status(
            &store,
            &VisibilityScope::OperatorOnly,
            &DreamSettings::default(),
            None,
            at(0)?,
        )
        .map_err(ctx("status fresh"))?;
        assert!(
            fresh.text.contains("Letzter Lauf: noch keiner"),
            "{}",
            fresh.text
        );
        assert!(fresh.text.contains("Läuft gerade: nein"));

        let outcome = dreamed(&store).await?;
        let status = run_status(
            &store,
            &VisibilityScope::OperatorOnly,
            &DreamSettings::default(),
            None,
            at(1_758_715_260)?,
        )
        .map_err(ctx("status after run"))?;
        let data = status.data.ok_or(TestError::Missing("status data"))?;
        assert_eq!(
            data["status"]["last_run"]["work_id"],
            outcome.work_id.as_str()
        );
        assert_eq!(data["status"]["last_run"]["status"], "succeeded");
        assert_eq!(data["status"]["next"]["kind"], "cooldown");
        assert_eq!(data["status"]["pending_suggestions"], 6);
        assert_eq!(data["status"]["running"], serde_json::Value::Null);
        assert!(
            status.text.contains("Nächster Lauf: frühestens"),
            "{}",
            status.text
        );

        let cron = DreamSettings {
            schedule: Some("0 3 * * 1".to_owned()),
            enabled: false,
            ..DreamSettings::default()
        };
        let disabled = run_status(
            &store,
            &VisibilityScope::OperatorOnly,
            &cron,
            None,
            at(1_758_715_260)?,
        )
        .map_err(ctx("status disabled"))?;
        assert!(disabled.text.contains("deaktiviert"), "{}", disabled.text);
        assert!(
            disabled.text.contains("Zeitplan „0 3 * * 1“"),
            "{}",
            disabled.text
        );
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[tokio::test]
    async fn the_op_runs_via_the_launcher_and_publishes_a_knowledge_event() -> TestResult {
        use harw_core::agent_events::{AgentEventHub, AgentEventKind};
        use harw_operations::context::ServiceMap;

        let store = temporary_store("op-run")?;
        let hub = Arc::new(AgentEventHub::default());
        let mut events = hub.subscribe();
        let mut services = ServiceMap::new();
        services.insert(Arc::new(store.clone()));
        services.insert(Arc::clone(&hub));
        let launcher: Arc<dyn DreamLauncher> = Arc::new(TestLauncher {
            store: store.clone(),
            now: at(1_758_715_200)?,
        });
        services.insert(launcher);
        let ctx_op = crate::knowledge_test_support::op_context(services)?;

        let output = super::DreamOperation
            .run(
                &ctx_op,
                harw_operations::OpInput::command("/dream", toks(&["run"])),
            )
            .await
            .map_err(ctx("dream run"))?;
        let data = output.data.ok_or(TestError::Missing("run data"))?;
        assert_eq!(data["run"]["suggestions"], 6);
        assert!(output.text.contains("/dream review"), "{}", output.text);
        let event = events.try_recv().map_err(ctx("knowledge event"))?;
        assert!(matches!(
            event.kind,
            AgentEventKind::Knowledge { ref area, .. } if area == "dream"
        ));

        let without = crate::knowledge_test_support::op_context({
            let mut services = ServiceMap::new();
            services.insert(Arc::new(store.clone()));
            services
        })?;
        assert!(matches!(
            super::DreamOperation
                .run(
                    &without,
                    harw_operations::OpInput::command("/dream", toks(&["run"])),
                )
                .await,
            Err(OpError::NotAvailable(_))
        ));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn proposals_ignore_follow_ups_and_empty_sections() {
        let body = "## Vorgeschlagene Topic-Updates\n\n_Keine._\n\n\
                    ## Vorgeschlagene Palace-Promotionen\n\n- `palace/a`: A\n\n\
                    ## Offene Fäden\n\n- `x`: kein Vorschlag\n";
        let found = proposals(body);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["target"], "palace/a");
        assert_eq!(found[0]["summary"], "A");
    }
}
