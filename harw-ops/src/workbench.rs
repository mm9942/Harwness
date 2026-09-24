//! `/workbench` — die Arbeitsfläche der Sitzung bzw. eines Projekts
//! (`docs/design/knowledge-surfaces.md` §5, `interaction-contract.md` §2.2).
//!
//! # Subcommands
//! - (bare) / `show` `[--scope=session|project:<slug>]` — Panel-Text:
//!   angeheftete Dateien, offene Hypothesen, Live-Tail von `NOTES.md`; dazu
//!   `OpOutput::data` = `{"scope","pinned":[{"path","note"}],
//!   "hypotheses":[{"id","text","status"}],"notes_tail":[..]}` für das
//!   TUI-Workbench-Pane.
//! - `pin <path> [note]` — heftet eine Datei an; ein relativer Pfad wird
//!   gegen das Arbeitsverzeichnis des Prozesses aufgelöst (TUI-cwd).
//! - `unpin <path>` — löst eine angeheftete Datei.
//! - `note <text>` — hängt an `NOTES.md` an.
//! - `hypothesis add|confirm|reject <text>` — pflegt `hypotheses.md`;
//!   `confirm`/`reject` wählen per exaktem Text oder `#<n>`.
//!
//! Jeder Subcommand akzeptiert `--scope=`; ohne Angabe gilt die aktive
//! Sitzung (`ctx.session_id()`).
//!
//! # Fläche
//! Nur `Surface::Command` (`channel_reduced`, Vertrag: `note`/`hypothesis`
//! reduziert, `pin`/bare TUI-only — eine Operation trägt nur eine
//! Sichtbarkeit, darum die weitere der beiden). Kein Modell-Werkzeug: das
//! Modell schreibt über `workbench.note`/`workbench.hypothesis`
//! (`harw-registry-defaults`), die nie Pfade anheften.
//!
//! # Dienste
//! `Arc<harw_knowledge::KnowledgeStore>` aus [`OpContext::service`] (L6,
//! Slash/Modell-Werkzeug); ohne ihn [`OpError::NotAvailable`]. Der Autor der
//! Einträge ist die Id des `harw_types::Principal`, sonst `operator`.
//!
//! # Gemeinsame Helfer
//! [`knowledge_store`], [`caller_agent`] und [`map_knowledge_error`] nutzen
//! auch `/kanban`, `/diary` und `/palace`.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store im Kontext.
//! - [`OpError::InvalidArguments`] — Grammatikfehler, ungültige Eingaben,
//!   unbekannte/bereits entschiedene Hypothese.
//! - [`OpError::Execution`] — Ein-/Ausgabefehler des Speichers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_knowledge::workbench::{self, HypothesisStatus, Workbench, WorkbenchScope};
use harw_knowledge::{AgentId, KnowledgeError, KnowledgeStore};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_types::Principal;

/// Anzahl der Notizzeilen im Panel-Tail.
const NOTES_TAIL_LINES: usize = 8;

/// Argument-Container für `/workbench`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct WorkbenchArgs {
    /// Alle Tokens nach `/workbench`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl harw_operations::FromRawArgs for WorkbenchArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Führt `/workbench` aus.
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "workbench",
    summary = "Arbeitsfläche: pin, unpin, note, hypothesis add|confirm|reject; ohne Subcommand das Panel.",
    domain = "knowledge",
    permission = "operator",
    command(path = "/workbench", visibility = "channel_reduced")
)]
async fn workbench(ctx: &OpContext, args: WorkbenchArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    let cwd = std::env::current_dir().ok();
    run_workbench(
        &store,
        ctx.session_id().as_str(),
        &caller_agent(ctx),
        &args.tokens,
        jiff::Timestamp::now(),
        cwd.as_deref(),
    )
}

/// Der reine Kern von `/workbench` (testbar ohne `OpContext`).
///
/// # Argumente
/// - `store` — Wissensspeicher.
/// - `session_id` — aktive Sitzung (Default-Scope).
/// - `author` — Autor neuer Einträge.
/// - `tokens` — Kommandotokens.
/// - `now` — Zeitstempel für Schreibvorgänge.
/// - `cwd` — Basis für relative `pin`-Pfade.
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_workbench(
    store: &KnowledgeStore,
    session_id: &str,
    author: &AgentId,
    tokens: &[String],
    now: jiff::Timestamp,
    cwd: Option<&Path>,
) -> Result<OpOutput, OpError> {
    let (scope_flag, rest) = split_flag(tokens, "--scope=");
    let scope = match scope_flag {
        Some(raw) => WorkbenchScope::parse(&raw, session_id).map_err(map_knowledge_error)?,
        None => WorkbenchScope::Session(session_id.to_owned()),
    };
    let sub = rest.first().map(String::as_str).unwrap_or("show");
    let tail = rest.get(1..).unwrap_or_default();
    match sub {
        "show" => render(&workbench::load(store, &scope).map_err(map_knowledge_error)?),
        "pin" => {
            let raw_path = tail
                .first()
                .ok_or_else(|| usage("/workbench pin <path> [note]"))?;
            let path = resolve_path(raw_path, cwd);
            let note = tail.get(1..).unwrap_or_default().join(" ");
            let added = workbench::pin(store, &scope, author, &path, &note, now)
                .map_err(map_knowledge_error)?;
            Ok(OpOutput::from(if added {
                format!("Angeheftet: {path}")
            } else {
                format!("Notiz aktualisiert: {path}")
            }))
        }
        "unpin" => {
            let raw_path = tail
                .first()
                .ok_or_else(|| usage("/workbench unpin <path>"))?;
            let path = resolve_path(raw_path, cwd);
            let removed =
                workbench::unpin(store, &scope, author, &path, now).map_err(map_knowledge_error)?;
            Ok(OpOutput::from(if removed {
                format!("Gelöst: {path}")
            } else {
                format!("Nicht angeheftet: {path}")
            }))
        }
        "note" => {
            let text = tail.join(" ");
            workbench::append_note(store, &scope, author, &text, now)
                .map_err(map_knowledge_error)?;
            Ok(OpOutput::from(format!(
                "Notiz in {} gespeichert.",
                scope.path_component()
            )))
        }
        "hypothesis" => run_hypothesis(store, &scope, author, tail, now),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter /workbench-Subcommand: {other} (show, pin, unpin, note, hypothesis)"
        ))),
    }
}

/// `hypothesis add|confirm|reject <text>`.
fn run_hypothesis(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    tail: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let grammar = "/workbench hypothesis add|confirm|reject <text>";
    let action = tail.first().ok_or_else(|| usage(grammar))?;
    let text = tail.get(1..).unwrap_or_default().join(" ");
    if text.trim().is_empty() {
        return Err(usage(grammar));
    }
    match action.as_str() {
        "add" => {
            let number = workbench::add_hypothesis(store, scope, author, &text, now)
                .map_err(map_knowledge_error)?;
            Ok(OpOutput::from(format!(
                "Hypothese #{number} angelegt: {text}"
            )))
        }
        "confirm" | "reject" => {
            let decided = if action == "confirm" {
                workbench::confirm(store, scope, author, &text, now)
            } else {
                workbench::reject(store, scope, author, &text, now)
            }
            .map_err(map_knowledge_error)?;
            Ok(OpOutput::from(format!(
                "Hypothese {}: {}",
                decided.status.label(),
                decided.text
            )))
        }
        other => Err(OpError::InvalidArguments(format!(
            "unbekannte Hypothesen-Aktion: {other} ({grammar})"
        ))),
    }
}

/// Strukturierte Nutzlast des Panels für die TUI (`OpOutput::data`).
///
/// Form: `{"scope", "pinned":[{"path","note"}],
/// "hypotheses":[{"id","text","status"}], "notes_tail":[..]}`; `id` ist die
/// Auswahl `#<n>`, die `hypothesis confirm|reject` direkt versteht.
fn panel_data(bench: &Workbench) -> serde_json::Value {
    let pinned: Vec<serde_json::Value> = bench
        .pinned
        .iter()
        .map(|entry| serde_json::json!({ "path": entry.absolute_path, "note": entry.note }))
        .collect();
    let hypotheses: Vec<serde_json::Value> = bench
        .hypotheses
        .iter()
        .enumerate()
        .map(|(index, hypothesis)| {
            serde_json::json!({
                "id": format!("#{}", index + 1),
                "text": hypothesis.text,
                "status": hypothesis.status.label(),
            })
        })
        .collect();
    serde_json::json!({
        "scope": bench.scope.path_component(),
        "pinned": pinned,
        "hypotheses": hypotheses,
        "notes_tail": bench.notes_tail(NOTES_TAIL_LINES),
    })
}

/// Rendert das Panel (§5.2) als Text plus [`panel_data`].
fn render(bench: &Workbench) -> Result<OpOutput, OpError> {
    let data = Some(panel_data(bench));
    let scope = bench.scope.path_component();
    if bench.is_empty() {
        return Ok(OpOutput {
            text: format!("Workbench {scope} ist leer."),
            data,
        });
    }
    let mut out = format!("Workbench {scope}\n");
    out.push_str(&format!("Angeheftet ({}):\n", bench.pinned.len()));
    for entry in &bench.pinned {
        if entry.note.is_empty() {
            out.push_str(&format!("  · {}\n", entry.absolute_path));
        } else {
            out.push_str(&format!("  · {} — {}\n", entry.absolute_path, entry.note));
        }
    }
    let open = bench.open_hypotheses();
    out.push_str(&format!("Offene Hypothesen ({}):\n", open.len()));
    for (number, hypothesis) in &open {
        out.push_str(&format!("  #{number} {}\n", hypothesis.text));
    }
    let decided = bench
        .hypotheses
        .iter()
        .filter(|hypothesis| hypothesis.status != HypothesisStatus::Testing)
        .count();
    if decided > 0 {
        out.push_str(&format!("Entschiedene Hypothesen: {decided}\n"));
    }
    let tail = bench.notes_tail(NOTES_TAIL_LINES);
    out.push_str(&format!("Notizen (letzte {} Zeilen):\n", tail.len()));
    for line in tail {
        out.push_str(&format!("  {line}\n"));
    }
    Ok(OpOutput { text: out, data })
}

/// Löst einen relativen Pfad gegen `cwd` auf; absolute bleiben unverändert.
fn resolve_path(raw: &str, cwd: Option<&Path>) -> String {
    let path = PathBuf::from(raw);
    match cwd {
        Some(base) if path.is_relative() => base.join(path).to_string_lossy().into_owned(),
        _ => raw.to_owned(),
    }
}

fn usage(grammar: &str) -> OpError {
    OpError::InvalidArguments(format!("Aufruf: {grammar}"))
}

// --- Gemeinsame Helfer der Wissens-Ops --------------------------------------

/// Löst `Arc<KnowledgeStore>` aus dem Kontext auf.
///
/// # Fehler
/// [`OpError::NotAvailable`], wenn die Fläche keinen Speicher trägt.
pub(crate) fn knowledge_store(ctx: &OpContext) -> Result<Arc<KnowledgeStore>, OpError> {
    ctx.service::<Arc<KnowledgeStore>>()
        .cloned()
        .ok_or_else(|| OpError::NotAvailable("kein Knowledge-Store im Kontext".to_owned()))
}

/// Die Identität, unter der eine Wissens-Op schreibt: die Principal-Id,
/// sonst `operator`.
pub(crate) fn caller_agent(ctx: &OpContext) -> AgentId {
    ctx.service::<Principal>()
        .map(|principal| AgentId::new(principal.id()))
        .unwrap_or_else(|| AgentId::new("operator"))
}

/// Übersetzt [`KnowledgeError`] in [`OpError`]: Eingabe-/Zustandsfehler
/// werden [`OpError::InvalidArguments`], alles andere [`OpError::Execution`].
pub(crate) fn map_knowledge_error(error: KnowledgeError) -> OpError {
    let caller_error = match &error {
        KnowledgeError::Io(io) => io.kind() == std::io::ErrorKind::InvalidInput,
        KnowledgeError::ArtifactNotFound(_)
        | KnowledgeError::IllegalTransition { .. }
        | KnowledgeError::ArchiveBlockedByChildren { .. }
        | KnowledgeError::ClaimRequiresApproval { .. }
        | KnowledgeError::PromotionNotReviewed { .. }
        | KnowledgeError::VisibilityDenied { .. }
        | KnowledgeError::RecallBoundExceeded { .. } => true,
        _ => false,
    };
    if caller_error {
        OpError::InvalidArguments(error.to_string())
    } else {
        OpError::Execution(error.to_string())
    }
}

/// Trennt das erste `<prefix><wert>`-Token ab.
///
/// # Rückgabe
/// `(Some(wert), restliche Tokens)` bzw. `(None, alle Tokens)`.
pub(crate) fn split_flag(tokens: &[String], prefix: &str) -> (Option<String>, Vec<String>) {
    let mut value = None;
    let mut rest = Vec::with_capacity(tokens.len());
    for token in tokens {
        match token.strip_prefix(prefix) {
            Some(found) if value.is_none() => value = Some(found.to_owned()),
            _ => rest.push(token.clone()),
        }
    }
    (value, rest)
}

/// Sammelt alle `<prefix><wert>`-Tokens (wiederholbare Flags).
pub(crate) fn split_repeated_flag(tokens: &[String], prefix: &str) -> (Vec<String>, Vec<String>) {
    let mut values = Vec::new();
    let mut rest = Vec::with_capacity(tokens.len());
    for token in tokens {
        match token.strip_prefix(prefix) {
            Some(found) => values.push(found.to_owned()),
            None => rest.push(token.clone()),
        }
    }
    (values, rest)
}

#[cfg(test)]
mod tests {
    use super::{WorkbenchArgs, run_workbench, split_flag};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_knowledge::{AgentId, KnowledgeStore};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpError, Operation};

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-ops-workbench-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create temporary knowledge root"))?;
        Ok(KnowledgeStore::new(&root))
    }

    fn run(store: &KnowledgeStore, tokens: &[&str]) -> Result<String, OpError> {
        run_workbench(
            store,
            "s-1",
            &AgentId::new("operator"),
            &toks(tokens),
            jiff::Timestamp::now(),
            Some(std::path::Path::new("/work/project")),
        )
        .map(|output| output.text)
    }

    #[test]
    fn workbench_is_a_command_without_a_model_tool_surface() {
        let surfaces = &super::WorkbenchOperation.meta().surfaces;
        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Command {
                path: "/workbench",
                visibility: CommandVisibility::ChannelReduced,
            }
        )));
    }

    #[test]
    fn from_raw_args_keeps_every_token() -> TestResult {
        let args =
            WorkbenchArgs::from_raw_args(&toks(&["note", "a", "b"])).map_err(ctx("parse"))?;
        assert_eq!(args.tokens, toks(&["note", "a", "b"]));
        Ok(())
    }

    #[test]
    fn bare_workbench_shows_an_empty_panel() -> TestResult {
        let store = temporary_store("bare")?;
        let text = run(&store, &[]).map_err(ctx("bare succeeds"))?;
        assert_eq!(text, "Workbench session:s-1 ist leer.");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn pin_note_and_hypotheses_show_up_in_the_panel() -> TestResult {
        let store = temporary_store("panel")?;
        run(&store, &["pin", "src/lib.rs", "Einstieg"]).map_err(ctx("pin"))?;
        run(&store, &["note", "Cache", "prüfen"]).map_err(ctx("note"))?;
        run(&store, &["hypothesis", "add", "Lock", "hängt"]).map_err(ctx("add"))?;
        run(&store, &["hypothesis", "add", "Netz", "langsam"]).map_err(ctx("add 2"))?;
        run(&store, &["hypothesis", "reject", "#2"]).map_err(ctx("reject"))?;

        let text = run(&store, &["show"]).map_err(ctx("show"))?;
        assert!(
            text.contains("/work/project/src/lib.rs — Einstieg"),
            "{text}"
        );
        assert!(text.contains("#1 Lock hängt"), "{text}");
        assert!(!text.contains("Netz langsam"), "{text}");
        assert!(text.contains("Entschiedene Hypothesen: 1"), "{text}");
        assert!(text.contains("Cache prüfen"), "{text}");

        let unpinned = run(&store, &["unpin", "/work/project/src/lib.rs"]).map_err(ctx("unpin"))?;
        assert!(unpinned.starts_with("Gelöst"));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn show_carries_the_panel_data_for_the_tui() -> TestResult {
        let store = temporary_store("data")?;
        run(&store, &["pin", "/abs/x.rs", "warum"]).map_err(ctx("pin"))?;
        run(&store, &["hypothesis", "add", "H1"]).map_err(ctx("add"))?;
        run(&store, &["note", "zeile"]).map_err(ctx("note"))?;
        let output = run_workbench(
            &store,
            "s-1",
            &AgentId::new("operator"),
            &toks(&["show"]),
            jiff::Timestamp::now(),
            None,
        )
        .map_err(ctx("show"))?;
        let data = output.data.ok_or(TestError::Missing("show data"))?;
        assert_eq!(data["scope"], "session:s-1");
        assert_eq!(data["pinned"][0]["path"], "/abs/x.rs");
        assert_eq!(data["pinned"][0]["note"], "warum");
        assert_eq!(data["hypotheses"][0]["id"], "#1");
        assert_eq!(data["hypotheses"][0]["status"], "testing");
        assert!(
            data["notes_tail"]
                .as_array()
                .is_some_and(|lines| lines.iter().any(|line| line == "zeile")),
            "{data}"
        );
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn project_scope_is_separate_from_the_session() -> TestResult {
        let store = temporary_store("project")?;
        run(&store, &["note", "geteilt", "--scope=project:harw"]).map_err(ctx("note"))?;
        let session = run(&store, &[]).map_err(ctx("session show"))?;
        assert!(session.ends_with("ist leer."));
        let project = run(&store, &["--scope=project:harw"]).map_err(ctx("project show"))?;
        assert!(project.contains("geteilt"));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn grammar_errors_are_invalid_arguments() -> TestResult {
        let store = temporary_store("grammar")?;
        for tokens in [
            vec!["frobnicate"],
            vec!["pin"],
            vec!["note"],
            vec!["hypothesis", "maybe", "x"],
            vec!["hypothesis", "confirm", "#7"],
            vec!["--scope=global"],
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
    fn split_flag_takes_the_first_occurrence_only() {
        let (value, rest) = split_flag(&toks(&["a", "--x=1", "--x=2"]), "--x=");
        assert_eq!(value.as_deref(), Some("1"));
        assert_eq!(rest, toks(&["a", "--x=2"]));
    }
}
