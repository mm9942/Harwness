//! Durable transcript discovery and session selection for the CLI.
//!
//! The pure discovery and selector helpers are reusable by chat startup and an
//! in-TUI `/resume` handoff. Terminal prompting stays here: callers outside the
//! CLI receive only session IDs or selectors, never stdin/stdout ownership.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
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

        let mut session = DiscoveredSession {
            id,
            path,
            modified_at,
            meta,
        };
        backfill_project_key(sessions_dir, &mut session);
        sessions.push(session);
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
/// Vergleicht `session.meta.project_key` mit `current_project_key`. Eine
/// Session **ohne** Projekt-Zuordnung matcht nur, wenn auch das aktuelle
/// Projekt keinen Schlüssel hat (`current_project_key == None`) — andernfalls
/// tauchte jede Alt-Session (vor `tag_session_project` in `chat.rs`, oder vor
/// [`backfill_project_key`]) in jedem Projekt auf, was `harw -r` ohne
/// `--all` faktisch zu einem globalen Picker machte (Bugreport: fremde
/// Projekt-Sessions erschienen ohne `Ctrl+A`). [`discover_sessions`] versucht
/// vorher bereits, fehlende `project_key`s aus dem Transcript nachzutragen
/// ([`backfill_project_key`]); nur echt unbestimmbare Alt-Sessions bleiben
/// hier untagged und sind dann ausschließlich über `--all` erreichbar. Wird
/// von `ProfileResumeSelector::available_sessions` (`chat.rs`) sowie vom
/// Non-TTY-Fallback in `chat.rs` verwendet, damit beide Auswahlwege denselben
/// Projektfilter anwenden (Contract §4: "`harw -r` zeigt standardmäßig die
/// Sessions des aktuellen Projekts").
#[must_use]
pub fn session_matches_project(
    session: &DiscoveredSession,
    current_project_key: Option<&str>,
) -> bool {
    match session
        .meta
        .as_ref()
        .and_then(|meta| meta.project_key.as_deref())
    {
        None => current_project_key.is_none(),
        Some(session_key) => Some(session_key) == current_project_key,
    }
}

/// Obergrenze für den Transcript-Scan in [`backfill_project_key`]: nur der
/// Anfang der Datei wird gelesen, damit ein Backfill-Versuch auf einem
/// riesigen Transcript nicht spürbar Zeit kostet.
const BACKFILL_SCAN_CAP_BYTES: usize = 5 * 1024 * 1024;

/// Höchstzahl der Pfad-Kandidaten (nach Häufigkeit sortiert), für die
/// [`backfill_project_key`] tatsächlich `discover_project` aufruft.
const BACKFILL_MAX_CANDIDATES: usize = 5;

/// Verzeichnispräfixe, die nie als Backfill-Kandidat zählen (temporäre,
/// virtuelle oder systemweite Pfade, nie ein Projekt-Root).
const BACKFILL_IGNORED_PREFIXES: &[&str] = &["/tmp", "/proc", "/dev", "/usr", "/etc"];

/// Prozessweiter Merker bereits versuchter Backfills, damit
/// [`backfill_project_key`] pro Session nur einmal je Prozesslauf einen
/// Transcript-Scan durchführt (siehe Funktionsdoku: `SessionMeta` bekommt
/// bewusst kein neues Feld für einen persistenten Marker).
fn attempted_backfills() -> &'static Mutex<HashSet<SessionId>> {
    static ATTEMPTED: OnceLock<Mutex<HashSet<SessionId>>> = OnceLock::new();
    ATTEMPTED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Markiert `id` als "Backfill versucht"; liefert `true`, wenn dies der
/// erste Versuch in diesem Prozesslauf ist (Aufrufer soll dann scannen).
fn mark_backfill_attempted(id: &SessionId) -> bool {
    let mutex = attempted_backfills();
    let mut attempted = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    attempted.insert(id.clone())
}

/// Trägt best-effort einen fehlenden `meta.project_key` für Alt-Sessions
/// nach, indem absolute Pfade im Transcript auf einen wahrscheinlichen
/// Projekt-Root zurückgeführt werden.
///
/// # Beschreibung
/// Läuft nur, wenn `session.meta` existiert, `project_key` fehlt und dieser
/// Prozess für `session.id` noch keinen Versuch unternommen hat (siehe
/// [`mark_backfill_attempted`]). Liest höchstens
/// [`BACKFILL_SCAN_CAP_BYTES`] des Transcripts und extrahiert absolute
/// Pfad-Kandidaten (siehe [`extract_path_candidates`]); zählt, wie oft jedes
/// übergeordnete Verzeichnis vorkommt, und probiert die bis zu
/// [`BACKFILL_MAX_CANDIDATES`] häufigsten Verzeichnisse — nach Häufigkeit
/// absteigend — mit `harw_home::project::discover_project`. Der erste
/// Treffer, dessen Root weder das Home-Verzeichnis der Nutzerin noch `/` ist,
/// gewinnt und wird über `harw_session_store::meta::set_project`
/// gespeichert; `session.meta` wird danach in-place aktualisiert, damit der
/// aufrufende `discover_sessions`-Lauf den neuen Schlüssel sofort für die
/// Sortierung/Filterung sieht.
///
/// # Fehlerverhalten
/// Jeder Fehler (Transcript nicht lesbar, kein Kandidat gefunden, Speichern
/// schlägt fehl) wird höchstens mit `tracing` geloggt; `discover_sessions`
/// darf nie an einem Backfill-Versuch scheitern.
pub fn backfill_project_key(sessions_dir: &Path, session: &mut DiscoveredSession) {
    let needs_backfill = session
        .meta
        .as_ref()
        .is_some_and(|meta| meta.project_key.is_none());
    if !needs_backfill {
        return;
    }
    if !mark_backfill_attempted(&session.id) {
        return;
    }

    let Some(text) = read_transcript_prefix(&session.path) else {
        return;
    };

    let candidates = rank_directory_candidates(&text);
    let home_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| fs::canonicalize(&home).ok());

    for candidate in candidates.into_iter().take(BACKFILL_MAX_CANDIDATES) {
        let Some(existing_ancestor) = nearest_existing_ancestor(&candidate) else {
            continue;
        };
        let project = match harw_home::project::discover_project(&existing_ancestor, &[]) {
            Ok(project) => project,
            Err(_) => continue,
        };
        if project.root == Path::new("/") {
            continue;
        }
        if home_dir.as_deref() == Some(project.root.as_path()) {
            continue;
        }

        let key = harw_home::project::project_key(&project.root);
        match meta::set_project(
            sessions_dir,
            &session.id,
            None,
            Some(&project.root),
            Some(&key),
        ) {
            Ok(updated_meta) => {
                tracing::debug!(
                    session = %session.id,
                    project = %key,
                    "resume.backfill_project"
                );
                session.meta = Some(updated_meta);
            }
            Err(error) => {
                tracing::warn!(
                    session = %session.id,
                    %error,
                    "resume: Projekt-Backfill konnte nicht gespeichert werden"
                );
            }
        }
        return;
    }
}

/// Liest bis zu [`BACKFILL_SCAN_CAP_BYTES`] am Anfang eines Transcripts als
/// verlustfrei-lossy UTF-8. `None` bei jedem Lesefehler.
fn read_transcript_prefix(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut limited = file.take(BACKFILL_SCAN_CAP_BYTES as u64);
    let mut buffer = Vec::new();
    limited.read_to_end(&mut buffer).ok()?;
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// Extrahiert absolute Pfad-Kandidaten aus Rohtext: Teilstrings, die mit `/`
/// beginnen, mindestens drei nicht-leere Segmente haben und an `"`, `'`,
/// Whitespace, `:` oder `,` enden.
fn extract_path_candidates(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut candidates = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'/' {
            index += 1;
            continue;
        }
        let start = index;
        let mut end = index;
        while end < bytes.len() {
            let byte = bytes[end];
            let is_stop = byte == b'"'
                || byte == b'\''
                || byte == b':'
                || byte == b','
                || byte.is_ascii_whitespace();
            if is_stop {
                break;
            }
            end += 1;
        }
        let candidate = &text[start..end];
        if candidate
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count()
            >= 3
        {
            candidates.push(candidate);
        }
        index = if end > start { end } else { index + 1 };
    }
    candidates
}

/// `true`, wenn `path` unter einem der [`BACKFILL_IGNORED_PREFIXES`] oder
/// unter `$HOME/.harw`/`$HOME/.cargo` liegt und damit nie ein
/// Backfill-Kandidat sein darf.
fn is_ignored_backfill_path(path: &str) -> bool {
    if BACKFILL_IGNORED_PREFIXES
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
    {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let home = home.to_string_lossy().into_owned();
        if path.starts_with(&format!("{home}/.harw")) || path.starts_with(&format!("{home}/.cargo"))
        {
            return true;
        }
    }
    false
}

/// Zählt für jeden extrahierten Pfad-Kandidaten das übergeordnete
/// Verzeichnis und liefert die Verzeichnisse nach Häufigkeit absteigend
/// (bei Gleichstand alphabetisch, für deterministische Reihenfolge).
fn rank_directory_candidates(text: &str) -> Vec<PathBuf> {
    let mut counts: HashMap<PathBuf, usize> = HashMap::new();
    for candidate in extract_path_candidates(text) {
        if is_ignored_backfill_path(candidate) {
            continue;
        }
        let path = Path::new(candidate);
        let directory = path.parent().unwrap_or(path);
        if directory.as_os_str().is_empty() {
            continue;
        }
        *counts.entry(directory.to_path_buf()).or_insert(0) += 1;
    }

    let mut ranked: Vec<(PathBuf, usize)> = counts.into_iter().collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    ranked.into_iter().map(|(path, _)| path).collect()
}

/// Läuft von `path` aufwärts bis zum ersten tatsächlich existierenden
/// Vorfahren (`discover_project` kanonisiert intern und scheitert an einem
/// nicht existierenden Startverzeichnis). `None`, wenn selbst `/` fehlt
/// (praktisch unerreichbar).
fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
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

    fn session_with_meta(
        id: &str,
        modified_after_epoch: u64,
        meta: SessionMeta,
    ) -> DiscoveredSession {
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
            usage_rounds: 0,
            total_usage: harw_types::TokenUsage::default(),
            drift_events: std::collections::BTreeMap::new(),
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
    fn session_matches_project_shows_untagged_sessions_only_without_a_current_project() {
        let session_without_project = session_with_meta("x", 1, fresh_meta("x"));
        assert!(session_matches_project(&session_without_project, None));
        assert!(!session_matches_project(
            &session_without_project,
            Some("harwness-abc123")
        ));
    }

    #[test]
    fn test_session_matches_project_tagged_session_with_different_key_does_not_match() {
        let mut meta = fresh_meta("tagged");
        meta.project_key = Some("harwness-abc123".to_owned());
        let tagged_session = session_with_meta("tagged", 1, meta);

        assert!(!session_matches_project(
            &tagged_session,
            Some("harwness-different")
        ));
        assert!(!session_matches_project(&tagged_session, None));
    }

    #[test]
    fn test_session_matches_project_tagged_session_with_same_key_matches() {
        let mut meta = fresh_meta("tagged");
        meta.project_key = Some("harwness-abc123".to_owned());
        let tagged_session = session_with_meta("tagged", 1, meta);

        assert!(session_matches_project(
            &tagged_session,
            Some("harwness-abc123")
        ));
    }

    // -- Backfill fehlender `project_key`s aus dem Transcript --------------

    #[test]
    fn backfill_project_key_tags_session_from_repeated_project_paths_in_transcript() {
        let sessions_dir = tempfile::tempdir().expect("temporary sessions directory");
        // Nicht unter `/tmp` anlegen: `BACKFILL_IGNORED_PREFIXES` verwirft
        // genau solche Pfade als Kandidaten (Rauschen aus Editor-/Build-Tools),
        // also braucht dieser Test einen Projekt-Root außerhalb davon.
        let project_dir = tempfile::Builder::new()
            .prefix("harw-backfill-project-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .expect("temporary project directory outside /tmp");
        fs::create_dir(project_dir.path().join(".git")).expect("fake git marker");
        let src_dir = project_dir.path().join("harw-core").join("src");
        fs::create_dir_all(&src_dir).expect("project source directory");
        let file_a = src_dir.join("lib.rs");
        let file_b = src_dir.join("state_store.rs");

        let id = SessionId::try_from("backfill-hit".to_owned()).expect("valid session id");
        let store = harw_session_store::store::TranscriptStore::new(sessions_dir.path());
        let thread = harw_types::ThreadRef::from_str("root");
        // Reale Transcript-Datensätze tragen `session_id`/`thread`/`sequence`
        // (`TranscriptRecord`, `deny_unknown_fields`) — von Hand geschriebenes
        // JSON ohne diese Felder scheitert an `meta::load_or_derive` mit
        // "missing field `session_id`". Über `TranscriptStore::append` bleibt
        // die Fixture an das echte Schema gebunden.
        store
            .append(&harw_session_store::record::TranscriptRecord::new(
                id.clone(),
                thread.clone(),
                0,
                jiff::Timestamp::now(),
                harw_session_store::record::RecordKind::Item,
                serde_json::json!({
                    "type": "tool_call",
                    "arguments": { "path": file_a.display().to_string() },
                }),
            ))
            .expect("append fake tool_call record for file_a");
        store
            .append(&harw_session_store::record::TranscriptRecord::new(
                id.clone(),
                thread.clone(),
                1,
                jiff::Timestamp::now(),
                harw_session_store::record::RecordKind::Item,
                serde_json::json!({
                    "type": "tool_result",
                    "output": format!("read {} and {}", file_a.display(), file_b.display()),
                }),
            ))
            .expect("append fake tool_result record referencing both files");
        store
            .append(&harw_session_store::record::TranscriptRecord::new(
                id.clone(),
                thread,
                2,
                jiff::Timestamp::now(),
                harw_session_store::record::RecordKind::Item,
                serde_json::json!({
                    "type": "tool_call",
                    "arguments": { "path": file_b.display().to_string() },
                }),
            ))
            .expect("append fake tool_call record for file_b");
        let transcript_path = store
            .transcript_path(&id)
            .expect("transcript path for a valid session id");

        let meta = meta::load_or_derive(sessions_dir.path(), &id).expect("derive fresh meta");
        assert_eq!(meta.project_key, None);
        let mut session = DiscoveredSession {
            id: id.clone(),
            path: transcript_path,
            modified_at: SystemTime::now(),
            meta: Some(meta),
        };

        backfill_project_key(sessions_dir.path(), &mut session);

        let expected_root = fs::canonicalize(project_dir.path()).expect("canonical project root");
        let expected_key = harw_home::project::project_key(&expected_root);
        let updated_meta = session
            .meta
            .as_ref()
            .expect("meta stays present after backfill");
        assert_eq!(
            updated_meta.project_key.as_deref(),
            Some(expected_key.as_str())
        );

        // Erneutes `load_or_derive` bestätigt, dass der Sidecar persistiert wurde.
        let reloaded =
            meta::load_or_derive(sessions_dir.path(), &id).expect("reload persisted meta");
        assert_eq!(reloaded.project_key.as_deref(), Some(expected_key.as_str()));
    }

    #[test]
    fn backfill_project_key_leaves_session_untagged_when_only_ignored_paths_are_present() {
        let sessions_dir = tempfile::tempdir().expect("temporary sessions directory");
        let id = SessionId::try_from("backfill-miss".to_owned()).expect("valid session id");
        let store = harw_session_store::store::TranscriptStore::new(sessions_dir.path());
        // Siehe Kommentar im Hit-Test oben: echte Transcript-Datensätze
        // brauchen `session_id`/`thread`/`sequence`, sonst scheitert
        // `meta::load_or_derive` mit "missing field `session_id`".
        store
            .append(&harw_session_store::record::TranscriptRecord::new(
                id.clone(),
                harw_types::ThreadRef::from_str("root"),
                0,
                jiff::Timestamp::now(),
                harw_session_store::record::RecordKind::Item,
                serde_json::json!({
                    "type": "tool_call",
                    "arguments": { "path": "/tmp/scratch/output.txt" },
                }),
            ))
            .expect("append fake tool_call record with only an ignored path");
        let transcript_path = store
            .transcript_path(&id)
            .expect("transcript path for a valid session id");

        let meta = meta::load_or_derive(sessions_dir.path(), &id).expect("derive fresh meta");
        let mut session = DiscoveredSession {
            id: id.clone(),
            path: transcript_path,
            modified_at: SystemTime::now(),
            meta: Some(meta),
        };

        backfill_project_key(sessions_dir.path(), &mut session);

        assert_eq!(
            session
                .meta
                .as_ref()
                .expect("meta stays present")
                .project_key,
            None
        );
    }
}
