//! Sitzungs-Metadaten-Sidecar: `<session-id>.meta.json` neben dem Transcript.
//!
//! Spec: `/home/mia/.claude/plans/nope-permissions-gibt-es-wild-lobster.md`
//! Schritt 7 ("Session-Titel und Resume-Picker") und Zeile A7 der
//! Slice-Tabelle in `harw-scopes-contract.md` §5.
//!
//! Dieses Modul besitzt **keinen** eigenen Store-Typ, sondern eine Handvoll
//! freier Funktionen über `<root>/<session-id>.meta.json` — bewusst analog zu
//! [`crate::store::TranscriptStore::transcript_path`], damit dieselbe
//! Session-ID-Validierung gilt (ein Sidecar kann nie außerhalb von `root`
//! landen). [`SessionMeta`] trägt Titel, Zeitstempel, Projekt-Zuordnung und
//! eine gekürzte erste Nutzernachricht — alles, was der Resume-Picker
//! (`harw-tui/src/session_picker.rs`) und `harw -r` ohne einen vollen
//! Transcript-Read brauchen.
//!
//! # Ableitung für alte Sessions
//! Fehlt der Sidecar (ältere Session, oder er wurde nie geschrieben),
//! leitet [`load_or_derive`] die Metadaten aus dem Transcript selbst ab:
//! `created_at` ist der `recorded_at`-Wert des ersten Datensatzes,
//! `last_opened_at` der des letzten (Fallback: Datei-`mtime`, wenn das
//! Transcript keinen einzigen Datensatz enthält), und `first_user_message`
//! ist der Text des ersten `RecordKind::Item`-Datensatzes vom Typ
//! `"user_message"` (siehe `harw_protocol::items::TurnItem` — dieses Crate
//! bleibt bewusst unabhängig von `harw-protocol`/`harw-core` und liest das
//! Payload nur als `serde_json::Value`, wie es die Modul-Doku von
//! [`crate::record`] vorschreibt).
//!
//! ## Zählweise von `turns`
//! Kein aktueller Schreiber in diesem Repository erzeugt
//! `RecordKind::Turn`-Datensätze mit einer unterscheidbaren
//! Start-/Ende-Phase (produktiv geschrieben werden nur `RecordKind::Item` und
//! `RecordKind::Lifecycle`, siehe `harw-core/src/state_store.rs`). Diese
//! Ableitung trifft daher folgende dokumentierte Entscheidung: trägt das
//! Payload eines `RecordKind::Turn`-Datensatzes ein String-Feld `"phase"`,
//! zählt nur `"phase" == "completed"` (ein `started`/`completed`-Paar wird
//! nicht doppelt gezählt); fehlt das Feld oder ist das Payload kein Objekt,
//! zählt der Datensatz — wie es der heutigen, phasenlosen Schreibweise
//! entspricht — als ein abgeschlossener Turn.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind zustandslos (`Send + Sync`). Lesen erfolgt
//! symlinkfest über [`harw_fsutil::open_nofollow`], Schreiben atomar über
//! [`harw_fsutil::write_atomic`] mit Rechten `0600`; es gibt keinen
//! Datei-Lock, weil `save` den Sidecar immer vollständig ersetzt (kein
//! Read-Modify-Write mehrerer Schreiber auf dieselbe Datei ist vorgesehen).
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_session_store::meta;
//! use harw_types::SessionId;
//!
//! # fn demo() -> harw_session_store::SessionStoreResult<()> {
//! let root = std::path::Path::new("/tmp/harw-sessions");
//! let session = SessionId::from_str("session-a");
//! let current = meta::touch_opened(root, &session, jiff::Timestamp::now())?;
//! println!("{}", current.display_title());
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use harw_fsutil::{AtomicWriteOptions, OpenMode, is_symlink_loop, open_nofollow, write_atomic};
use harw_types::SessionId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{SessionStoreError, SessionStoreResult};
use crate::record::RecordKind;
use crate::store::TranscriptStore;

/// Aktuelle Version des Sidecar-Formats (`SessionMeta::version`).
pub const SESSION_META_VERSION: u32 = 1;

/// Dateiendung des Sidecars: `<session-id>.meta.json`.
const META_EXTENSION: &str = "meta.json";

/// Obergrenze für `first_user_message` (zeichensicher gekürzt).
const MAX_FIRST_USER_MESSAGE_CHARS: usize = 200;

/// Obergrenze für einen über [`set_title`] gesetzten Titel.
const MAX_TITLE_CHARS: usize = 80;

/// Zielbreite für [`fallback_title`] (ohne das abschließende `…`).
const FALLBACK_TITLE_CHARS: usize = 60;

/// Platzhalter-Ellipse für gekürzte Fallback-Titel.
const ELLIPSIS: char = '…';

/// Woher der aktuelle Titel einer Session stammt.
///
/// Serialisiert kleingeschrieben (`model`, `manual`, `fallback`, `none`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleSource {
    /// Vom Titel-Modell generiert (`harw-runtime/src/session_title.rs`).
    Model,
    /// Von der Nutzerin explizit gesetzt (z. B. über den Picker).
    Manual,
    /// Aus der ersten Nutzernachricht abgeleitet (`fallback_title`).
    Fallback,
    /// Noch kein Titel vergeben.
    None,
}

/// Persistente Metadaten einer Session, Sidecar zum Transcript.
///
/// # Description
/// Ein `SessionMeta` beschreibt eine Session, ohne dass ihr Transcript
/// gelesen werden muss — für den Resume-Picker und `harw -r` reicht dieser
/// eine, kleine Datensatz. `first_user_message` ist immer schon auf
/// [`MAX_FIRST_USER_MESSAGE_CHARS`] Zeichen gekürzt (zeichensicher, nie
/// mitten in einem UTF-8-Codepunkt).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Format-Version des Sidecars (derzeit immer [`SESSION_META_VERSION`]).
    pub version: u32,
    /// Session, zu der dieser Sidecar gehört.
    pub session_id: SessionId,
    /// Von der Nutzerin oder dem Titel-Modell gesetzter Titel, falls vorhanden.
    pub title: Option<String>,
    /// Herkunft von `title`.
    pub title_source: TitleSource,
    /// Zeitpunkt des ersten Datensatzes der Session.
    pub created_at: Timestamp,
    /// Zeitpunkt, zu dem die Session zuletzt geöffnet/fortgesetzt wurde.
    pub last_opened_at: Timestamp,
    /// Arbeitsverzeichnis, aus dem die Session gestartet wurde.
    pub cwd: Option<PathBuf>,
    /// Kanonische Projekt-Wurzel (`harw-home::project::ProjectRoot::root`).
    pub project_root: Option<PathBuf>,
    /// Stabiler Projekt-Schlüssel (`harw-home::project::project_key`).
    pub project_key: Option<String>,
    /// Erste Nutzernachricht der Session, auf höchstens
    /// [`MAX_FIRST_USER_MESSAGE_CHARS`] Zeichen gekürzt.
    pub first_user_message: Option<String>,
    /// Anzahl der Turns dieser Session (Zählweise: siehe Modul-Doku).
    pub turns: u64,
    /// Anzahl der über [`add_usage_round`] aufgezeichneten Nutzungsrunden.
    #[serde(default)]
    pub usage_rounds: u64,
    /// Über alle Runden dieser Session aufsummierte Token-Nutzung
    /// ([`harw_types::TokenUsage::add`]).
    #[serde(default)]
    pub total_usage: harw_types::TokenUsage,
    /// Wächter-Ereignisse dieser Session (Addendum F+G), gezählt je Art.
    ///
    /// Schlüssel ist [`harw_core::guard::DriftKind::key`] (z. B.
    /// `"repeated_failing_call"`); Wert ist die Anzahl, wie oft
    /// [`add_drift_event`] mit dieser Art aufgerufen wurde. Dieses Crate
    /// bleibt bewusst unabhängig von `harw-core` (siehe Modul-Doku), daher
    /// wird die Art hier als `String` geführt statt als `DriftKind`.
    #[serde(default)]
    pub drift_events: BTreeMap<String, u64>,
}

impl SessionMeta {
    /// Baut einen leeren Metadatensatz für eine Session ohne jede Historie.
    ///
    /// `created_at` und `last_opened_at` erhalten beide `now`; jeder weitere
    /// Aufruf von [`touch_opened`] verschiebt nur noch `last_opened_at`.
    fn fresh(session_id: SessionId, now: Timestamp) -> Self {
        Self {
            version: SESSION_META_VERSION,
            session_id,
            title: None,
            title_source: TitleSource::None,
            created_at: now,
            last_opened_at: now,
            cwd: None,
            project_root: None,
            project_key: None,
            first_user_message: None,
            turns: 0,
            usage_rounds: 0,
            total_usage: harw_types::TokenUsage::default(),
            drift_events: BTreeMap::new(),
        }
    }

    /// Anzeigbarer Titel: gesetzter Titel, sonst aus der ersten
    /// Nutzernachricht abgeleitet, sonst `"(ohne Titel)"`.
    ///
    /// # Returns
    /// Nie leer — der Resume-Picker kann das Ergebnis ungeprüft anzeigen.
    #[must_use]
    pub fn display_title(&self) -> String {
        if let Some(title) = self.title.as_ref().filter(|title| !title.is_empty()) {
            return title.to_owned();
        }
        if let Some(first_message) = self.first_user_message.as_deref() {
            let derived = fallback_title(first_message);
            if !derived.is_empty() {
                return derived;
            }
        }
        "(ohne Titel)".to_owned()
    }
}

/// Leitet den Sidecar-Pfad `<root>/<id>.meta.json` her.
///
/// # Description
/// Rein syntaktisch (keine I/O, kein Fehlerfall) — die Sicherheitsprüfung
/// der Session-ID passiert in [`load`]/[`save`] über
/// [`TranscriptStore::transcript_path`], damit dieselben Regeln wie für das
/// Transcript selbst gelten und nirgends dupliziert werden müssen.
#[must_use]
pub fn meta_path(root: &Path, id: &SessionId) -> PathBuf {
    root.join(format!("{}.{META_EXTENSION}", id.as_str()))
}

/// Prüft, dass `id` als Transcript-Pfad unterhalb von `root` adressierbar
/// wäre, und lehnt damit dieselben unsicheren IDs ab wie
/// [`TranscriptStore::transcript_path`] (z. B. `../escape`).
fn ensure_addressable(root: &Path, id: &SessionId) -> SessionStoreResult<()> {
    TranscriptStore::new(root).transcript_path(id).map(|_| ())
}

/// Liest den Sidecar einer Session, falls einer existiert und lesbar ist.
///
/// # Description
/// Öffnet symlinkfest über [`harw_fsutil::open_nofollow`]: ein Sidecar, der
/// (oder dessen letztes Pfadglied) ein Symlink ist, wird **nicht** gefolgt
/// und wie ein fehlender Sidecar behandelt (`Ok(None)`, mit `tracing::warn!`).
/// Ebenso wird nicht dekodierbarer Inhalt als fehlend behandelt, nie als
/// Fehler — der Aufrufer (typischerweise [`load_or_derive`]) leitet in
/// beiden Fällen frisch aus dem Transcript ab, statt eine ganze Session
/// unbenutzbar zu machen.
///
/// # Errors
/// - [`SessionStoreError::UnsafeTranscriptPath`]: `id` ist keine gültige
///   Transcript-Session-ID.
/// - [`SessionStoreError::Io`]: sonstiger Lesefehler (z. B. Rechteproblem).
pub fn load(root: &Path, id: &SessionId) -> SessionStoreResult<Option<SessionMeta>> {
    ensure_addressable(root, id)?;
    let path = meta_path(root, id);
    let mut file = match open_nofollow(&path, OpenMode::read_only()) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if is_symlink_loop(&error) => {
            tracing::warn!(
                session = %id,
                path = %path.display(),
                "session meta sidecar is a symlink; ignored"
            );
            return Ok(None);
        }
        Err(error) => return Err(SessionStoreError::Io(error)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(SessionStoreError::Io)?;
    match serde_json::from_slice::<SessionMeta>(&bytes) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) => {
            tracing::warn!(
                session = %id,
                path = %path.display(),
                error = %error,
                "session meta sidecar is malformed; will be re-derived"
            );
            Ok(None)
        }
    }
}

/// Schreibt den Sidecar einer Session atomar und mit Rechten `0600`.
///
/// # Description
/// Nutzt [`harw_fsutil::write_atomic`] (Tempdatei im selben Verzeichnis,
/// `fsync`, `rename`): ein vorhandenes Ziel — auch ein Symlink — wird
/// ersetzt, nie gefolgt.
///
/// # Errors
/// - [`SessionStoreError::UnsafeTranscriptPath`]: `meta.session_id` ist
///   keine gültige Transcript-Session-ID.
/// - [`SessionStoreError::Io`]: Anlegen des Verzeichnisses oder atomares
///   Schreiben ist gescheitert.
/// - [`SessionStoreError::Serde`]: `meta` ließ sich nicht kodieren.
pub fn save(root: &Path, meta: &SessionMeta) -> SessionStoreResult<()> {
    ensure_addressable(root, &meta.session_id)?;
    let path = meta_path(root, &meta.session_id);
    let parent = path.parent().ok_or_else(|| {
        SessionStoreError::Io(std::io::Error::other("session meta path has no parent"))
    })?;
    std::fs::create_dir_all(parent).map_err(SessionStoreError::Io)?;
    let bytes = serde_json::to_vec(meta)?;
    write_atomic(&path, &bytes, AtomicWriteOptions::private()).map_err(SessionStoreError::Io)
}

/// Liefert den Sidecar einer Session, oder leitet ihn aus dem Transcript ab.
///
/// # Description
/// Existiert ein gültiger Sidecar (siehe [`load`]), wird er unverändert
/// zurückgegeben. Sonst wird das Transcript sequenziell gelesen: `created_at`
/// kommt vom ersten Datensatz, `last_opened_at` vom letzten (fehlt jeder
/// Datensatz — leeres oder fehlendes Transcript — fällt beides auf die
/// Datei-`mtime`, sonst auf `Timestamp::now()` zurück), `first_user_message`
/// vom ersten `RecordKind::Item`-Datensatz vom Typ `"user_message"`
/// (gekürzt, siehe Modul-Doku), und `turns` zählt `RecordKind::Turn`-Datensätze
/// (Zählweise siehe Modul-Doku). Das Ergebnis wird anschließend best-effort
/// gespeichert ([`save`]-Fehler werden nur mit `tracing::warn!` gemeldet,
/// nie propagiert), damit der nächste Aufruf den Sidecar direkt liest.
///
/// # Errors
/// - [`SessionStoreError::UnsafeTranscriptPath`]: `id` ist keine gültige
///   Transcript-Session-ID.
/// - [`SessionStoreError::CorruptRecord`], [`SessionStoreError::Io`]: das
///   Transcript selbst ist nicht lesbar (defekter Datensatz, Symlink, …).
pub fn load_or_derive(root: &Path, id: &SessionId) -> SessionStoreResult<SessionMeta> {
    if let Some(meta) = load(root, id)? {
        return Ok(meta);
    }
    let derived = derive_from_transcript(root, id)?;
    if let Err(error) = save(root, &derived) {
        tracing::warn!(
            session = %id,
            error = %error,
            "failed to persist derived session meta sidecar; continuing without it"
        );
    }
    Ok(derived)
}

// Kern von `load_or_derive`, wenn kein gültiger Sidecar existiert.
fn derive_from_transcript(root: &Path, id: &SessionId) -> SessionStoreResult<SessionMeta> {
    let store = TranscriptStore::new(root);
    let transcript_path = store.transcript_path(id)?;
    let now = Timestamp::now();

    let reader = match store.reader(id) {
        Ok(reader) => reader,
        Err(SessionStoreError::NotFound { .. }) => {
            return Ok(SessionMeta::fresh(id.clone(), now));
        }
        Err(error) => return Err(error),
    };

    let mut created_at: Option<Timestamp> = None;
    let mut last_recorded_at: Option<Timestamp> = None;
    let mut first_user_message: Option<String> = None;
    let mut turns: u64 = 0;

    for record in reader {
        let record = record?;
        if created_at.is_none() {
            created_at = Some(record.recorded_at);
        }
        last_recorded_at = Some(record.recorded_at);

        if first_user_message.is_none() && record.kind == RecordKind::Item {
            if let Some(text) = extract_user_message_text(&record.payload) {
                first_user_message = Some(truncate_chars(&text, MAX_FIRST_USER_MESSAGE_CHARS));
            }
        }
        if record.kind == RecordKind::Turn && counts_as_completed_turn(&record.payload) {
            turns += 1;
        }
    }

    let fallback_time = || mtime_or(&transcript_path, now);
    let created_at = created_at.unwrap_or_else(fallback_time);
    let last_opened_at = last_recorded_at.unwrap_or_else(fallback_time);

    Ok(SessionMeta {
        version: SESSION_META_VERSION,
        session_id: id.clone(),
        title: None,
        title_source: TitleSource::None,
        created_at,
        last_opened_at,
        cwd: None,
        project_root: None,
        project_key: None,
        first_user_message,
        turns,
        usage_rounds: 0,
        total_usage: harw_types::TokenUsage::default(),
        drift_events: BTreeMap::new(),
    })
}

// Extrahiert den zusammengefügten Text aller Textteile eines
// `{"type": "user_message", "content": [...]}`-Payloads (die intern getaggte
// Wire-Form von `harw_protocol::items::TurnItem::UserMessage` — dieses Crate
// deserialisiert absichtlich nicht in den echten Typ, siehe Modul-Doku).
// `None`, wenn das Payload kein Nutzer-Item ist oder keinen Text enthält.
fn extract_user_message_text(payload: &serde_json::Value) -> Option<String> {
    if payload.get("type").and_then(serde_json::Value::as_str) != Some("user_message") {
        return None;
    }
    let parts = payload.get("content")?.as_array()?;
    let mut text = String::new();
    for part in parts {
        if part.get("type").and_then(serde_json::Value::as_str) != Some("text") {
            continue;
        }
        if let Some(part_text) = part.get("text").and_then(serde_json::Value::as_str) {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(part_text);
        }
    }
    if text.is_empty() { None } else { Some(text) }
}

// Zählweise für `RecordKind::Turn` (siehe Modul-Doku "Zählweise von `turns`").
fn counts_as_completed_turn(payload: &serde_json::Value) -> bool {
    match payload.get("phase").and_then(serde_json::Value::as_str) {
        Some(phase) => phase == "completed",
        None => true,
    }
}

// `mtime` von `path`, oder `fallback`, wenn die Datei fehlt/unlesbar ist oder
// ihre Zeit sich nicht in einen `jiff::Timestamp` übersetzen lässt.
fn mtime_or(path: &Path, fallback: Timestamp) -> Timestamp {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| Timestamp::try_from(modified).ok())
        .unwrap_or(fallback)
}

/// Aktualisiert `last_opened_at` einer Session und speichert den Sidecar.
///
/// # Description
/// Lädt zunächst über [`load_or_derive`] (leitet also bei Bedarf ab), setzt
/// dann `last_opened_at` auf `now` und schreibt den Sidecar durch.
///
/// # Arguments
/// - `now` (`jiff::Timestamp`): Server-/Aufrufer-Uhrzeit; wird nicht selbst
///   erhoben, damit Aufrufer eine `Clock`-Abstraktion einsetzen können.
///
/// # Errors
/// Wie [`load_or_derive`] und [`save`].
pub fn touch_opened(root: &Path, id: &SessionId, now: Timestamp) -> SessionStoreResult<SessionMeta> {
    let mut meta = load_or_derive(root, id)?;
    meta.last_opened_at = now;
    save(root, &meta)?;
    Ok(meta)
}

/// Setzt einen Titel (normalisiert) und dessen Herkunft, dann speichert.
///
/// # Description
/// `title` wird auf zusammenhängenden Text normalisiert (führende/folgende
/// Leerzeichen entfernt, innere Lauf von Leerraum zu einem einzelnen
/// Leerzeichen kollabiert) und danach zeichensicher auf höchstens
/// [`MAX_TITLE_CHARS`] Zeichen gekürzt.
///
/// # Errors
/// Wie [`load_or_derive`] und [`save`].
pub fn set_title(
    root: &Path,
    id: &SessionId,
    title: &str,
    source: TitleSource,
) -> SessionStoreResult<SessionMeta> {
    let mut meta = load_or_derive(root, id)?;
    meta.title = Some(truncate_chars(&normalize_whitespace(title), MAX_TITLE_CHARS));
    meta.title_source = source;
    save(root, &meta)?;
    Ok(meta)
}

/// Setzt die Projekt-Zuordnung einer Session (`cwd`, `project_root`,
/// `project_key`) und speichert. Jedes `None` löscht das jeweilige Feld.
///
/// # Errors
/// Wie [`load_or_derive`] und [`save`].
pub fn set_project(
    root: &Path,
    id: &SessionId,
    cwd: Option<&Path>,
    project_root: Option<&Path>,
    project_key: Option<&str>,
) -> SessionStoreResult<SessionMeta> {
    let mut meta = load_or_derive(root, id)?;
    meta.cwd = cwd.map(Path::to_path_buf);
    meta.project_root = project_root.map(Path::to_path_buf);
    meta.project_key = project_key.map(str::to_owned);
    save(root, &meta)?;
    Ok(meta)
}

/// Zeichnet eine weitere Nutzungsrunde einer Session auf und speichert.
///
/// # Description
/// Lädt zunächst über [`load_or_derive`] (leitet also bei Bedarf ab), erhöht
/// dann sowohl `usage_rounds` als auch `turns` um je `1` und akkumuliert
/// `usage` in `total_usage` über [`harw_types::TokenUsage::add`], bevor der
/// Sidecar gespeichert wird. `turns` wird bewusst mitgezählt: eine
/// aufgezeichnete Nutzungsrunde ist immer auch ein abgeschlossener Turn (siehe
/// Modul-Doku „Zählweise von `turns`").
///
/// # Arguments
/// - `usage` (`&harw_types::TokenUsage`): Nutzung der aktuellen Runde, wird
///   additiv in `total_usage` übernommen.
///
/// # Errors
/// Wie [`load_or_derive`] und [`save`].
pub fn add_usage_round(
    root: &Path,
    id: &SessionId,
    usage: &harw_types::TokenUsage,
) -> SessionStoreResult<SessionMeta> {
    let mut meta = load_or_derive(root, id)?;
    meta.usage_rounds += 1;
    meta.turns += 1;
    meta.total_usage.add(usage);
    save(root, &meta)?;
    Ok(meta)
}

/// Zeichnet ein weiteres Wächter-Ereignis einer Session auf und speichert
/// (Addendum F+G, Agent F-FIX).
///
/// # Description
/// Lädt zunächst über [`load_or_derive`] (leitet also bei Bedarf ab), erhöht
/// dann `meta.drift_events[kind]` um `1` (legt den Eintrag mit `1` an, falls
/// die Art noch nicht vorkam), und speichert den Sidecar — dasselbe Muster
/// wie [`add_usage_round`]. Der Aufrufer (`harw-core`,
/// `TranscriptStateStore::record_drift`) behandelt einen Fehler hier als
/// Best-Effort-Fehlschlag: das bereits durabel gespeicherte Ereignis bleibt
/// gültig, nur der Sidecar hinkt hinterher.
///
/// # Arguments
/// - `kind` (`&str`): die Wächter-Art, üblicherweise
///   `harw_core::guard::DriftKind::key()`.
///
/// # Errors
/// Wie [`load_or_derive`] und [`save`].
pub fn add_drift_event(root: &Path, id: &SessionId, kind: &str) -> SessionStoreResult<SessionMeta> {
    let mut meta = load_or_derive(root, id)?;
    *meta.drift_events.entry(kind.to_owned()).or_insert(0) += 1;
    save(root, &meta)?;
    Ok(meta)
}

/// Leitet einen Fallback-Titel aus der ersten Nutzernachricht ab.
///
/// # Description
/// Nimmt nur die erste Zeile, normalisiert Leerraum und kürzt zeichensicher
/// auf [`FALLBACK_TITLE_CHARS`] Zeichen mit abschließendem `…`, falls nötig.
/// Eine leere oder nur aus Leerraum bestehende Nachricht ergibt einen leeren
/// String — [`SessionMeta::display_title`] fällt dann auf den Platzhalter
/// zurück, statt einen leeren Titel anzuzeigen.
#[must_use]
pub fn fallback_title(first_user_message: &str) -> String {
    let first_line = first_user_message.lines().next().unwrap_or("");
    let normalized = normalize_whitespace(first_line);
    if normalized.is_empty() {
        return String::new();
    }
    if normalized.chars().count() <= FALLBACK_TITLE_CHARS {
        return normalized;
    }
    let mut truncated = truncate_chars(&normalized, FALLBACK_TITLE_CHARS);
    truncated.push(ELLIPSIS);
    truncated
}

// Kollabiert jeden Lauf von Leerraum (inkl. führendem/folgendem) zu einem
// einzelnen ASCII-Leerzeichen.
fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Kürzt `input` auf höchstens `max_chars` Unicode-Codepoints — nie mitten in
// einem Mehrbyte-Zeichen, anders als eine Byte-Kürzung.
fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        input.to_owned()
    } else {
        input.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::ThreadRef;

    use crate::record::TranscriptRecord;

    fn user_message_payload(text: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "user_message",
            "id": "item-1",
            "content": [{ "type": "text", "text": text }],
        })
    }

    #[test]
    fn save_and_load_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");
        let meta = SessionMeta {
            version: SESSION_META_VERSION,
            session_id: session.clone(),
            title: Some("Mein Titel".to_owned()),
            title_source: TitleSource::Manual,
            created_at: Timestamp::now(),
            last_opened_at: Timestamp::now(),
            cwd: Some(PathBuf::from("/home/mia/projects/harwness")),
            project_root: Some(PathBuf::from("/home/mia/projects/harwness")),
            project_key: Some("harwness-abc123".to_owned()),
            first_user_message: Some("Hallo".to_owned()),
            turns: 3,
            usage_rounds: 2,
            total_usage: harw_types::TokenUsage {
                input_tokens: 100,
                output_tokens: 40,
                reasoning_tokens: None,
                cached_tokens: Some(10),
                cache_write_tokens: Some(5),
            },
            drift_events: BTreeMap::from([("repeated_failing_call".to_owned(), 2)]),
        };

        save(temp.path(), &meta).unwrap();
        let loaded = load(temp.path(), &session).unwrap();

        assert_eq!(loaded, Some(meta));
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_a_private_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");
        let meta = SessionMeta::fresh(session.clone(), Timestamp::now());

        save(temp.path(), &meta).unwrap();

        let mode = std::fs::metadata(meta_path(temp.path(), &session))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn load_missing_sidecar_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");

        assert_eq!(load(temp.path(), &session).unwrap(), None);
    }

    #[test]
    fn load_or_derive_extracts_first_user_message_and_persists_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let session = SessionId::from_str("session-a");
        let thread = ThreadRef::from_str("root");
        store
            .append(&TranscriptRecord::new(
                session.clone(),
                thread.clone(),
                0,
                Timestamp::now(),
                RecordKind::Item,
                user_message_payload("Hallo Welt, wie geht es dir heute?"),
            ))
            .unwrap();
        store
            .append(&TranscriptRecord::new(
                session.clone(),
                thread,
                1,
                Timestamp::now(),
                RecordKind::Item,
                serde_json::json!({
                    "type": "assistant_message",
                    "id": "item-2",
                    "content": [{ "type": "text", "text": "Mir geht es gut." }],
                }),
            ))
            .unwrap();

        assert!(!meta_path(temp.path(), &session).exists());
        let meta = load_or_derive(temp.path(), &session).unwrap();

        assert_eq!(
            meta.first_user_message.as_deref(),
            Some("Hallo Welt, wie geht es dir heute?")
        );
        assert_eq!(meta.title, None);
        assert_eq!(meta.title_source, TitleSource::None);
        assert_eq!(meta.turns, 0);
        assert!(
            meta_path(temp.path(), &session).exists(),
            "load_or_derive must persist the derived sidecar"
        );
        assert_eq!(load(temp.path(), &session).unwrap(), Some(meta));
    }

    #[test]
    fn load_or_derive_without_a_transcript_returns_a_fresh_meta() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");

        let meta = load_or_derive(temp.path(), &session).unwrap();

        assert_eq!(meta.created_at, meta.last_opened_at);
        assert_eq!(meta.first_user_message, None);
        assert_eq!(meta.turns, 0);
    }

    #[test]
    fn load_or_derive_counts_turn_records_respecting_phase_when_present() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(temp.path());
        let session = SessionId::from_str("session-a");
        let thread = ThreadRef::from_str("root");
        // Kein `phase`-Feld: zählt wie die heutige, phasenlose Schreibweise.
        store
            .append(&TranscriptRecord::new(
                session.clone(),
                thread.clone(),
                0,
                Timestamp::now(),
                RecordKind::Turn,
                serde_json::json!({ "sequence": 0 }),
            ))
            .unwrap();
        // Started/completed-Paar: nur "completed" zählt, kein Doppelzählen.
        store
            .append(&TranscriptRecord::new(
                session.clone(),
                thread.clone(),
                1,
                Timestamp::now(),
                RecordKind::Turn,
                serde_json::json!({ "phase": "started" }),
            ))
            .unwrap();
        store
            .append(&TranscriptRecord::new(
                session.clone(),
                thread,
                2,
                Timestamp::now(),
                RecordKind::Turn,
                serde_json::json!({ "phase": "completed" }),
            ))
            .unwrap();

        let meta = load_or_derive(temp.path(), &session).unwrap();

        assert_eq!(meta.turns, 2);
    }

    #[test]
    fn touch_opened_updates_last_opened_at_and_persists() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");
        let initial = load_or_derive(temp.path(), &session).unwrap();
        let later = initial
            .created_at
            .checked_add(jiff::SignedDuration::from_secs(60))
            .unwrap();

        let touched = touch_opened(temp.path(), &session, later).unwrap();

        assert_eq!(touched.last_opened_at, later);
        assert_eq!(touched.created_at, initial.created_at);
        let reloaded = load(temp.path(), &session).unwrap().unwrap();
        assert_eq!(reloaded.last_opened_at, later);
    }

    #[test]
    fn set_title_normalizes_whitespace_and_enforces_max_length() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");
        let messy = format!("  Hallo   {}  Welt  ", "x".repeat(90));

        let meta = set_title(temp.path(), &session, &messy, TitleSource::Model).unwrap();

        assert_eq!(meta.title_source, TitleSource::Model);
        let title = meta.title.unwrap();
        assert!(!title.contains("  "));
        assert!(!title.starts_with(' ') && !title.ends_with(' '));
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn set_project_updates_and_clears_fields() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");
        let cwd = PathBuf::from("/home/mia/projects/harwness/sub");
        let root = PathBuf::from("/home/mia/projects/harwness");

        let meta = set_project(
            temp.path(),
            &session,
            Some(cwd.as_path()),
            Some(root.as_path()),
            Some("harwness-abc123"),
        )
        .unwrap();
        assert_eq!(meta.cwd.as_deref(), Some(cwd.as_path()));
        assert_eq!(meta.project_root.as_deref(), Some(root.as_path()));
        assert_eq!(meta.project_key.as_deref(), Some("harwness-abc123"));

        let cleared = set_project(temp.path(), &session, None, None, None).unwrap();
        assert_eq!(cleared.cwd, None);
        assert_eq!(cleared.project_root, None);
        assert_eq!(cleared.project_key, None);
    }

    #[test]
    fn fallback_title_truncates_first_line_char_safely() {
        let long_line = "x".repeat(90);
        let message = format!("{long_line}\nzweite Zeile wird ignoriert");

        let title = fallback_title(&message);

        assert_eq!(title.chars().count(), FALLBACK_TITLE_CHARS + 1);
        assert!(title.ends_with(ELLIPSIS));
    }

    #[test]
    fn fallback_title_of_blank_message_is_empty() {
        assert_eq!(fallback_title("   \n\t  "), "");
    }

    #[test]
    fn display_title_prefers_title_then_fallback_then_placeholder() {
        let session = SessionId::from_str("session-a");
        let mut meta = SessionMeta::fresh(session, Timestamp::now());

        assert_eq!(meta.display_title(), "(ohne Titel)");

        meta.first_user_message = Some("Hallo Welt".to_owned());
        assert_eq!(meta.display_title(), "Hallo Welt");

        meta.title = Some("Eigener Titel".to_owned());
        assert_eq!(meta.display_title(), "Eigener Titel");
    }

    #[test]
    fn malformed_sidecar_is_ignored_and_rederived() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");
        std::fs::write(meta_path(temp.path(), &session), b"{ not json").unwrap();

        assert_eq!(load(temp.path(), &session).unwrap(), None);

        let derived = load_or_derive(temp.path(), &session).unwrap();
        assert_eq!(derived.title, None);
        assert_eq!(derived.turns, 0);
        // `load_or_derive` must have overwritten the malformed file.
        assert_eq!(load(temp.path(), &session).unwrap(), Some(derived));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_sidecar_is_not_followed_for_read_or_write() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");
        let path = meta_path(temp.path(), &session);
        let external = temp.path().join("outside-meta.json");
        std::fs::write(&external, b"sentinel-should-not-be-read").unwrap();
        symlink(&external, &path).unwrap();

        assert_eq!(load(temp.path(), &session).unwrap(), None);
        assert_eq!(
            std::fs::read(&external).unwrap(),
            b"sentinel-should-not-be-read"
        );

        let meta = SessionMeta::fresh(session.clone(), Timestamp::now());
        save(temp.path(), &meta).unwrap();

        assert!(!std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read(&external).unwrap(),
            b"sentinel-should-not-be-read"
        );
        assert_eq!(load(temp.path(), &session).unwrap(), Some(meta));
    }

    #[test]
    fn add_usage_round_accumulates_usage_and_increments_counters() {
        let temp = tempfile::tempdir().unwrap();
        let session = SessionId::from_str("session-a");
        let first = harw_types::TokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            reasoning_tokens: None,
            cached_tokens: Some(10),
            cache_write_tokens: None,
        };
        let second = harw_types::TokenUsage {
            input_tokens: 50,
            output_tokens: 5,
            reasoning_tokens: None,
            cached_tokens: Some(5),
            cache_write_tokens: Some(3),
        };

        let after_first = add_usage_round(temp.path(), &session, &first).unwrap();
        assert_eq!(after_first.usage_rounds, 1);
        assert_eq!(after_first.turns, 1);
        assert_eq!(after_first.total_usage, first);

        let after_second = add_usage_round(temp.path(), &session, &second).unwrap();
        assert_eq!(after_second.usage_rounds, 2);
        assert_eq!(after_second.turns, 2);
        assert_eq!(after_second.total_usage.input_tokens, 150);
        assert_eq!(after_second.total_usage.output_tokens, 25);
        assert_eq!(after_second.total_usage.cached_tokens, Some(15));
        assert_eq!(after_second.total_usage.cache_write_tokens, Some(3));

        let reloaded = load(temp.path(), &session).unwrap().unwrap();
        assert_eq!(reloaded, after_second);
    }

    #[test]
    fn unsafe_session_id_is_rejected_by_load_and_save() {
        let temp = tempfile::tempdir().unwrap();
        let unsafe_session = SessionId::from_str("../escape");

        assert!(matches!(
            load(temp.path(), &unsafe_session),
            Err(SessionStoreError::UnsafeTranscriptPath(value)) if value == "../escape"
        ));
        let meta = SessionMeta::fresh(unsafe_session.clone(), Timestamp::now());
        assert!(matches!(
            save(temp.path(), &meta),
            Err(SessionStoreError::UnsafeTranscriptPath(value)) if value == "../escape"
        ));
    }
}
