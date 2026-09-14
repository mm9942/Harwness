//! Durable transcript discovery and session selection for the CLI.
//!
//! The pure discovery and selector helpers are reusable by chat startup and an
//! in-TUI `/resume` handoff. Terminal prompting stays here: callers outside the
//! CLI receive only session IDs or selectors, never stdin/stdout ownership.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use harw_session_store::meta::{self, SessionMeta};
use harw_types::SessionId;

/// A durable transcript discovered beneath a sessions directory.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredSession {
    /// Validated session ID derived from the transcript filename stem.
    pub id: SessionId,
    /// Full path to the regular `<session-id>.jsonl` transcript file.
    pub path: PathBuf,
    /// Filesystem modification time used for newest-first ordering.
    pub modified_at: SystemTime,
    /// Sitzungs-Metadaten-Sidecar (Schritt 7, `harw_session_store::meta`).
    ///
    /// `None`, wenn weder ein gültiger Sidecar existiert noch aus dem
    /// Transcript abgeleitet werden konnte (z. B. defekter Datensatz); die
    /// Session wird in diesem Fall trotzdem gelistet, nur ohne Titel,
    /// Projekt-Zuordnung oder Turn-Zahl (siehe [`discover_sessions`]).
    pub meta: Option<SessionMeta>,
}

/// Errors produced while discovering or selecting durable transcripts.
#[derive(Debug)]
pub enum ResumeError {
    /// A filesystem operation failed while inspecting the sessions directory.
    Io {
        action: &'static str,
        source: std::io::Error,
    },
    /// A regular JSONL filename cannot safely become a session ID.
    InvalidSessionFilename { path: PathBuf },
    /// An explicit selector was blank after trimming surrounding whitespace.
    EmptySelector,
    /// A selector did not identify any discovered session.
    UnknownSelector { selector: String },
    /// A prefix selector identified more than one discovered session.
    AmbiguousSelector {
        selector: String,
        candidates: Vec<SessionId>,
    },
    /// An interactive picker cannot select from an empty discovery result.
    NoSessions,
    /// An interactive numeric selection was outside the displayed range.
    InvalidSelection { selection: String },
}

impl fmt::Display for ResumeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { action, source } => write!(formatter, "{action}: {source}"),
            Self::InvalidSessionFilename { path } => {
                write!(
                    formatter,
                    "ungültiger Session-Dateiname: {}",
                    path.display()
                )
            }
            Self::EmptySelector => write!(formatter, "Session-Auswahl darf nicht leer sein"),
            Self::UnknownSelector { selector } => {
                write!(formatter, "unbekannte Session-Auswahl: {selector}")
            }
            Self::AmbiguousSelector {
                selector,
                candidates,
            } => write!(
                formatter,
                "mehrdeutige Session-Auswahl '{selector}': {}",
                candidates
                    .iter()
                    .map(SessionId::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::NoSessions => write!(formatter, "keine dauerhaften Sessions gefunden"),
            Self::InvalidSelection { selection } => {
                write!(formatter, "ungültige Session-Nummer: {selection}")
            }
        }
    }
}

impl Error for ResumeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidSessionFilename { .. }
            | Self::EmptySelector
            | Self::UnknownSelector { .. }
            | Self::AmbiguousSelector { .. }
            | Self::NoSessions
            | Self::InvalidSelection { .. } => None,
        }
    }
}

/// Result type for durable transcript discovery and session selection.
pub type ResumeResult<T> = Result<T, ResumeError>;

/// Lists regular `.jsonl` transcripts in deterministic newest-first order.
///
/// Each filename stem must pass [`SessionId`] validation. A malformed regular
/// JSONL candidate fails closed rather than being silently omitted, so a
/// transcript directory cannot present a partial or misleading resume list.
pub fn discover_sessions(sessions_dir: &Path) -> ResumeResult<Vec<DiscoveredSession>> {
    let entries = fs::read_dir(sessions_dir).map_err(|source| ResumeError::Io {
        action: "Sessions-Verzeichnis lesen",
        source,
    })?;

    let mut sessions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ResumeError::Io {
            action: "Sessions-Verzeichniseintrag lesen",
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| ResumeError::Io {
            action: "Dateityp der Session prüfen",
            source,
        })?;
        let file_type = metadata.file_type();
        if !file_type.is_file() {
            continue;
        }

        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }

        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| ResumeError::InvalidSessionFilename { path: path.clone() })?;
        let id = SessionId::try_from(stem.to_owned())
            .map_err(|_| ResumeError::InvalidSessionFilename { path: path.clone() })?;
        let modified_at = metadata.modified().map_err(|source| ResumeError::Io {
            action: "Session-Änderungszeit lesen",
            source,
        })?;

        // Ein fehlender oder defekter Sidecar darf die Session nicht aus der
        // Liste werfen (Schritt 7: "Fehler → warn! und Eintrag trotzdem
        // listen") — nur Titel, Projekt-Zuordnung und Turn-Zahl fehlen dann.
        let meta = match meta::load_or_derive(sessions_dir, &id) {
            Ok(meta) => Some(meta),
            Err(error) => {
                tracing::warn!(
                    session = %id,
                    %error,
                    "resume: Sitzungs-Metadaten konnten nicht geladen/abgeleitet werden"
                );
                None
            }
        };

        sessions.push(DiscoveredSession {
            id,
            path,
            modified_at,
            meta,
        });
    }

    sort_sessions(&mut sessions);
    Ok(sessions)
}

/// Resolves an explicit full ID or unique ID prefix without terminal I/O.
///
/// Exact ID matches take precedence over prefix matching. Empty, unknown, and
/// ambiguous selectors all fail closed.
pub fn resolve_session_selector(
    sessions: &[DiscoveredSession],
    selector: &str,
) -> ResumeResult<SessionId> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(ResumeError::EmptySelector);
    }

    if let Some(session) = sessions
        .iter()
        .find(|session| session.id.as_str() == selector)
    {
        return Ok(session.id.clone());
    }

    let matches = sessions
        .iter()
        .filter(|session| session.id.as_str().starts_with(selector))
        .map(|session| session.id.clone())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(ResumeError::UnknownSelector {
            selector: selector.to_owned(),
        }),
        [session] => Ok(session.clone()),
        _ => Err(ResumeError::AmbiguousSelector {
            selector: selector.to_owned(),
            candidates: matches,
        }),
    }
}

/// Discovers durable sessions then resolves an explicit selector.
///
/// Production resume paths retain the discovered list for interactive display
/// or later `/resume` selections, so they call the two primitives separately.
/// This composition is kept only for source-local coverage of that boundary.
#[cfg(test)]
fn discover_and_resolve_session(sessions_dir: &Path, selector: &str) -> ResumeResult<SessionId> {
    let sessions = discover_sessions(sessions_dir)?;
    resolve_session_selector(&sessions, selector)
}

/// Renders a numbered session list and reads one number or ID selector.
///
/// A blank line and EOF both cancel cleanly (`Ok(None)`). The input-independent
/// decision logic lives in [`resolve_interactive_selection`] for focused tests
/// and for alternate CLI front ends.
///
/// # Rolle seit Schritt 7
/// `harw -r` ohne Wert nutzt für ein vorhandenes Terminal den TUI-Picker
/// (`harw_tui::runtime_root::session_entries` über
/// `harw_tui::session_picker::SessionPicker`, verdrahtet in `chat.rs`).
/// Diese Funktion bleibt nur noch der Fallback,
/// wenn `stdin`/`stdout` kein Terminal sind (z. B. Pipes, nicht-interaktive
/// Tests) — dort ist kein Vollbild-Picker möglich.
pub fn prompt_for_session<R: BufRead, W: Write>(
    sessions: &[DiscoveredSession],
    input: &mut R,
    output: &mut W,
) -> ResumeResult<Option<SessionId>> {
    if sessions.is_empty() {
        return Err(ResumeError::NoSessions);
    }

    writeln!(output, "Verfügbare Sessions:").map_err(output_error)?;
    for (index, session) in sessions.iter().enumerate() {
        writeln!(output, "  {}. {}", index + 1, session.id).map_err(output_error)?;
    }
    write!(
        output,
        "Session auswählen (Nummer oder ID, leer = Abbruch): "
    )
    .map_err(output_error)?;
    output.flush().map_err(output_error)?;

    let mut line = String::new();
    let bytes_read = input
        .read_line(&mut line)
        .map_err(|source| ResumeError::Io {
            action: "Session-Auswahl von stdin lesen",
            source,
        })?;
    if bytes_read == 0 {
        return Ok(None);
    }

    resolve_interactive_selection(sessions, &line)
}

/// Resolves a picker line without performing terminal I/O.
///
/// Blank input cancels. A positive number selects its displayed one-based
/// index; every other non-empty value is handled as an exact/unique prefix.
pub fn resolve_interactive_selection(
    sessions: &[DiscoveredSession],
    input: &str,
) -> ResumeResult<Option<SessionId>> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }

    if input.bytes().all(|byte| byte.is_ascii_digit()) {
        let index = input
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .filter(|&index| index < sessions.len())
            .ok_or_else(|| ResumeError::InvalidSelection {
                selection: input.to_owned(),
            })?;
        return Ok(Some(sessions[index].id.clone()));
    }

    resolve_session_selector(sessions, input).map(Some)
}

fn sort_sessions(sessions: &mut [DiscoveredSession]) {
    sessions.sort_by(|left, right| {
        effective_last_active(right)
            .cmp(&effective_last_active(left))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
}

/// Der für Sortierung und Anzeige maßgebliche "zuletzt aktiv"-Zeitpunkt: der
/// Sidecar-Wert `meta.last_opened_at`, falls vorhanden, sonst die
/// Transcript-`mtime` (Schritt 7: "Sortierung neu: nach
/// `meta.last_opened_at`, Fallback mtime").
fn effective_last_active(session: &DiscoveredSession) -> SystemTime {
    session
        .meta
        .as_ref()
        .map(|meta| SystemTime::from(meta.last_opened_at))
        .unwrap_or(session.modified_at)
}

/// Prüft, ob eine entdeckte Session zum aktuellen Projekt gehört.
///
/// # Beschreibung
/// Vergleicht `session.meta.project_key` mit `current_project_key`; beide
/// `None` gelten als Treffer (eine Session ohne Projekt-Zuordnung erscheint
/// dann nur, wenn auch das aktuelle Arbeitsverzeichnis keinem Projekt
/// zugeordnet werden konnte). Wird von `ProfileResumeSelector::available_sessions`
/// (`chat.rs`) sowie vom Non-TTY-Fallback in `chat.rs` verwendet, damit beide
/// Auswahlwege denselben Projektfilter anwenden (Contract §4: "`harw -r`
/// zeigt standardmäßig die Sessions des aktuellen Projekts").
#[must_use]
pub fn session_matches_project(session: &DiscoveredSession, current_project_key: Option<&str>) -> bool {
    session
        .meta
        .as_ref()
        .and_then(|meta| meta.project_key.as_deref())
        == current_project_key
}

fn output_error(source: std::io::Error) -> ResumeError {
    ResumeError::Io {
        action: "Session-Auswahl nach stdout schreiben",
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;

    fn session(id: &str, modified_after_epoch: u64) -> DiscoveredSession {
        DiscoveredSession {
            id: SessionId::try_from(id.to_owned()).expect("test session ID is valid"),
            path: PathBuf::from(format!("{id}.jsonl")),
            modified_at: SystemTime::UNIX_EPOCH + Duration::from_secs(modified_after_epoch),
            meta: None,
        }
    }

    fn session_with_meta(id: &str, modified_after_epoch: u64, meta: SessionMeta) -> DiscoveredSession {
        DiscoveredSession {
            meta: Some(meta),
            ..session(id, modified_after_epoch)
        }
    }

    fn fresh_meta(id: &str) -> SessionMeta {
        meta_at(id, jiff::Timestamp::now())
    }

    fn meta_at(id: &str, last_opened_at: jiff::Timestamp) -> SessionMeta {
        SessionMeta {
            version: harw_session_store::meta::SESSION_META_VERSION,
            session_id: SessionId::try_from(id.to_owned()).expect("test session ID is valid"),
            title: None,
            title_source: harw_session_store::meta::TitleSource::None,
            created_at: last_opened_at,
            last_opened_at,
            cwd: None,
            project_root: None,
            project_key: None,
            first_user_message: None,
            turns: 0,
        }
    }

    #[test]
    fn discovery_lists_only_regular_jsonl_files_and_sorts_deterministically() {
        let directory = tempfile::tempdir().expect("temporary sessions directory");
        File::create(directory.path().join("first.jsonl")).expect("first transcript");
        File::create(directory.path().join("second.jsonl")).expect("second transcript");
        File::create(directory.path().join("ignored.txt")).expect("unrelated file");
        fs::create_dir(directory.path().join("directory.jsonl")).expect("jsonl-looking directory");

        let discovered = discover_sessions(directory.path()).expect("discover transcripts");
        assert_eq!(discovered.len(), 2);
        assert!(discovered.iter().all(|item| item.path.is_file()));
        assert!(
            discovered
                .iter()
                .all(|item| item.path.extension() == Some("jsonl".as_ref()))
        );

        let mut tied = vec![
            session("zeta", 42),
            session("alpha", 42),
            session("newest", 43),
        ];
        sort_sessions(&mut tied);
        assert_eq!(
            tied.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["newest", "alpha", "zeta"]
        );
    }

    #[test]
    fn discovery_rejects_malformed_jsonl_filename_stems() {
        let directory = tempfile::tempdir().expect("temporary sessions directory");
        File::create(directory.path().join("   .jsonl")).expect("malformed transcript name");

        let error = discover_sessions(directory.path()).expect_err("blank stem must fail closed");
        assert!(matches!(error, ResumeError::InvalidSessionFilename { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn discovery_ignores_symlinked_jsonl_transcripts() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("temporary sessions directory");
        let sessions_dir = directory.path().join("sessions");
        fs::create_dir(&sessions_dir).expect("sessions directory");
        let target = directory.path().join("target.jsonl");
        File::create(&target).expect("transcript target");
        symlink(&target, sessions_dir.join("linked-111.jsonl")).expect("symlink transcript");

        assert!(
            discover_sessions(&sessions_dir)
                .expect("discover transcripts")
                .is_empty()
        );
    }

    #[test]
    fn discovery_and_resolution_selects_a_unique_prefix() {
        let directory = tempfile::tempdir().expect("temporary sessions directory");
        File::create(directory.path().join("alpha-111.jsonl")).expect("first transcript");
        File::create(directory.path().join("bravo-222.jsonl")).expect("second transcript");

        assert_eq!(
            discover_and_resolve_session(directory.path(), "brav")
                .expect("unique transcript prefix resolves"),
            SessionId::try_from("bravo-222".to_owned()).expect("test session ID is valid")
        );
    }

    #[test]
    fn selector_accepts_exact_ids_and_unique_prefixes() {
        let sessions = vec![session("alpha-111", 2), session("bravo-222", 1)];

        assert_eq!(
            resolve_session_selector(&sessions, "alpha-111").expect("exact match"),
            sessions[0].id
        );
        assert_eq!(
            resolve_session_selector(&sessions, "brav").expect("unique prefix"),
            sessions[1].id
        );
    }

    #[test]
    fn selector_fails_closed_for_empty_unknown_and_ambiguous_values() {
        let sessions = vec![session("alpha-111", 2), session("alpha-222", 1)];

        assert!(matches!(
            resolve_session_selector(&sessions, "  "),
            Err(ResumeError::EmptySelector)
        ));
        assert!(matches!(
            resolve_session_selector(&sessions, "missing"),
            Err(ResumeError::UnknownSelector { .. })
        ));
        assert!(matches!(
            resolve_session_selector(&sessions, "alpha"),
            Err(ResumeError::AmbiguousSelector { .. })
        ));
    }

    #[test]
    fn interactive_selection_cancels_on_blank_input() {
        let sessions = vec![session("alpha-111", 1)];
        assert_eq!(
            resolve_interactive_selection(&sessions, " \n").expect("blank is cancellation"),
            None
        );
    }

    #[test]
    fn prompt_cancels_cleanly_at_eof() {
        let sessions = vec![session("alpha-111", 1)];
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        assert_eq!(
            prompt_for_session(&sessions, &mut input, &mut output).expect("EOF is cancellation"),
            None
        );
        assert!(
            String::from_utf8(output)
                .expect("picker output is utf-8")
                .contains("alpha-111")
        );
    }

    // -- Schritt 7: Sortierung nach `meta.last_opened_at` ----------------

    #[test]
    fn sort_prefers_meta_last_opened_at_over_stale_mtime() {
        // `older` hat die neuere `mtime`, aber der Sidecar sagt, sie wurde
        // vor langer Zeit zuletzt geöffnet — `newer` ist im Sidecar frischer,
        // trotz älterer `mtime`. Die Sortierung muss dem Sidecar folgen.
        let long_ago = jiff::Timestamp::UNIX_EPOCH;
        let just_now = jiff::Timestamp::now();
        let older = session_with_meta("older-by-meta", 1_000, meta_at("older-by-meta", long_ago));
        let newer = session_with_meta("newer-by-meta", 10, meta_at("newer-by-meta", just_now));

        let mut sessions = vec![older, newer];
        sort_sessions(&mut sessions);

        assert_eq!(sessions[0].id.as_str(), "newer-by-meta");
        assert_eq!(sessions[1].id.as_str(), "older-by-meta");
    }

    #[test]
    fn sort_falls_back_to_mtime_without_meta() {
        let mut sessions = vec![session("old", 10), session("new", 20)];
        sort_sessions(&mut sessions);

        assert_eq!(sessions[0].id.as_str(), "new");
        assert_eq!(sessions[1].id.as_str(), "old");
    }

    // -- Schritt 7: Projektfilter -----------------------------------------
    //
    // `session_entries`/`session_entry` samt `~`-Pfadkürzung wurden entfernt
    // (Duplikat ohne Aufrufer, siehe Modul-Kommentar zu
    // [`session_matches_project`]); der Projektfilter selbst bleibt, da ihn
    // `ProfileResumeSelector::available_sessions` (`chat.rs`) weiterhin nutzt.

    #[test]
    fn session_matches_project_treats_both_none_as_a_match() {
        let session_without_project = session_with_meta("x", 1, fresh_meta("x"));
        assert!(session_matches_project(&session_without_project, None));
        assert!(!session_matches_project(
            &session_without_project,
            Some("harwness-abc123")
        ));
    }
}
