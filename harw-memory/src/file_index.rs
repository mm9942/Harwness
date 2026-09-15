//! Dateiwissen-Index (Langzeitgedächtnis v3, Addendum B), siehe
//! `CONTRACT.md` Addendum B ("Dateiwissen: NEU `<memories>/files/index.json>`").
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt genau eine Datei-Wahrheit: `<memories_root>/files/index.json`,
//! eine flache JSON-Liste von [`FileKnowledge`]-Einträgen, je einer pro
//! gelesenem Pfad. Es implementiert:
//! - [`is_secret_path`] — erkennt Pfade, deren Inhalt niemals gespeichert
//!   werden darf (nur Pfad + Digest sind erlaubt).
//! - [`extract_file_knowledge`] — extrahiert aus einem gelesenen Dateiinhalt
//!   ausschließlich Metadaten (Größe, Digest, Sprache, Kurzbeschreibung,
//!   Symbolnamen) — **niemals** den Rohinhalt selbst.
//! - [`FileKnowledgeIndex`] — Laden/Schreiben/Suchen des Index.
//!
//! # Nebenläufigkeit
//! [`FileKnowledgeIndex`] ist `Send + Sync`. Schreibende Zugriffe
//! (`upsert`) serialisieren sich prozessintern über einen `std::sync::Mutex`;
//! die eigentliche Datei wird über [`harw_fsutil::write_atomic`] ersetzt.
//! Keine Serialisierung über mehrere Prozesse hinweg (analog zu
//! `crate::facts::FactStore`).
//!
//! # Fehler
//! [`crate::error::MemoryError::Io`], [`crate::error::MemoryError::Serde`].

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::MemoryError;
use crate::MemoryResult;

/// Rechte-Bits für `<memories_root>/files/index.json`.
const INDEX_FILE_MODE: u32 = 0o600;

/// Höchstzahl an Einträgen im Dateiwissen-Index. Wird die Grenze
/// überschritten, verdrängt [`FileKnowledgeIndex::upsert`] die Einträge mit
/// dem ältesten `last_seen` zuerst.
pub const FILE_INDEX_MAX_ENTRIES: usize = 2000;

/// Höchstzahl extrahierter Symbolnamen je Datei.
const MAX_SYMBOLS: usize = 40;

/// Höchstlänge der `summary` in Zeichen.
const MAX_SUMMARY_CHARS: usize = 160;

/// Metadaten-Eintrag zu genau einer gelesenen Datei — **nie** ihr Rohinhalt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileKnowledge {
    /// Pfad, wie ihn das Modell übergeben hat (workspace-relativ).
    pub path: String,
    /// Größe des zuletzt gesehenen Inhalts in Bytes.
    pub size_bytes: u64,
    /// Zeilenzahl des zuletzt gesehenen Inhalts.
    pub line_count: u64,
    /// Inhalts-Digest (hex), siehe [`extract_file_knowledge`].
    pub digest: String,
    /// Erkannte Sprache anhand der Dateiendung, `None` wenn unbekannt.
    pub language: Option<String>,
    /// Erste sinnvolle Dokumentationszeile, `None` bei Geheimnis-Pfaden oder
    /// wenn keine gefunden wurde.
    pub summary: Option<String>,
    /// Erkannte Symbolnamen (Funktionen, Typen, Überschriften, …), leer bei
    /// Geheimnis-Pfaden.
    pub symbols: Vec<String>,
    /// Zeitpunkt des letzten Lesens (RFC 3339).
    pub last_seen: String,
    /// Anzahl aufgezeichneter Lesevorgänge.
    pub read_count: u32,
}

/// Prüft, ob `path` auf eine Datei zeigt, deren Inhalt niemals gespeichert
/// werden darf.
///
/// # Beschreibung
/// Case-insensitive Prüfung von Basisname und jedem Pfadsegment gegen:
/// `.env`/`.env.*`, `*.pem`, `*.key`, `*.p12`, `id_rsa*`, `id_ed25519*`,
/// Namen mit `secret`/`credential`, `auth.toml`, sowie ein Pfadsegment
/// `secrets`.
#[must_use]
pub fn is_secret_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let segments: Vec<String> = normalized
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect();
    let Some(basename) = segments.last() else {
        return false;
    };
    if basename == "auth.toml" {
        return true;
    }
    if basename == ".env" || basename.starts_with(".env.") {
        return true;
    }
    if basename.ends_with(".pem") || basename.ends_with(".key") || basename.ends_with(".p12") {
        return true;
    }
    if basename.starts_with("id_rsa") || basename.starts_with("id_ed25519") {
        return true;
    }
    if basename.contains("secret") || basename.contains("credential") {
        return true;
    }
    segments.iter().any(|s| s == "secrets")
}

/// Erkennt die Sprache anhand der Dateiendung von `path`.
fn detect_language(path: &str) -> Option<String> {
    let ext = Path::new(path).extension()?.to_str()?.to_lowercase();
    let lang = match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" => "javascript",
        "jsx" => "jsx",
        "go" => "go",
        "toml" => "toml",
        "md" => "markdown",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" => "shell",
        _ => return None,
    };
    Some(lang.to_owned())
}

/// Kürzt `s` auf höchstens `max` Zeichen (nicht Bytes), unicode-sicher.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect()
    }
}

/// Extrahiert die erste sinnvolle Dokumentationszeile von `content`, je nach
/// `language`, siehe Moduldoku.
fn compute_summary(language: Option<&str>, content: &str) -> Option<String> {
    let raw: Option<&str> = match language {
        Some("rust") => content
            .lines()
            .find_map(|l| l.trim_start().strip_prefix("//!"))
            .map(str::trim),
        Some("markdown") => content
            .lines()
            .find_map(|l| l.trim_start().strip_prefix("# "))
            .map(str::trim),
        Some("python") => content.lines().find_map(|l| {
            let t = l.trim();
            if let Some(rest) = t.strip_prefix("\"\"\"") {
                let rest = rest.trim_end_matches("\"\"\"").trim();
                (!rest.is_empty()).then_some(rest)
            } else if t.starts_with('#') && !t.starts_with("#!") {
                Some(t.trim_start_matches('#').trim())
            } else {
                None
            }
        }),
        Some("shell") => content.lines().find_map(|l| {
            let t = l.trim();
            (t.starts_with('#') && !t.starts_with("#!")).then(|| t.trim_start_matches('#').trim())
        }),
        _ => None,
    };
    raw.map(|s| truncate_chars(s, MAX_SUMMARY_CHARS))
        .filter(|s| !s.is_empty())
}

/// Liest einen führenden Bezeichner (`[A-Za-z0-9_]+`) aus `rest`, `None` wenn
/// `rest` (nach Trimmen) nicht mit einem solchen Zeichen beginnt.
fn ident(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| rest[..end].to_owned())
}

/// Symbol-Präfixe für Rust (Reihenfolge = Priorität bei der Suche je Zeile).
const RUST_SYMBOL_PREFIXES: [&str; 10] = [
    "pub fn ",
    "pub struct ",
    "pub enum ",
    "pub trait ",
    "pub type ",
    "pub const ",
    "pub mod ",
    "fn ",
    "struct ",
    "enum ",
];

/// Extrahiert einen Symbolnamen aus einer Rust-Codezeile, `None` wenn keines
/// der bekannten Muster (`pub fn`, `struct`, `impl X ...`, …) passt.
fn rust_symbol(trimmed: &str) -> Option<String> {
    for prefix in RUST_SYMBOL_PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return ident(rest);
        }
    }
    if let Some(rest) = trimmed.strip_prefix("impl ") {
        let rest = rest.trim_start();
        // "impl<...> X" → das Generics-Präfix überspringen.
        let rest = if rest.starts_with('<') {
            rest.find('>').map_or(rest, |pos| rest[pos + 1..].trim_start())
        } else {
            rest
        };
        return ident(rest);
    }
    None
}

/// Extrahiert einen Symbolnamen aus einer Codezeile, je nach `language`.
fn symbol_from_line(language: Option<&str>, line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    match language {
        Some("rust") => rust_symbol(trimmed),
        Some("python") => {
            if let Some(rest) = trimmed.strip_prefix("def ") {
                ident(rest)
            } else if let Some(rest) = trimmed.strip_prefix("class ") {
                ident(rest)
            } else {
                None
            }
        }
        Some("typescript") | Some("tsx") | Some("javascript") | Some("jsx") => {
            let rest = trimmed.strip_prefix("export ")?;
            let rest = rest.strip_prefix("default ").unwrap_or(rest);
            for kw in ["function ", "class ", "const ", "interface ", "type "] {
                if let Some(r) = rest.strip_prefix(kw) {
                    return ident(r);
                }
            }
            None
        }
        Some("toml") => (trimmed.starts_with('[') && trimmed.contains(']'))
            .then(|| trimmed.trim_end().to_owned()),
        Some("markdown") => trimmed.strip_prefix("## ").map(|s| s.trim().to_owned()),
        _ => None,
    }
}

/// Extrahiert bis zu [`MAX_SYMBOLS`] eindeutige, reihenfolgestabile
/// Symbolnamen aus `content`, je nach `language`.
fn extract_symbols(language: Option<&str>, content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for line in content.lines() {
        if out.len() >= MAX_SYMBOLS {
            break;
        }
        if let Some(sym) = symbol_from_line(language, line) {
            if seen.insert(sym.clone()) {
                out.push(sym);
            }
        }
    }
    out
}

/// Extrahiert [`FileKnowledge`] aus einem gelesenen Dateiinhalt — niemals den
/// Inhalt selbst.
///
/// # Beschreibung
/// `size_bytes`/`line_count`/`digest` werden immer berechnet (der Digest
/// über [`harw_types::ContentDigest::of`], hex-kodiert). Ist
/// [`is_secret_path`] wahr, bleiben `summary` und `symbols` leer — nur
/// Pfad, Größe und Digest werden erfasst. Sonst wird `summary` per
/// [`compute_summary`] und `symbols` per [`extract_symbols`] ermittelt,
/// beide abhängig von der über die Dateiendung erkannten Sprache.
/// `read_count` startet bei `1`, `last_seen` wird von `now_rfc3339`
/// übernommen.
#[must_use]
pub fn extract_file_knowledge(path: &str, content: &str, now_rfc3339: &str) -> FileKnowledge {
    let size_bytes = content.len() as u64;
    let line_count = content.lines().count() as u64;
    let digest = harw_types::ContentDigest::of(content.as_bytes()).to_string();
    let language = detect_language(path);
    let secret = is_secret_path(path);
    let (summary, symbols) = if secret {
        (None, Vec::new())
    } else {
        (
            compute_summary(language.as_deref(), content),
            extract_symbols(language.as_deref(), content),
        )
    };
    FileKnowledge {
        path: path.to_owned(),
        size_bytes,
        line_count,
        digest,
        language,
        summary,
        symbols,
        last_seen: now_rfc3339.to_owned(),
        read_count: 1,
    }
}

/// Rohformat von `files/index.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IndexFile {
    /// Formatversion (aktuell immer `1`).
    version: u32,
    /// Alle Einträge, unsortiert.
    entries: Vec<FileKnowledge>,
}

/// Parst `last_seen` als RFC-3339-Zeitstempel; ein nicht lesbarer Wert gilt
/// als „ältestmöglich" (fließt zuerst in die Verdrängung ein).
fn parse_last_seen(entry: &FileKnowledge) -> OffsetDateTime {
    OffsetDateTime::parse(entry.last_seen.trim(), &Rfc3339).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Dateiwissen-Index unter `<memories_root>/files/index.json`, siehe
/// Moduldoku.
pub struct FileKnowledgeIndex {
    /// `<memories_root>/files/`.
    dir: PathBuf,
    /// Serialisiert Schreibzugriffe prozessintern.
    write_lock: Mutex<()>,
}

impl FileKnowledgeIndex {
    /// Öffnet den Index an `memories_root`, legt `<memories_root>/files/`
    /// mit Rechten `0700` an, falls es fehlt.
    ///
    /// # Errors
    /// [`MemoryError::Io`], wenn das Verzeichnis nicht angelegt oder seine
    /// Rechte nicht gesetzt werden können.
    pub fn open(memories_root: &Path) -> MemoryResult<Self> {
        let dir = memories_root.join("files");
        fs::create_dir_all(&dir).map_err(|e| MemoryError::Io {
            path: dir.clone(),
            source: e,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| {
                MemoryError::Io {
                    path: dir.clone(),
                    source: e,
                }
            })?;
        }
        Ok(Self {
            dir,
            write_lock: Mutex::new(()),
        })
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    fn read_index(&self) -> MemoryResult<IndexFile> {
        let path = self.index_path();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| MemoryError::Serde {
                context: "files/index.json lesen",
                source: e,
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(IndexFile {
                version: 1,
                entries: Vec::new(),
            }),
            Err(e) => Err(MemoryError::Io { path, source: e }),
        }
    }

    fn write_index(&self, index: &IndexFile) -> MemoryResult<()> {
        let path = self.index_path();
        let bytes = serde_json::to_vec_pretty(index).map_err(|e| MemoryError::Serde {
            context: "files/index.json schreiben",
            source: e,
        })?;
        harw_fsutil::write_atomic(
            &path,
            &bytes,
            harw_fsutil::AtomicWriteOptions::with_mode(INDEX_FILE_MODE),
        )
        .map_err(|e| MemoryError::Io { path, source: e })
    }

    /// Fügt `entry` ein oder aktualisiert den bestehenden Eintrag für
    /// `entry.path`.
    ///
    /// # Beschreibung
    /// Gleicher Digest wie der bestehende Eintrag: nur `read_count`
    /// (+1) und `last_seen` werden übernommen. Anderer Digest: alle Felder
    /// werden durch `entry` ersetzt, `read_count` wird auf
    /// `alt.read_count + 1` gesetzt. Kein bestehender Eintrag: `entry` wird
    /// unverändert eingefügt. Überschreitet die Gesamtzahl danach
    /// [`FILE_INDEX_MAX_ENTRIES`], werden die Einträge mit dem ältesten
    /// `last_seen` verworfen, bis die Grenze wieder eingehalten ist.
    ///
    /// # Errors
    /// Fehler von [`Self::read_index`]/[`Self::write_index`].
    pub fn upsert(&self, entry: FileKnowledge) -> MemoryResult<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut index = self.read_index()?;
        match index.entries.iter_mut().find(|e| e.path == entry.path) {
            Some(existing) if existing.digest == entry.digest => {
                existing.read_count = existing.read_count.saturating_add(1);
                existing.last_seen = entry.last_seen;
            }
            Some(existing) => {
                let read_count = existing.read_count.saturating_add(1);
                *existing = FileKnowledge { read_count, ..entry };
            }
            None => index.entries.push(entry),
        }
        if index.entries.len() > FILE_INDEX_MAX_ENTRIES {
            index
                .entries
                .sort_by_key(|e| std::cmp::Reverse(parse_last_seen(e)));
            index.entries.truncate(FILE_INDEX_MAX_ENTRIES);
        }
        self.write_index(&index)
    }

    /// Liest den Eintrag für `path`, `None` wenn nicht vorhanden.
    ///
    /// # Errors
    /// Fehler von [`Self::read_index`].
    pub fn get(&self, path: &str) -> MemoryResult<Option<FileKnowledge>> {
        Ok(self
            .read_index()?
            .entries
            .into_iter()
            .find(|e| e.path == path))
    }

    /// Listet alle Einträge, unsortiert.
    ///
    /// # Errors
    /// Fehler von [`Self::read_index`].
    pub fn list(&self) -> MemoryResult<Vec<FileKnowledge>> {
        Ok(self.read_index()?.entries)
    }

    /// Stichwortsuche über `path`, `summary` und `symbols`.
    ///
    /// # Beschreibung
    /// Score je Stichwort (klein geschrieben, `contains`-Vergleich): ein
    /// Treffer je Feld (`path`, `summary`, ein beliebiges Symbol) zählt 1.
    /// Einträge mit Score 0 fallen heraus. Sortiert nach Score absteigend,
    /// bei Gleichstand nach `read_count` absteigend; liefert höchstens
    /// `limit` Treffer.
    ///
    /// # Errors
    /// Fehler von [`Self::read_index`].
    pub fn search(&self, keywords: &[String], limit: usize) -> MemoryResult<Vec<FileKnowledge>> {
        let index = self.read_index()?;
        let lowered: Vec<String> = keywords
            .iter()
            .map(|k| k.to_lowercase())
            .filter(|k| !k.is_empty())
            .collect();
        let mut scored: Vec<(i64, FileKnowledge)> = index
            .entries
            .into_iter()
            .filter_map(|entry| {
                let score = score_entry(&entry, &lowered);
                (score > 0).then_some((score, entry))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.read_count.cmp(&a.1.read_count)));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, e)| e).collect())
    }
}

/// Score einer Stichwortsuche für einen [`FileKnowledge`]-Eintrag, siehe
/// [`FileKnowledgeIndex::search`].
fn score_entry(entry: &FileKnowledge, lowered_keywords: &[String]) -> i64 {
    let path_l = entry.path.to_lowercase();
    let summary_l = entry.summary.as_deref().unwrap_or("").to_lowercase();
    let symbols_l: Vec<String> = entry.symbols.iter().map(|s| s.to_lowercase()).collect();
    let mut score = 0i64;
    for kw in lowered_keywords {
        if path_l.contains(kw.as_str()) {
            score += 1;
        }
        if summary_l.contains(kw.as_str()) {
            score += 1;
        }
        if symbols_l.iter().any(|s| s.contains(kw.as_str())) {
            score += 1;
        }
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-file-index-{tag}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn now() -> String {
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default()
    }

    // -- is_secret_path / extract_file_knowledge ---------------------------

    #[test]
    fn is_secret_path_matches_known_patterns() {
        assert!(is_secret_path(".env"));
        assert!(is_secret_path(".env.local"));
        assert!(is_secret_path("keys/id_rsa"));
        assert!(is_secret_path("certs/server.PEM"));
        assert!(is_secret_path("conf/auth.toml"));
        assert!(is_secret_path("x/secrets/foo.txt"));
        assert!(is_secret_path("MY_SECRET_TOKEN.txt"));
        assert!(!is_secret_path("src/main.rs"));
    }

    #[test]
    fn extract_file_knowledge_rust_summary_and_symbols() {
        let content = "//! Modul-Doku erste Zeile.\n\npub fn foo() {}\npub struct Bar;\nimpl Bar {}\n";
        let knowledge = extract_file_knowledge("src/lib.rs", content, &now());
        assert_eq!(knowledge.language.as_deref(), Some("rust"));
        assert_eq!(knowledge.summary.as_deref(), Some("Modul-Doku erste Zeile."));
        assert_eq!(knowledge.symbols, vec!["foo".to_owned(), "Bar".to_owned()]);
        assert_eq!(knowledge.read_count, 1);
        assert_eq!(knowledge.size_bytes, content.len() as u64);
    }

    #[test]
    fn extract_file_knowledge_markdown_heading() {
        let content = "# Überschrift\n\nText.\n";
        let knowledge = extract_file_knowledge("docs/readme.md", content, &now());
        assert_eq!(knowledge.language.as_deref(), Some("markdown"));
        assert_eq!(knowledge.summary.as_deref(), Some("Überschrift"));
    }

    #[test]
    fn extract_file_knowledge_secret_path_has_no_summary_or_symbols() {
        let content = "//! Sollte niemals gelesen werden.\npub fn secret_thing() {}\n";
        let knowledge = extract_file_knowledge(".env", content, &now());
        assert!(knowledge.summary.is_none());
        assert!(knowledge.symbols.is_empty());
        assert!(!knowledge.digest.is_empty());
        assert_eq!(knowledge.size_bytes, content.len() as u64);
    }

    // -- FileKnowledgeIndex --------------------------------------------------

    #[test]
    fn upsert_same_digest_increments_read_count() {
        let root = tmp_root("upsert-same-digest");
        let index = FileKnowledgeIndex::open(&root).unwrap();
        let first = extract_file_knowledge("src/a.rs", "pub fn a() {}\n", &now());
        index.upsert(first.clone()).unwrap();
        let second = extract_file_knowledge("src/a.rs", "pub fn a() {}\n", &now());
        index.upsert(second).unwrap();
        let stored = index.get("src/a.rs").unwrap().unwrap();
        assert_eq!(stored.read_count, 2);
        assert_eq!(stored.digest, first.digest);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn upsert_different_digest_replaces_fields_and_bumps_read_count() {
        let root = tmp_root("upsert-diff-digest");
        let index = FileKnowledgeIndex::open(&root).unwrap();
        index
            .upsert(extract_file_knowledge("src/a.rs", "pub fn a() {}\n", &now()))
            .unwrap();
        index
            .upsert(extract_file_knowledge(
                "src/a.rs",
                "pub fn a() {}\npub fn b() {}\n",
                &now(),
            ))
            .unwrap();
        let stored = index.get("src/a.rs").unwrap().unwrap();
        assert_eq!(stored.read_count, 2);
        assert_eq!(stored.symbols, vec!["a".to_owned(), "b".to_owned()]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn upsert_evicts_oldest_last_seen_beyond_cap() {
        let root = tmp_root("upsert-eviction");
        let index = FileKnowledgeIndex::open(&root).unwrap();
        let base = OffsetDateTime::now_utc();
        for i in 0..(FILE_INDEX_MAX_ENTRIES + 5) {
            let ts = (base + time::Duration::seconds(i as i64))
                .format(&Rfc3339)
                .unwrap();
            let mut entry = extract_file_knowledge(&format!("f{i}.rs"), "pub fn x() {}\n", &ts);
            entry.digest = format!("digest-{i}");
            index.upsert(entry).unwrap();
        }
        let all = index.list().unwrap();
        assert_eq!(all.len(), FILE_INDEX_MAX_ENTRIES);
        // Die fünf ältesten (f0..f4) wurden verdrängt.
        assert!(index.get("f0.rs").unwrap().is_none());
        assert!(index.get(&format!("f{}.rs", FILE_INDEX_MAX_ENTRIES + 4)).unwrap().is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_ranks_by_score_then_read_count() {
        let root = tmp_root("search-ranking");
        let index = FileKnowledgeIndex::open(&root).unwrap();
        // Beide Einträge erzielen identischen Score (Treffer in path, summary
        // und einem Symbol) — Sortierung muss dann über read_count entscheiden.
        let mut low = extract_file_knowledge(
            "src/session_a.rs",
            "//! Session Info.\npub fn session_one() {}\n",
            &now(),
        );
        low.read_count = 1;
        index.upsert(low).unwrap();
        let mut high = extract_file_knowledge(
            "src/session_b.rs",
            "//! Session Info.\npub fn session_two() {}\n",
            &now(),
        );
        high.read_count = 9;
        index.upsert(high).unwrap();

        let hits = index.search(&["session".to_owned()], 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "src/session_b.rs");
        assert_eq!(hits[0].read_count, 9);
        let _ = fs::remove_dir_all(&root);
    }
}
