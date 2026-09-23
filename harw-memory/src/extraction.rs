//! Extraktion (Phase 1) des Langzeitgedächtnisses, siehe
//! `docs/design/memory-v3-ltm.md` §5.2 (Extraktion nach der Session), §5.4
//! (Verdrängung, für die Zeit-/Marker-Semantik) und §7 (Invarianten).
//!
//! # Verantwortungsbereich
//! Dieses Modul enthält **nur** die reine, deterministische Logik der
//! Phase-1-Extraktion — keinen Modellaufruf, keinen Netzzugriff. Der
//! eigentliche Aufruf des Sprachmodells (`harw_core::one_shot::complete_text`)
//! wird von einem anderen Slice verdrahtet: dieses Modul liefert dafür nur
//! die Bausteine:
//! - [`ExtractionCandidate`], [`ExtractionPolicy`] — Eingabedaten und
//!   Grenzwerte für die Sessionauswahl.
//! - [`select_sessions`] — welche abgeschlossenen Sessions werden extrahiert.
//! - [`TranscriptEntry`], [`EntryRole`], [`build_input`] — Filterung und
//!   Kürzung des Modell-Eingabetexts.
//! - [`system_prompt`], [`user_prompt`] — die Prompt-Bausteine nach §5.2.
//! - [`parse_response`] — toleranter Parser der Modellantwort zu [`Fact`]en.
//! - [`IncomingStore`] — Ablage der Kandidaten unter `facts/_incoming/` samt
//!   Extraktions-Marker (`extraction_state.json`).
//!
//! # Nebenläufigkeit
//! Alle freien Funktionen sind zustandslos und `Send + Sync`. [`IncomingStore`]
//! schreibt ausschließlich über [`harw_fsutil::write_atomic`] (Tempdatei +
//! `fsync` + `rename`) und ist damit für sich genommen atomar; es serialisiert
//! aber **nicht** zwischen mehreren Prozessen — das ist Aufgabe des
//! Aufrufers, analog zu `FactStore` (siehe `crate::facts`-Moduldoku).
//!
//! # Fehler
//! [`ExtractionError`] — I/O-Fehler unter der Extraktions-Wurzel, JSON-Fehler
//! beim Parsen der Modellantwort oder des Marker-Zustands, sowie der Fall,
//! dass in der Modellantwort keine JSON-Struktur gefunden wurde.
//!
//! # Beispiel
//! ```no_run
//! use harw_memory::extraction::{
//!     ExtractionPolicy, IncomingStore, parse_response, system_prompt, user_prompt,
//! };
//! use harw_memory::facts::FactScope;
//! use time::OffsetDateTime;
//!
//! let policy = ExtractionPolicy::default();
//! let _system = system_prompt();
//! let _user = user_prompt("User: Beispieltext\n");
//! // `response_json` käme vom Modellaufruf eines anderen Slices.
//! let response_json = r#"{"facts": []}"#;
//! let facts = parse_response(
//!     response_json,
//!     &policy,
//!     FactScope::Project,
//!     OffsetDateTime::now_utc(),
//!     "01H-example",
//! )
//! .unwrap();
//! let store = IncomingStore::open("/tmp/harw-mem-example").unwrap();
//! store.write_candidates(&facts).unwrap();
//! ```

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use crate::facts::{Fact, FactScope, FactType, redact, slugify};

// ---------------------------------------------------------------------
// Fehler
// ---------------------------------------------------------------------

/// Fehler der Phase-1-Extraktion.
///
/// # Beschreibung
/// Trägt genügend Kontext, um die Ursache ohne Sourcen-Blick zu verstehen.
/// `Debug` delegiert auf `Display` — eine Implementierung, keine Dopplung.
///
/// # Nebenläufigkeit
/// `Send + Sync + 'static`.
#[non_exhaustive]
pub enum ExtractionError {
    /// I/O-Fehler beim Lesen/Schreiben unter der Extraktions-Wurzel.
    Io {
        /// Betroffener Pfad.
        path: PathBuf,
        /// Ursache.
        source: io::Error,
    },
    /// JSON-Fehler beim Parsen der Modellantwort oder von
    /// `extraction_state.json`.
    Json {
        /// Ursache.
        source: serde_json::Error,
    },
    /// In der Modellantwort wurde keine JSON-Struktur gefunden (weder direkt
    /// noch in einem Markdown-Codefence).
    NoJsonFound,
}

impl fmt::Display for ExtractionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "Extraktion: I/O-Fehler bei {}: {source}", path.display())
            }
            Self::Json { source } => write!(f, "Extraktion: JSON-Fehler: {source}"),
            Self::NoJsonFound => write!(
                f,
                "Extraktion: keine JSON-Struktur in der Modellantwort gefunden"
            ),
        }
    }
}

impl fmt::Debug for ExtractionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ExtractionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source } => Some(source),
            Self::NoJsonFound => None,
        }
    }
}

// ---------------------------------------------------------------------
// Sessionauswahl (§5.2)
// ---------------------------------------------------------------------

/// Ein abgeschlossenes Transcript als Kandidat für die Extraktion.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractionCandidate {
    /// Eindeutige Session-Kennung.
    pub session_id: String,
    /// Zeitpunkt der letzten Aktivität in der Session (Ende, nicht Start).
    pub recorded_at: OffsetDateTime,
    /// Größe des Transcripts in Bytes (informativ, fließt nicht in die
    /// Auswahl ein — nur `build_input` kürzt tatsächlich).
    pub bytes: usize,
}

/// Grenzwerte der Extraktion, siehe Design §5.2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractionPolicy {
    /// Mindest-Ruhezeit seit `recorded_at`, bevor eine Session als
    /// abgeschlossen gilt (Default 300 s).
    pub idle_min_secs: u64,
    /// Höchstalter einer Session, ab dem sie nicht mehr extrahiert wird
    /// (Default 30 Tage).
    pub max_age_days: u64,
    /// Höchstzahl an Sessions je Extraktionslauf (Default 5).
    pub max_sessions_per_run: usize,
    /// Höchstgröße des gefilterten Eingabetexts in Bytes (Default 30 KiB).
    pub max_input_bytes: usize,
    /// Höchstzahl an Fakten je Session (Default 5).
    pub max_facts_per_session: usize,
}

impl Default for ExtractionPolicy {
    /// Defaults aus Design §5.2: 300 s Ruhezeit, 30 Tage Höchstalter, 5
    /// Sessions je Lauf, 30 KiB Eingabe, 5 Fakten je Session.
    fn default() -> Self {
        Self {
            idle_min_secs: 300,
            max_age_days: 30,
            max_sessions_per_run: 5,
            max_input_bytes: 30 * 1024,
            max_facts_per_session: 5,
        }
    }
}

/// Wählt die für diesen Lauf zu extrahierenden Sessions aus, siehe Design
/// §5.2.
///
/// # Beschreibung
/// Filtert `candidates` auf: noch nicht in `already_done` markiert, mindestens
/// `policy.idle_min_secs` seit `recorded_at` vergangen, und höchstens
/// `policy.max_age_days` alt. Die verbleibenden Kandidaten werden nach
/// `recorded_at` absteigend (neueste zuerst) sortiert und auf
/// `policy.max_sessions_per_run` begrenzt.
///
/// # Returns
/// Die `session_id`s der ausgewählten Sessions, neueste zuerst.
#[must_use]
pub fn select_sessions(
    candidates: &[ExtractionCandidate],
    already_done: &BTreeSet<String>,
    now: OffsetDateTime,
    policy: &ExtractionPolicy,
) -> Vec<String> {
    let mut selected: Vec<&ExtractionCandidate> = candidates
        .iter()
        .filter(|c| !already_done.contains(&c.session_id))
        .filter(|c| {
            let idle_secs = (now - c.recorded_at).whole_seconds();
            idle_secs >= policy.idle_min_secs as i64
        })
        .filter(|c| {
            let age_days = (now - c.recorded_at).whole_days();
            (0..=policy.max_age_days as i64).contains(&age_days)
        })
        .collect();
    selected.sort_by_key(|candidate| std::cmp::Reverse(candidate.recorded_at));
    selected.truncate(policy.max_sessions_per_run);
    selected.into_iter().map(|c| c.session_id.clone()).collect()
}

// ---------------------------------------------------------------------
// Eingabefilterung (§5.2)
// ---------------------------------------------------------------------

/// Rolle eines Transcript-Eintrags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryRole {
    /// Nutzernachricht.
    User,
    /// Antwort des Assistenten.
    Assistant,
    /// Werkzeugausgabe (Tool-Ergebnis, Kommandozeilen-Output).
    Tool,
    /// Systeminterner Eintrag (z. B. Prompt-Gerüst) — nie gedächtnisrelevant.
    System,
}

/// Ein einzelner Eintrag eines Transcripts.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptEntry {
    /// Rolle des Eintrags.
    pub role: EntryRole,
    /// Rohtext des Eintrags.
    pub text: String,
}

/// Rollenpräfix für eine Zeile in [`build_input`].
const fn role_prefix(role: EntryRole) -> &'static str {
    match role {
        EntryRole::User => "User: ",
        EntryRole::Assistant => "Assistant: ",
        EntryRole::Tool => "Tool: ",
        EntryRole::System => "System: ",
    }
}

/// Baut den gefilterten, gekürzten Eingabetext für den Modellaufruf, siehe
/// Design §5.2.
///
/// # Beschreibung
/// Behält je Eintrag nur den gedächtnisrelevanten Teil:
/// - `User`: der volle (getrimmte) Text — Nutzertext ist immer relevant.
/// - `Assistant`: nur das letzte Absatz (das „Fazit der Antwort").
/// - `Tool`: nur Zeilen, die wie eine Fehlermeldung, ein Dateiname oder ein
///   Befehlsname aussehen.
/// - `System`: wird verworfen.
///
/// Jede verbleibende Zeile bekommt einen Rollenpräfix (`"User: "`,
/// `"Assistant: "`, `"Tool: "`). Das Ergebnis wird danach an einer
/// Zeichengrenze (nicht mitten in einem UTF-8-Zeichen) auf höchstens
/// `policy.max_input_bytes` Bytes gekürzt.
#[must_use]
pub fn build_input(entries: &[TranscriptEntry], policy: &ExtractionPolicy) -> String {
    let mut out = String::new();
    for entry in entries {
        let Some(relevant) = extract_relevant(entry) else {
            continue;
        };
        for line in relevant.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            out.push_str(role_prefix(entry.role));
            out.push_str(line);
            out.push('\n');
        }
    }
    truncate_to_byte_limit(&out, policy.max_input_bytes)
}

/// Extrahiert den gedächtnisrelevanten Teil eines Eintrags, siehe
/// [`build_input`]. `None`, wenn nichts Relevantes übrig bleibt.
fn extract_relevant(entry: &TranscriptEntry) -> Option<String> {
    let trimmed = entry.text.trim();
    if trimmed.is_empty() {
        return None;
    }
    match entry.role {
        EntryRole::User => Some(trimmed.to_owned()),
        EntryRole::Assistant => {
            let fazit = last_paragraph(trimmed);
            (!fazit.is_empty()).then(|| fazit.to_owned())
        }
        EntryRole::Tool => {
            let lines: Vec<&str> = trimmed.lines().filter(|l| tool_line_relevant(l)).collect();
            (!lines.is_empty()).then(|| lines.join("\n"))
        }
        EntryRole::System => None,
    }
}

/// Liefert den letzten nicht-leeren Absatz von `text` (getrennt durch
/// Leerzeilen) — das „Fazit" einer Assistenzantwort.
fn last_paragraph(text: &str) -> &str {
    // `Split` mit `&str`-Muster ist kein `DoubleEndedIterator`; `last()`
    // liefert denselben letzten Treffer ohne diese Anforderung.
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .last()
        .unwrap_or(text)
}

/// Ob eine Tool-Zeile wie eine Fehlermeldung, ein Datei- oder ein Befehlsname
/// aussieht.
fn tool_line_relevant(line: &str) -> bool {
    let lower = line.to_lowercase();
    const ERROR_KEYWORDS: [&str; 6] = ["error", "fehler", "panic", "exception", "failed", "fatal"];
    if ERROR_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return true;
    }
    if line.contains('/') || line.contains('\\') {
        return true;
    }
    if has_file_extension(line) {
        return true;
    }
    let trimmed = line.trim_start();
    const COMMAND_PREFIXES: [&str; 6] = ["$ ", "# ", "cargo ", "git ", "npm ", "cd "];
    COMMAND_PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

/// Ob eine der Whitespace-getrennten Tokens von `line` wie ein Dateiname mit
/// Endung (`foo.rs`, `bar.toml`) aussieht.
fn has_file_extension(line: &str) -> bool {
    line.split_whitespace().any(|token| {
        let token = token
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '_' && c != '-');
        token.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 5
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
    })
}

/// Kürzt `s` auf höchstens `max_bytes` Bytes, ohne ein UTF-8-Zeichen
/// mittendrin abzuschneiden.
fn truncate_to_byte_limit(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

// ---------------------------------------------------------------------
// Prompts (§5.2)
// ---------------------------------------------------------------------

/// Systemprompt der Extraktion nach Design §5.2.
///
/// # Beschreibung
/// Verlangt eine strenge JSON-Struktur (`facts: [{name, description, type,
/// body, confidence, sources}]`), höchstens `ExtractionPolicy::default()`s
/// `max_facts_per_session` (5) Einträge, ausdrücklich nichts, was aus Code
/// oder git-Log ablesbar wäre, und Beibehaltung der Nutzersprache. Die feste
/// Zahl „5" im Text muss mit `ExtractionPolicy::default().max_facts_per_session`
/// übereinstimmen — [`parse_response`] setzt die tatsächliche Grenze
/// unabhängig davon über `policy.max_facts_per_session` durch.
#[must_use]
pub const fn system_prompt() -> &'static str {
    "Du bist der Extraktions-Schritt eines Gedächtnissystems für einen \
Coding-Assistenten. Du bekommst gefilterte Ausschnitte einer abgeschlossenen \
Session (Nutzertext, Fazit der Antworten, Fehlermeldungen, Datei- und \
Befehlsnamen) und sollst daraus wiederverwendbare Gedächtniseinträge \
destillieren.\n\
\n\
Antworte AUSSCHLIESSLICH mit einem einzigen JSON-Objekt, keinem Fließtext, \
keiner Erklärung, keinem Markdown drumherum:\n\
{\"facts\": [{\"name\": \"kebab-case-name\", \"description\": \"Kurzbeschreibung\", \
\"type\": \"fact|decision|preference|pitfall|reference\", \"body\": \"Freitext, \
höchstens etwa 15 Zeilen\", \"confidence\": 0.0, \"sources\": [\"session:<id>\"]}]}\n\
\n\
Regeln:\n\
- Höchstens 5 Einträge, auch wenn mehr möglich wären.\n\
- Nimm NUR auf, was aus dem Transcript selbst hervorgeht — nichts, was ein \
Blick in den Code oder das git-Log ohnehin zeigen würde (keine Datei-Inhalte \
referieren, keine Commit-Historie zusammenfassen).\n\
- Ein Eintrag ist EIN Sachverhalt: eine Präferenz, eine Entscheidung samt \
Begründung, eine bekannte Falle, ein Fakt oder ein Verweis.\n\
- Kein Geheimnis, kein Token, kein Passwort im Text.\n\
- Behalte die Sprache des Nutzers aus dem Transcript bei.\n\
- confidence ist eine Zahl zwischen 0.0 und 1.0, wie sicher der Eintrag aus \
dem Transcript hervorgeht.\n\
- Wenn nichts Erinnerungswürdiges vorliegt, antworte mit {\"facts\": []}."
}

/// Baut den Nutzerprompt für einen konkreten, bereits gefilterten
/// Eingabetext (siehe [`build_input`]), nach Design §5.2.
#[must_use]
pub fn user_prompt(input: &str) -> String {
    format!(
        "Transcript-Ausschnitt der abgeschlossenen Session (gefiltert, mit \
Rollenpräfix je Zeile):\n\n{input}\n\nExtrahiere daraus höchstens 5 \
Gedächtniseinträge nach der oben beschriebenen JSON-Struktur. Antworte nur \
mit dem JSON-Objekt."
    )
}

// ---------------------------------------------------------------------
// Antwort-Parsing (§5.2, §7)
// ---------------------------------------------------------------------

/// Parst die Modellantwort zu [`Fact`]en, siehe Design §5.2.
///
/// # Beschreibung
/// Toleranter Parser: zieht den JSON-Block auch aus einem
/// Markdown-Codefence oder umgebendem Fließtext heraus (siehe
/// [`extract_json_block`]). Jeder Eintrag im `facts`-Array wird einzeln
/// verarbeitet — ein ungültiger Eintrag (fehlender `name`/`description`,
/// unbekannter `type`-Wert) wird übersprungen, statt den gesamten Lauf
/// scheitern zu lassen; unbekannte oder falsch typisierte Zusatzfelder
/// werden ignoriert bzw. auf ihren Default gesetzt. `confidence` wird auf
/// `0.0..=1.0` geklemmt. `redact` läuft über `description`, `body`, `tags`
/// und `sources`. Der `name` wird über [`slugify`] in einen stabilen
/// Dateinamen überführt. Fakten ohne `sources` bekommen `session:<session_id>`
/// als einzige Quelle (Design §7, Invariante 5: kein automatischer Fakt ohne
/// Quelle). Die Anzahl wird auf `policy.max_facts_per_session` begrenzt.
///
/// # Errors
/// [`ExtractionError::NoJsonFound`], wenn `json` keine JSON-Struktur enthält;
/// [`ExtractionError::Json`], wenn die gefundene Struktur kein gültiges JSON
/// ist.
pub fn parse_response(
    json: &str,
    policy: &ExtractionPolicy,
    scope: FactScope,
    now: OffsetDateTime,
    session_id: &str,
) -> Result<Vec<Fact>, ExtractionError> {
    let block = extract_json_block(json).ok_or(ExtractionError::NoJsonFound)?;
    let value: serde_json::Value =
        serde_json::from_str(&block).map_err(|source| ExtractionError::Json { source })?;

    let Some(items) = value.get("facts").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };

    let default_source = format!("session:{session_id}");
    let mut out = Vec::new();
    for item in items {
        if out.len() >= policy.max_facts_per_session {
            break;
        }
        if let Some(fact) = build_fact_from_value(item, scope, now, &default_source) {
            out.push(fact);
        }
    }
    Ok(out)
}

/// Sucht eine JSON-Struktur in `text`: zuerst in einem Markdown-Codefence
/// (```` ```json ... ``` ```` oder ```` ``` ... ``` ````), sonst zwischen dem
/// ersten `{` und dem letzten `}` im Text.
fn extract_json_block(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        let after_lang = after_fence
            .find('\n')
            .map_or(after_fence, |i| &after_fence[i + 1..]);
        if let Some(end) = after_lang.find("```") {
            let candidate = after_lang[..end].trim();
            if !candidate.is_empty() {
                return Some(candidate.to_owned());
            }
        }
    }
    let first = trimmed.find('{')?;
    let last = trimmed.rfind('}')?;
    (last > first).then(|| trimmed[first..=last].to_owned())
}

/// Baut einen [`Fact`] aus einem einzelnen `facts[]`-Element, `None` wenn der
/// Eintrag kaputt ist (siehe [`parse_response`]).
fn build_fact_from_value(
    item: &serde_json::Value,
    scope: FactScope,
    now: OffsetDateTime,
    default_source: &str,
) -> Option<Fact> {
    let obj = item.as_object()?;

    let name_raw = obj.get("name")?.as_str()?;
    let name = slugify(name_raw);

    let description_raw = obj.get("description")?.as_str()?;
    let description = redact(description_raw.trim());
    if description.is_empty() {
        return None;
    }

    let fact_type = match obj.get("type").and_then(|v| v.as_str()) {
        Some(raw) => raw.parse::<FactType>().ok()?,
        None => FactType::Fact,
    };

    let body_raw = obj.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let body = redact(body_raw);

    let confidence_raw = obj
        .get("confidence")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.8);
    let confidence = confidence_raw.clamp(0.0, 1.0) as f32;

    let mut sources: Vec<String> = obj
        .get("sources")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|s| s.as_str()).map(redact).collect())
        .unwrap_or_default();
    if sources.is_empty() {
        sources.push(default_source.to_owned());
    }

    let tags: Vec<String> = obj
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|s| s.as_str()).map(redact).collect())
        .unwrap_or_default();

    Some(Fact {
        name,
        description,
        fact_type,
        scope,
        created: now,
        updated: now,
        confidence,
        sources,
        tags,
        body,
    })
}

// ---------------------------------------------------------------------
// IncomingStore (facts/_incoming/)
// ---------------------------------------------------------------------

/// Ablage für Extraktions-Kandidaten unter `facts/_incoming/` samt
/// Extraktions-Marker (`extraction_state.json`), siehe Design §5.2/§5.3.
///
/// # Beschreibung
/// Layout unterhalb von `root`:
/// ```text
/// <root>/
///   facts/_incoming/<slug>.md   ← ein Kandidat je Datei (Frontmatter + Body,
///                                  im selben Aufbau wie ein regulärer Fakt)
///   extraction_state.json       ← Menge bereits extrahierter session_id
/// ```
/// Die Konsolidierung (Phase 2, ein anderer Slice) liest `list()`/`take_all()`
/// und verschmilzt die Kandidaten in die regulären Fakten.
pub struct IncomingStore {
    /// Speicherwurzel (Projekt- oder Global-Verzeichnis, wie `FactStore`).
    root: PathBuf,
}

impl IncomingStore {
    /// Öffnet den Store an `root`, legt `<root>/facts/_incoming/` an, falls
    /// es fehlt.
    ///
    /// # Errors
    /// [`ExtractionError::Io`], wenn das Verzeichnis nicht angelegt werden
    /// kann.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ExtractionError> {
        let root = root.as_ref().to_path_buf();
        let dir = incoming_dir(&root);
        fs::create_dir_all(&dir).map_err(|e| ExtractionError::Io {
            path: dir,
            source: e,
        })?;
        Ok(Self { root })
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("extraction_state.json")
    }

    /// Schreibt jeden Fakt in `facts` atomar als
    /// `facts/_incoming/<name>.md`-Kandidat.
    ///
    /// # Errors
    /// [`ExtractionError::Io`] bei Schreibfehlern.
    pub fn write_candidates(&self, facts: &[Fact]) -> Result<(), ExtractionError> {
        let dir = incoming_dir(&self.root);
        for fact in facts {
            let path = dir.join(format!("{}.md", fact.name));
            let markdown = candidate_to_markdown(fact);
            harw_fsutil::write_atomic(
                &path,
                markdown.as_bytes(),
                harw_fsutil::AtomicWriteOptions::with_mode(0o600),
            )
            .map_err(|e| ExtractionError::Io {
                path: path.clone(),
                source: e,
            })?;
        }
        Ok(())
    }

    /// Liest alle Kandidaten unter `facts/_incoming/`, sortiert nach `name`.
    ///
    /// Beschädigte Kandidatendateien werden übersprungen und per
    /// `tracing::warn!` gemeldet, statt den gesamten Aufruf scheitern zu
    /// lassen (analog zu `FactStore::list`).
    ///
    /// # Errors
    /// [`ExtractionError::Io`], wenn `facts/_incoming/` selbst nicht gelesen
    /// werden kann.
    pub fn list(&self) -> Result<Vec<Fact>, ExtractionError> {
        let dir = incoming_dir(&self.root);
        let mut out = Vec::new();
        let entries = fs::read_dir(&dir).map_err(|e| ExtractionError::Io {
            path: dir.clone(),
            source: e,
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| ExtractionError::Io {
                path: dir.clone(),
                source: e,
            })?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let raw = match fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "extraction: überspringe unlesbaren Kandidaten"
                    );
                    continue;
                }
            };
            match candidate_from_markdown(&raw, stem) {
                Ok(fact) => out.push(fact),
                Err(reason) => {
                    tracing::warn!(
                        path = %path.display(),
                        reason = %reason,
                        "extraction: überspringe defekten Kandidaten"
                    );
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Liest alle Kandidaten wie [`Self::list`] und löscht sie anschließend
    /// aus `facts/_incoming/`.
    ///
    /// # Errors
    /// Fehler von [`Self::list`]; [`ExtractionError::Io`], wenn eine
    /// Kandidatendatei nicht gelöscht werden kann.
    pub fn take_all(&self) -> Result<Vec<Fact>, ExtractionError> {
        let facts = self.list()?;
        let dir = incoming_dir(&self.root);
        for fact in &facts {
            let path = dir.join(format!("{}.md", fact.name));
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(ExtractionError::Io { path, source: e });
                }
            }
        }
        Ok(facts)
    }

    /// Markiert `session_id` in `extraction_state.json` als bereits
    /// extrahiert.
    ///
    /// # Errors
    /// [`ExtractionError::Io`]/[`ExtractionError::Json`] beim Lesen/Schreiben
    /// des Marker-Zustands.
    pub fn mark_session_done(&self, session_id: &str) -> Result<(), ExtractionError> {
        let mut state = self.read_state()?;
        if state.insert(session_id.to_owned()) {
            self.write_state(&state)?;
        }
        Ok(())
    }

    /// Ob `session_id` bereits als extrahiert markiert ist.
    ///
    /// Bei einem Lesefehler des Marker-Zustands wird `false` geliefert (und
    /// per `tracing::warn!` geloggt) statt eines Panics — ein reiner
    /// Lesehelfer, analog zu `FactStore::usage`.
    #[must_use]
    pub fn is_session_done(&self, session_id: &str) -> bool {
        match self.read_state() {
            Ok(state) => state.contains(session_id),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "extraction: extraction_state.json konnte nicht gelesen werden"
                );
                false
            }
        }
    }

    fn read_state(&self) -> Result<BTreeSet<String>, ExtractionError> {
        let path = self.state_path();
        match fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|source| ExtractionError::Json { source })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(e) => Err(ExtractionError::Io { path, source: e }),
        }
    }

    fn write_state(&self, state: &BTreeSet<String>) -> Result<(), ExtractionError> {
        let path = self.state_path();
        let bytes =
            serde_json::to_vec_pretty(state).map_err(|source| ExtractionError::Json { source })?;
        harw_fsutil::write_atomic(
            &path,
            &bytes,
            harw_fsutil::AtomicWriteOptions::with_mode(0o600),
        )
        .map_err(|e| ExtractionError::Io { path, source: e })
    }
}

/// Pfad von `<root>/facts/_incoming/`.
fn incoming_dir(root: &Path) -> PathBuf {
    root.join("facts").join("_incoming")
}

/// Serialisiert einen Kandidaten-[`Fact`] als Markdown-Dokument im selben
/// Aufbau wie ein regulärer Fakt (`---\n<frontmatter>\n---\n\n<body>`), aber
/// mit JSON-escapten Skalar-/Listenwerten — dieses Modul ist alleiniger
/// Schreiber und Leser dieser Dateien, ein Rundtrip genügt.
fn candidate_to_markdown(fact: &Fact) -> String {
    let mut fm = String::new();
    fm.push_str(&format!("name: {}\n", fact.name));
    fm.push_str(&format!(
        "description: {}\n",
        json_scalar(&fact.description)
    ));
    fm.push_str(&format!("type: {}\n", fact.fact_type.as_str()));
    fm.push_str(&format!("scope: {}\n", fact.scope.as_str()));
    fm.push_str(&format!("created: {}\n", format_rfc3339(fact.created)));
    fm.push_str(&format!("updated: {}\n", format_rfc3339(fact.updated)));
    fm.push_str(&format!("confidence: {:.2}\n", fact.confidence));
    fm.push_str(&format!("sources: {}\n", json_list(&fact.sources)));
    fm.push_str(&format!("tags: {}\n", json_list(&fact.tags)));
    let body = if fact.body.ends_with('\n') || fact.body.is_empty() {
        fact.body.clone()
    } else {
        format!("{}\n", fact.body)
    };
    format!("---\n{fm}---\n\n{body}")
}

/// Parst ein von [`candidate_to_markdown`] geschriebenes Dokument zurück in
/// einen [`Fact`].
///
/// # Errors
/// Textbeschreibung, wenn das Dokument nicht mit `---\n` beginnt oder kein
/// schließendes `---\n` gefunden wird — nie ein Panic.
fn candidate_from_markdown(raw: &str, fallback_name: &str) -> Result<Fact, String> {
    let normalized = raw.replace("\r\n", "\n");
    let rest = normalized
        .strip_prefix("---\n")
        .ok_or_else(|| "Dokument beginnt nicht mit '---'".to_owned())?;
    let end_idx = rest
        .find("---\n")
        .ok_or_else(|| "kein schließendes '---' gefunden".to_owned())?;
    let frontmatter = &rest[..end_idx];
    let after = &rest[end_idx + "---\n".len()..];
    let body = after.strip_prefix('\n').unwrap_or(after).to_owned();

    let mut name = fallback_name.to_owned();
    let mut description = String::new();
    let mut fact_type = FactType::Fact;
    let mut scope = FactScope::Project;
    let mut created = OffsetDateTime::now_utc();
    let mut updated = created;
    let mut confidence = 0.8_f32;
    let mut sources = Vec::new();
    let mut tags = Vec::new();

    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => name = value.to_owned(),
            "description" => {
                description = serde_json::from_str(value).unwrap_or_else(|_| value.to_owned());
            }
            "type" => fact_type = value.parse().unwrap_or(FactType::Fact),
            "scope" => scope = value.parse().unwrap_or(FactScope::Project),
            "created" => created = parse_rfc3339(value).unwrap_or(created),
            "updated" => updated = parse_rfc3339(value).unwrap_or(updated),
            "confidence" => confidence = value.parse().unwrap_or(confidence),
            "sources" => sources = serde_json::from_str(value).unwrap_or_default(),
            "tags" => tags = serde_json::from_str(value).unwrap_or_default(),
            _ => {}
        }
    }

    Ok(Fact {
        name,
        description,
        fact_type,
        scope,
        created,
        updated,
        confidence,
        sources,
        tags,
        body,
    })
}

/// JSON-escapter Skalarwert für [`candidate_to_markdown`].
fn json_scalar(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned())
}

/// JSON-escapte Liste für [`candidate_to_markdown`].
fn json_list(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_owned())
}

/// Formatiert einen Zeitstempel als RFC 3339 (`…Z`).
fn format_rfc3339(ts: OffsetDateTime) -> String {
    ts.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| ts.unix_timestamp().to_string())
}

/// Parst einen RFC-3339-Zeitstempel.
fn parse_rfc3339(raw: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use time::Duration;

    fn tmp_root(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-extraction-{tag}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    // -- select_sessions --------------------------------------------------

    #[test]
    fn select_sessions_respects_idle_age_marker_and_limit() {
        let now = OffsetDateTime::now_utc();
        let policy = ExtractionPolicy {
            idle_min_secs: 300,
            max_age_days: 30,
            max_sessions_per_run: 2,
            ..ExtractionPolicy::default()
        };
        let candidates = vec![
            // zu jung (Ruhezeit nicht erreicht)
            ExtractionCandidate {
                session_id: "too-fresh".to_owned(),
                recorded_at: now - Duration::seconds(60),
                bytes: 100,
            },
            // zu alt
            ExtractionCandidate {
                session_id: "too-old".to_owned(),
                recorded_at: now - Duration::days(31),
                bytes: 100,
            },
            // bereits extrahiert
            ExtractionCandidate {
                session_id: "already-done".to_owned(),
                recorded_at: now - Duration::seconds(600),
                bytes: 100,
            },
            // gültig, älter
            ExtractionCandidate {
                session_id: "valid-older".to_owned(),
                recorded_at: now - Duration::days(2),
                bytes: 100,
            },
            // gültig, neuer
            ExtractionCandidate {
                session_id: "valid-newer".to_owned(),
                recorded_at: now - Duration::seconds(900),
                bytes: 100,
            },
            // gültig, aber über der Obergrenze
            ExtractionCandidate {
                session_id: "valid-oldest".to_owned(),
                recorded_at: now - Duration::days(5),
                bytes: 100,
            },
        ];
        let mut already_done = BTreeSet::new();
        already_done.insert("already-done".to_owned());

        let selected = select_sessions(&candidates, &already_done, now, &policy);

        assert_eq!(
            selected,
            vec!["valid-newer".to_owned(), "valid-older".to_owned()]
        );
    }

    // -- build_input --------------------------------------------------------

    #[test]
    fn build_input_keeps_roles_and_filters_by_role() {
        let entries = vec![
            TranscriptEntry {
                role: EntryRole::User,
                text: "Bitte behebe den Bug in src/main.rs".to_owned(),
            },
            TranscriptEntry {
                role: EntryRole::Assistant,
                text: "Erst habe ich den Code gelesen.\n\nDanach den Bug in main.rs behoben."
                    .to_owned(),
            },
            TranscriptEntry {
                role: EntryRole::Tool,
                text: "compiling...\nerror: mismatched types in src/main.rs:12".to_owned(),
            },
            TranscriptEntry {
                role: EntryRole::System,
                text: "Du bist ein hilfreicher Assistent.".to_owned(),
            },
        ];
        let policy = ExtractionPolicy::default();
        let input = build_input(&entries, &policy);

        assert!(input.contains("User: Bitte behebe den Bug in src/main.rs"));
        assert!(input.contains("Assistant: Danach den Bug in main.rs behoben."));
        assert!(!input.contains("Erst habe ich den Code gelesen."));
        assert!(input.contains("Tool: error: mismatched types in src/main.rs:12"));
        assert!(!input.contains("compiling..."));
        assert!(!input.contains("hilfreicher Assistent"));
    }

    #[test]
    fn build_input_truncates_at_char_boundary() {
        let entries = vec![TranscriptEntry {
            role: EntryRole::User,
            text: "ü".repeat(50),
        }];
        let policy = ExtractionPolicy {
            max_input_bytes: 10,
            ..ExtractionPolicy::default()
        };
        let input = build_input(&entries, &policy);
        // "ü" ist 2 Bytes in UTF-8; das Ergebnis darf nicht mittendrin enden.
        assert!(input.len() <= 10);
        assert!(std::str::from_utf8(input.as_bytes()).is_ok());
    }

    // -- parse_response -----------------------------------------------------

    #[test]
    fn parse_response_extracts_json_from_codefence() -> TestResult {
        let response = "Hier ist das Ergebnis:\n```json\n{\"facts\": [{\"name\": \"Test Fakt\", \
             \"description\": \"Eine Beschreibung\", \"type\": \"fact\", \"body\": \"Text\", \
             \"confidence\": 0.9, \"sources\": [\"session:abc\"]}]}\n```\nDanke.";
        let policy = ExtractionPolicy::default();
        let facts = parse_response(
            response,
            &policy,
            FactScope::Project,
            OffsetDateTime::now_utc(),
            "sess-1",
        )
        .map_err(ctx("parse_response"))?;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].name, "test-fakt");
        assert_eq!(facts[0].sources, vec!["session:abc".to_owned()]);
        Ok(())
    }

    #[test]
    fn parse_response_tolerates_garbage_fields_and_defaults_source() -> TestResult {
        let response = r#"{"facts": [{"name": "x", "description": "y", "confidence": "not-a-number", "unknown_field": 123}]}"#;
        let policy = ExtractionPolicy::default();
        let facts = parse_response(
            response,
            &policy,
            FactScope::Project,
            OffsetDateTime::now_utc(),
            "sess-2",
        )
        .map_err(ctx("parse_response"))?;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].fact_type, FactType::Fact);
        assert_eq!(facts[0].confidence, 0.8);
        assert_eq!(facts[0].sources, vec!["session:sess-2".to_owned()]);
        Ok(())
    }

    #[test]
    fn parse_response_skips_broken_entries_but_keeps_valid_ones() -> TestResult {
        let response = r#"{"facts": [
            {"description": "kein Name"},
            {"name": "kaputter-typ", "description": "x", "type": "unsinn"},
            "not-an-object",
            {"name": "gueltig", "description": "y", "type": "decision", "confidence": 5.0}
        ]}"#;
        let policy = ExtractionPolicy::default();
        let facts = parse_response(
            response,
            &policy,
            FactScope::Project,
            OffsetDateTime::now_utc(),
            "sess-3",
        )
        .map_err(ctx("parse_response"))?;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].name, "gueltig");
        assert_eq!(facts[0].fact_type, FactType::Decision);
        // confidence wurde auf 1.0 geklemmt
        assert_eq!(facts[0].confidence, 1.0);
        Ok(())
    }

    #[test]
    fn parse_response_limits_to_max_facts_per_session() -> TestResult {
        let items: Vec<String> = (0..10)
            .map(|i| format!("{{\"name\": \"fakt-{i}\", \"description\": \"Beschreibung {i}\"}}"))
            .collect();
        let response = format!("{{\"facts\": [{}]}}", items.join(","));
        let policy = ExtractionPolicy {
            max_facts_per_session: 3,
            ..ExtractionPolicy::default()
        };
        let facts = parse_response(
            &response,
            &policy,
            FactScope::Project,
            OffsetDateTime::now_utc(),
            "sess-4",
        )
        .map_err(ctx("parse_response"))?;
        assert_eq!(facts.len(), 3);
        Ok(())
    }

    #[test]
    fn parse_response_redacts_secrets_in_description_and_body() -> TestResult {
        let response = r#"{"facts": [{"name": "geheim", "description": "token=sk-abcdefghijklmnopqrstuvwxyz1234", "body": "api_key = sk-abcdefghijklmnopqrstuvwxyz1234"}]}"#;
        let policy = ExtractionPolicy::default();
        let facts = parse_response(
            response,
            &policy,
            FactScope::Project,
            OffsetDateTime::now_utc(),
            "sess-5",
        )
        .map_err(ctx("parse_response"))?;
        assert_eq!(facts.len(), 1);
        assert!(
            !facts[0]
                .description
                .contains("sk-abcdefghijklmnopqrstuvwxyz1234")
        );
        assert!(!facts[0].body.contains("sk-abcdefghijklmnopqrstuvwxyz1234"));
        Ok(())
    }

    #[test]
    fn parse_response_without_json_returns_error() -> TestResult {
        let policy = ExtractionPolicy::default();
        let Err(err) = parse_response(
            "kein JSON hier",
            &policy,
            FactScope::Project,
            OffsetDateTime::now_utc(),
            "sess-6",
        ) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, ExtractionError::NoJsonFound));
        Ok(())
    }

    // -- prompts --------------------------------------------------------

    #[test]
    fn prompts_are_non_empty_and_echo_input() {
        assert!(system_prompt().contains("facts"));
        let user = user_prompt("User: Beispiel\n");
        assert!(user.contains("User: Beispiel"));
    }

    // -- IncomingStore --------------------------------------------------

    fn sample_fact(name: &str) -> Fact {
        let now = OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: "Beschreibung".to_owned(),
            fact_type: FactType::Decision,
            scope: FactScope::Project,
            created: now,
            updated: now,
            confidence: 0.75,
            sources: vec!["session:xyz".to_owned()],
            tags: vec!["tag-a".to_owned()],
            body: "Zeile 1\nZeile 2\n".to_owned(),
        }
    }

    #[test]
    fn incoming_store_roundtrip_write_list_take_all() -> TestResult {
        let root = tmp_root("roundtrip");
        let store = IncomingStore::open(&root).map_err(ctx("open store"))?;
        let fact = sample_fact("kandidat-eins");
        store
            .write_candidates(std::slice::from_ref(&fact))
            .map_err(ctx("write_candidates"))?;

        let listed = store.list().map_err(ctx("list"))?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "kandidat-eins");
        assert_eq!(listed[0].description, fact.description);
        assert_eq!(listed[0].body, fact.body);
        assert_eq!(listed[0].sources, fact.sources);
        assert_eq!(listed[0].tags, fact.tags);
        assert_eq!(listed[0].fact_type, FactType::Decision);

        let taken = store.take_all().map_err(ctx("take_all"))?;
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].name, "kandidat-eins");

        let after_take = store.list().map_err(ctx("list"))?;
        assert!(after_take.is_empty());

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn incoming_store_marker_roundtrip() -> TestResult {
        let root = tmp_root("marker");
        let store = IncomingStore::open(&root).map_err(ctx("open store"))?;

        assert!(!store.is_session_done("sess-a"));
        store
            .mark_session_done("sess-a")
            .map_err(ctx("mark_session_done"))?;
        assert!(store.is_session_done("sess-a"));
        assert!(!store.is_session_done("sess-b"));

        // idempotent
        store
            .mark_session_done("sess-a")
            .map_err(ctx("mark_session_done"))?;
        assert!(store.is_session_done("sess-a"));

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
