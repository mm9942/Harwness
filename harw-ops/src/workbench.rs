//! `/workbench` — die Arbeitsfläche der Sitzung bzw. eines Projekts
//! (`docs/design/knowledge-surfaces.md` §5, `interaction-contract.md` §2.2).
//!
//! # Subcommands
//! - (bare) / `show` `[--scope=session|project|project:<slug>]` — Panel-Text:
//!   angeheftete Dateien (mit Zustand), offene Hypothesen, Live-Tail von
//!   `NOTES.md`; dazu `OpOutput::data` = `{"scope","retention",
//!   "pinned":[{"path","note","status","pinned_at","preview"}],
//!   "hypotheses":[{"id","text","status"}],"notes":[{"id","at","text"}],
//!   "notes_tail":[..]}` für das TUI-Workbench-Pane. `status` ist
//!   `present|changed|missing|not_a_file`, `preview` die ersten Zeilen.
//! - `pin <path> [note]` — heftet eine Datei an; ein relativer Pfad wird
//!   gegen das Arbeitsverzeichnis des Prozesses aufgelöst (TUI-cwd).
//! - `unpin <path>` — löst eine angeheftete Datei.
//! - `note <text>` — hängt an `NOTES.md` an.
//! - `note edit <n> <text>` / `note rm <n>` — ersetzt bzw. löscht eine Notiz;
//!   `<n>` ist `#<n>`/`<n>` (1-basiert) oder ihr Zeitstempel. Nur wenn das
//!   zweite Token so eine Auswahl ist (und bei `rm` nichts folgt), gilt die
//!   Zeile als Bearbeitung; sonst ist sie eine normale Notiz.
//! - `hypothesis add|confirm|reject <text>` — pflegt `hypotheses.md`;
//!   `confirm`/`reject` wählen per exaktem Text oder `#<n>`.
//! - `retention [keep|<tage>d]` — zeigt bzw. setzt die Aufbewahrung des
//!   Scopes (`harw_knowledge::workbench::Retention`).
//!
//! Jeder Subcommand akzeptiert `--scope=`; ohne Angabe gilt die aktive
//! Sitzung (`ctx.session_id()`). `--scope=project` ohne Slug meint das
//! Projekt des Arbeitsverzeichnisses (`WorkbenchScope::project_for_path`).
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
//! Einträge ist die Id des `harw_types::Principal`, sonst `operator`
//! ([`crate::knowledge_common`]). Nach jedem Schreibvorgang geht ein
//! `AgentEventKind::Knowledge { area: "workbench", id: <scope> }` über den
//! `AgentEventHub`, falls der Kontext einen trägt.
//!
//! # Argumente
//! Geparst über [`crate::knowledge_args`]: `--scope` als `--scope=<s>` oder
//! `--scope <s>`, an beliebiger Stelle.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store im Kontext.
//! - [`OpError::InvalidArguments`] — Grammatikfehler, ungültige Eingaben,
//!   unbekannte/bereits entschiedene Hypothese.
//! - [`OpError::Execution`] — Ein-/Ausgabefehler des Speichers.

use std::path::{Path, PathBuf};

use harw_knowledge::workbench::{
    self, HypothesisStatus, PinState, Retention, Workbench, WorkbenchScope,
};
use harw_knowledge::{AgentId, KnowledgeStore};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

use crate::knowledge_args::{FlagSpec, KnowledgeArgs};
use crate::knowledge_common::{
    AREA_WORKBENCH, caller_agent, knowledge_store, map_knowledge_error, publish_knowledge,
};

/// Flags von `/workbench`.
const FLAGS: &[FlagSpec] = &[FlagSpec::value("scope")];

/// Subcommands, die schreiben (und darum ein Knowledge-Event auslösen);
/// `retention` nur mit Argument.
const WRITE_SUBCOMMANDS: &[&str] = &["pin", "unpin", "note", "hypothesis"];

/// Anzahl der Notizzeilen im Panel-Tail.
const NOTES_TAIL_LINES: usize = 8;

/// Anzahl der Vorschauzeilen je angehefteter Datei im Panel.
const PIN_PREVIEW_LINES: usize = 3;

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
    summary = "Arbeitsfläche: pin, unpin, note [edit|rm <n>], hypothesis add|confirm|reject, retention; ohne Subcommand das Panel.",
    domain = "knowledge",
    permission = "operator",
    command(
        path = "/workbench",
        visibility = "channel_reduced",
        channel_subcommands = "note,hypothesis",
        busy_subcommands = "-=immediate, show=immediate"
    )
)]
async fn workbench(ctx: &OpContext, args: WorkbenchArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    let cwd = std::env::current_dir().ok();
    let output = run_workbench(
        &store,
        ctx.session_id().as_str(),
        &caller_agent(ctx),
        &args.tokens,
        jiff::Timestamp::now(),
        cwd.as_deref(),
    )?;
    let parsed = KnowledgeArgs::parse(&args.tokens, FLAGS)?;
    let writes = parsed.subcommand().is_some_and(|sub| {
        WRITE_SUBCOMMANDS.contains(&sub) || (sub == "retention" && !parsed.rest().is_empty())
    });
    if writes {
        let scope = resolve_scope(&parsed, ctx.session_id().as_str(), cwd.as_deref())?;
        publish_knowledge(ctx, AREA_WORKBENCH, Some(scope.path_component()));
    }
    Ok(output)
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
    let args = KnowledgeArgs::parse(tokens, FLAGS)?;
    let scope = resolve_scope(&args, session_id, cwd)?;
    let sub = args.subcommand().unwrap_or("show");
    let tail = args.rest();
    match sub {
        "show" => render(
            &workbench::load(store, &scope).map_err(map_knowledge_error)?,
            workbench::retention(store, &scope).map_err(map_knowledge_error)?,
        ),
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
        "note" if note_edit_selector(tail).is_some() => {
            run_note_edit(store, &scope, author, tail, now)
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
        "retention" => run_retention(store, &scope, tail),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter /workbench-Subcommand: {other} (show, pin, unpin, note, hypothesis, retention)"
        ))),
    }
}

/// Löst `--scope` auf; ohne Angabe die Sitzung, `project` ohne Slug das
/// Projekt von `cwd`.
fn resolve_scope(
    args: &KnowledgeArgs,
    session_id: &str,
    cwd: Option<&Path>,
) -> Result<WorkbenchScope, OpError> {
    match args.value("scope").map(str::trim) {
        None => Ok(WorkbenchScope::Session(session_id.to_owned())),
        Some("project") => cwd
            .and_then(WorkbenchScope::project_for_path)
            .ok_or_else(|| {
                OpError::InvalidArguments(
                    "--scope=project: kein Projekt aus dem Arbeitsverzeichnis ableitbar \
                     (project:<slug> angeben)"
                        .to_owned(),
                )
            }),
        Some(raw) => WorkbenchScope::parse(raw, session_id).map_err(map_knowledge_error),
    }
}

/// `true`, wenn `token` eine Notiz-Auswahl ist (`#<n>`, `<n>`, Zeitstempel).
fn is_note_selector(token: &str) -> bool {
    let number = token.strip_prefix('#').unwrap_or(token);
    (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        || token.parse::<jiff::Timestamp>().is_ok()
}

/// Erkennt `edit <n> <text…>` bzw. `rm <n>` im Rest von `note`.
fn note_edit_selector(tail: &[String]) -> Option<&str> {
    let (action, selector) = (tail.first()?, tail.get(1)?);
    let shape_ok = match action.as_str() {
        "edit" => tail.len() > 2,
        "rm" => tail.len() == 2,
        _ => false,
    };
    (shape_ok && is_note_selector(selector)).then_some(selector.as_str())
}

/// `note edit <n> <text>` / `note rm <n>`.
fn run_note_edit(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    tail: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let selector =
        note_edit_selector(tail).ok_or_else(|| usage("/workbench note edit|rm <n> [text]"))?;
    if tail.first().is_some_and(|action| action == "rm") {
        let removed = workbench::remove_note(store, scope, author, selector, now)
            .map_err(map_knowledge_error)?;
        return Ok(OpOutput::from(format!(
            "Notiz #{} ({}) gelöscht.",
            removed.number, removed.recorded_at
        )));
    }
    let text = tail.get(2..).unwrap_or_default().join(" ");
    let edited = workbench::edit_note(store, scope, author, selector, &text, now)
        .map_err(map_knowledge_error)?;
    Ok(OpOutput::from(format!(
        "Notiz #{} ({}) geändert.",
        edited.number, edited.recorded_at
    )))
}

/// `retention [keep|<tage>d]`.
fn run_retention(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    tail: &[String],
) -> Result<OpOutput, OpError> {
    match tail {
        [] => {
            let policy = workbench::retention(store, scope).map_err(map_knowledge_error)?;
            Ok(OpOutput::from(format!(
                "Aufbewahrung {}: {}",
                scope.path_component(),
                policy.label()
            )))
        }
        [raw] => {
            let policy = Retention::parse(raw).map_err(map_knowledge_error)?;
            workbench::set_retention(store, scope, policy).map_err(map_knowledge_error)?;
            Ok(OpOutput::from(format!(
                "Aufbewahrung {} gesetzt: {}",
                scope.path_component(),
                policy.label()
            )))
        }
        _ => Err(usage("/workbench retention [keep|<tage>d]")),
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
fn panel_data(bench: &Workbench, retention: Retention) -> serde_json::Value {
    let pinned: Vec<serde_json::Value> = bench
        .pinned
        .iter()
        .map(|entry| {
            let inspection = workbench::inspect_pin(entry, PIN_PREVIEW_LINES);
            serde_json::json!({
                "path": entry.absolute_path,
                "note": entry.note,
                "status": inspection.state.label(),
                "pinned_at": entry.pinned_at.map(|at| at.to_string()),
                "preview": inspection.preview,
            })
        })
        .collect();
    let notes: Vec<serde_json::Value> = bench
        .note_entries()
        .into_iter()
        .map(|note| {
            serde_json::json!({
                "id": format!("#{}", note.number),
                "at": note.recorded_at,
                "text": note.text,
            })
        })
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
        "notes": notes,
        "notes_tail": bench.notes_tail(NOTES_TAIL_LINES),
        "retention": retention.label(),
    })
}

/// Rendert das Panel (§5.2) als Text plus [`panel_data`].
fn render(bench: &Workbench, retention: Retention) -> Result<OpOutput, OpError> {
    let data = Some(panel_data(bench, retention));
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
        let marker = match workbench::inspect_pin(entry, 0).state {
            PinState::Present => "",
            PinState::Changed => " [geändert]",
            PinState::Missing => " [fehlt]",
            PinState::NotAFile => " [keine Datei]",
        };
        if entry.note.is_empty() {
            out.push_str(&format!("  · {}{marker}\n", entry.absolute_path));
        } else {
            out.push_str(&format!(
                "  · {} — {}{marker}\n",
                entry.absolute_path, entry.note
            ));
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

#[cfg(test)]
mod tests {
    use super::{WorkbenchArgs, run_workbench};
    use crate::knowledge_test_support::temporary_store;
    use crate::test_support::ctx as ctx_err;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_knowledge::{AgentId, KnowledgeStore};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpError, Operation};

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
        run(&store, &["--scope", "project:harw", "note", "zwei"]).map_err(ctx("note 2"))?;
        let session = run(&store, &[]).map_err(ctx("session show"))?;
        assert!(session.ends_with("ist leer."));
        let project = run(&store, &["--scope=project:harw"]).map_err(ctx("project show"))?;
        assert!(project.contains("geteilt"));
        assert!(project.contains("zwei"));
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
    fn notes_can_be_edited_and_removed() -> TestResult {
        let store = temporary_store("note-edit")?;
        run(&store, &["note", "erste"]).map_err(ctx("note 1"))?;
        run(&store, &["note", "zweite"]).map_err(ctx("note 2"))?;
        let edited = run(&store, &["note", "edit", "#2", "zweite", "neu"]).map_err(ctx("edit"))?;
        assert!(edited.starts_with("Notiz #2"), "{edited}");
        let removed = run(&store, &["note", "rm", "1"]).map_err(ctx("rm"))?;
        assert!(removed.contains("gelöscht"), "{removed}");
        // Keine Auswahl an zweiter Stelle: eine ganz normale Notiz.
        run(&store, &["note", "rm", "den", "Cache"]).map_err(ctx("plain note"))?;
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
        let notes = data["notes"]
            .as_array()
            .ok_or(TestError::Missing("notes"))?;
        assert_eq!(notes.len(), 2, "{data}");
        assert_eq!(notes[0]["id"], "#1");
        assert_eq!(notes[0]["text"], "zweite neu");
        assert_eq!(notes[1]["text"], "rm den Cache");
        match run(&store, &["note", "rm", "#9"]) {
            Err(OpError::InvalidArguments(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "unknown note must be InvalidArguments, got {other:?}"
                )));
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn show_reports_pin_status_and_preview() -> TestResult {
        let store = temporary_store("pin-status")?;
        let file = store.root().join("vorschau.txt");
        std::fs::write(&file, "zeile eins\nzeile zwei\n").map_err(ctx("write file"))?;
        let path = file.to_string_lossy().into_owned();
        run(&store, &["pin", &path]).map_err(ctx("pin existing"))?;
        run(&store, &["pin", "/gibt/es/nicht.rs"]).map_err(ctx("pin missing"))?;
        let output = run_workbench(
            &store,
            "s-1",
            &AgentId::new("operator"),
            &toks(&["show"]),
            jiff::Timestamp::now(),
            None,
        )
        .map_err(ctx("show"))?;
        assert!(output.text.contains("nicht.rs [fehlt]"), "{}", output.text);
        let data = output.data.ok_or(TestError::Missing("show data"))?;
        assert_eq!(data["pinned"][0]["status"], "present", "{data}");
        assert_eq!(data["pinned"][0]["preview"][0], "zeile eins");
        assert!(data["pinned"][0]["pinned_at"].is_string());
        assert_eq!(data["pinned"][1]["status"], "missing");
        assert_eq!(data["retention"], "14d");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn project_shorthand_uses_the_working_directory_and_retention_round_trips() -> TestResult {
        let store = temporary_store("project-shorthand")?;
        run(&store, &["--scope=project", "note", "im", "Projekt"]).map_err(ctx("note"))?;
        let explicit = run(&store, &["--scope=project:project"]).map_err(ctx("show"))?;
        assert!(explicit.contains("im Projekt"), "{explicit}");
        let shown = run(&store, &["--scope=project", "retention"]).map_err(ctx("retention"))?;
        assert!(shown.ends_with("keep"), "{shown}");
        run(&store, &["retention", "3d"]).map_err(ctx("set retention"))?;
        let session = run(&store, &["retention"]).map_err(ctx("show retention"))?;
        assert!(session.ends_with("3d"), "{session}");
        for tokens in [vec!["retention", "bald"], vec!["retention", "1d", "2d"]] {
            if !matches!(run(&store, &tokens), Err(OpError::InvalidArguments(_))) {
                return Err(TestError::Unexpected(format!("{tokens:?}")));
            }
        }
        let no_cwd = run_workbench(
            &store,
            "s-1",
            &AgentId::new("operator"),
            &toks(&["--scope=project"]),
            jiff::Timestamp::now(),
            None,
        );
        assert!(matches!(no_cwd, Err(OpError::InvalidArguments(_))));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    /// Die Op-Hülle meldet Schreibvorgänge über den Hub im Kontext, reine
    /// Lesevorgänge nicht.
    #[tokio::test]
    async fn writes_publish_a_knowledge_event_reads_do_not() -> TestResult {
        use harw_core::agent_events::{AgentEventHub, AgentEventKind};
        use std::sync::Arc;

        let store = temporary_store("event")?;
        let hub = Arc::new(AgentEventHub::new(8));
        let mut rx = hub.subscribe();
        let mut services = harw_operations::context::ServiceMap::new();
        services.insert(Arc::new(store.clone()));
        services.insert(Arc::clone(&hub));
        let ctx = crate::knowledge_test_support::op_context(services)?;

        super::workbench(
            &ctx,
            WorkbenchArgs {
                tokens: toks(&["show"]),
            },
        )
        .await
        .map_err(ctx_err("show"))?;
        assert!(rx.try_recv().is_err(), "show must not publish");

        super::workbench(
            &ctx,
            WorkbenchArgs {
                tokens: toks(&["note", "hallo"]),
            },
        )
        .await
        .map_err(ctx_err("note"))?;
        let event = rx.try_recv().map_err(ctx_err("note publishes"))?;
        match event.kind {
            AgentEventKind::Knowledge { area, id } => {
                assert_eq!(area, "workbench");
                assert_eq!(id, Some(format!("session:{}", ctx.session_id().as_str())));
            }
            other => {
                return Err(TestError::Unexpected(format!("{other:?}")));
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }
}
