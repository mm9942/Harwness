//! `harw knowledge index|memory|diary|proposals` für das aktive lokale Profil.
//!
//! `index` baut bzw. prüft den Wissensindex. Die anderen Unterbefehle
//! verwenden dieselben Operationen wie `/memory`, `/context-proposal`
//! und `/diary`.

use std::io::Read;

use harw_ops::diary::MAX_DIARY_NOTE_BYTES;

use crate::cli::{DiaryAction, GlobalArgs, KnowledgeAction};
use crate::jobs_cmd::{run_and_print, working_dir};
use crate::op_bridge::{OpTarget, run_operation};
use crate::output::Printer;

/// Führt `harw knowledge …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn `--json` für `index` verlangt wird (nur
/// Textausgabe), der Indexbau bzw. die Operation scheitert oder die Ausgabe
/// fehlschlägt.
pub fn run(g: &GlobalArgs, a: KnowledgeAction) -> Result<(), String> {
    match a {
        KnowledgeAction::Index { action } => {
            Printer::new(g.output()).require_text("harw knowledge index")?;
            crate::lens::run(g.home.clone(), action)
        }
        KnowledgeAction::Memory { args } => run_and_print(g, "/memory", args),
        KnowledgeAction::Diary { action } => run_diary(g, action),
        KnowledgeAction::Proposals { args } => run_and_print(g, "/context-proposal", args),
    }
}

fn run_diary(g: &GlobalArgs, action: DiaryAction) -> Result<(), String> {
    let mut args = Vec::new();
    match action {
        DiaryAction::Today => args.push("today".to_owned()),
        DiaryAction::Show {
            agent,
            date,
            from,
            to,
        } => {
            args.push("show".to_owned());
            add_flag(&mut args, "agent", agent);
            add_flag(&mut args, "date", date);
            add_flag(&mut args, "from", from);
            add_flag(&mut args, "to", to);
        }
        DiaryAction::Search {
            text,
            agent,
            from,
            to,
        } => {
            args.push("search".to_owned());
            add_flag(&mut args, "query", Some(text));
            add_flag(&mut args, "agent", agent);
            add_flag(&mut args, "from", from);
            add_flag(&mut args, "to", to);
        }
        DiaryAction::Agents => args.push("agents".to_owned()),
        DiaryAction::Note => {
            args.push("note".to_owned());
            args.push(read_note(std::io::stdin().lock())?);
        }
    }

    let cwd = working_dir(g)?;
    let mut output = run_operation(
        OpTarget {
            home: g.home.clone(),
            cwd: &cwd,
        },
        "/diary",
        args,
    )
    .map_err(plain_terminal_text)?;
    // Diary bodies can contain arbitrary stored text. Only plain text is
    // rendered directly; structured data stays unchanged for JSON clients.
    output.text = plain_terminal_text(output.text);
    Printer::new(g.output()).op_output(&output)
}

fn plain_terminal_text(mut text: String) -> String {
    text.retain(|c| !c.is_control() || matches!(c, '\n' | '\t'));
    text
}

fn add_flag(args: &mut Vec<String>, name: &str, value: Option<String>) {
    if let Some(value) = value {
        args.push(format!("--{name}={value}"));
    }
}

fn read_note(input: impl Read) -> Result<String, String> {
    let mut bytes = Vec::new();
    input
        .take((MAX_DIARY_NOTE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Tagebuchnotiz konnte nicht von stdin gelesen werden: {error}"))?;
    if bytes.len() > MAX_DIARY_NOTE_BYTES {
        return Err(format!("Tagebuchnotiz ist länger als {MAX_DIARY_NOTE_BYTES} Bytes"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "Tagebuchnotiz muss UTF-8 sein".to_owned())?;
    if text.trim().is_empty()
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("Tagebuchnotiz muss nichtleeren Text ohne Steuerzeichen enthalten".to_owned());
    }
    Ok(text)
}
