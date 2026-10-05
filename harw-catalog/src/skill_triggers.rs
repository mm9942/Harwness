//! Trigger-Manifest (`triggers.toml`) und der unveränderliche `TriggerIndex`
//! (Plan PL-95, Zyklus S1).
//!
//! # Verantwortung
//! Ein Skill kann **optional** eine Datei `triggers.toml` neben seiner
//! `skill.toml` mitbringen. Sie sagt, *wann* sein Text dem Modell
//! untergeschoben werden soll. `skill.toml`/`SkillToml` bleiben bewusst
//! unverändert: `SkillToml` kennt `deny_unknown_fields`, und ein älteres
//! `harw` würde einen Skill mit neuen Feldern ganz verlieren. Eine Datei, die
//! ältere Fassungen nie lesen, ist rückwärts- und vorwärtskompatibel.
//!
//! ```toml
//! # triggers.toml
//! when = "Rust-Borrow-Checker-Fehler E0499/E0502/E0505"   # <= 160 Zeichen
//!
//! [triggers]
//! keywords    = ["borrow checker", "cannot move out"]   # im Aufgabentext
//! error_codes = ["E0505", "E0499", "E0502"]             # in Werkzeugausgaben
//! paths       = ["**/*.rs"]                              # berührte Dateien
//! tools       = ["process.execute"]                      # erste Nutzung
//! commands    = ["cargo test", "cargo clippy"]           # ausgeführte Befehle
//! priority    = 50                                       # u8, Standard 50
//! ```
//!
//! Alle Felder sind optional.
//!
//! # Grenzen (je Liste und Eintrag)
//! | Feld | max. Einträge | max. Länge | erlaubte Zeichen |
//! |---|---|---|---|
//! | `when` | – | [`MAX_WHEN_CHARS`] (160) | keine Steuerzeichen/Zeilenumbrüche |
//! | `keywords` | 16 | 64 | keine Steuerzeichen, mind. 3 Buchstaben/Ziffern |
//! | `error_codes` | 16 | 24 | `A-Z a-z 0-9 _ -`, beginnt alphanumerisch |
//! | `paths` | 16 | 128 | keine Steuerzeichen, höchstens 16 `*` |
//! | `tools` | 16 | 64 | `A-Z a-z 0-9 . _ - : /` |
//! | `commands` | 16 | 64 | keine Steuerzeichen, keine Shell-Trenner `; & \|` |
//!
//! # Ungültiges verwirft, es blockiert nicht
//! Ein ungültiger Eintrag, eine ungültige Liste, ein unbekannter Schlüssel
//! oder eine unlesbare Datei machen den **Skill nicht unladbar**: der
//! betroffene Teil wird verworfen und mit Grund als [`SkillWarning`] im Index
//! geführt ([`crate::SkillIndex::warnings`]).
//!
//! # Matching (deterministisch, ohne Modellaufruf)
//! - **Schlüsselwörter**: Wortgrenzen, ohne Groß-/Kleinschreibung (Umlaute
//!   gefaltet), als zusammenhängende Wortfolge.
//! - **Fehlercodes**: Token-Suche; `E0505` trifft weder `E05051` noch
//!   `XE0505`. Trenner sind alle nicht-alphanumerischen Zeichen; ohne
//!   Groß-/Kleinschreibung.
//! - **Pfade**: eigener Glob (`*` ohne `/`, `**` über Verzeichnisse, `?`).
//!   Ohne `/` im Muster zählt der Dateiname (wie `.gitignore`); `**/` darf
//!   null Verzeichnisse umfassen; ein Muster mit führendem `/` trifft nur
//!   absolute Pfade. Groß-/Kleinschreibung zählt.
//! - **Befehle**: Wort-Präfix mit Argumentgrenze (`cargo test` trifft
//!   `cargo test -p x`, nicht `cargo testfoo`); verkettete Befehle
//!   (`&&`, `||`, `;`, `|`) werden einzeln geprüft, führende
//!   `NAME=wert`-Zuweisungen und `sudo` übersprungen.
//! - **Werkzeuge**: genauer Name (erste Nutzung).
//!
//! Sehr lange Eingaben werden begrenzt gescannt ([`MAX_SCAN_BYTES`]: Anfang
//! und Ende), damit die Laufzeit begrenzt bleibt.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

/// Höchstlänge von `when` in Zeichen (wie `SHORT_DESCRIPTION_CHARS`).
pub const MAX_WHEN_CHARS: usize = 160;
/// Höchstzahl Einträge je Trigger-Liste.
pub const MAX_TRIGGER_ENTRIES: usize = 16;
/// Standard-Priorität.
pub const DEFAULT_TRIGGER_PRIORITY: u8 = 50;
/// Höchstgröße einer `triggers.toml`.
pub const MAX_TRIGGERS_FILE_BYTES: usize = 64 * 1024;
/// Obergrenze des gescannten Textes je Ereignis (Anfang + Ende).
pub const MAX_SCAN_BYTES: usize = 256 * 1024;

const MAX_KEYWORD_CHARS: usize = 64;
const MAX_ERROR_CODE_CHARS: usize = 24;
const MAX_PATH_GLOB_CHARS: usize = 128;
const MAX_COMMAND_CHARS: usize = 64;
const MAX_TOOL_CHARS: usize = 64;
const MAX_GLOB_STARS: usize = 16;
const MAX_PATH_BYTES: usize = 4096;
const MAX_COMMAND_SCAN_BYTES: usize = 64 * 1024;

/// Eine Warnung beim Lesen der Trigger eines Skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillWarning {
    /// Betroffener Skill (Name bzw. Ort, falls der Name unbekannt ist).
    pub skill: String,
    /// Was verworfen wurde und warum.
    pub reason: String,
}

/// Art eines Triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TriggerKind {
    /// Fehlercode in einer Werkzeugausgabe.
    ErrorCode,
    /// Ausgeführter Befehl.
    Command,
    /// Erste Nutzung eines Werkzeugs.
    Tool,
    /// Berührter Pfad.
    Path,
    /// Schlüsselwort im Aufgabentext.
    Keyword,
}

impl TriggerKind {
    /// Stabiler Name (für Rahmenzeile und Messung).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ErrorCode => "error_code",
            Self::Command => "command",
            Self::Tool => "tool",
            Self::Path => "path",
            Self::Keyword => "keyword",
        }
    }
}

/// Warum ein Skill getroffen wurde: Art und Schlüssel.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TriggerReason {
    /// Trigger-Art.
    pub kind: TriggerKind,
    /// Der Manifest-Eintrag, der traf (z. B. `E0505`).
    pub key: String,
}

impl fmt::Display for TriggerReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key: String = self
            .key
            .chars()
            .map(|ch| {
                if ch == ']' || ch.is_control() {
                    ' '
                } else {
                    ch
                }
            })
            .collect();
        write!(f, "{} {}", self.kind.as_str(), key)
    }
}

/// Die geprüften Trigger eines Skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillTriggers {
    /// Schlüsselwörter (Aufgabentext).
    pub keywords: Vec<String>,
    /// Fehlercodes (Werkzeugausgaben, Aufgabentext).
    pub error_codes: Vec<String>,
    /// Glob-Muster berührter Pfade.
    pub paths: Vec<String>,
    /// Werkzeuge (erste Nutzung).
    pub tools: Vec<String>,
    /// Befehlspräfixe.
    pub commands: Vec<String>,
    /// Priorität (höher zuerst).
    pub priority: u8,
}

impl Default for SkillTriggers {
    fn default() -> Self {
        Self {
            keywords: Vec::new(),
            error_codes: Vec::new(),
            paths: Vec::new(),
            tools: Vec::new(),
            commands: Vec::new(),
            priority: DEFAULT_TRIGGER_PRIORITY,
        }
    }
}

impl SkillTriggers {
    /// Ob kein einziger Trigger gesetzt ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keywords.is_empty()
            && self.error_codes.is_empty()
            && self.paths.is_empty()
            && self.tools.is_empty()
            && self.commands.is_empty()
    }
}

/// Inhalt einer `triggers.toml` nach der Prüfung.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillTriggerSpec {
    /// Der `when`-Satz, falls gültig.
    pub when: Option<String>,
    /// Die gültigen Trigger.
    pub triggers: SkillTriggers,
}

/// Prüft `source` (Inhalt einer `triggers.toml`) nachsichtig.
///
/// Rückgabe: die gültigen Teile und je verworfenem Teil eine Begründung.
#[must_use]
pub fn parse_triggers_file(source: &str) -> (SkillTriggerSpec, Vec<String>) {
    let mut warnings = Vec::new();
    let mut spec = SkillTriggerSpec::default();
    if source.len() > MAX_TRIGGERS_FILE_BYTES {
        warnings.push(format!(
            "triggers.toml größer als {MAX_TRIGGERS_FILE_BYTES} Bytes, verworfen"
        ));
        return (spec, warnings);
    }
    let table: toml::Table = match toml::from_str(source) {
        Ok(table) => table,
        Err(error) => {
            warnings.push(format!("triggers.toml nicht lesbar, verworfen: {error}"));
            return (spec, warnings);
        }
    };
    for key in table.keys() {
        if key != "when" && key != "triggers" {
            warnings.push(format!("unbekannter Schlüssel {key:?} ignoriert"));
        }
    }
    match table.get("when") {
        None => {}
        Some(toml::Value::String(text)) => match validate_when(text) {
            Ok(when) => spec.when = when,
            Err(reason) => warnings.push(format!("when verworfen: {reason}")),
        },
        Some(_) => warnings.push("when verworfen: kein Text".to_owned()),
    }
    match table.get("triggers") {
        None => {}
        Some(toml::Value::Table(triggers)) => {
            spec.triggers = parse_trigger_table(triggers, &mut warnings);
        }
        Some(_) => warnings.push("triggers verworfen: keine Tabelle".to_owned()),
    }
    (spec, warnings)
}

fn validate_when(text: &str) -> Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().any(char::is_control) {
        return Err("enthält Steuerzeichen oder Zeilenumbruch".to_owned());
    }
    let chars = text.chars().count();
    if chars > MAX_WHEN_CHARS {
        return Err(format!("{chars} Zeichen, erlaubt sind {MAX_WHEN_CHARS}"));
    }
    Ok(Some(text.to_owned()))
}

fn parse_trigger_table(table: &toml::Table, warnings: &mut Vec<String>) -> SkillTriggers {
    let mut out = SkillTriggers::default();
    for key in table.keys() {
        if !matches!(
            key.as_str(),
            "keywords" | "error_codes" | "paths" | "tools" | "commands" | "priority"
        ) {
            warnings.push(format!("triggers.{key} unbekannt, ignoriert"));
        }
    }
    out.keywords = string_list(table, "keywords", validate_keyword, warnings);
    out.error_codes = string_list(table, "error_codes", validate_error_code, warnings);
    out.paths = string_list(table, "paths", validate_path_glob, warnings);
    out.tools = string_list(table, "tools", validate_tool, warnings);
    out.commands = string_list(table, "commands", validate_command, warnings);
    match table.get("priority") {
        None => {}
        Some(toml::Value::Integer(value)) => match u8::try_from(*value) {
            Ok(priority) => out.priority = priority,
            Err(_) => warnings.push(format!(
                "triggers.priority {value} außerhalb 0..=255, Standard {DEFAULT_TRIGGER_PRIORITY}"
            )),
        },
        Some(_) => warnings.push("triggers.priority keine Ganzzahl, Standard verwendet".to_owned()),
    }
    out
}

fn string_list(
    table: &toml::Table,
    key: &str,
    validate: fn(&str) -> Result<String, String>,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(value) = table.get(key) else {
        return out;
    };
    let toml::Value::Array(items) = value else {
        warnings.push(format!("triggers.{key} verworfen: keine Liste"));
        return out;
    };
    if items.len() > MAX_TRIGGER_ENTRIES {
        warnings.push(format!(
            "triggers.{key}: {} Einträge, nur die ersten {MAX_TRIGGER_ENTRIES} behalten",
            items.len()
        ));
    }
    for (position, item) in items.iter().take(MAX_TRIGGER_ENTRIES).enumerate() {
        let toml::Value::String(text) = item else {
            warnings.push(format!("triggers.{key}[{position}] verworfen: kein Text"));
            continue;
        };
        match validate(text) {
            Ok(entry) => {
                if !out.contains(&entry) {
                    out.push(entry);
                }
            }
            Err(reason) => warnings.push(format!("triggers.{key}[{position}] verworfen: {reason}")),
        }
    }
    out
}

fn check_len(text: &str, max: usize) -> Result<(), String> {
    let chars = text.chars().count();
    if chars > max {
        return Err(format!("{chars} Zeichen, erlaubt sind {max}"));
    }
    Ok(())
}

fn check_plain(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("leer".to_owned());
    }
    if text.chars().any(char::is_control) {
        return Err("enthält Steuerzeichen".to_owned());
    }
    Ok(())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate_keyword(text: &str) -> Result<String, String> {
    let text = collapse_whitespace(text);
    check_plain(&text)?;
    check_len(&text, MAX_KEYWORD_CHARS)?;
    if text.chars().filter(|ch| ch.is_alphanumeric()).count() < 3 {
        return Err("weniger als 3 Buchstaben/Ziffern".to_owned());
    }
    Ok(text)
}

fn validate_error_code(text: &str) -> Result<String, String> {
    let text = text.trim();
    check_plain(text)?;
    check_len(text, MAX_ERROR_CODE_CHARS)?;
    if text.chars().count() < 2 {
        return Err("kürzer als 2 Zeichen".to_owned());
    }
    if !text
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err("nur A-Z, a-z, 0-9, _ und - erlaubt".to_owned());
    }
    if !text.starts_with(|ch: char| ch.is_ascii_alphanumeric()) {
        return Err("muss alphanumerisch beginnen".to_owned());
    }
    Ok(text.to_owned())
}

fn validate_path_glob(text: &str) -> Result<String, String> {
    let text = text.trim().replace('\\', "/");
    check_plain(&text)?;
    check_len(&text, MAX_PATH_GLOB_CHARS)?;
    if text.chars().filter(|ch| *ch == '*').count() > MAX_GLOB_STARS {
        return Err(format!("mehr als {MAX_GLOB_STARS} `*`"));
    }
    let text = text.strip_prefix("./").unwrap_or(&text).to_owned();
    if text.is_empty() {
        return Err("leer".to_owned());
    }
    Ok(text)
}

fn validate_tool(text: &str) -> Result<String, String> {
    let text = text.trim();
    check_plain(text)?;
    check_len(text, MAX_TOOL_CHARS)?;
    if !text
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | ':' | '/'))
    {
        return Err("nur A-Z, a-z, 0-9 und . _ - : / erlaubt".to_owned());
    }
    Ok(text.to_owned())
}

fn validate_command(text: &str) -> Result<String, String> {
    let text = collapse_whitespace(text);
    check_plain(&text)?;
    check_len(&text, MAX_COMMAND_CHARS)?;
    if text.contains([';', '&', '|']) {
        return Err("Shell-Trenner `; & |` nicht erlaubt".to_owned());
    }
    Ok(text)
}

// ---------------------------------------------------------------------------
// Text-Hilfen
// ---------------------------------------------------------------------------

/// Kürzt `text` auf Anfang + Ende, wenn er `max` Bytes übersteigt.
fn scan_window(text: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if text.len() <= max {
        return std::borrow::Cow::Borrowed(text);
    }
    let half = max / 2;
    let mut head = half;
    while head > 0 && !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len().saturating_sub(half);
    while tail < text.len() && !text.is_char_boundary(tail) {
        tail += 1;
    }
    let mut out = String::with_capacity(max + 1);
    out.push_str(text.get(..head).unwrap_or_default());
    out.push('\n');
    out.push_str(text.get(tail..).unwrap_or_default());
    std::borrow::Cow::Owned(out)
}

/// Gefaltete Wörter (Buchstaben/Ziffern, Umlaute gefaltet, ohne
/// Längenfilter) — Grundlage der Wortgrenzen-Suche.
pub(crate) fn word_tokens(text: &str) -> Vec<String> {
    crate::skill_index::fold(text)
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// Glob
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobSegment {
    /// `**` als ganzes Segment: null oder mehr Verzeichnisse.
    AnyDirs,
    /// Muster innerhalb eines Segments (`*`, `?`, Literale).
    Pattern(Vec<char>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Glob {
    absolute: bool,
    basename_only: bool,
    segments: Vec<GlobSegment>,
}

impl Glob {
    fn compile(pattern: &str) -> Self {
        let pattern = pattern.replace('\\', "/");
        let pattern = pattern.strip_prefix("./").unwrap_or(&pattern);
        let absolute = pattern.starts_with('/');
        let basename_only = !pattern.trim_matches('/').contains('/');
        let segments = pattern
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| {
                if segment == "**" {
                    GlobSegment::AnyDirs
                } else {
                    GlobSegment::Pattern(segment.chars().collect())
                }
            })
            .collect();
        Self {
            absolute,
            basename_only,
            segments,
        }
    }

    fn is_match(&self, path: &str) -> bool {
        if path.len() > MAX_PATH_BYTES {
            return false;
        }
        let normalized = path.replace('\\', "/");
        let path = normalized.strip_prefix("./").unwrap_or(&normalized);
        if self.absolute && !path.starts_with('/') {
            return false;
        }
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        if self.basename_only {
            return match (self.segments.as_slice(), parts.last()) {
                ([GlobSegment::Pattern(pattern)], Some(name)) => {
                    let name: Vec<char> = name.chars().collect();
                    segment_match(pattern, &name)
                }
                ([GlobSegment::AnyDirs], _) => true,
                _ => false,
            };
        }
        segments_match(&self.segments, &parts)
    }
}

/// Segmentweises Matching mit dynamischer Programmierung (kein exponentielles
/// Zurückverfolgen): O(Muster × Pfadsegmente).
fn segments_match(pattern: &[GlobSegment], parts: &[&str]) -> bool {
    let count = parts.len();
    let mut current = vec![false; count + 1];
    if let Some(first) = current.first_mut() {
        *first = true;
    }
    for segment in pattern {
        let mut next = vec![false; count + 1];
        match segment {
            GlobSegment::AnyDirs => {
                let mut any = false;
                for (reach, slot) in current.iter().zip(next.iter_mut()) {
                    any = any || *reach;
                    *slot = any;
                }
            }
            GlobSegment::Pattern(chars) => {
                for (index, part) in parts.iter().enumerate() {
                    if current.get(index).copied().unwrap_or(false) {
                        let part: Vec<char> = part.chars().collect();
                        if segment_match(chars, &part) {
                            if let Some(slot) = next.get_mut(index + 1) {
                                *slot = true;
                            }
                        }
                    }
                }
            }
        }
        current = next;
    }
    current.last().copied().unwrap_or(false)
}

/// `*` (beliebig viele Zeichen) und `?` (genau eins) innerhalb eines Segments.
fn segment_match(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || Some(&c) == text.get(t) => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern
        .get(p..)
        .is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}

// ---------------------------------------------------------------------------
// TriggerIndex
// ---------------------------------------------------------------------------

/// Ein Ereignis, gegen das Trigger geprüft werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerEvent<'a> {
    /// Aufgabentext (Nutzereingabe): Schlüsselwörter und Fehlercodes.
    TaskText(&'a str),
    /// Ausgabe eines Werkzeugs: Fehlercodes. `tool` dient der Messung und
    /// trifft selbst keinen Trigger.
    ToolOutput {
        /// Name des Werkzeugs.
        tool: &'a str,
        /// Die Ausgabe.
        text: &'a str,
    },
    /// Ein Werkzeug wird zum ersten Mal benutzt.
    ToolFirstUse(&'a str),
    /// Ein Pfad wurde gelesen oder geändert.
    PathTouched(&'a str),
    /// Ein Befehl wird ausgeführt.
    Command(&'a str),
}

#[derive(Debug, Clone)]
struct KeywordRule {
    skill: String,
    key: String,
    tokens: Vec<String>,
}

#[derive(Debug, Clone)]
struct CommandRule {
    skill: String,
    key: String,
    words: Vec<String>,
}

#[derive(Debug, Clone)]
struct PathRule {
    skill: String,
    key: String,
    glob: Glob,
}

/// Treffer je Skill: die Gründe, nach Art und Schlüssel geordnet.
pub(crate) type Hits = BTreeMap<String, BTreeSet<TriggerReason>>;

/// Der einmal je Index gebaute, danach unveränderliche Trigger-Index.
///
/// `Send + Sync`; hält nur Regeln und Prioritäten, keine Skilltexte.
#[derive(Debug, Clone, Default)]
pub struct TriggerIndex {
    priorities: BTreeMap<String, u8>,
    keywords: HashMap<String, Vec<KeywordRule>>,
    error_codes: HashMap<String, Vec<(String, String)>>,
    paths: Vec<PathRule>,
    tools: HashMap<String, Vec<String>>,
    commands: HashMap<String, Vec<CommandRule>>,
}

impl TriggerIndex {
    /// Baut den Index aus `(Skill-Name, Trigger)`-Paaren.
    pub(crate) fn from_entries<'a>(
        entries: impl IntoIterator<Item = (&'a str, &'a SkillTriggers)>,
    ) -> Self {
        let mut index = Self::default();
        for (skill, triggers) in entries {
            index.priorities.insert(skill.to_owned(), triggers.priority);
            for keyword in &triggers.keywords {
                let tokens = word_tokens(keyword);
                if let Some(first) = tokens.first() {
                    index
                        .keywords
                        .entry(first.clone())
                        .or_default()
                        .push(KeywordRule {
                            skill: skill.to_owned(),
                            key: keyword.clone(),
                            tokens,
                        });
                }
            }
            for code in &triggers.error_codes {
                index
                    .error_codes
                    .entry(code.to_ascii_lowercase())
                    .or_default()
                    .push((skill.to_owned(), code.clone()));
            }
            for glob in &triggers.paths {
                index.paths.push(PathRule {
                    skill: skill.to_owned(),
                    key: glob.clone(),
                    glob: Glob::compile(glob),
                });
            }
            for tool in &triggers.tools {
                index
                    .tools
                    .entry(tool.clone())
                    .or_default()
                    .push(skill.to_owned());
            }
            for command in &triggers.commands {
                let words: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
                if let Some(first) = words.first() {
                    index
                        .commands
                        .entry(first.clone())
                        .or_default()
                        .push(CommandRule {
                            skill: skill.to_owned(),
                            key: command.clone(),
                            words,
                        });
                }
            }
        }
        index
    }

    /// Die Priorität eines Skills (Standard, wenn unbekannt).
    #[must_use]
    pub fn priority(&self, skill: &str) -> u8 {
        self.priorities
            .get(skill)
            .copied()
            .unwrap_or(DEFAULT_TRIGGER_PRIORITY)
    }

    /// Ob der Index keinerlei Regel enthält.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keywords.is_empty()
            && self.error_codes.is_empty()
            && self.paths.is_empty()
            && self.tools.is_empty()
            && self.commands.is_empty()
    }

    /// Alle Treffer der `events`, unabhängig von deren Reihenfolge.
    pub(crate) fn hits(&self, events: &[TriggerEvent<'_>]) -> Hits {
        let mut hits = Hits::new();
        let mut add = |skill: &str, kind: TriggerKind, key: &str| {
            hits.entry(skill.to_owned())
                .or_default()
                .insert(TriggerReason {
                    kind,
                    key: key.to_owned(),
                });
        };
        for event in events {
            match *event {
                TriggerEvent::TaskText(text) => {
                    let window = scan_window(text, MAX_SCAN_BYTES);
                    self.match_keywords(&window, &mut add);
                    self.match_error_codes(&window, &mut add);
                }
                TriggerEvent::ToolOutput { text, .. } => {
                    let window = scan_window(text, MAX_SCAN_BYTES);
                    self.match_error_codes(&window, &mut add);
                }
                TriggerEvent::ToolFirstUse(tool) => {
                    if let Some(skills) = self.tools.get(tool.trim()) {
                        for skill in skills {
                            add(skill, TriggerKind::Tool, tool.trim());
                        }
                    }
                }
                TriggerEvent::PathTouched(path) => {
                    for rule in &self.paths {
                        if rule.glob.is_match(path) {
                            add(&rule.skill, TriggerKind::Path, &rule.key);
                        }
                    }
                }
                TriggerEvent::Command(command) => {
                    self.match_command(command, &mut add);
                }
            }
        }
        hits
    }

    /// Wörter- und Phrasensuche mit Wortgrenzen.
    fn match_keywords(&self, text: &str, add: &mut impl FnMut(&str, TriggerKind, &str)) {
        if self.keywords.is_empty() {
            return;
        }
        let tokens = word_tokens(text);
        for (position, token) in tokens.iter().enumerate() {
            let Some(rules) = self.keywords.get(token) else {
                continue;
            };
            for rule in rules {
                let end = position + rule.tokens.len();
                if tokens.get(position..end) == Some(rule.tokens.as_slice()) {
                    add(&rule.skill, TriggerKind::Keyword, &rule.key);
                }
            }
        }
    }

    /// Token-Suche: ein Kandidat ist eine maximale alphanumerische Folge oder
    /// eine maximale Folge aus Alphanumerischem, `_`, `-` (ohne Rand-`-`/`_`).
    /// So trifft `E0505` weder `E05051` noch `XE0505`.
    fn match_error_codes(&self, text: &str, add: &mut impl FnMut(&str, TriggerKind, &str)) {
        if self.error_codes.is_empty() {
            return;
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut candidates = |run: &str| {
            let candidate = run.to_lowercase();
            if candidate.len() <= MAX_ERROR_CODE_CHARS * 4 && seen.insert(candidate.clone()) {
                if let Some(rules) = self.error_codes.get(&candidate) {
                    for (skill, key) in rules {
                        add(skill, TriggerKind::ErrorCode, key);
                    }
                }
            }
        };
        for run in text.split(|ch: char| !ch.is_alphanumeric()) {
            if !run.is_empty() {
                candidates(run);
            }
        }
        for run in text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '-')) {
            let run = run.trim_matches(['_', '-']);
            if !run.is_empty() {
                candidates(run);
            }
        }
    }

    fn match_command(&self, command: &str, add: &mut impl FnMut(&str, TriggerKind, &str)) {
        if self.commands.is_empty() {
            return;
        }
        let window = scan_window(command, MAX_COMMAND_SCAN_BYTES);
        for part in window.split(['\n', ';', '|', '&']) {
            let words: Vec<&str> = part
                .split_whitespace()
                .skip_while(|word| is_env_assignment(word) || *word == "sudo")
                .collect();
            let Some(first) = words.first() else {
                continue;
            };
            let Some(rules) = self.commands.get(*first) else {
                continue;
            };
            for rule in rules {
                let prefix = words.get(..rule.words.len());
                if prefix.is_some_and(|prefix| {
                    prefix
                        .iter()
                        .zip(&rule.words)
                        .all(|(a, b)| *a == b.as_str())
                }) {
                    add(&rule.skill, TriggerKind::Command, &rule.key);
                }
            }
        }
    }
}

fn is_env_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|ch: char| ch.is_ascii_digit())
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn index_with(triggers: SkillTriggers) -> TriggerIndex {
        TriggerIndex::from_entries([("s", &triggers)])
    }

    fn hit_keys(index: &TriggerIndex, event: TriggerEvent<'_>) -> Vec<String> {
        index
            .hits(&[event])
            .values()
            .flatten()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn glob_matches_and_edge_cases() {
        let m = |pattern: &str, path: &str| Glob::compile(pattern).is_match(path);
        assert!(m("**/*.rs", "main.rs"));
        assert!(m("**/*.rs", "src/a/b/main.rs"));
        assert!(!m("**/*.rs", "src/main.rsx"));
        assert!(!m("**/*.rs", "src/main.r"));
        assert!(m("*.rs", "src/deep/main.rs"), "ohne / zählt der Dateiname");
        assert!(m("src/*.rs", "src/x.rs"));
        assert!(!m("src/*.rs", "src/a/x.rs"), "* überschreitet kein /");
        assert!(m("src/**", "src"));
        assert!(m("src/**", "src/a/b"));
        assert!(!m("src/**", "other/a"));
        assert!(m("a/**/z", "a/z"));
        assert!(m("a/**/z", "a/b/c/z"));
        assert!(m("file?.txt", "file1.txt"));
        assert!(!m("file?.txt", "file10.txt"));
        assert!(m("/etc/**", "/etc/passwd"));
        assert!(!m("/etc/**", "etc/passwd"));
        assert!(m("./src/*.rs", "./src/a.rs"));
        assert!(m("Cargo.toml", "workspace/Cargo.toml"));
        assert!(!m("Cargo.toml", "workspace/cargo.toml"), "Groß/klein zählt");
        assert!(m("src\\*.rs", "src\\a.rs"));
        assert!(!m("**/*.rs", ""));
    }

    #[test]
    fn glob_worst_cases_stay_fast() {
        let started = std::time::Instant::now();
        let pattern = "*a".repeat(64);
        let glob = Glob::compile(&pattern);
        let path = format!("{}b", "a".repeat(4000));
        assert!(!glob.is_match(&path));
        let deep = Glob::compile(&"**/x/".repeat(25));
        assert!(deep.is_match(&"x/".repeat(1500)));
        assert!(!deep.is_match(&"y/".repeat(1500)));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn error_code_needs_token_boundaries() {
        let triggers = SkillTriggers {
            error_codes: vec!["E0505".to_owned()],
            ..SkillTriggers::default()
        };
        let index = index_with(triggers);
        let hit =
            |text: &str| !hit_keys(&index, TriggerEvent::ToolOutput { tool: "t", text }).is_empty();
        assert!(hit("error[E0505]: cannot move out"));
        assert!(hit("e0505"));
        assert!(hit("E0505"));
        assert!(hit("see E0505, then"));
        assert!(!hit("error E05051"));
        assert!(!hit("XE0505"));
        assert!(!hit("E050"));
        assert!(!hit("E0505x"));
        assert!(!hit(""));
    }

    #[test]
    fn commands_need_an_argument_boundary() {
        let triggers = SkillTriggers {
            commands: vec!["cargo test".to_owned()],
            ..SkillTriggers::default()
        };
        let index = index_with(triggers);
        let hit = |cmd: &str| !hit_keys(&index, TriggerEvent::Command(cmd)).is_empty();
        assert!(hit("cargo test"));
        assert!(hit("cargo test -p harw-catalog"));
        assert!(hit("  cargo   test  "));
        assert!(hit("cd x && cargo test --all"));
        assert!(hit("RUST_LOG=debug cargo test"));
        assert!(!hit("cargo testfoo"));
        assert!(!hit("cargo"));
        assert!(!hit("mycargo test"));
        assert!(!hit("echo cargo test"));
        assert!(!hit(""));
    }

    #[test]
    fn keywords_use_word_boundaries_and_ignore_case() {
        let triggers = SkillTriggers {
            keywords: vec!["borrow checker".to_owned()],
            ..SkillTriggers::default()
        };
        let index = index_with(triggers);
        let hit = |text: &str| !hit_keys(&index, TriggerEvent::TaskText(text)).is_empty();
        assert!(hit("Der Borrow Checker meckert"));
        assert!(hit("BORROW-CHECKER"));
        assert!(!hit("borrow checkers"));
        assert!(!hit("unborrow checker"));
        assert!(!hit("borrow the checker"));
    }

    #[test]
    fn parse_drops_invalid_parts_with_reasons() -> TestResult {
        let source = r#"
when = "kurz"
bogus = 1
[triggers]
keywords = ["ok wort", "a", 5, "x\ty\u0001z"]
error_codes = ["E0505", "bad code!", "E0505"]
paths = "nicht eine liste"
tools = ["process.execute", "bad tool"]
commands = ["cargo test", "a && b"]
priority = 300
extra = true
"#;
        let (spec, warnings) = parse_triggers_file(source);
        assert_eq!(spec.when.as_deref(), Some("kurz"));
        assert_eq!(spec.triggers.keywords, ["ok wort"]);
        assert_eq!(spec.triggers.error_codes, ["E0505"]);
        assert!(spec.triggers.paths.is_empty());
        assert_eq!(spec.triggers.tools, ["process.execute"]);
        assert_eq!(spec.triggers.commands, ["cargo test"]);
        assert_eq!(spec.triggers.priority, DEFAULT_TRIGGER_PRIORITY);
        let joined = warnings.join("\n");
        for needle in [
            "bogus",
            "keywords[1]",
            "keywords[2]",
            "keywords[3]",
            "error_codes[1]",
            "triggers.paths",
            "tools[1]",
            "commands[1]",
            "priority",
            "triggers.extra",
        ] {
            assert!(joined.contains(needle), "{needle} fehlt in {joined}");
        }
        Ok(())
    }

    #[test]
    fn parse_limits_when_and_entry_counts() -> TestResult {
        let long_when = "x".repeat(MAX_WHEN_CHARS + 1);
        let (spec, warnings) = parse_triggers_file(&format!("when = \"{long_when}\"\n"));
        assert!(spec.when.is_none());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let ok_when = "x".repeat(MAX_WHEN_CHARS);
        let (spec, _) = parse_triggers_file(&format!("when = \"{ok_when}\"\n"));
        assert_eq!(spec.when.as_deref().map(str::len), Some(MAX_WHEN_CHARS));

        let many: Vec<String> = (0..40).map(|n| format!("\"kw wort{n}\"")).collect();
        let (spec, warnings) =
            parse_triggers_file(&format!("[triggers]\nkeywords = [{}]\n", many.join(",")));
        assert_eq!(spec.triggers.keywords.len(), MAX_TRIGGER_ENTRIES);
        assert!(warnings.iter().any(|w| w.contains("40 Einträge")));

        let (spec, warnings) = parse_triggers_file("kein toml [[[");
        assert!(spec.triggers.is_empty() && spec.when.is_none());
        assert_eq!(warnings.len(), 1);
        let (_, warnings) = parse_triggers_file(&"#".repeat(MAX_TRIGGERS_FILE_BYTES + 1));
        assert_eq!(warnings.len(), 1);
        let (empty, warnings) = parse_triggers_file("");
        assert!(warnings.is_empty());
        assert_eq!(empty, SkillTriggerSpec::default());
        Ok(())
    }

    #[test]
    fn scan_window_never_splits_a_character() -> TestResult {
        let text = "ä".repeat(MAX_SCAN_BYTES);
        let window = scan_window(&text, MAX_SCAN_BYTES);
        assert!(window.len() <= MAX_SCAN_BYTES + 1);
        let tiny = scan_window("äöüäöü", 5);
        assert!(!tiny.is_empty());
        Ok(())
    }

    #[test]
    fn tool_and_path_events_hit() {
        let triggers = SkillTriggers {
            tools: vec!["process.execute".to_owned()],
            paths: vec!["**/*.rs".to_owned()],
            ..SkillTriggers::default()
        };
        let index = index_with(triggers);
        assert_eq!(
            hit_keys(&index, TriggerEvent::ToolFirstUse("process.execute")),
            ["tool process.execute"]
        );
        assert!(hit_keys(&index, TriggerEvent::ToolFirstUse("process.executex")).is_empty());
        assert_eq!(
            hit_keys(&index, TriggerEvent::PathTouched("src/lib.rs")),
            ["path **/*.rs"]
        );
        assert!(hit_keys(&index, TriggerEvent::PathTouched("README.md")).is_empty());
    }
}
