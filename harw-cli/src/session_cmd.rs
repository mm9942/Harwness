//! `harw session list|show|resume`: gespeicherte Sitzungen auflisten,
//! einzeln anzeigen und zum Fortsetzen auswählen.
//!
//! Nutzt dieselbe Sitzungssuche wie `harw -r` ([`crate::resume`]), damit
//! Liste, Detailansicht und Fortsetzen dieselben Sitzungen und denselben
//! Projektfilter sehen.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cli::{GlobalArgs, SessionAction};
use crate::output::Printer;
use crate::resume::{
    DiscoveredSession, discover_sessions, resolve_session_selector, session_matches_project,
};

/// Platzhalter für fehlende Angaben in Tabelle und Detailansicht.
const MISSING: &str = "–";

/// Höchstlänge eines aus der ersten Nachricht abgeleiteten Titels.
const MAX_TITLE_CHARS: usize = 60;

/// Führt `harw session …` aus.
///
/// # Returns
/// `Ok(Some(id))` bei `resume`: der Aufrufer startet danach den Chat mit
/// dieser Sitzung. `Ok(None)` bei `list`/`show` (Ausgabe bereits erfolgt).
///
/// # Errors
/// Ein deutscher Fehlertext, wenn das Home-/Sitzungsverzeichnis nicht
/// auflösbar ist, die Sitzungssuche scheitert, eine Auswahl unbekannt oder
/// mehrdeutig ist oder die Ausgabe fehlschlägt.
pub fn run(g: &GlobalArgs, a: SessionAction) -> Result<Option<String>, String> {
    let home = crate::home::resolve_home(g.home.clone())?;
    let sessions_root = crate::runtime_entry::profile_sessions_root(&home)?;
    let printer = Printer::new(g.output());
    match a {
        SessionAction::List { all } => {
            let cwd = working_dir(g)?;
            list(&printer, g.json, &sessions_root, &cwd, all)?;
            Ok(None)
        }
        SessionAction::Show { id } => {
            show(&printer, &sessions_root, &id)?;
            Ok(None)
        }
        SessionAction::Resume { id } => {
            let sessions = load_sessions(&sessions_root)?;
            let resolved = resolve_session_selector(&sessions, &id)
                .map_err(|error| format!("Sitzung kann nicht fortgesetzt werden: {error}"))?;
            Ok(Some(resolved.as_str().to_owned()))
        }
    }
}

/// Arbeitsverzeichnis für den Projektfilter: `-C/--cwd`, sonst das
/// aktuelle Prozessverzeichnis.
fn working_dir(g: &GlobalArgs) -> Result<PathBuf, String> {
    match &g.cwd {
        Some(dir) => Ok(dir.clone()),
        None => std::env::current_dir().map_err(|error| {
            format!("Aktuelles Arbeitsverzeichnis nicht ermittelbar: {error}")
        }),
    }
}

fn load_sessions(sessions_root: &Path) -> Result<Vec<DiscoveredSession>, String> {
    discover_sessions(sessions_root)
        .map_err(|error| format!("Sitzungen konnten nicht gelesen werden: {error}"))
}

/// Projekt-Schlüssel des Arbeitsverzeichnisses; `None`, wenn kein Projekt
/// erkannt wird (dann gelten nur Sitzungen ohne Projekt als passend).
fn current_project_key(cwd: &Path) -> Option<String> {
    match harw_home::project::discover_project(cwd, &[]) {
        Ok(project) => Some(harw_home::project::project_key(&project.root)),
        Err(error) => {
            tracing::debug!(%error, "session list: kein Projekt erkannt");
            None
        }
    }
}

fn list(
    printer: &Printer,
    json: bool,
    sessions_root: &Path,
    cwd: &Path,
    all: bool,
) -> Result<(), String> {
    let project_key = current_project_key(cwd);
    let rows: Vec<Vec<String>> = load_sessions(sessions_root)?
        .iter()
        .filter(|session| all || session_matches_project(session, project_key.as_deref()))
        .map(|session| {
            vec![
                session.id.as_str().to_owned(),
                title(session),
                format_time(last_active(session)),
                project(session),
            ]
        })
        .collect();
    if rows.is_empty() && !json {
        if all {
            println!("Keine gespeicherten Sitzungen gefunden.");
        } else {
            println!(
                "Keine Sitzungen für dieses Projekt gefunden. Mit `--all` werden alle Sitzungen angezeigt."
            );
        }
        return Ok(());
    }
    printer.table(&["id", "titel", "zuletzt aktiv", "projekt"], &rows)
}

fn show(printer: &Printer, sessions_root: &Path, selector: &str) -> Result<(), String> {
    let sessions = load_sessions(sessions_root)?;
    let id = resolve_session_selector(&sessions, selector)
        .map_err(|error| format!("Sitzung nicht gefunden: {error}"))?;
    let Some(session) = sessions.iter().find(|session| session.id == id) else {
        return Err(format!("Sitzung nicht gefunden: {}", id.as_str()));
    };
    let meta = session.meta.as_ref();
    let created = meta.map(|meta| format_time(SystemTime::from(meta.created_at)));
    let cwd = meta
        .and_then(|meta| meta.cwd.as_ref())
        .map(|path| path.display().to_string());
    let turns = meta.map(|meta| meta.turns);
    let first_message = meta.and_then(|meta| meta.first_user_message.clone());

    let mut text = String::new();
    push_line(&mut text, "ID", session.id.as_str());
    push_line(&mut text, "Titel", &title(session));
    push_line(&mut text, "Erstellt", created.as_deref().unwrap_or(MISSING));
    push_line(&mut text, "Zuletzt aktiv", &format_time(last_active(session)));
    push_line(&mut text, "Projekt", &project(session));
    push_line(
        &mut text,
        "Arbeitsverzeichnis",
        cwd.as_deref().unwrap_or(MISSING),
    );
    push_line(
        &mut text,
        "Turns",
        &turns.map_or_else(|| MISSING.to_owned(), |turns| turns.to_string()),
    );
    push_line(
        &mut text,
        "Transcript",
        &session.path.display().to_string(),
    );
    if let Some(message) = &first_message {
        push_line(&mut text, "Erste Nachricht", message);
    }
    text.push_str(&format!(
        "\nFortsetzen mit: harw session resume {}",
        session.id.as_str()
    ));

    let json = serde_json::json!({
        "id": session.id.as_str(),
        "title": meta.and_then(|meta| meta.title.clone()),
        "created_at": meta.map(|meta| meta.created_at.to_string()),
        "last_active": format_time(last_active(session)),
        "project_root": meta
            .and_then(|meta| meta.project_root.as_ref())
            .map(|path| path.display().to_string()),
        "project_key": meta.and_then(|meta| meta.project_key.clone()),
        "cwd": cwd,
        "turns": turns,
        "first_user_message": first_message,
        "transcript": session.path.display().to_string(),
    });
    printer.value(&text, json)
}

fn push_line(text: &mut String, label: &str, value: &str) {
    text.push_str(&format!("{label:<20}{value}\n"));
}

/// Titel der Sitzung: gesetzter Titel, sonst die gekürzte erste Nachricht.
fn title(session: &DiscoveredSession) -> String {
    let Some(meta) = session.meta.as_ref() else {
        return MISSING.to_owned();
    };
    if let Some(title) = meta.title.as_deref().filter(|title| !title.trim().is_empty()) {
        return title.trim().to_owned();
    }
    match meta.first_user_message.as_deref() {
        Some(message) if !message.trim().is_empty() => truncate(message.trim()),
        _ => MISSING.to_owned(),
    }
}

fn truncate(text: &str) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_TITLE_CHARS {
        return single_line;
    }
    let mut short: String = single_line.chars().take(MAX_TITLE_CHARS - 1).collect();
    short.push('…');
    short
}

fn project(session: &DiscoveredSession) -> String {
    let Some(meta) = session.meta.as_ref() else {
        return MISSING.to_owned();
    };
    if let Some(root) = &meta.project_root {
        return root.display().to_string();
    }
    meta.project_key
        .clone()
        .unwrap_or_else(|| MISSING.to_owned())
}

/// "Zuletzt aktiv": Metadaten-Zeitpunkt, sonst Änderungszeit der Datei.
fn last_active(session: &DiscoveredSession) -> SystemTime {
    session
        .meta
        .as_ref()
        .map(|meta| SystemTime::from(meta.last_opened_at))
        .unwrap_or(session.modified_at)
}

/// Formatiert einen Zeitpunkt in der lokalen Zeitzone (`JJJJ-MM-TT HH:MM`).
fn format_time(time: SystemTime) -> String {
    match jiff::Timestamp::try_from(time) {
        Ok(timestamp) => timestamp
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => MISSING.to_owned(),
    }
}
