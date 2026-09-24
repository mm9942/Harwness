//! `agent.status` — Status und Fortschritt der eigenen Hintergrund-Agenten
//! (Runde 5, Teil K).
//!
//! # Verantwortungsbereich
//! Rein lesend und auto-freigegeben. Listet laufende und zuletzt beendete
//! Hintergrund-Läufe der **aufrufenden** Sitzung (aus dem Register des
//! Spawners, `harw_core::background_children`) mit Werkzeugaufrufen, Tokens,
//! Laufzeit und letztem Schritt:
//!
//! ```text
//! agent.status { child_id? }
//! ```
//!
//! # Erweiterung
//! Jeder Lauf wird genau an **einer** Stelle zu JSON ([`status_entry`]) und
//! zu Text ([`status_line`]). Zusätzliche Felder — etwa Journal-Schritte
//! (`steps`, `recent`, `changed_files`) — werden dort angehängt, nicht über
//! ein zweites Werkzeug.
//!
//! # Sicherheit
//! Die aufrufende Sitzung kommt aus dem Ausführungskontext, nie aus
//! Modell-Argumenten; eine fremde oder unbekannte Kind-ID bekommt immer
//! dieselbe Meldung.

use std::sync::OnceLock;

use harw_core::background_children::BackgroundRun;
use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::op_schema::{object_schema, string_schema};
use harw_operations::operation::{
    ApprovalPolicy, ArgsSchemaFn, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface,
};
use serde_json::{Map, Value, json};

use crate::agent_background::{AGENT_STATUS_TOOL, parse_child_id};
use crate::context_ext::OpContextCoreExt;

/// Ein Lauf als JSON-Objekt für das Modell.
///
/// # Erweiterungsstelle
/// Weitere Felder je Lauf (z. B. `steps`, `recent`, `changed_files` aus
/// einem Aktivitätsjournal) werden **hier** in `entry` eingefügt; Liste und
/// Einzelabfrage übernehmen sie automatisch.
#[must_use]
pub fn status_entry(run: &BackgroundRun) -> Map<String, Value> {
    let mut entry = Map::new();
    entry.insert("child_id".to_owned(), json!(run.child.as_str()));
    entry.insert("role".to_owned(), json!(run.role));
    entry.insert("status".to_owned(), json!(run.status.as_str()));
    entry.insert("task".to_owned(), json!(run.task));
    entry.insert("elapsed_s".to_owned(), json!(run.elapsed().as_secs()));
    entry.insert("tool_calls".to_owned(), json!(run.progress.tool_calls));
    entry.insert("tokens".to_owned(), json!(run.progress.tokens));
    entry.insert("last_step".to_owned(), json!(run.progress.last_step));
    entry.insert("summary".to_owned(), json!(run.summary));
    entry
}

/// Eine Textzeile je Lauf (Erweiterungsstelle wie [`status_entry`]).
#[must_use]
pub fn status_line(run: &BackgroundRun) -> String {
    let mut line = format!(
        "- {} ({}) · {} · {} s · {} Werkzeugaufrufe · {} Tokens",
        run.role,
        run.child,
        run.status.label_de(),
        run.elapsed().as_secs(),
        run.progress.tool_calls,
        run.progress.tokens,
    );
    if let Some(step) = &run.progress.last_step {
        line.push_str(&format!(" · letzter Schritt: {step}"));
    }
    if let Some(summary) = &run.summary {
        line.push_str(&format!("\n  Ergebnis (Anfang): {summary}"));
    }
    line
}

/// Führt `agent.status` aus.
///
/// # Errors
/// [`OpError::NotAvailable`] ohne Spawner oder für eine fremde/unbekannte
/// `child_id`.
pub fn agent_status(ctx: &OpContext, child_id: Option<&str>) -> Result<OpOutput, OpError> {
    let spawner = ctx.managed_spawner().ok_or_else(|| {
        OpError::NotAvailable("kein Agent-Spawner in diesem Kontext konfiguriert".to_owned())
    })?;
    let registry = spawner.background_children();
    // Runde 5, Teil M: eigene Kinder ohne Hintergrund-Lauf (synchron
    // gestartet) sind über ihr Aktivitätsjournal ebenfalls abfragbar.
    let mut journal_only: Vec<harw_core::ChildJournal> = Vec::new();
    let runs: Vec<BackgroundRun> = match child_id {
        Some(id) => match registry.run_for(ctx.session_id(), id) {
            Some(run) => vec![run],
            None => {
                let journal = spawner
                    .child_journal_for(ctx.session_id(), id)
                    .ok_or_else(|| {
                        OpError::NotAvailable(format!(
                            "{AGENT_STATUS_TOOL}: kein eigener Agent mit der ID {id}"
                        ))
                    })?;
                journal_only.push(journal);
                Vec::new()
            }
        },
        None => {
            let runs = registry.runs_for(ctx.session_id());
            journal_only = spawner
                .child_comms()
                .journals_for(ctx.session_id())
                .into_iter()
                .filter(|journal| {
                    journal.is_running() && !runs.iter().any(|run| run.child == journal.child)
                })
                .collect();
            runs
        }
    };
    let journal_of =
        |run: &BackgroundRun| spawner.child_journal_for(ctx.session_id(), run.child.as_str());
    let text = if runs.is_empty() && journal_only.is_empty() {
        "Keine Hintergrund-Agenten in dieser Sitzung.".to_owned()
    } else {
        let mut text = String::from("Hintergrund-Agenten:\n");
        let mut lines: Vec<String> = runs
            .iter()
            .map(|run| match journal_of(run) {
                Some(journal) => format!(
                    "{}\n{}",
                    status_line(run),
                    crate::agent_messaging::journal_status_line(&journal)
                ),
                None => status_line(run),
            })
            .collect();
        lines.extend(
            journal_only
                .iter()
                .map(crate::agent_messaging::journal_only_line),
        );
        text.push_str(&lines.join("\n"));
        text.push_str(
            "\nDas vollständige Ergebnis eines beendeten Laufs liefert agent.result {child_id}.",
        );
        text
    };
    let mut agents: Vec<Value> = runs
        .iter()
        .map(|run| {
            let mut entry = status_entry(run);
            if let Some(journal) = journal_of(run) {
                crate::agent_messaging::journal_status_fields(&mut entry, &journal);
            }
            Value::Object(entry)
        })
        .collect();
    agents.extend(
        journal_only
            .iter()
            .map(|journal| Value::Object(crate::agent_messaging::journal_only_entry(journal))),
    );
    Ok(OpOutput {
        text,
        data: Some(json!({ "agents": agents })),
    })
}

/// Die Operation `agent.status` als Modell-Werkzeug (rein lesend).
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentStatusOperation;

const AGENT_STATUS_ARGS_SCHEMA: ArgsSchemaFn = || {
    object_schema(
        vec![(
            "child_id",
            string_schema("Optional: nur diesen Hintergrund-Agenten zeigen (null = alle eigenen)."),
        )],
        &[],
    )
};

impl Operation for AgentStatusOperation {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: AGENT_STATUS_TOOL,
            summary: "Zeigt eigene Hintergrund-Agenten (laufend und zuletzt beendet) mit \
                      Fortschritt: Werkzeugaufrufe, Tokens, Laufzeit, letzter Schritt.",
            domain: OperationDomain::Agents,
            permission: PermissionTier::Observer,
            surfaces: vec![Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None,
            }],
            category: OperationCategory::Agent,
            args_schema: Some(AGENT_STATUS_ARGS_SCHEMA),
            ..OperationMeta::default()
        })
    }

    fn run<'a>(&'a self, ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            if !input.invocation.is_model_tool() {
                return Err(OpError::InvalidArguments(format!(
                    "{AGENT_STATUS_TOOL}: is only available as a model tool"
                )));
            }
            let child_id = parse_child_id(AGENT_STATUS_TOOL, input.invocation.json_args(), false)?;
            agent_status(ctx, child_id.as_deref())
        })
    }
}
