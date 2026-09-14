//! Adressierbare Fakten (Langzeitgedächtnis v3), siehe
//! `docs/design/memory-v3-ltm.md` §3 (Der Fakt), §5.4 (Verdrängung) und §7
//! (Invarianten).
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt genau eine Datei-Wahrheit: `facts/<name>.md`, ein
//! Markdown-Dokument aus strengem YAML-ähnlichem Frontmatter plus Freitext.
//! Es implementiert:
//! - [`FactType`], [`FactScope`], [`Fact`] — die Datentypen aus §3.
//! - [`slugify`] — stabile Dateinamen aus Titeln.
//! - [`redact`] — Muster-basierte Schwärzung von Geheimnissen vor jedem
//!   Schreiben (§3: „Kein Geheimnis im Fakt").
//! - [`FactStore`] — Laden/Schreiben/Löschen/Suchen einzelner Fakten, den
//!   generierten Index `MEMORY.md` und die Nutzungszähler `usage.json`.
//!
//! Kein Modellaufruf, keine `serde_yaml`-Abhängigkeit: das Frontmatter kennt
//! nur die in §3 gezeigten Schlüssel (`name`, `description`, `type`, `scope`,
//! `created`, `updated`, `confidence`, `sources`, `tags`) und wird von Hand
//! geparst/serialisiert. Unbekannte Schlüssel werden beim Lesen ignoriert und
//! beim Schreiben verworfen — ein Fakt trägt nur, was dieses Modul kennt.
//!
//! # Nebenläufigkeit
//! Alle Schreibvorgänge laufen über [`harw_fsutil::write_atomic`] (Tempdatei
//! + `fsync` + `rename`), sind also für sich atomar. `FactStore` serialisiert
//!   selbst nicht zwischen mehreren Schreibern über mehrere Prozesse hinweg —
//!   das ist Aufgabe des Aufrufers (vgl. `FileMemoryStore`s `maintain()`-Lock).
//!
//! # Fehler
//! [`MemoryError::Io`], [`MemoryError::FrontmatterInvalid`],
//! [`MemoryError::InvalidFactName`], [`MemoryError::InvalidEnumValue`],
//! [`MemoryError::Serde`] (nur für `usage.json`, das regulär JSON ist).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{MemoryError, MemoryResult};

/// Rechte-Bits für neu angelegte Fakt-Dateien und `MEMORY.md`/`usage.json`.
const FACT_FILE_MODE: u32 = 0o600;

/// Art eines Fakts — steuert laut Design §3/§4 die Ladepriorität
/// (`preference`/`decision` werden bevorzugt geladen, `reference` nur bei
/// Stichworttreffern).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactType {
    /// Ein Sachverhalt ohne besonderen Status.
    Fact,
    /// Eine getroffene Entscheidung samt Begründung.
    Decision,
    /// Eine Präferenz des Nutzers.
    Preference,
    /// Eine bekannte Falle/ein bekannter Fehler, den man vermeiden soll.
    Pitfall,
    /// Ein Verweis auf externes Material.
    Reference,
}

impl FactType {
    /// Alle Varianten in stabiler, für Index-Gruppierung genutzter
    /// Reihenfolge.
    pub const ALL: [Self; 5] = [
        Self::Fact,
        Self::Decision,
        Self::Preference,
        Self::Pitfall,
        Self::Reference,
    ];

    /// Kanonischer Frontmatter-Wert (`type: <as_str>`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Preference => "preference",
            Self::Pitfall => "pitfall",
            Self::Reference => "reference",
        }
    }

    /// Deutsche Überschrift für die `MEMORY.md`-Gruppierung.
    #[must_use]
    const fn heading(self) -> &'static str {
        match self {
            Self::Fact => "Fakt",
            Self::Decision => "Entscheidung",
            Self::Preference => "Präferenz",
            Self::Pitfall => "Falle",
            Self::Reference => "Referenz",
        }
    }
}

impl fmt::Display for FactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FactType {
    type Err = MemoryError;

    /// Parst den Frontmatter-Wert von `type`.
    ///
    /// # Errors
    /// [`MemoryError::InvalidEnumValue`], wenn `s` keiner Variante entspricht.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fact" => Ok(Self::Fact),
            "decision" => Ok(Self::Decision),
            "preference" => Ok(Self::Preference),
            "pitfall" => Ok(Self::Pitfall),
            "reference" => Ok(Self::Reference),
            other => Err(MemoryError::InvalidEnumValue {
                field: "type",
                value: other.to_owned(),
            }),
        }
    }
}

/// Lebensdauer-Scope eines Fakts, siehe Design §2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactScope {
    /// `<projekt>/.harw/memories/` — bleibt im Repo.
    Project,
    /// `~/.harw/profiles/<p>/memories/` — projektübergreifend.
    Global,
}

impl FactScope {
    /// Kanonischer Frontmatter-Wert (`scope: <as_str>`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}

impl fmt::Display for FactScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FactScope {
    type Err = MemoryError;

    /// Parst den Frontmatter-Wert von `scope`.
    ///
    /// # Errors
    /// [`MemoryError::InvalidEnumValue`], wenn `s` keiner Variante entspricht.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "project" => Ok(Self::Project),
            "global" => Ok(Self::Global),
            other => Err(MemoryError::InvalidEnumValue {
                field: "scope",
                value: other.to_owned(),
            }),
        }
    }
}

/// Ein Fakt: Frontmatter-Metadaten plus Freitext-`body`, siehe Design §3.
#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    /// Stabiler Name (Dateiname ohne `.md`), kebab-case.
    pub name: String,
    /// Kurzbeschreibung — erscheint im `MEMORY.md`-Index.
    pub description: String,
    /// Art des Fakts.
    pub fact_type: FactType,
    /// Lebensdauer-Scope.
    pub scope: FactScope,
    /// Anlagezeitpunkt (bleibt über Updates hinweg stabil).
    pub created: OffsetDateTime,
    /// Zeitpunkt der letzten Änderung.
    pub updated: OffsetDateTime,
    /// Vertrauenswert `0.0..=1.0`, siehe §5.4 (Verdrängung).
    pub confidence: f32,
    /// Belege (`session:…`, `file:…#L…`), optional.
    pub sources: Vec<String>,
    /// Freie Schlagworte.
    pub tags: Vec<String>,
    /// Freitext-Inhalt nach dem Frontmatter.
    pub body: String,
}

/// Baut einen Titel in einen stabilen, dateinamentauglichen Slug um.
///
/// # Beschreibung
/// ASCII-Kleinschreibung, jede Nicht-alphanumerische-Sequenz wird zu einem
/// einzelnen `-`, führende/folgende `-` werden entfernt, das Ergebnis wird
/// auf 60 Zeichen gekappt (erneut ohne folgenden `-`). Ein leeres Ergebnis
/// (z. B. Titel nur aus Symbolen) liefert `"fakt"` statt einer leeren
/// Zeichenkette — [`FactStore::path_for`] verlangt ein nicht-leeres Präfix.
///
/// # Examples
/// ```
/// use harw_memory::facts::slugify;
///
/// assert_eq!(slugify("TUI Approval Arming!"), "tui-approval-arming");
/// assert_eq!(slugify("   "), "fakt");
/// assert_eq!(slugify("###"), "fakt");
/// ```
#[must_use]
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_was_dash = false;
    for ch in title.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_was_dash = false;
        } else if !out.is_empty() && !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 60 {
        out.truncate(60);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "fakt".to_owned()
    } else {
        out
    }
}

/// Prüft, dass `name` dem Muster `[a-z0-9][a-z0-9-]{0,59}` entspricht.
///
/// # Errors
/// [`MemoryError::InvalidFactName`] bei leerem Namen, Großbuchstaben, `/`,
/// `.` oder anderen Zeichen außerhalb der erlaubten Menge, oder bei mehr als
/// 60 Zeichen — schützt `facts/<name>.md` vor Pfad-Traversal.
fn validate_name(name: &str) -> MemoryResult<()> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if first_ok && rest_ok && name.len() <= 60 {
        Ok(())
    } else {
        Err(MemoryError::InvalidFactName {
            name: name.to_owned(),
        })
    }
}

// ---------------------------------------------------------------------
// Redaction (§3: „Kein Geheimnis im Fakt")
// ---------------------------------------------------------------------

/// Ersetzt offensichtliche Geheimnisse in `text` durch `[redacted]`.
///
/// # Beschreibung
/// Handgeschriebene Erkennung (keine `regex`-Abhängigkeit, da `harw-memory`
/// bislang keine hat) für: `sk-…`-Schlüssel, GitHub-Tokens (`ghp_`/`gho_`/
/// `ghu_`/`ghs_`/`ghr_`), AWS-Zugriffsschlüssel (`AKIA…`), Slack-Tokens
/// (`xoxb-`/`xoxa-`/`xoxp-`/`xoxr-`/`xoxs-`), PEM-Private-Key-Blöcke
/// (`-----BEGIN … PRIVATE KEY-----` bis `-----END …-----`),
/// `Bearer <token>`-Header, `password=`/`api_key=`/`apikey=`/`token=`/
/// `secret=`-Werte, sowie jeden Base64-artigen Lauf ab 32 Zeichen, dem
/// innerhalb von 20 Zeichen `key`/`secret`/`token` vorausgeht.
/// [`FactStore::write`] ruft dies auf `body`, `description` und jedes `tag`
/// an, bevor geschrieben wird.
///
/// # Examples
/// ```
/// use harw_memory::facts::redact;
///
/// assert_eq!(redact("normaler Text ohne Geheimnis"), "normaler Text ohne Geheimnis");
/// assert!(redact("key = sk-abcdefghijklmnopqrstuvwxyz").contains("[redacted]"));
/// ```
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    out = redact_pem_blocks(&out);
    out = redact_bearer(&out);
    out = redact_key_value_pairs(&out);
    out = redact_prefix_tokens(&out, &["sk-"], |c| c.is_ascii_alphanumeric(), 16);
    out = redact_prefix_tokens(
        &out,
        &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        |c| c.is_ascii_alphanumeric(),
        20,
    );
    out = redact_prefix_tokens(
        &out,
        &["AKIA"],
        |c| c.is_ascii_uppercase() || c.is_ascii_digit(),
        16,
    );
    out = redact_prefix_tokens(
        &out,
        &["xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-"],
        |c| c.is_ascii_alphanumeric() || c == '-',
        10,
    );
    out = redact_near_keyword_base64(&out);
    out
}

/// Ersetzt `-----BEGIN … PRIVATE KEY-----`-Blöcke (bis zur nächsten
/// `-----END …-----`-Zeile) durch `[redacted]`.
fn redact_pem_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(begin_pos) = rest.find("-----BEGIN ") else {
            out.push_str(rest);
            return out;
        };
        let after_begin = &rest[begin_pos..];
        let line_len = after_begin.find('\n').unwrap_or(after_begin.len());
        let begin_line = &after_begin[..line_len];
        if !begin_line.contains("PRIVATE KEY") {
            let advance = (begin_pos + line_len + 1).min(rest.len());
            out.push_str(&rest[..advance]);
            rest = &rest[advance..];
            continue;
        }
        let Some(end_rel) = after_begin.find("-----END ") else {
            out.push_str(rest);
            return out;
        };
        let after_end_marker = &after_begin[end_rel..];
        let end_line_len = after_end_marker
            .find('\n')
            .map_or(after_end_marker.len(), |p| p + 1);
        out.push_str(&rest[..begin_pos]);
        out.push_str("[redacted]");
        let consumed = begin_pos + end_rel + end_line_len;
        rest = &rest[consumed..];
    }
}

/// Byte-Vergleich, ASCII-case-insensitive; liefert einen zeichengrenzen-
/// sicheren Byte-Index (nötig, da `text` Unicode enthalten darf).
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    for start in 0..=(h.len() - n.len()) {
        if !haystack.is_char_boundary(start) {
            continue;
        }
        if h[start..start + n.len()].eq_ignore_ascii_case(n) {
            return Some(start);
        }
    }
    None
}

/// Ersetzt `Bearer <token>` (case-insensitives `bearer `) durch
/// `Bearer [redacted]`.
fn redact_bearer(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(pos) = find_ci(rest, "bearer ") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..pos]);
        out.push_str("Bearer [redacted]");
        let after = &rest[pos + "bearer ".len()..];
        let token_end = after.find(char::is_whitespace).unwrap_or(after.len());
        rest = &after[token_end..];
    }
}

/// Wortzeichen für Grenzprüfung um `key=value`-Muster.
const fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Ersetzt `password=`/`api_key=`/`apikey=`/`token=`/`secret=`-Werte.
fn redact_key_value_pairs(text: &str) -> String {
    let mut out = text.to_owned();
    for key in ["password", "api_key", "apikey", "token", "secret"] {
        out = redact_one_kv(&out, key);
    }
    out
}

/// Ersetzt genau ein `<key>=<value>`-Muster (alle Vorkommen von `key`).
fn redact_one_kv(text: &str, key: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(pos) = find_ci(rest, key) else {
            out.push_str(rest);
            return out;
        };
        let before_ok = rest[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_key = &rest[pos + key.len()..];
        let after_ok = after_key.chars().next().is_none_or(|c| !is_word_char(c));
        if !before_ok || !after_ok {
            out.push_str(&rest[..pos + key.len()]);
            rest = after_key;
            continue;
        }
        let stripped_ws = after_key.trim_start_matches(' ');
        if !stripped_ws.starts_with('=') {
            out.push_str(&rest[..pos + key.len()]);
            rest = after_key;
            continue;
        }
        let after_eq = &stripped_ws[1..];
        let value_start = after_eq.trim_start_matches(' ');
        let prefix_len = after_key.len() - value_start.len();
        let value_end = value_start
            .find(char::is_whitespace)
            .unwrap_or(value_start.len());
        out.push_str(&rest[..pos + key.len()]);
        out.push_str(&after_key[..prefix_len]);
        out.push_str("[redacted]");
        rest = &value_start[value_end..];
    }
}

/// Ersetzt jedes Vorkommen von `prefix` gefolgt von mindestens `min_len`
/// Zeichen aus `class` durch `[redacted]` (samt Präfix).
fn redact_prefix_tokens(
    text: &str,
    prefixes: &[&str],
    class: impl Fn(char) -> bool,
    min_len: usize,
) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < n {
        let mut matched = false;
        for prefix in prefixes {
            let pchars: Vec<char> = prefix.chars().collect();
            let plen = pchars.len();
            if i + plen <= n && chars[i..i + plen] == pchars[..] {
                let mut j = i + plen;
                while j < n && class(chars[j]) {
                    j += 1;
                }
                if j - (i + plen) >= min_len {
                    out.push_str("[redacted]");
                    i = j;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Ersetzt Base64-artige Läufe ab 32 Zeichen, denen innerhalb von 20 Zeichen
/// `key`/`secret`/`token` vorausgeht.
fn redact_near_keyword_base64(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let is_b64 = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-');
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if is_b64(chars[i]) {
            let start = i;
            while i < n && is_b64(chars[i]) {
                i += 1;
            }
            if i - start >= 32 {
                runs.push((start, i));
            }
        } else {
            i += 1;
        }
    }
    if runs.is_empty() {
        return text.to_owned();
    }
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut redacted = vec![false; n];
    for &(start, end) in &runs {
        let window_start = start.saturating_sub(20);
        let window: String = lower[window_start..start].iter().collect();
        if ["key", "secret", "token"].iter().any(|k| window.contains(k)) {
            for flag in &mut redacted[start..end] {
                *flag = true;
            }
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < n {
        if redacted[i] {
            out.push_str("[redacted]");
            while i < n && redacted[i] {
                i += 1;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------
// Frontmatter: Parsen
// ---------------------------------------------------------------------

/// Ein roher Frontmatter-Wert vor der Typisierung.
enum RawValue {
    /// Ein einzelner Skalarwert.
    Scalar(String),
    /// Eine Liste (inline `[a, b]` oder Block `- item`).
    List(Vec<String>),
}

/// Entfernt umschließende `"…"`/`'…'` und entdoppelt interne Anführungszeichen
/// (YAML-artig: `""` → `"` innerhalb `"…"`, `''` → `'` innerhalb `'…'`).
fn unquote(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return s[1..s.len() - 1].replace("\"\"", "\"");
    }
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        return s[1..s.len() - 1].replace("''", "'");
    }
    s.to_owned()
}

/// Zerlegt ein Dokument in Frontmatter-Block und Body.
///
/// # Errors
/// Textbeschreibung, wenn das Dokument nicht mit `---\n` beginnt oder kein
/// schließendes `---` gefunden wird.
fn split_frontmatter(raw: &str) -> Result<(String, String), String> {
    let normalized = raw.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return Err("Dokument beginnt nicht mit '---'".to_owned());
    };
    let Some(end_idx) = find_closing_delimiter(rest) else {
        return Err("kein schließendes '---' gefunden".to_owned());
    };
    let frontmatter = rest[..end_idx].to_owned();
    let after = &rest[end_idx..];
    let after_delim = after
        .strip_prefix("---\n")
        .or_else(|| after.strip_prefix("---"))
        .unwrap_or(after);
    let body = after_delim.strip_prefix('\n').unwrap_or(after_delim);
    Ok((frontmatter, body.to_owned()))
}

/// Sucht die erste Zeile, die exakt `---` lautet; liefert deren Startoffset.
fn find_closing_delimiter(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if trimmed == "---" {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// Parst die Zeilen eines Frontmatter-Blocks in Schlüssel/Rohwert-Paare.
///
/// # Errors
/// Textbeschreibung bei Zeilen ohne `:`, ungültigen Schlüsseln oder
/// unvollständigen `[…]`-Listen.
fn parse_frontmatter_fields(frontmatter: &str) -> Result<HashMap<String, RawValue>, String> {
    let mut fields = HashMap::new();
    let lines: Vec<&str> = frontmatter.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let Some(colon) = line.find(':') else {
            return Err(format!("Zeile ohne ':': {line:?}"));
        };
        let key = line[..colon].trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("ungültiger Schlüssel: {key:?}"));
        }
        let value_part = line[colon + 1..].trim();
        i += 1;
        if value_part.is_empty() {
            let mut items = Vec::new();
            let mut saw_item = false;
            while i < lines.len() {
                let candidate = lines[i];
                let trimmed = candidate.trim_start();
                if trimmed.is_empty() {
                    break;
                }
                if let Some(item) = trimmed.strip_prefix("- ") {
                    items.push(unquote(item));
                    saw_item = true;
                    i += 1;
                } else if trimmed == "-" {
                    items.push(String::new());
                    saw_item = true;
                    i += 1;
                } else {
                    break;
                }
            }
            if saw_item {
                fields.insert(key.to_owned(), RawValue::List(items));
            } else {
                fields.insert(key.to_owned(), RawValue::Scalar(String::new()));
            }
        } else if let Some(rest) = value_part.strip_prefix('[') {
            let Some(inner) = rest.strip_suffix(']') else {
                return Err(format!("nicht geschlossene Liste bei Schlüssel {key:?}"));
            };
            let items = if inner.trim().is_empty() {
                Vec::new()
            } else {
                inner.split(',').map(|s| unquote(s.trim())).collect()
            };
            fields.insert(key.to_owned(), RawValue::List(items));
        } else {
            fields.insert(key.to_owned(), RawValue::Scalar(unquote(value_part)));
        }
    }
    Ok(fields)
}

/// Parst einen RFC-3339-Zeitstempel.
fn parse_rfc3339(raw: &str) -> Result<OffsetDateTime, String> {
    OffsetDateTime::parse(raw.trim(), &time::format_description::well_known::Rfc3339)
        .map_err(|e| format!("ungültiger Zeitstempel {raw:?}: {e}"))
}

/// Baut einen [`Fact`] aus geparsten Frontmatter-Feldern und Body.
///
/// Fehlende optionale Schlüssel bekommen die in Design §3 genannten
/// Defaults: `confidence = 0.8`, `tags`/`sources` leer, `type = fact`,
/// `scope` = `default_scope` (der Scope der [`FactStore`]-Wurzel, in der der
/// Fakt liegt). `description` ist das einzige Pflichtfeld.
///
/// # Errors
/// Textbeschreibung, wenn `description` fehlt oder ein Feld nicht in seinen
/// Zieltyp parst (`type`, `scope`, `created`, `updated`, `confidence`).
fn fact_from_fields(
    fields: &HashMap<String, RawValue>,
    body: String,
    fallback_name: &str,
    default_scope: FactScope,
) -> Result<Fact, String> {
    let get_scalar = |key: &str| -> Option<String> {
        match fields.get(key) {
            Some(RawValue::Scalar(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    };
    let get_list = |key: &str| -> Vec<String> {
        match fields.get(key) {
            Some(RawValue::List(items)) => items.clone(),
            _ => Vec::new(),
        }
    };

    let name = get_scalar("name").unwrap_or_else(|| fallback_name.to_owned());
    let description =
        get_scalar("description").ok_or_else(|| "Pflichtfeld 'description' fehlt".to_owned())?;
    let fact_type = match get_scalar("type") {
        Some(raw) => raw
            .parse::<FactType>()
            .map_err(|_| format!("unbekannter type: {raw:?}"))?,
        None => FactType::Fact,
    };
    let scope = match get_scalar("scope") {
        Some(raw) => raw
            .parse::<FactScope>()
            .map_err(|_| format!("unbekannter scope: {raw:?}"))?,
        None => default_scope,
    };
    let created = match get_scalar("created") {
        Some(raw) => parse_rfc3339(&raw)?,
        None => OffsetDateTime::now_utc(),
    };
    let updated = match get_scalar("updated") {
        Some(raw) => parse_rfc3339(&raw)?,
        None => created,
    };
    let confidence = match get_scalar("confidence") {
        Some(raw) => raw
            .trim()
            .parse::<f32>()
            .map_err(|_| format!("ungültige confidence: {raw:?}"))?,
        None => 0.8,
    };
    let sources = get_list("sources");
    let tags = get_list("tags");

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

/// Parst ein vollständiges `facts/<name>.md`-Dokument.
///
/// # Errors
/// Textbeschreibung bei fehlerhaftem Frontmatter (siehe
/// [`split_frontmatter`], [`parse_frontmatter_fields`],
/// [`fact_from_fields`]) — nie ein Panic.
fn fact_from_markdown(
    raw: &str,
    fallback_name: &str,
    default_scope: FactScope,
) -> Result<Fact, String> {
    let (frontmatter, body) = split_frontmatter(raw)?;
    let fields = parse_frontmatter_fields(&frontmatter)?;
    fact_from_fields(&fields, body, fallback_name, default_scope)
}

// ---------------------------------------------------------------------
// Frontmatter: Serialisieren
// ---------------------------------------------------------------------

/// Quotet einen Skalarwert, falls er Zeichen enthält, die das strenge
/// Parsing sonst falsch verstehen würde (`:`, `#`, `"`, Zeilenumbruch, oder
/// ein Listen-/Kommentar-einleitendes Zeichen am Anfang).
fn escape_scalar(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.contains(':')
        || s.contains('#')
        || s.contains('"')
        || s.contains('\n')
        || s.starts_with([' ', '-', '[', '\'']);
    if needs_quote {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// Formatiert einen Zeitstempel als RFC 3339 (`…Z`).
fn format_rfc3339(ts: OffsetDateTime) -> String {
    ts.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| ts.unix_timestamp().to_string())
}

/// Serialisiert einen [`Fact`] als vollständiges Markdown-Dokument
/// (`---\n<frontmatter>\n---\n\n<body>`).
///
/// Schreibt ausschließlich die in Design §3 gezeigten Schlüssel — jedes
/// unbekannte Feld eines zuvor gelesenen Dokuments ist damit beim Schreiben
/// verloren (siehe Modul-Doku).
fn to_markdown(fact: &Fact) -> String {
    let mut fm = String::new();
    fm.push_str(&format!("name: {}\n", fact.name));
    fm.push_str(&format!(
        "description: {}\n",
        escape_scalar(&fact.description)
    ));
    fm.push_str(&format!("type: {}\n", fact.fact_type.as_str()));
    fm.push_str(&format!("scope: {}\n", fact.scope.as_str()));
    fm.push_str(&format!("created: {}\n", format_rfc3339(fact.created)));
    fm.push_str(&format!("updated: {}\n", format_rfc3339(fact.updated)));
    fm.push_str(&format!("confidence: {:.2}\n", fact.confidence));
    if fact.sources.is_empty() {
        fm.push_str("sources: []\n");
    } else {
        fm.push_str("sources:\n");
        for source in &fact.sources {
            fm.push_str(&format!("  - {}\n", escape_scalar(source)));
        }
    }
    let tags = fact
        .tags
        .iter()
        .map(|t| escape_scalar(t))
        .collect::<Vec<_>>()
        .join(", ");
    fm.push_str(&format!("tags: [{tags}]\n"));

    let body = if fact.body.ends_with('\n') || fact.body.is_empty() {
        fact.body.clone()
    } else {
        format!("{}\n", fact.body)
    };
    format!("---\n{fm}---\n\n{body}")
}

// ---------------------------------------------------------------------
// Nutzungszähler (`usage.json`)
// ---------------------------------------------------------------------

/// Ein Eintrag in `usage.json`: Trefferzahl plus letzter Zugriff.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct UsageEntry {
    /// Anzahl aufgezeichneter Nutzungen.
    count: u64,
    /// Zeitpunkt der letzten Nutzung.
    #[serde(with = "time::serde::rfc3339")]
    last_used: OffsetDateTime,
}

/// Liest eine Datei symlink-sicher; `Ok(None)` wenn sie nicht existiert.
fn read_optional_no_symlink(path: &Path) -> MemoryResult<Option<Vec<u8>>> {
    match harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::read_only()) {
        Ok(mut file) => {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map_err(|e| MemoryError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            Ok(Some(buf))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(MemoryError::Io {
            path: path.to_path_buf(),
            source: e,
        }),
    }
}

// ---------------------------------------------------------------------
// FactStore
// ---------------------------------------------------------------------

/// Dateibasierter Speicher für Fakten unter einer Wurzel (Projekt oder
/// Global), siehe Design §2/§3.
///
/// # Beschreibung
/// Layout unterhalb von `root`:
/// ```text
/// <root>/
///   facts/<name>.md   ← ein Fakt je Datei
///   MEMORY.md         ← generierter Index (write_index)
///   usage.json        ← Nutzungszähler (record_usage/usage/decay)
/// ```
pub struct FactStore {
    /// Speicherwurzel (Projekt- oder Global-Verzeichnis).
    root: PathBuf,
    /// Scope dieser Wurzel — Default für Fakten ohne `scope`-Frontmatter.
    scope: FactScope,
}

impl FactStore {
    /// Öffnet den Store an `root`, legt `<root>/facts/` mit Rechten `0700`
    /// an, falls es fehlt.
    ///
    /// # Errors
    /// [`MemoryError::Io`], wenn das Verzeichnis nicht angelegt oder seine
    /// Rechte nicht gesetzt werden können.
    pub fn open(root: impl AsRef<Path>, scope: FactScope) -> MemoryResult<Self> {
        let root = root.as_ref().to_path_buf();
        let facts_dir = root.join("facts");
        fs::create_dir_all(&facts_dir).map_err(|e| MemoryError::Io {
            path: facts_dir.clone(),
            source: e,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&facts_dir, fs::Permissions::from_mode(0o700)).map_err(|e| {
                MemoryError::Io {
                    path: facts_dir.clone(),
                    source: e,
                }
            })?;
        }
        Ok(Self { root, scope })
    }

    /// Pfad von `facts/<name>.md`, nach Namensvalidierung.
    ///
    /// # Errors
    /// [`MemoryError::InvalidFactName`], wenn `name` nicht
    /// `[a-z0-9][a-z0-9-]{0,59}` entspricht (Pfad-Traversal-Schutz).
    pub fn path_for(&self, name: &str) -> MemoryResult<PathBuf> {
        validate_name(name)?;
        Ok(self.root.join("facts").join(format!("{name}.md")))
    }

    fn usage_path(&self) -> PathBuf {
        self.root.join("usage.json")
    }

    fn index_path(&self) -> PathBuf {
        self.root.join("MEMORY.md")
    }

    /// Schreibt `fact` atomar nach `facts/<name>.md`.
    ///
    /// # Beschreibung
    /// `updated` wird auf jetzt gesetzt; existiert bereits eine Datei für
    /// `fact.name`, bleibt deren `created` erhalten (sonst wird
    /// `fact.created` übernommen). `body`, `description` und jedes `tag`
    /// laufen vor dem Schreiben durch [`redact`]. Regeneriert danach
    /// `MEMORY.md` über [`Self::write_index`].
    ///
    /// # Errors
    /// [`MemoryError::InvalidFactName`] bei ungültigem `fact.name`;
    /// [`MemoryError::Io`] bei Schreibfehlern; Fehler von
    /// [`Self::read`]/[`Self::write_index`] werden durchgereicht.
    pub fn write(&self, fact: &Fact) -> MemoryResult<()> {
        validate_name(&fact.name)?;
        let path = self.path_for(&fact.name)?;
        let created = match self.read(&fact.name)? {
            Some(existing) => existing.created,
            None => fact.created,
        };

        let mut to_write = fact.clone();
        to_write.created = created;
        to_write.updated = OffsetDateTime::now_utc();
        to_write.description = redact(&to_write.description);
        to_write.body = redact(&to_write.body);
        to_write.tags = to_write.tags.iter().map(|t| redact(t)).collect();

        let markdown = to_markdown(&to_write);
        harw_fsutil::write_atomic(
            &path,
            markdown.as_bytes(),
            harw_fsutil::AtomicWriteOptions::with_mode(FACT_FILE_MODE),
        )
        .map_err(|e| MemoryError::Io {
            path: path.clone(),
            source: e,
        })?;
        self.write_index()
    }

    /// Schreibt `fact` ohne `created`/`updated`-Pflege und ohne erneute
    /// Redaction — genutzt von [`Self::decay`], das nur `confidence` ändert
    /// und keine „Nutzung" des Inhalts darstellt.
    fn write_raw(&self, fact: &Fact) -> MemoryResult<()> {
        let path = self.path_for(&fact.name)?;
        let markdown = to_markdown(fact);
        harw_fsutil::write_atomic(
            &path,
            markdown.as_bytes(),
            harw_fsutil::AtomicWriteOptions::with_mode(FACT_FILE_MODE),
        )
        .map_err(|e| MemoryError::Io { path, source: e })
    }

    /// Liest `facts/<name>.md`, `Ok(None)` wenn die Datei fehlt.
    ///
    /// # Errors
    /// [`MemoryError::InvalidFactName`] bei ungültigem `name`;
    /// [`MemoryError::FrontmatterInvalid`] bei fehlerhaftem Inhalt;
    /// [`MemoryError::Io`] bei sonstigen Lesefehlern (auch: `name` zeigt auf
    /// einen Symlink — das letzte Pfadglied wird nie verfolgt).
    pub fn read(&self, name: &str) -> MemoryResult<Option<Fact>> {
        let path = self.path_for(name)?;
        let Some(bytes) = read_optional_no_symlink(&path)? else {
            return Ok(None);
        };
        let raw = String::from_utf8(bytes).map_err(|e| MemoryError::FrontmatterInvalid {
            path: path.clone(),
            reason: format!("ungültiges UTF-8: {e}"),
        })?;
        fact_from_markdown(&raw, name, self.scope)
            .map(Some)
            .map_err(|reason| MemoryError::FrontmatterInvalid { path, reason })
    }

    /// Listet alle Fakten, sortiert nach `updated` absteigend.
    ///
    /// Beschädigte oder ungültig benannte Dateien werden übersprungen und
    /// per `tracing::warn!` gemeldet, statt den gesamten Aufruf scheitern zu
    /// lassen.
    ///
    /// # Errors
    /// [`MemoryError::Io`], wenn `facts/` selbst nicht gelesen werden kann.
    pub fn list(&self) -> MemoryResult<Vec<Fact>> {
        let dir = self.root.join("facts");
        let mut out = Vec::new();
        let entries = fs::read_dir(&dir).map_err(|e| MemoryError::Io {
            path: dir.clone(),
            source: e,
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| MemoryError::Io {
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
            match self.read(stem) {
                Ok(Some(fact)) => out.push(fact),
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "facts: überspringe defekten Fakt"
                    );
                }
            }
        }
        out.sort_by_key(|fact| std::cmp::Reverse(fact.updated));
        Ok(out)
    }

    /// Löscht `facts/<name>.md`, seinen `usage.json`-Eintrag und
    /// regeneriert `MEMORY.md`.
    ///
    /// # Returns
    /// `true`, wenn eine Datei gelöscht wurde; `false`, wenn keine existierte.
    ///
    /// # Errors
    /// [`MemoryError::InvalidFactName`] bei ungültigem `name`;
    /// [`MemoryError::Io`] bei Lösch-/Lesefehlern.
    pub fn delete(&self, name: &str) -> MemoryResult<bool> {
        let path = self.path_for(name)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                return Err(MemoryError::Io {
                    path: path.clone(),
                    source: e,
                });
            }
        }
        fs::remove_file(&path).map_err(|e| MemoryError::Io {
            path: path.clone(),
            source: e,
        })?;
        self.remove_usage_entry(name)?;
        self.write_index()?;
        Ok(true)
    }

    /// Stichwortsuche über alle Fakten.
    ///
    /// # Beschreibung
    /// Score je Stichwort (klein geschrieben, `contains`-Vergleich):
    /// Treffer im `name` zählt 3×, in `description`/`tags` 2×, im `body` 1×.
    /// Fakten mit Score 0 fallen heraus. Sortiert nach Score absteigend,
    /// bei Gleichstand nach `updated` absteigend; liefert höchstens `limit`
    /// Treffer.
    ///
    /// # Errors
    /// Fehler von [`Self::list`].
    pub fn search(&self, keywords: &[&str], limit: usize) -> MemoryResult<Vec<Fact>> {
        let facts = self.list()?;
        let lowered: Vec<String> = keywords
            .iter()
            .map(|k| k.to_lowercase())
            .filter(|k| !k.is_empty())
            .collect();
        let mut scored: Vec<(i64, Fact)> = facts
            .into_iter()
            .filter_map(|fact| {
                let score = score_fact(&fact, &lowered);
                (score > 0).then_some((score, fact))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.updated.cmp(&a.1.updated)));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, fact)| fact).collect())
    }

    /// Regeneriert `MEMORY.md`: eine Überschrift je [`FactType`] (in
    /// [`FactType::ALL`]-Reihenfolge), darunter je Fakt eine Zeile,
    /// alphabetisch nach `name` sortiert — deterministisch bei jedem Lauf.
    ///
    /// # Errors
    /// Fehler von [`Self::list`]; [`MemoryError::Io`] beim Schreiben.
    pub fn write_index(&self) -> MemoryResult<()> {
        let facts = self.list()?;
        let mut by_type: HashMap<FactType, Vec<&Fact>> = HashMap::new();
        for fact in &facts {
            by_type.entry(fact.fact_type).or_default().push(fact);
        }
        let mut out = format!("# Gedächtnis ({})\n\n", self.scope.as_str());
        for fact_type in FactType::ALL {
            let Some(mut group) = by_type.remove(&fact_type) else {
                continue;
            };
            group.sort_by(|a, b| a.name.cmp(&b.name));
            out.push_str(&format!("## {}\n", fact_type.heading()));
            for fact in group {
                let desc = truncate_chars(&fact.description, 80);
                let date = format_date(fact.updated);
                out.push_str(&format!(
                    "- [{desc}](facts/{}.md) — {}, aktualisiert {date}\n",
                    fact.name,
                    fact_type.as_str()
                ));
            }
            out.push('\n');
        }
        let path = self.index_path();
        harw_fsutil::write_atomic(
            &path,
            out.as_bytes(),
            harw_fsutil::AtomicWriteOptions::with_mode(FACT_FILE_MODE),
        )
        .map_err(|e| MemoryError::Io { path, source: e })
    }

    fn read_usage_map(&self) -> MemoryResult<HashMap<String, UsageEntry>> {
        let path = self.usage_path();
        match read_optional_no_symlink(&path)? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| MemoryError::Serde {
                    context: "usage.json lesen",
                    source: e,
                })
            }
            None => Ok(HashMap::new()),
        }
    }

    fn write_usage_map(&self, map: &HashMap<String, UsageEntry>) -> MemoryResult<()> {
        let path = self.usage_path();
        let bytes = serde_json::to_vec_pretty(map).map_err(|e| MemoryError::Serde {
            context: "usage.json schreiben",
            source: e,
        })?;
        harw_fsutil::write_atomic(
            &path,
            &bytes,
            harw_fsutil::AtomicWriteOptions::with_mode(FACT_FILE_MODE),
        )
        .map_err(|e| MemoryError::Io { path, source: e })
    }

    fn remove_usage_entry(&self, name: &str) -> MemoryResult<()> {
        let mut map = self.read_usage_map()?;
        if map.remove(name).is_some() {
            self.write_usage_map(&map)?;
        }
        Ok(())
    }

    /// Zeichnet eine Nutzung für jeden Namen in `names` auf (Zähler +1,
    /// `last_used = jetzt`), gepuffert in einem Schreibvorgang.
    ///
    /// # Errors
    /// [`MemoryError::Serde`]/[`MemoryError::Io`] beim Lesen/Schreiben von
    /// `usage.json`.
    pub fn record_usage(&self, names: &[&str]) -> MemoryResult<()> {
        let mut map = self.read_usage_map()?;
        let now = OffsetDateTime::now_utc();
        for name in names {
            let entry = map.entry((*name).to_owned()).or_insert(UsageEntry {
                count: 0,
                last_used: now,
            });
            entry.count = entry.count.saturating_add(1);
            entry.last_used = now;
        }
        self.write_usage_map(&map)
    }

    /// Liest den Nutzungszähler für `name`, `None` wenn nie genutzt oder
    /// `usage.json` nicht lesbar ist (dann wird nur `tracing::warn!`
    /// geloggt — kein Panic, kein Bubble-up für einen reinen Lesehelfer).
    #[must_use]
    pub fn usage(&self, name: &str) -> Option<(u64, OffsetDateTime)> {
        match self.read_usage_map() {
            Ok(map) => map.get(name).map(|e| (e.count, e.last_used)),
            Err(err) => {
                tracing::warn!(error = %err, "facts: usage.json konnte nicht gelesen werden");
                None
            }
        }
    }

    /// Verdrängung nach Design §5.4: für jeden Fakt ohne Nutzung
    /// (`usage_count == 0`) seit mindestens `max_unused_days` (gemessen ab
    /// `last_used`, oder — ohne Nutzungseintrag — ab `updated`) wird
    /// `confidence` halbiert und die Datei aktualisiert. Fakten, deren neue
    /// `confidence` unter `0.2` fällt, werden **nicht** gelöscht, sondern
    /// nur in der Rückgabe gemeldet.
    ///
    /// # Errors
    /// Fehler von [`Self::list`]/[`Self::write_index`]; `usage.json`- und
    /// Schreibfehler pro Fakt.
    pub fn decay(&self, max_unused_days: i64, now: OffsetDateTime) -> MemoryResult<Vec<String>> {
        let facts = self.list()?;
        let usage_map = self.read_usage_map()?;
        let mut fallen_below = Vec::new();
        for mut fact in facts {
            let (count, last_used) = usage_map
                .get(&fact.name)
                .map_or((0, fact.updated), |e| (e.count, e.last_used));
            if count != 0 {
                continue;
            }
            let age_days = (now - last_used).whole_days();
            if age_days < max_unused_days {
                continue;
            }
            let new_confidence = fact.confidence * 0.5;
            fact.confidence = new_confidence;
            self.write_raw(&fact)?;
            if new_confidence < 0.2 {
                fallen_below.push(fact.name.clone());
            }
        }
        self.write_index()?;
        Ok(fallen_below)
    }
}

/// Score einer Stichwortsuche für einen Fakt, siehe [`FactStore::search`].
fn score_fact(fact: &Fact, lowered_keywords: &[String]) -> i64 {
    let name_l = fact.name.to_lowercase();
    let desc_l = fact.description.to_lowercase();
    let body_l = fact.body.to_lowercase();
    let tags_l: Vec<String> = fact.tags.iter().map(|t| t.to_lowercase()).collect();
    let mut score = 0i64;
    for kw in lowered_keywords {
        if name_l.contains(kw.as_str()) {
            score += 3;
        }
        if desc_l.contains(kw.as_str()) || tags_l.iter().any(|t| t.contains(kw.as_str())) {
            score += 2;
        }
        if body_l.contains(kw.as_str()) {
            score += 1;
        }
    }
    score
}

/// Kürzt `s` auf höchstens `max` Zeichen (nicht Bytes) — unicode-sicher für
/// Umlaute in `description`.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect()
    }
}

/// Formatiert `ts` als `yyyy-mm-dd` für den `MEMORY.md`-Index.
fn format_date(ts: OffsetDateTime) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    ts.format(&format)
        .unwrap_or_else(|_| ts.unix_timestamp().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    fn tmp_root(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-facts-{tag}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn sample_fact(name: &str) -> Fact {
        let now = OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: "Warum der Freigabe-Dialog eine Verzögerung hat".to_owned(),
            fact_type: FactType::Decision,
            scope: FactScope::Project,
            created: now,
            updated: now,
            confidence: 0.8,
            sources: vec!["session:01H".to_owned()],
            tags: vec!["tui".to_owned(), "approval".to_owned()],
            body: "Tastendrücke gelten erst nach 250 ms.\nGrund: Race mit Panel-Öffnen.\n"
                .to_owned(),
        }
    }

    // -- slugify ---------------------------------------------------------

    #[test]
    fn slugify_basic_cases() {
        assert_eq!(slugify("TUI Approval Arming!"), "tui-approval-arming");
        assert_eq!(slugify("  leading and trailing  "), "leading-and-trailing");
        assert_eq!(slugify("a---b__c"), "a-b-c");
        assert_eq!(slugify(""), "fakt");
        assert_eq!(slugify("   "), "fakt");
        assert_eq!(slugify("###"), "fakt");
    }

    #[test]
    fn slugify_caps_at_60_chars_without_trailing_dash() {
        let long = "a".repeat(70);
        let slug = slugify(&long);
        assert_eq!(slug.len(), 60);
        assert!(!slug.ends_with('-'));

        let long_with_boundary_dash = format!("{}-{}", "a".repeat(59), "b".repeat(10));
        let slug = slugify(&long_with_boundary_dash);
        assert!(slug.len() <= 60);
        assert!(!slug.ends_with('-'));
    }

    // -- round trip --------------------------------------------------------

    #[test]
    fn write_then_read_round_trips_umlauts_and_multiline_body() {
        let root = tmp_root("roundtrip");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        let mut fact = sample_fact("tui-approval-arming");
        fact.description = "Ünïcödé: äöüß Beschreibung".to_owned();
        fact.body = "Zeile 1 mit ö.\nZeile 2 mit ü und ß.\n".to_owned();

        store.write(&fact).unwrap();
        let read_back = store.read("tui-approval-arming").unwrap().unwrap();

        assert_eq!(read_back.name, "tui-approval-arming");
        assert_eq!(read_back.description, fact.description);
        assert_eq!(read_back.body, fact.body);
        assert_eq!(read_back.fact_type, FactType::Decision);
        assert_eq!(read_back.scope, FactScope::Project);
        assert_eq!(read_back.sources, fact.sources);
        assert_eq!(read_back.tags, fact.tags);
        assert!((read_back.confidence - 0.8).abs() < 0.01);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn write_preserves_created_and_bumps_updated_on_overwrite() {
        let root = tmp_root("preserve-created");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        let fact = sample_fact("stable-name");
        store.write(&fact).unwrap();
        let first = store.read("stable-name").unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut second_write = fact.clone();
        second_write.description = "geändert".to_owned();
        store.write(&second_write).unwrap();
        let second = store.read("stable-name").unwrap().unwrap();

        assert_eq!(second.created, first.created);
        assert!(second.updated >= first.updated);
        assert_eq!(second.description, "geändert");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_fact_reads_as_none() {
        let root = tmp_root("missing");
        let store = FactStore::open(&root, FactScope::Global).unwrap();
        assert!(store.read("does-not-exist").unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn defaults_apply_when_optional_keys_are_absent() {
        let root = tmp_root("defaults");
        let store = FactStore::open(&root, FactScope::Global).unwrap();
        let raw = "---\nname: minimal\ndescription: nur das Nötigste\n---\n\nInhalt.\n";
        fs::write(root.join("facts/minimal.md"), raw).unwrap();

        let fact = store.read("minimal").unwrap().unwrap();
        assert_eq!(fact.fact_type, FactType::Fact);
        assert_eq!(fact.scope, FactScope::Global);
        assert!((fact.confidence - 0.8).abs() < f32::EPSILON);
        assert!(fact.tags.is_empty());
        assert!(fact.sources.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    // -- malformed frontmatter --------------------------------------------

    #[test]
    fn missing_closing_delimiter_is_an_error() {
        let root = tmp_root("malformed-no-close");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        fs::write(
            root.join("facts/broken.md"),
            "---\nname: broken\ndescription: x\n\nInhalt ohne Ende.\n",
        )
        .unwrap();
        assert!(matches!(
            store.read("broken").unwrap_err(),
            MemoryError::FrontmatterInvalid { .. }
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_description_is_an_error() {
        let root = tmp_root("malformed-no-desc");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        fs::write(
            root.join("facts/broken.md"),
            "---\nname: broken\n---\n\nInhalt.\n",
        )
        .unwrap();
        assert!(matches!(
            store.read("broken").unwrap_err(),
            MemoryError::FrontmatterInvalid { .. }
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unclosed_inline_list_is_an_error() {
        let root = tmp_root("malformed-list");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        fs::write(
            root.join("facts/broken.md"),
            "---\nname: broken\ndescription: x\ntags: [a, b\n---\n\nInhalt.\n",
        )
        .unwrap();
        assert!(matches!(
            store.read("broken").unwrap_err(),
            MemoryError::FrontmatterInvalid { .. }
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn list_skips_malformed_files_and_logs() {
        let root = tmp_root("list-skips");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        store.write(&sample_fact("good-one")).unwrap();
        fs::write(
            root.join("facts/broken.md"),
            "---\nname: broken\n---\n\nkeine description.\n",
        )
        .unwrap();

        let facts = store.list().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].name, "good-one");
        let _ = fs::remove_dir_all(&root);
    }

    // -- traversal ----------------------------------------------------------

    #[test]
    fn invalid_names_are_rejected() {
        let root = tmp_root("traversal");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        for bad in ["../x", "a/b", "", "Uppercase", "a.b", "a b"] {
            assert!(
                matches!(
                    store.path_for(bad),
                    Err(MemoryError::InvalidFactName { .. })
                ),
                "expected rejection for {bad:?}"
            );
            assert!(matches!(
                store.read(bad),
                Err(MemoryError::InvalidFactName { .. })
            ));
        }
        let _ = fs::remove_dir_all(&root);
    }

    // -- search ---------------------------------------------------------

    #[test]
    fn search_ranks_name_match_over_body_match() {
        let root = tmp_root("search");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        let mut name_hit = sample_fact("popup-timing");
        name_hit.description = "irrelevant".to_owned();
        name_hit.body = "irrelevant".to_owned();
        store.write(&name_hit).unwrap();

        let mut body_hit = sample_fact("other-topic");
        body_hit.description = "irrelevant".to_owned();
        body_hit.body = "erwähnt popup nur im Fließtext".to_owned();
        store.write(&body_hit).unwrap();

        let hits = store.search(&["popup"], 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "popup-timing");
        assert_eq!(hits[1].name, "other-topic");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_with_no_match_returns_empty() {
        let root = tmp_root("search-empty");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        store.write(&sample_fact("some-fact")).unwrap();
        let hits = store.search(&["nirgendwo-erwaehnt"], 10).unwrap();
        assert!(hits.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    // -- index -----------------------------------------------------------

    #[test]
    fn index_groups_by_type_in_stable_order() {
        let root = tmp_root("index");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        let mut pref = sample_fact("z-pref");
        pref.fact_type = FactType::Preference;
        store.write(&pref).unwrap();
        let mut fact_b = sample_fact("b-fact");
        fact_b.fact_type = FactType::Fact;
        store.write(&fact_b).unwrap();
        let mut fact_a = sample_fact("a-fact");
        fact_a.fact_type = FactType::Fact;
        store.write(&fact_a).unwrap();

        let index = fs::read_to_string(root.join("MEMORY.md")).unwrap();
        assert!(index.starts_with("# Gedächtnis (project)\n\n"));
        let fact_heading = index.find("## Fakt").unwrap();
        let pref_heading = index.find("## Präferenz").unwrap();
        assert!(fact_heading < pref_heading, "Fakt muss vor Präferenz stehen");
        let a_pos = index.find("a-fact.md").unwrap();
        let b_pos = index.find("b-fact.md").unwrap();
        assert!(a_pos < b_pos, "innerhalb der Gruppe alphabetisch nach name");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_removes_file_usage_and_index_line() {
        let root = tmp_root("delete");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        store.write(&sample_fact("to-delete")).unwrap();
        store.record_usage(&["to-delete"]).unwrap();
        assert!(store.usage("to-delete").is_some());

        let deleted = store.delete("to-delete").unwrap();
        assert!(deleted);
        assert!(!root.join("facts/to-delete.md").exists());
        assert!(store.usage("to-delete").is_none());
        let index = fs::read_to_string(root.join("MEMORY.md")).unwrap();
        assert!(!index.contains("to-delete.md"));

        assert!(!store.delete("to-delete").unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    // -- usage / decay -----------------------------------------------------

    #[test]
    fn usage_counters_persist_across_calls() {
        let root = tmp_root("usage");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        store.write(&sample_fact("tracked")).unwrap();
        store.record_usage(&["tracked"]).unwrap();
        store.record_usage(&["tracked", "tracked"]).unwrap();

        let (count, _last_used) = store.usage("tracked").unwrap();
        assert_eq!(count, 3);
        assert!(store.usage("never-used").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn decay_halves_confidence_after_unused_window_and_reports_low_confidence() {
        let root = tmp_root("decay");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        let mut fact = sample_fact("stale-fact");
        fact.confidence = 0.3;
        store.write(&fact).unwrap();

        let now = OffsetDateTime::now_utc();
        // Innerhalb des Fensters: keine Änderung.
        let unchanged = store.decay(90, now).unwrap();
        assert!(unchanged.is_empty());
        let still = store.read("stale-fact").unwrap().unwrap();
        assert!((still.confidence - 0.3).abs() < 0.01);

        // Nach dem Fenster (kein usage-Eintrag => Alter ab `updated`).
        let far_future = now + Duration::days(91);
        let fallen = store.decay(90, far_future).unwrap();
        assert_eq!(fallen, vec!["stale-fact".to_owned()]);
        let decayed = store.read("stale-fact").unwrap().unwrap();
        assert!((decayed.confidence - 0.15).abs() < 0.01);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn decay_skips_facts_with_recorded_usage() {
        let root = tmp_root("decay-used");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        let fact = sample_fact("used-fact");
        store.write(&fact).unwrap();
        store.record_usage(&["used-fact"]).unwrap();

        let far_future = OffsetDateTime::now_utc() + Duration::days(200);
        let fallen = store.decay(90, far_future).unwrap();
        assert!(fallen.is_empty());
        let unchanged = store.read("used-fact").unwrap().unwrap();
        assert!((unchanged.confidence - 0.8).abs() < 0.01);
        let _ = fs::remove_dir_all(&root);
    }

    // -- redact ---------------------------------------------------------

    #[test]
    fn redact_leaves_normal_prose_untouched() {
        let prose = "Der Freigabe-Dialog wartet 250 ms, bevor er Eingaben annimmt.";
        assert_eq!(redact(prose), prose);
    }

    #[test]
    fn redact_catches_openai_style_keys() {
        let text = "geheim: sk-abcdefghijklmnopqrstuvwxyz0123456789";
        assert_eq!(redact(text), "geheim: [redacted]");
    }

    #[test]
    fn redact_catches_github_tokens() {
        let text = "token ghp_ABCDEFGHIJ0123456789klmnopqrst im Log";
        assert_eq!(redact(text), "token [redacted] im Log");
    }

    #[test]
    fn redact_catches_aws_access_keys() {
        let text = "AKIAABCDEFGHIJKLMNOP steht im Klartext";
        assert_eq!(redact(text), "[redacted] steht im Klartext");
    }

    #[test]
    fn redact_catches_slack_tokens() {
        let text = "xoxb-1234567890-abcdefghij";
        assert_eq!(redact(text), "[redacted]");
    }

    #[test]
    fn redact_catches_pem_private_key_blocks() {
        let text = "vorher\n-----BEGIN RSA PRIVATE KEY-----\nMIIBogIBAAKC\n-----END RSA PRIVATE KEY-----\nnachher";
        assert_eq!(redact(text), "vorher\n[redacted]nachher");
    }

    #[test]
    fn redact_catches_bearer_tokens() {
        let text = "Authorization: Bearer abc123.def456-ghi789";
        assert_eq!(redact(text), "Authorization: Bearer [redacted]");
    }

    #[test]
    fn redact_catches_key_value_secrets() {
        assert_eq!(
            redact("password=hunter2geheim"),
            "password=[redacted]"
        );
        assert_eq!(redact("api_key=abcdef123456"), "api_key=[redacted]");
        assert_eq!(redact("token = zyxwv98765"), "token = [redacted]");
    }

    #[test]
    fn redact_catches_base64_run_near_keyword() {
        let text = "secret_key: aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxk value";
        let redacted = redact(text);
        assert!(redacted.contains("[redacted]"));
        assert!(!redacted.contains("aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxk"));
    }

    #[test]
    fn redact_does_not_touch_long_run_without_keyword_context() {
        let text = "Dieser Hash hat viele Zeichen: aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxk am Ende.";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn write_redacts_body_description_and_tags() {
        let root = tmp_root("write-redacts");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        let mut fact = sample_fact("has-secret");
        fact.description = "enthält sk-abcdefghijklmnopqrstuvwxyz".to_owned();
        fact.body = "Bearer supersecrettoken123 im Body".to_owned();
        fact.tags = vec!["AKIAABCDEFGHIJKLMNOP".to_owned()];
        store.write(&fact).unwrap();

        let read_back = store.read("has-secret").unwrap().unwrap();
        assert!(!read_back.description.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        assert!(read_back.body.contains("[redacted]"));
        assert_eq!(read_back.tags[0], "[redacted]");
        let _ = fs::remove_dir_all(&root);
    }
}
