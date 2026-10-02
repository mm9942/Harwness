//! Projektgedächtnis-Erfassung (Langzeitgedächtnis v3, Addendum B), siehe
//! `CONTRACT.md` Addendum B ("Erfassen → Konsolidieren → Abrufen").
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt die Brücke zwischen einzelnen Tool-Ergebnissen
//! während einer laufenden Session und den bestehenden Speichern
//! (`crate::facts::FactStore`, `crate::extraction::IncomingStore`,
//! `crate::file_index::FileKnowledgeIndex`):
//! - [`ProjectMemoryCapture`] beobachtet Tool-Ergebnisse
//!   ([`ProjectMemoryCapture::record_tool_outcome`]) und leitet daraus drei
//!   Arten von Gedächtnis ab:
//!   - **Dateiwissen**: erfolgreiche `fs.read`-Aufrufe (voller Inhalt, kein
//!     `offset`, keine Kürzung) aktualisieren den [`crate::file_index::FileKnowledgeIndex`].
//!   - **Recherche-Funde**: Ausgaben von Recherche-Werkzeugen (oder jede
//!     Ausgabe, die wie ein `ResearchFinding`-JSON aussieht) mit Konfidenz
//!     `medium`/`high`/`verified` und mindestens einem Beleg werden als
//!     [`crate::facts::Fact`]-Kandidat (`FactType::Fact`) in den
//!     [`crate::extraction::IncomingStore`] geschrieben.
//!   - **Lektionen**: ein Fehler, dem später (im selben Werkzeug bzw. bei
//!     `shell.exec` mit gleichem Befehlspräfix) ein Erfolg folgt, wird als
//!     `FactType::Pitfall`-Kandidat geschrieben; unaufgelöste Fehler landen
//!     beim Session-Ende über [`ProjectMemoryCapture::flush_session`] mit
//!     niedrigerer Konfidenz im Incoming-Speicher.
//! - [`consolidate_project_memories`] führt die bestehende Konsolidierung
//!   (`crate::consolidation::{plan_consolidation, apply_plan, ConsolidationLock}`)
//!   gegen die Projekt-Wurzel aus.
//!
//! Kein Rohinhalt einer Datei wird je gespeichert (siehe
//! `crate::file_index::extract_file_knowledge`); alles Geschriebene läuft
//! über [`crate::facts::redact`], bevor es den Incoming-Speicher erreicht.
//!
//! # Nebenläufigkeit
//! [`ProjectMemoryCapture`] ist `Send + Sync`. Der In-Prozess-Sitzungszustand
//! (offene Fehler je Session) liegt hinter einem `std::sync::Mutex`; alle
//! Datei-Schreibvorgänge laufen über die bereits atomaren Operationen von
//! [`crate::extraction::IncomingStore`] und [`crate::file_index::FileKnowledgeIndex`].
//!
//! # Fehler
//! [`crate::error::MemoryError`]. Einzelne Tool-Ergebnisse dürfen nie einen
//! laufenden Turn abbrechen — Fehler beim Erfassen werden ausschließlich per
//! `tracing::warn!` gemeldet, nie propagiert (siehe Signaturen unten: kein
//! `Result` auf `record_tool_outcome`/`flush_session`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::MemoryResult;
use crate::consolidation::{
    Conflict, ConsolidationError, ConsolidationLock, ConsolidationReport, Deadline, apply_plan,
    plan_consolidation,
};
use crate::error::MemoryError;
use crate::extraction::{ExtractionError, IncomingStore};
use crate::facts::{Fact, FactScope, FactStore, FactType, redact, slugify};
use crate::file_index::{FileKnowledgeIndex, extract_file_knowledge};

/// Marker im `fs.read`-Ausgabetext für gekürzte Ausgaben (siehe
/// `harw-tool-fs/src/read.rs`). Solche Ausgaben enthalten nicht den vollen
/// Dateiinhalt und dürfen nicht als Dateiwissen erfasst werden.
const FS_READ_TRUNCATION_MARKER: &str = "[fs.read: Ausgabe gekürzt";

/// Fehlermuster, die auf einen vorübergehenden (nicht lehrreichen) Fehler
/// hindeuten — solche Fehler werden nie als Lektion erfasst.
const TRANSIENT_ERROR_PATTERNS: [&str; 9] = [
    "429",
    "rate limit",
    "timeout",
    "timed out",
    "connection reset",
    "connection refused",
    "temporarily unavailable",
    "503",
    "502",
];

/// Höchstlänge einer Argument-Zusammenfassung in Zeichen.
const ARGS_SUMMARY_MAX_CHARS: usize = 200;

/// Höchstlänge eines Fehlerauszugs in Bytes.
const ERROR_EXCERPT_MAX_BYTES: usize = 300;

/// Nach dieser Wartezeit seit der letzten Konsolidierung wird bei leerem
/// Incoming-Speicher trotzdem ein `decay`-Lauf ausgeführt (§Addendum B:
/// „höchstens einmal pro Tag").
const DAILY_DECAY_MIN_HOURS: i64 = 24;

/// Höchstalter unbenutzter Fakten, ab dem [`crate::facts::FactStore::decay`]
/// ihre `confidence` halbiert.
const DECAY_MAX_UNUSED_DAYS: i64 = 90;

// ---------------------------------------------------------------------
// Kleine Textwerkzeuge
// ---------------------------------------------------------------------

/// Kürzt `s` auf höchstens `max` Zeichen (nicht Bytes), unicode-sicher.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect()
    }
}

/// Kürzt `s` auf höchstens `max_bytes` Bytes, ohne ein UTF-8-Zeichen
/// mittendrin abzuschneiden.
fn truncate_bytes_utf8_safe(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// Formatiert `ts` als RFC 3339.
fn format_rfc3339(ts: OffsetDateTime) -> String {
    ts.format(&Rfc3339)
        .unwrap_or_else(|_| ts.unix_timestamp().to_string())
}

/// Aktueller Zeitpunkt als RFC 3339.
fn now_rfc3339() -> String {
    format_rfc3339(OffsetDateTime::now_utc())
}

/// `true`, wenn `text` auf einen vorübergehenden Fehler hindeutet (siehe
/// [`TRANSIENT_ERROR_PATTERNS`]).
fn is_transient_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    TRANSIENT_ERROR_PATTERNS.iter().any(|p| lower.contains(p))
}

/// Baut eine kompakte, redigierte Zusammenfassung von `arguments`.
fn args_summary(arguments: &serde_json::Value) -> String {
    let raw = serde_json::to_string(arguments).unwrap_or_default();
    redact(&truncate_chars(&raw, ARGS_SUMMARY_MAX_CHARS))
}

/// Sammelt Pfad-artige Argumentwerte (`path`, `paths`, `file`, `files`) für
/// die `files`-Angabe einer Lektion.
fn extract_file_args(arguments: &serde_json::Value) -> Vec<String> {
    let Some(obj) = arguments.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["path", "file"] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
            out.push(s.to_owned());
        }
    }
    for key in ["paths", "files"] {
        if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
            out.extend(arr.iter().filter_map(|v| v.as_str()).map(str::to_owned));
        }
    }
    out
}

/// Normalisiert einen Fehlertext für die Signatur-Bildung: klein
/// geschrieben, Ziffernläufe entfernt, Pfad- und Hex-artige Tokens durch
/// Platzhalter ersetzt.
fn normalize_error_signature(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut out = String::new();
    for token in lower.split_whitespace() {
        if token.contains('/') || token.contains('\\') {
            out.push_str("<path>");
        } else if token.len() >= 6 && token.chars().all(|c| c.is_ascii_hexdigit()) {
            out.push_str("<hex>");
        } else {
            out.extend(token.chars().filter(|c| !c.is_ascii_digit()));
        }
        out.push(' ');
    }
    out.trim().to_owned()
}

/// Stabiler 32-Bit-Hash (`DefaultHasher`, deterministisch je Prozess-Start
/// wie -Kompilat, kein zufälliger Seed) für die Pitfall-Namensbildung.
fn signature_hash(signature: &str) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    signature.hash(&mut hasher);
    (hasher.finish() & 0xFFFF_FFFF) as u32
}

/// Vergleichsschlüssel, unter dem ein Erfolg einen offenen Fehler auflösen
/// darf: für `shell.exec` die ersten beiden Leerzeichen-getrennten Tokens
/// des `command`-Arguments, sonst der Werkzeugname selbst.
fn match_key(tool_name: &str, arguments: &serde_json::Value) -> String {
    if tool_name == "shell.exec" {
        let prefix = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(|cmd| cmd.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        format!("shell.exec:{prefix}")
    } else {
        tool_name.to_owned()
    }
}

/// Prüft eine `shell.exec`-Erfolgsausgabe (JSON mit `exit_code`/`stdout`/
/// `stderr`, siehe `harw-tool-shell/src/exec.rs`) auf einen faktischen
/// Fehlschlag: `exit_code != 0` oder ein Fehlermuster in der Ausgabe.
///
/// # Returns
/// `Some(<kombinierte Ausgabe oder Kurznotiz>)`, wenn ein Fehlschlag erkannt
/// wurde; `None` bei Erfolg oder wenn `output_text` kein JSON dieser Form ist.
fn shell_exec_failure_text(output_text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(output_text).ok()?;
    let exit_code = value
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let stdout = value.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = value.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
    let combined = format!("{stdout}\n{stderr}");
    let looks_failed = combined.contains("error[E")
        || combined.contains("FAILED")
        || combined.contains("panicked at");
    if exit_code != 0 || looks_failed {
        Some(if combined.trim().is_empty() {
            format!("shell.exec exit_code={exit_code}")
        } else {
            combined
        })
    } else {
        None
    }
}

/// Übersetzt einen [`ExtractionError`] (Fehlertyp von
/// [`crate::extraction::IncomingStore`]) in einen [`MemoryError`], damit
/// dieses Modul einheitlich [`MemoryResult`] zurückgibt.
fn extraction_to_memory(err: ExtractionError, root: &Path) -> MemoryError {
    match err {
        ExtractionError::Io { path, source } => MemoryError::Io { path, source },
        ExtractionError::Json { source } => MemoryError::Serde {
            context: "incoming-kandidaten",
            source,
        },
        ExtractionError::NoJsonFound => MemoryError::Io {
            path: root.to_path_buf(),
            source: std::io::Error::other(
                "extraction: unerwartet keine JSON-Struktur in incoming-Kandidat gefunden",
            ),
        },
    }
}

// ---------------------------------------------------------------------
// Lektionen: In-Prozess-Sitzungszustand
// ---------------------------------------------------------------------

/// Ein noch nicht aufgelöster Fehlschlag innerhalb einer Session.
struct PendingFailure {
    /// Werkzeugname, wie er im Tool-Aufruf stand.
    tool: String,
    /// Vergleichsschlüssel, siehe [`match_key`].
    key: String,
    /// Redigierte Argument-Zusammenfassung des Fehlschlags.
    args_summary: String,
    /// Redigierter, längenbegrenzter Fehlerauszug.
    error_excerpt: String,
    /// Erste Zeile des Fehlerauszugs (für die `description`).
    first_line: String,
    /// Pfad-artige Argumentwerte des fehlgeschlagenen Aufrufs.
    files: Vec<String>,
    /// Normalisierte Fehler-Signatur (Namensbildung, Dedup).
    signature: String,
}

/// Pro-Session-Zustand: offene Fehlschläge, die auf einen Erfolg warten.
#[derive(Default)]
struct SessionState {
    /// Chronologisch (älteste zuerst); Auflösung sucht vom Ende her.
    pending_failures: Vec<PendingFailure>,
}

// ---------------------------------------------------------------------
// ProjectMemoryCapture
// ---------------------------------------------------------------------

/// Beobachtet Tool-Ergebnisse einer Session und leitet Dateiwissen,
/// Recherche-Funde und Lektionen daraus ab, siehe Moduldoku.
pub struct ProjectMemoryCapture {
    /// Projekt-Gedächtniswurzel (`<projekt>/.harw/memories/`).
    memories_root: PathBuf,
    /// Dateiwissen-Index.
    file_index: FileKnowledgeIndex,
    /// Episodischer Puffer für neue Fakt-Kandidaten.
    incoming: IncomingStore,
    /// Offene Fehlschläge je Session-ID.
    sessions: Mutex<HashMap<String, SessionState>>,
}

impl ProjectMemoryCapture {
    /// Öffnet die Erfassung an `memories_root`; legt darunter `files/` und
    /// `facts/_incoming/` an, falls sie fehlen (siehe
    /// [`FileKnowledgeIndex::open`], [`IncomingStore::open`]).
    ///
    /// # Errors
    /// Fehler von [`FileKnowledgeIndex::open`]/[`IncomingStore::open`].
    pub fn open(memories_root: &Path) -> MemoryResult<Self> {
        let file_index = FileKnowledgeIndex::open(memories_root)?;
        let incoming = IncomingStore::open(memories_root)
            .map_err(|e| extraction_to_memory(e, memories_root))?;
        Ok(Self {
            memories_root: memories_root.to_path_buf(),
            file_index,
            incoming,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// Die Gedächtniswurzel, an der diese Erfassung geöffnet wurde.
    #[must_use]
    pub fn memories_root(&self) -> &Path {
        &self.memories_root
    }

    /// Der Dateiwissen-Index dieser Erfassung.
    #[must_use]
    pub fn file_index(&self) -> &FileKnowledgeIndex {
        &self.file_index
    }

    /// Verarbeitet ein einzelnes Tool-Ergebnis, siehe Moduldoku.
    ///
    /// # Beschreibung
    /// - `fs.read`-Fehler: nur `tracing::warn!`.
    /// - Andere Fehler: als möglicher Lektions-Auslöser erfasst (siehe
    ///   [`Self::record_failure`]), sofern nicht vorübergehend.
    /// - `fs.read`-Erfolg: aktualisiert das Dateiwissen (siehe
    ///   [`Self::handle_fs_read_success`]).
    /// - `shell.exec`-Erfolg mit faktischem Fehlschlag (siehe
    ///   [`shell_exec_failure_text`]): wie ein Fehler behandelt.
    /// - Jeder andere Erfolg: versucht, einen offenen Fehlschlag desselben
    ///   Werkzeugs aufzulösen (siehe [`Self::try_resolve_pending`]), und
    ///   prüft die Ausgabe auf Recherche-Funde (siehe
    ///   [`Self::try_capture_findings`]).
    ///
    /// Nie ein `Result` — jeder interne Fehler landet nur in `tracing::warn!`
    /// (siehe Moduldoku).
    pub fn record_tool_outcome(
        &self,
        session_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        is_error: bool,
        output_text: &str,
    ) {
        if is_error {
            if tool_name == "fs.read" {
                tracing::warn!(session_id, tool_name, "capture: fs.read fehlgeschlagen");
                return;
            }
            self.record_failure(session_id, tool_name, arguments, output_text);
            return;
        }

        if tool_name == "fs.read" {
            self.handle_fs_read_success(arguments, output_text);
            return;
        }

        if tool_name == "shell.exec" {
            if let Some(failure_text) = shell_exec_failure_text(output_text) {
                self.record_failure(session_id, tool_name, arguments, &failure_text);
            } else {
                self.try_resolve_pending(session_id, tool_name, arguments);
            }
            return;
        }

        self.try_resolve_pending(session_id, tool_name, arguments);
        self.try_capture_findings(session_id, tool_name, output_text);
    }

    /// Aktualisiert das Dateiwissen für einen erfolgreichen `fs.read`, sofern
    /// `offset` fehlt/`0` ist und die Ausgabe nicht gekürzt wurde.
    fn handle_fs_read_success(&self, arguments: &serde_json::Value, output_text: &str) {
        let Some(path) = arguments.get("path").and_then(|v| v.as_str()) else {
            return;
        };
        let offset_is_zero_or_absent = match arguments.get("offset") {
            None | Some(serde_json::Value::Null) => true,
            Some(v) => v.as_u64() == Some(0),
        };
        if !offset_is_zero_or_absent {
            return;
        }
        if output_text.contains(FS_READ_TRUNCATION_MARKER) {
            return;
        }
        let knowledge = extract_file_knowledge(path, output_text, &now_rfc3339());
        if let Err(err) = self.file_index.upsert(knowledge) {
            tracing::warn!(error = %err, path, "capture: dateiwissen konnte nicht aktualisiert werden");
        }
    }

    /// Erfasst einen Fehlschlag als offenen Pending-Eintrag der Session,
    /// sofern er nicht vorübergehend ist (siehe [`is_transient_error`]) und
    /// noch keine identische Signatur für diesen Vergleichsschlüssel offen
    /// ist (Dedup innerhalb der Session).
    fn record_failure(
        &self,
        session_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        error_text: &str,
    ) {
        if is_transient_error(error_text) {
            return;
        }
        let excerpt = truncate_bytes_utf8_safe(&redact(error_text), ERROR_EXCERPT_MAX_BYTES);
        let first_line = truncate_chars(excerpt.lines().next().unwrap_or(""), 120);
        let signature = normalize_error_signature(&excerpt);
        let key = match_key(tool_name, arguments);

        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let state = sessions.entry(session_id.to_owned()).or_default();
        if state
            .pending_failures
            .iter()
            .any(|pf| pf.signature == signature && pf.key == key)
        {
            return;
        }
        state.pending_failures.push(PendingFailure {
            tool: tool_name.to_owned(),
            key,
            args_summary: args_summary(arguments),
            error_excerpt: excerpt,
            first_line,
            files: extract_file_args(arguments),
            signature,
        });
    }

    /// Sucht den jüngsten offenen Fehlschlag mit passendem
    /// [`match_key`] und schreibt bei Erfolg einen Pitfall-Kandidaten (mit
    /// „Funktionierte mit") in den Incoming-Speicher.
    fn try_resolve_pending(
        &self,
        session_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) {
        let key = match_key(tool_name, arguments);
        let resolved = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            match sessions.get_mut(session_id) {
                Some(state) => state
                    .pending_failures
                    .iter()
                    .rposition(|pf| pf.key == key)
                    .map(|pos| state.pending_failures.remove(pos)),
                None => None,
            }
        };
        let Some(pf) = resolved else {
            return;
        };

        let success_args = args_summary(arguments);
        let fact = pitfall_fact(session_id, &pf, Some(&success_args), 0.5);
        if let Err(err) = self.incoming.write_candidates(&[fact]) {
            tracing::warn!(error = %err, "capture: pitfall-kandidat konnte nicht geschrieben werden");
        }
    }

    /// Prüft `output_text` auf Recherche-Fund-artige JSON-Objekte
    /// (`conclusion: String`, `evidence: [..]`, ggf. verschachtelt in einem
    /// `FindingBundle` oder Fan-out-Array) und schreibt für jeden Fund mit
    /// Konfidenz `medium`/`high`/`verified` und mindestens einem Beleg einen
    /// `Fact`-Kandidaten.
    fn try_capture_findings(&self, session_id: &str, _tool_name: &str, output_text: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(output_text) else {
            return;
        };
        let mut found = Vec::new();
        collect_finding_objects(&value, &mut found);
        if found.is_empty() {
            return;
        }
        let facts: Vec<Fact> = found
            .into_iter()
            .filter_map(|obj| finding_to_fact(&obj, session_id))
            .collect();
        if facts.is_empty() {
            return;
        }
        if let Err(err) = self.incoming.write_candidates(&facts) {
            tracing::warn!(error = %err, "capture: finding-kandidaten konnten nicht geschrieben werden");
        }
    }

    /// Schließt die Erfassung für `session_id` ab: jeder noch offene
    /// Fehlschlag (dedupliziert nach Signatur) wird als Pitfall-Kandidat
    /// ohne „Funktionierte mit" und mit Konfidenz `0.35` in den
    /// Incoming-Speicher geschrieben; der Sitzungszustand wird danach
    /// verworfen.
    pub fn flush_session(&self, session_id: &str) {
        let pending = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions
                .remove(session_id)
                .map(|s| s.pending_failures)
                .unwrap_or_default()
        };
        if pending.is_empty() {
            return;
        }
        let mut seen = HashSet::new();
        let facts: Vec<Fact> = pending
            .into_iter()
            .filter(|pf| seen.insert(pf.signature.clone()))
            .map(|pf| pitfall_fact(session_id, &pf, None, 0.35))
            .collect();
        if let Err(err) = self.incoming.write_candidates(&facts) {
            tracing::warn!(error = %err, "capture: pitfall-kandidaten beim flush konnten nicht geschrieben werden");
        }
    }
}

/// Baut den Pitfall-[`Fact`]-Kandidaten für einen [`PendingFailure`].
///
/// `successful_args`: `Some(...)` bei Auflösung durch einen Erfolg (`body`
/// bekommt eine „Funktionierte mit"-Zeile), `None` bei unaufgelösten
/// Fehlschlägen aus [`ProjectMemoryCapture::flush_session`].
fn pitfall_fact(
    session_id: &str,
    pf: &PendingFailure,
    successful_args: Option<&str>,
    confidence: f32,
) -> Fact {
    let name = format!("pitfall-{:08x}", signature_hash(&pf.signature));
    let description = truncate_chars(&format!("{}: {}", pf.tool, pf.first_line), 200);
    let mut body = format!(
        "Fehler:\n{}\nFehlgeschlagen mit: {}\n",
        pf.error_excerpt, pf.args_summary
    );
    if let Some(success) = successful_args {
        body.push_str(&format!("Funktionierte mit: {success}\n"));
    }
    if !pf.files.is_empty() {
        body.push_str(&format!("Betroffene Dateien: {}\n", pf.files.join(", ")));
    }
    let now = OffsetDateTime::now_utc();
    Fact {
        name,
        description,
        fact_type: FactType::Pitfall,
        scope: FactScope::Project,
        created: now,
        updated: now,
        confidence,
        sources: vec![format!("session:{session_id}")],
        tags: vec!["auto".to_owned(), "pitfall".to_owned(), pf.tool.clone()],
        body,
    }
}

/// Durchsucht `value` rekursiv nach Objekten mit `conclusion: String` und
/// `evidence: Array` (Fund-Form), sammelt sie in `out`.
fn collect_finding_objects(
    value: &serde_json::Value,
    out: &mut Vec<serde_json::Map<String, serde_json::Value>>,
) {
    match value {
        serde_json::Value::Object(map) => {
            if is_finding_shape(map) {
                out.push(map.clone());
            }
            for v in map.values() {
                collect_finding_objects(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_finding_objects(item, out);
            }
        }
        _ => {}
    }
}

/// `true`, wenn `map` wie ein `ResearchFinding` aussieht: `conclusion` als
/// String, `evidence` als Array.
fn is_finding_shape(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    matches!(map.get("conclusion"), Some(serde_json::Value::String(_)))
        && matches!(map.get("evidence"), Some(serde_json::Value::Array(_)))
}

/// Baut einen `Fact`-Kandidaten (`FactType::Fact`) aus einem Fund-Objekt,
/// `None` bei Konfidenz `low`/unbekannt oder ohne Belege.
fn finding_to_fact(
    obj: &serde_json::Map<String, serde_json::Value>,
    session_id: &str,
) -> Option<Fact> {
    let _ = session_id; // Findings tragen ausschließlich evidence[].locator als sources.
    let conclusion = obj.get("conclusion")?.as_str()?.trim();
    if conclusion.is_empty() {
        return None;
    }
    let confidence = match obj
        .get("confidence")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase)
    {
        Some(ref s) if s == "medium" => 0.6_f32,
        Some(ref s) if s == "high" => 0.8_f32,
        Some(ref s) if s == "verified" => 0.95_f32,
        _ => return None,
    };

    let sources: Vec<String> = obj
        .get("evidence")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("locator").and_then(|v| v.as_str()))
                .map(redact)
                .collect()
        })
        .unwrap_or_default();
    if sources.is_empty() {
        return None;
    }

    let produced_by = obj
        .get("produced_by")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let constraints: Vec<String> = string_array(obj.get("constraints"));
    let unresolved: Vec<String> = string_array(obj.get("unresolved_questions"));

    let mut body = redact(conclusion);
    if !constraints.is_empty() {
        body.push_str("\n\nRandbedingungen:\n");
        for c in &constraints {
            body.push_str(&format!("- {}\n", redact(c)));
        }
    }
    if !unresolved.is_empty() {
        body.push_str("\nOffene Fragen:\n");
        for u in &unresolved {
            body.push_str(&format!("- {}\n", redact(u)));
        }
    }

    let title = truncate_chars(conclusion, 60);
    let now = OffsetDateTime::now_utc();
    Some(Fact {
        name: slugify(&format!("finding {title}")),
        description: redact(&truncate_chars(conclusion, 200)),
        fact_type: FactType::Fact,
        scope: FactScope::Project,
        created: now,
        updated: now,
        confidence,
        sources,
        tags: vec![
            "auto".to_owned(),
            "finding".to_owned(),
            produced_by.to_owned(),
        ],
        body,
    })
}

/// Liest ein Array von Strings aus einem optionalen JSON-Wert.
fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------
// consolidate_project_memories
// ---------------------------------------------------------------------

/// Pfad der Marker-Datei für den letzten (leeren) Konsolidierungslauf.
fn last_consolidation_marker_path(memories_root: &Path) -> PathBuf {
    memories_root.join("facts").join("_last_consolidation")
}

/// Pfad der gesammelten, nicht automatisch aufgelösten Widersprüche.
fn conflicts_path(memories_root: &Path) -> PathBuf {
    memories_root.join("facts").join("_conflicts.json")
}

/// `true`, wenn seit dem letzten vermerkten (leeren) Konsolidierungslauf
/// mindestens [`DAILY_DECAY_MIN_HOURS`] vergangen sind oder noch nie einer
/// vermerkt wurde.
fn should_run_daily_decay(memories_root: &Path, now: OffsetDateTime) -> MemoryResult<bool> {
    let path = last_consolidation_marker_path(memories_root);
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(OffsetDateTime::parse(content.trim(), &Rfc3339)
            .map(|ts| (now - ts).whole_hours() >= DAILY_DECAY_MIN_HOURS)
            .unwrap_or(true)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(MemoryError::Io { path, source: e }),
    }
}

/// Schreibt den Zeitpunkt des letzten Konsolidierungslaufs.
fn write_last_consolidation_marker(memories_root: &Path, now: OffsetDateTime) -> MemoryResult<()> {
    let path = last_consolidation_marker_path(memories_root);
    harw_fsutil::write_atomic(
        &path,
        format_rfc3339(now).as_bytes(),
        harw_fsutil::AtomicWriteOptions::private(),
    )
    .map_err(|e| MemoryError::Io { path, source: e })
}

/// Hängt `conflicts` an `<memories>/facts/_conflicts.json` an (liest die
/// bestehende JSON-Liste, ergänzt, schreibt atomar zurück).
fn write_conflicts(
    memories_root: &Path,
    conflicts: &[Conflict],
    now: OffsetDateTime,
) -> MemoryResult<()> {
    let path = conflicts_path(memories_root);
    let mut existing: Vec<serde_json::Value> = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(MemoryError::Io { path, source: e }),
    };
    let now_str = format_rfc3339(now);
    for c in conflicts {
        // Derselbe Widerspruch (gleiches Paar + Begründung) wird nicht bei
        // jedem Lauf erneut angehängt — wichtig für den wiederholten
        // Dedupe-Lauf über bestehende globale Fakten.
        let already = existing.iter().any(|v| {
            v.get("left").and_then(|x| x.as_str()) == Some(c.left.as_str())
                && v.get("right").and_then(|x| x.as_str()) == Some(c.right.as_str())
                && v.get("note").and_then(|x| x.as_str()) == Some(c.note.as_str())
        });
        if already {
            continue;
        }
        existing.push(serde_json::json!({
            "left": c.left,
            "right": c.right,
            "note": c.note,
            "recorded_at": now_str,
        }));
    }
    let bytes = serde_json::to_vec_pretty(&existing).map_err(|source| MemoryError::Serde {
        context: "_conflicts.json schreiben",
        source,
    })?;
    harw_fsutil::write_atomic(&path, &bytes, harw_fsutil::AtomicWriteOptions::private())
        .map_err(|e| MemoryError::Io { path, source: e })
}

/// Führt die Konsolidierung (Phase 2, `crate::consolidation`) gegen die
/// Projekt-Gedächtniswurzel `memories_root` aus.
///
/// # Beschreibung
/// Erwirbt [`ConsolidationLock::try_acquire`] an `memories_root`; bei
/// Kontention wird der Lauf übersprungen (`tracing::debug!`) und ein leerer
/// [`ConsolidationReport`] geliefert — kein Fehler. Andernfalls: liest die
/// Kandidaten aus `facts/_incoming/`. Sind keine vorhanden, läuft höchstens
/// einmal pro Tag trotzdem [`crate::facts::FactStore::decay`] (siehe
/// [`should_run_daily_decay`]). Sind welche vorhanden, plant
/// [`plan_consolidation`] gegen die bestehenden Fakten, schreibt etwaige
/// Widersprüche nach `facts/_conflicts.json` (siehe [`write_conflicts`]),
/// wendet den Plan über [`apply_plan`] an, lässt `decay(90, now)` laufen und
/// regeneriert den Index. Der Lock wird in jedem Fall freigegeben.
///
/// # Errors
/// Fehler von [`FactStore::open`]/[`IncomingStore::open`]/[`plan_consolidation`]-
/// Anwendung ([`apply_plan`])/[`FactStore::decay`]/[`FactStore::write_index`];
/// [`MemoryError::Io`] bei Fehlern der Marker-Datei. Ein Fehler beim
/// Schreiben der Widersprüche wird nicht propagiert (nur `tracing::warn!`),
/// da er die eigentliche Konsolidierung nicht ungültig macht.
pub fn consolidate_project_memories(memories_root: &Path) -> MemoryResult<ConsolidationReport> {
    consolidate_memories(memories_root, FactScope::Project)
}

/// Wie [`consolidate_project_memories`], aber für die globale Wurzel
/// (`<profil>/memories`): zusätzlich werden doppelte bestehende globale
/// Fakten verschmolzen und Widersprüche nach `facts/_conflicts.json`
/// geschrieben.
///
/// # Errors
/// Siehe [`consolidate_memories`].
pub fn consolidate_global_memories(memories_root: &Path) -> MemoryResult<ConsolidationReport> {
    consolidate_memories(memories_root, FactScope::Global)
}

/// Konsolidierung für beliebigen `scope` mit der Standard-Frist
/// ([`DEFAULT_DELETION_DEADLINE`]). Ein Fristablauf wird als
/// [`MemoryError::Io`] mit [`std::io::ErrorKind::TimedOut`] gemeldet (der
/// Bestand bleibt dabei unverändert); wer den typisierten Fehler braucht,
/// ruft [`consolidate_memories_with_deadline`].
///
/// # Errors
/// Siehe [`consolidate_memories_with_deadline`].
pub fn consolidate_memories(
    memories_root: &Path,
    scope: FactScope,
) -> MemoryResult<ConsolidationReport> {
    consolidate_memories_with_deadline(memories_root, scope, Deadline::default_deletion()).map_err(
        |e| match e {
            ConsolidationError::Memory(m) => m,
            ConsolidationError::Deadline(d) => MemoryError::Io {
                path: memories_root.to_path_buf(),
                source: std::io::Error::new(std::io::ErrorKind::TimedOut, d.to_string()),
            },
        },
    )
}

/// Fristgebundene, scope-generische Konsolidierung.
///
/// # Beschreibung
/// Nimmt den [`ConsolidationLock`] an `memories_root` (für Global wie für
/// Projekt); bei Kontention wird der Lauf übersprungen und ein leerer
/// Report geliefert. Alles Löschende folgt „erst prüfen, dann anwenden":
/// Lesen und Planen laufen im Speicher, die `deadline` wird vor dem
/// Commit-Punkt (vor `IncomingStore::take_all` bzw. vor dem ersten
/// Schreiben/Löschen des Dedupe-Passes) geprüft. Ist sie abgelaufen, bleibt
/// der Bestand unverändert (inkl. `_incoming/`), der Lock wird freigegeben
/// und [`ConsolidationError::Deadline`] geliefert. Nach dem Commit-Punkt
/// läuft der Vorgang zu Ende; Ergebnisse gehen direkt in den Ziel-Store.
///
/// Für [`FactScope::Global`] verschmilzt zusätzlich ein Dedupe-Pass die
/// bestehenden Fakten untereinander (selbe Heuristik wie
/// [`plan_consolidation`]) und hält Widersprüche in `_conflicts.json` fest.
///
/// # Errors
/// [`ConsolidationError::Deadline`] bei Fristablauf, sonst
/// [`ConsolidationError::Memory`] wie bei [`consolidate_project_memories`].
pub fn consolidate_memories_with_deadline(
    memories_root: &Path,
    scope: FactScope,
    deadline: Deadline,
) -> Result<ConsolidationReport, ConsolidationError> {
    let lock = match ConsolidationLock::try_acquire(memories_root) {
        Ok(lock) => lock,
        Err(MemoryError::LockContention { .. }) => {
            tracing::debug!("memory: konsolidierung übersprungen (lock belegt)");
            return Ok(ConsolidationReport::default());
        }
        Err(e) => return Err(e.into()),
    };
    let result = run_consolidation(memories_root, scope, deadline);
    let _ = lock.release();
    result
}

/// Dedupe-Pass über bestehende Fakten (nur Global): verschmilzt Duplikate
/// untereinander, vermerkt Widersprüche. Plant vollständig im Speicher,
/// prüft die Frist, wendet dann an (erst Merge-Ergebnisse schreiben, dann
/// Duplikate löschen — ein Abbruch dazwischen verliert nie Inhalt).
fn dedupe_existing(
    store: &FactStore,
    memories_root: &Path,
    now: OffsetDateTime,
    deadline: Deadline,
) -> Result<ConsolidationReport, ConsolidationError> {
    deadline.check("dedupe-read")?;
    let mut facts = store.list()?;
    facts.sort_by(|a, b| a.name.cmp(&b.name));
    let mut kept: Vec<Fact> = Vec::new();
    let mut changed: HashSet<String> = HashSet::new();
    let mut removed: Vec<String> = Vec::new();
    let mut conflicts: Vec<Conflict> = Vec::new();
    for fact in facts {
        deadline.check("dedupe-plan")?;
        let plan = plan_consolidation(&kept, std::slice::from_ref(&fact));
        match (plan.merges.first(), plan.updates.first()) {
            (Some(merge), Some(merged)) => {
                if let Some(slot) = kept.iter_mut().find(|k| k.name == merge.target) {
                    *slot = merged.clone();
                    changed.insert(merge.target.clone());
                    removed.push(fact.name.clone());
                }
            }
            _ => {
                conflicts.extend(plan.conflicts);
                kept.push(fact);
            }
        }
    }
    if removed.is_empty() && conflicts.is_empty() {
        return Ok(ConsolidationReport::default());
    }
    // Commit-Punkt: ab hier kein Abbruch mehr.
    deadline.check("dedupe-apply")?;
    if !conflicts.is_empty() {
        if let Err(err) = write_conflicts(memories_root, &conflicts, now) {
            tracing::warn!(error = %err, "memory: widersprüche konnten nicht geschrieben werden");
        }
    }
    let mut written = 0usize;
    for fact in kept.iter().filter(|k| changed.contains(&k.name)) {
        store.write(fact)?;
        written += 1;
    }
    let mut deleted = 0usize;
    for name in &removed {
        if store.delete(name)? {
            deleted += 1;
        }
    }
    store.write_index()?;
    Ok(ConsolidationReport {
        merged: removed.len(),
        written,
        deleted,
        conflicts: conflicts.len(),
    })
}

/// Innerer Ablauf von [`consolidate_memories_with_deadline`] (ohne
/// Lock-Handling), siehe dort.
fn run_consolidation(
    memories_root: &Path,
    scope: FactScope,
    deadline: Deadline,
) -> Result<ConsolidationReport, ConsolidationError> {
    let incoming_store =
        IncomingStore::open(memories_root).map_err(|e| extraction_to_memory(e, memories_root))?;
    let fact_store = FactStore::open(memories_root, scope)?;
    let now = OffsetDateTime::now_utc();

    let dedupe = if scope == FactScope::Global {
        dedupe_existing(&fact_store, memories_root, now, deadline)?
    } else {
        ConsolidationReport::default()
    };

    let pending = incoming_store
        .list()
        .map_err(|e| extraction_to_memory(e, memories_root))?;
    if pending.is_empty() {
        if should_run_daily_decay(memories_root, now)? {
            fact_store.decay(DECAY_MAX_UNUSED_DAYS, now)?;
            fact_store.write_index()?;
            write_last_consolidation_marker(memories_root, now)?;
        }
        return Ok(dedupe);
    }

    // Commit-Punkt: `take_all` leert `_incoming/` — vorher Frist prüfen,
    // damit ein Abbruch die Kandidaten nicht verliert.
    deadline.check("incoming-take")?;
    let mut taken = incoming_store
        .take_all()
        .map_err(|e| extraction_to_memory(e, memories_root))?;
    for fact in &mut taken {
        fact.scope = scope;
    }
    let existing = fact_store.list()?;
    let plan = plan_consolidation(&existing, &taken);

    if !plan.conflicts.is_empty() {
        if let Err(err) = write_conflicts(memories_root, &plan.conflicts, now) {
            tracing::warn!(error = %err, "memory: widersprüche konnten nicht geschrieben werden");
        }
    }

    let mut report = apply_plan(&fact_store, &plan)?;
    report.merged += dedupe.merged;
    report.written += dedupe.written;
    report.deleted += dedupe.deleted;
    report.conflicts += dedupe.conflicts;
    fact_store.decay(DECAY_MAX_UNUSED_DAYS, now)?;
    fact_store.write_index()?;
    write_last_consolidation_marker(memories_root, now)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::FactStore;
    use crate::test_support::{TestResult, ctx};

    fn tmp_root(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-capture-{tag}-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    // -- Lektionen -----------------------------------------------------

    #[test]
    fn error_then_success_creates_single_pitfall_candidate_with_both_args() -> TestResult {
        let root = tmp_root("lessons-resolve");
        let capture = ProjectMemoryCapture::open(&root).map_err(ctx("Capture öffnen"))?;
        let fail_args = serde_json::json!({"path": "src/lib.rs"});
        capture.record_tool_outcome(
            "sess-1",
            "cargo.check",
            &fail_args,
            true,
            "error[E0308]: mismatched types at src/lib.rs:12",
        );
        let ok_args = serde_json::json!({"path": "src/lib.rs", "fixed": true});
        capture.record_tool_outcome("sess-1", "cargo.check", &ok_args, false, "ok");

        let incoming =
            crate::extraction::IncomingStore::open(&root).map_err(ctx("IncomingStore öffnen"))?;
        let candidates = incoming.list().map_err(ctx("Kandidaten auflisten"))?;
        assert_eq!(candidates.len(), 1);
        let fact = &candidates[0];
        assert_eq!(fact.fact_type, crate::facts::FactType::Pitfall);
        assert!(fact.body.contains("Fehlgeschlagen mit"));
        assert!(fact.body.contains("Funktionierte mit"));
        assert!(fact.body.contains("fixed"));
        assert_eq!(fact.confidence, 0.5);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn transient_error_is_never_recorded_as_lesson() -> TestResult {
        let root = tmp_root("lessons-transient");
        let capture = ProjectMemoryCapture::open(&root).map_err(ctx("Capture öffnen"))?;
        capture.record_tool_outcome(
            "sess-2",
            "http.fetch",
            &serde_json::json!({}),
            true,
            "429 Too Many Requests: rate limit exceeded",
        );
        capture.flush_session("sess-2");

        let incoming =
            crate::extraction::IncomingStore::open(&root).map_err(ctx("IncomingStore öffnen"))?;
        assert!(
            incoming
                .list()
                .map_err(ctx("Kandidaten auflisten"))?
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn flush_session_writes_unresolved_failure_with_lower_confidence() -> TestResult {
        let root = tmp_root("lessons-flush");
        let capture = ProjectMemoryCapture::open(&root).map_err(ctx("Capture öffnen"))?;
        capture.record_tool_outcome(
            "sess-3",
            "cargo.check",
            &serde_json::json!({"path": "src/main.rs"}),
            true,
            "error: unresolved import `foo::bar`",
        );
        capture.flush_session("sess-3");

        let incoming =
            crate::extraction::IncomingStore::open(&root).map_err(ctx("IncomingStore öffnen"))?;
        let candidates = incoming.list().map_err(ctx("Kandidaten auflisten"))?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].confidence, 0.35);
        assert!(!candidates[0].body.contains("Funktionierte mit"));
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    // -- Recherche-Funde -------------------------------------------------

    #[test]
    fn medium_confidence_finding_with_evidence_creates_fact_candidate() -> TestResult {
        let root = tmp_root("finding-medium");
        let capture = ProjectMemoryCapture::open(&root).map_err(ctx("Capture öffnen"))?;
        let output = serde_json::json!({
            "conclusion": "jiff 0.2 ist die aktuelle Version",
            "evidence": [{"locator": "crates.io/crates/jiff", "kind": "cargo_registry_source"}],
            "confidence": "medium",
            "produced_by": "explorer-1"
        })
        .to_string();
        capture.record_tool_outcome(
            "sess-4",
            "research-deps",
            &serde_json::json!({}),
            false,
            &output,
        );

        let incoming =
            crate::extraction::IncomingStore::open(&root).map_err(ctx("IncomingStore öffnen"))?;
        let candidates = incoming.list().map_err(ctx("Kandidaten auflisten"))?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].fact_type, crate::facts::FactType::Fact);
        assert_eq!(candidates[0].confidence, 0.6);
        assert_eq!(
            candidates[0].sources,
            vec!["crates.io/crates/jiff".to_owned()]
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn low_confidence_finding_is_ignored() -> TestResult {
        let root = tmp_root("finding-low");
        let capture = ProjectMemoryCapture::open(&root).map_err(ctx("Capture öffnen"))?;
        let output = serde_json::json!({
            "conclusion": "unklar",
            "evidence": [{"locator": "web:example.com"}],
            "confidence": "low"
        })
        .to_string();
        capture.record_tool_outcome(
            "sess-5",
            "research-web",
            &serde_json::json!({}),
            false,
            &output,
        );

        let incoming =
            crate::extraction::IncomingStore::open(&root).map_err(ctx("IncomingStore öffnen"))?;
        assert!(
            incoming
                .list()
                .map_err(ctx("Kandidaten auflisten"))?
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    // -- consolidate_project_memories -----------------------------------

    #[test]
    fn consolidate_moves_incoming_candidates_into_facts() -> TestResult {
        let root = tmp_root("consolidate");
        let capture = ProjectMemoryCapture::open(&root).map_err(ctx("Capture öffnen"))?;
        let output = serde_json::json!({
            "conclusion": "harw-fsutil erzwingt symlinkfestes Öffnen",
            "evidence": [{"locator": "file:harw-fsutil/src/open.rs"}],
            "confidence": "high",
            "produced_by": "explorer-2"
        })
        .to_string();
        capture.record_tool_outcome("sess-6", "explore", &serde_json::json!({}), false, &output);

        let report = consolidate_project_memories(&root).map_err(ctx("Konsolidierung"))?;
        assert_eq!(report.written, 1);

        let incoming =
            crate::extraction::IncomingStore::open(&root).map_err(ctx("IncomingStore öffnen"))?;
        assert!(
            incoming
                .list()
                .map_err(ctx("Kandidaten auflisten"))?
                .is_empty()
        );

        let store = FactStore::open(&root, crate::facts::FactScope::Project)
            .map_err(ctx("FactStore öffnen"))?;
        let facts = store.list().map_err(ctx("Facts auflisten"))?;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].fact_type, crate::facts::FactType::Fact);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
