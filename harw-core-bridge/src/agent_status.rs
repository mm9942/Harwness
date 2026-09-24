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
    // Provider und Modell, die das Kind tatsächlich anspricht.
    entry.insert("model".to_owned(), json!(run.model_route));
    entry.insert("status".to_owned(), json!(run.status.as_str()));
    entry.insert("task".to_owned(), json!(run.task));
    entry.insert("elapsed_s".to_owned(), json!(run.elapsed().as_secs()));
    entry.insert("tool_calls".to_owned(), json!(run.progress.tool_calls));
    // Runde 7, Teil A4: Tokens einschließlich der laufenden Runde — nicht
    // mehr 0, solange die erste Modellrunde noch läuft.
    entry.insert("tokens".to_owned(), json!(run.progress.tokens_with_live()));
    entry.insert(
        "live_round_tokens".to_owned(),
        json!(run.progress.live_round_tokens),
    );
    entry.insert("rounds".to_owned(), json!(run.progress.rounds));
    entry.insert(
        "context_tokens".to_owned(),
        json!(run.progress.context_tokens),
    );
    entry.insert("last_step".to_owned(), json!(run.progress.last_step));
    entry.insert("summary".to_owned(), json!(run.summary));
    entry
}

/// Eine Textzeile je Lauf (Erweiterungsstelle wie [`status_entry`]).
#[must_use]
pub fn status_line(run: &BackgroundRun) -> String {
    let route = run
        .model_route
        .as_deref()
        .map(|route| format!(" · {route}"))
        .unwrap_or_default();
    let mut line = format!(
        "- {} ({}){route} · {} · {} s · {} Werkzeugaufrufe · {} Tokens",
        run.role,
        run.child,
        run.status.label_de(),
        run.elapsed().as_secs(),
        run.progress.tool_calls,
        run.progress.tokens_with_live(),
    );
    // Runde 7, Teil A4: Live-Anteil, Runden und Kontextbelegung.
    if run.progress.live_round_tokens > 0 {
        line.push_str(&format!(
            " (davon {} in der laufenden Runde)",
            run.progress.live_round_tokens
        ));
    }
    if run.progress.rounds > 0 {
        line.push_str(&format!(" · {} Runden", run.progress.rounds));
    }
    if let Some(context) = run.progress.context_tokens {
        line.push_str(&format!(" · Kontext {context} Tokens"));
    }
    if let Some(step) = &run.progress.last_step {
        line.push_str(&format!(" · letzter Schritt: {step}"));
    }
    if let Some(summary) = &run.summary {
        line.push_str(&format!("\n  Ergebnis (Anfang): {summary}"));
    }
    line
}

/// Ergänzt den Token-Stand eines laufenden Laufs aus dem Admission-Record
/// des Kindes (Runde 7, Teil A4).
///
/// # Beschreibung
/// Der Record zählt jede abgeschlossene Modellrunde über den
/// Fortschritts-Beobachter des Spawners — unabhängig vom Ereignisbus. Ist er
/// höher als der Stand aus dem Bus, gilt er.
fn with_record_tokens(
    spawner: &harw_core::ManagedAgentSpawner,
    mut run: BackgroundRun,
) -> BackgroundRun {
    if let Some(record) = spawner.child_record(&run.child) {
        let recorded = record.live.usage.fresh_tokens();
        if recorded > run.progress.tokens {
            run.progress.tokens = recorded;
        }
    }
    run
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
    // Runde 7, Teil A4: fehlt dem Ereignisbus ein Verbrauch (kein Bus,
    // verpasste Ereignisse), zählt der Verbrauch aus dem Admission-Record.
    let runs: Vec<BackgroundRun> = runs
        .into_iter()
        .map(|run| with_record_tokens(spawner.as_ref(), run))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_core::background_children::BackgroundChildren;
    use harw_types::SessionId;

    /// Runde 7, Teil A4: der Status zeigt die Tokens der laufenden Runde
    /// (statt 0), die Rundenzahl und die Kontextbelegung.
    #[test]
    fn live_round_tokens_appear_in_the_status() -> TestResult {
        let registry = BackgroundChildren::default();
        let parent = SessionId::new();
        let child = SessionId::new();
        registry.register(&child, &parent, "root-orchestrator", Some("Analyse"));
        registry.set_model_route(&child, Some("anthropic/claude-opus-5-5".to_owned()));
        registry.record_progress(child.as_str(), |progress| {
            progress.tokens = 1_000;
            progress.live_round_tokens = 250;
            progress.rounds = 3;
            progress.context_tokens = Some(40_000);
        });
        let run = registry
            .run_for(&parent, child.as_str())
            .ok_or(TestError::Missing("Lauf"))?;

        let entry = status_entry(&run);
        assert_eq!(entry["tokens"], json!(1_250));
        assert_eq!(entry["live_round_tokens"], json!(250));
        assert_eq!(entry["rounds"], json!(3));
        assert_eq!(entry["context_tokens"], json!(40_000));
        assert_eq!(entry["model"], json!("anthropic/claude-opus-5-5"));

        let line = status_line(&run);
        assert!(line.contains("1250 Tokens"), "{line}");
        assert!(line.contains(" · anthropic/claude-opus-5-5 · "), "{line}");
        assert!(line.contains("davon 250 in der laufenden Runde"), "{line}");
        assert!(line.contains("3 Runden"), "{line}");
        assert!(line.contains("Kontext 40000 Tokens"), "{line}");
        Ok(())
    }
}
