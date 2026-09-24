//! `@`-Erwähnungen in Chat-Eingaben: Rollen-Hinweise und Dateianhänge.
//!
//! # Verantwortung
//! - [`classify_mention`] unterscheidet `@rolle` (bekannte Agentenrolle) von
//!   `@pfad` (Datei im Projekt).
//! - [`expand_file_mentions`] hängt erwähnte Dateien als
//!   `<datei pfad="rel/pfad">…</datei>`-Blöcke an den Chattext an.
//! - [`role_mention_text`] formuliert `@rolle text` als *Bitte* an die UIA, an
//!   diese Rolle zu delegieren (kein direktes Routing).
//! - [`scan_mention_candidates`] liefert Dateikandidaten für das
//!   Erwähnungs-Popup (`mention_popup.rs`).
//!
//! # Sicherheitsregeln für Dateianhänge
//! - Jeder Pfad wird kanonisiert und muss unter der *kanonischen*
//!   Projektwurzel liegen — `..`-Traversal und Symlinks nach außen werden
//!   abgelehnt.
//! - Sperrliste: `.env*`, `*.pem`, `*.key`, `id_*` sowie alles unter `.git/`
//!   (geprüft auf dem getippten *und* dem aufgelösten Pfad, damit ein
//!   harmlos benannter Symlink auf `.env` nicht durchrutscht).
//! - Binärdateien (NUL-Byte) und ungültiges UTF-8 werden abgelehnt.
//! - Größen- und Anzahlgrenzen aus [`MentionLimits`].
//!
//! Alle Ablehnungen tragen einen deutschen Grund und landen in
//! [`ExpandedChat::rejected`]; der Aufrufer zeigt sie dem Nutzer an.
//!
//! # Fehlertypen
//! Keine — alle Funktionen sind infallibel; Probleme werden als Ablehnung
//! bzw. [`MentionTarget::Unknown`] gemeldet.

use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

// ---------------------------------------------------------------------------
// Öffentliche Typen
// ---------------------------------------------------------------------------

/// Ergebnis der Klassifikation eines `@`-Tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MentionTarget {
    /// Eine reguläre Datei im Projekt; enthält den **kanonischen** absoluten
    /// Pfad (garantiert unter der kanonischen Projektwurzel).
    File(PathBuf),
    /// Eine bekannte Agentenrolle; enthält den Rollennamen in der
    /// Schreibweise der übergebenen Rollenliste.
    Role(String),
    /// Weder bekannte Rolle noch zulässige Datei im Projekt.
    Unknown,
}

/// Obergrenzen für Dateianhänge per `@pfad`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MentionLimits {
    /// Maximale Größe einer einzelnen Datei in Bytes.
    pub max_file_bytes: usize,
    /// Maximale Summe aller angehängten Dateien in Bytes.
    pub max_total_bytes: usize,
    /// Maximale Anzahl angehängter Dateien je Nachricht.
    pub max_files: usize,
}

impl Default for MentionLimits {
    /// 64 KiB je Datei, 256 KiB gesamt, höchstens 8 Dateien.
    fn default() -> Self {
        Self {
            max_file_bytes: 64 * 1024,
            max_total_bytes: 256 * 1024,
            max_files: 8,
        }
    }
}

/// Ergebnis von [`expand_file_mentions`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ExpandedChat {
    /// Originaltext, gefolgt von je einem `<datei pfad="…">…</datei>`-Block
    /// pro angehängter Datei.
    pub text: String,
    /// Kanonische Pfade der angehängten Dateien (Reihenfolge des Auftretens,
    /// ohne Duplikate).
    pub attached: Vec<PathBuf>,
    /// Abgelehnte Erwähnungen als `(token, grund)`; `token` ohne führendes `@`.
    pub rejected: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Klassifikation
// ---------------------------------------------------------------------------

/// Klassifiziert ein `@`-Token (mit oder ohne führendes `@`).
///
/// # Beschreibung
/// Eine exakte (case-insensitive) Übereinstimmung mit einem Eintrag aus
/// `roles` hat Vorrang; eine Datei, die wie eine Rolle heißt, lässt sich über
/// `./name` erwähnen. Sonst wird das Token als Pfad relativ zu `root`
/// aufgelöst und nur dann als [`MentionTarget::File`] gemeldet, wenn alle
/// Sicherheitsregeln (Wurzel, Sperrliste, reguläre Datei) erfüllt sind.
/// Größe und Inhalt prüft erst [`expand_file_mentions`].
pub(crate) fn classify_mention(token: &str, root: &Path, roles: &[&str]) -> MentionTarget {
    let token = token.strip_prefix('@').unwrap_or(token);
    if token.is_empty() {
        return MentionTarget::Unknown;
    }
    if let Some(role) = roles.iter().find(|role| role.eq_ignore_ascii_case(token)) {
        return MentionTarget::Role((*role).to_owned());
    }
    let Ok(canonical_root) = fs::canonicalize(root) else {
        return MentionTarget::Unknown;
    };
    match resolve_file(token, &canonical_root) {
        Ok(resolved) => MentionTarget::File(resolved.canonical),
        Err(_) => MentionTarget::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Rollen-Erwähnung
// ---------------------------------------------------------------------------

/// Formuliert `@rolle body` als Delegationsbitte an die UIA.
///
/// Der Text macht ausdrücklich klar, dass es sich um einen *Wunsch* handelt:
/// Die UIA entscheidet selbst, ob und wie sie an die Rolle delegiert — die
/// TUI routet nicht direkt an einen Agenten.
pub(crate) fn role_mention_text(role: &str, body: &str) -> String {
    let body = body.trim();
    let body = if body.is_empty() {
        "(kein weiterer Text)"
    } else {
        body
    };
    format!(
        "[Delegationswunsch an Rolle „{role}“]\n\
         Bitte delegiere die folgende Anfrage an einen Agenten der Rolle „{role}“. \
         Dies ist eine Bitte des Nutzers an dich (UIA), kein direktes Routing: \
         Du entscheidest, ob und wie du delegierst, und berichtest das Ergebnis.\n\n\
         {body}"
    )
}

// ---------------------------------------------------------------------------
// Dateianhänge
// ---------------------------------------------------------------------------

/// Hängt alle per `@pfad` erwähnten Dateien an `text` an.
///
/// # Beschreibung
/// Ein Erwähnungs-Token beginnt mit `@` am Textanfang oder nach Leerraum und
/// endet am nächsten Leerraum. Satzzeichen am Ende (`,;:!?)"'.`) werden
/// abgeschnitten, falls das ungekürzte Token nicht auflösbar ist. Tokens ohne
/// `/` und `.`, die nicht existieren (z. B. `@uia` oder `@jemand`), gelten
/// nicht als Dateiversuch und werden stillschweigend ignoriert; alle anderen
/// nicht anhängbaren Tokens landen mit Grund in [`ExpandedChat::rejected`].
///
/// Der Originaltext bleibt unverändert; jede Datei folgt als
/// `<datei pfad="rel/pfad">\n…\n</datei>`. Ein im Dateiinhalt vorkommendes
/// `</datei>` wird zu `<\/datei>` entschärft, damit der Block nicht vorzeitig
/// endet. Dieselbe Datei wird nur einmal angehängt.
pub(crate) fn expand_file_mentions(text: &str, root: &Path, limits: MentionLimits) -> ExpandedChat {
    let mut out = ExpandedChat {
        text: text.to_owned(),
        ..ExpandedChat::default()
    };
    let tokens = mention_tokens(text);
    if tokens.is_empty() {
        return out;
    }
    let canonical_root = match fs::canonicalize(root) {
        Ok(path) => path,
        Err(error) => {
            for token in tokens {
                if looks_like_path(token) {
                    out.rejected.push((
                        token.to_owned(),
                        format!("Projektwurzel nicht auflösbar: {error}"),
                    ));
                }
            }
            return out;
        }
    };

    let mut total_bytes = 0usize;
    let mut blocks = String::new();
    for token in tokens {
        let resolved = match resolve_with_trim(token, &canonical_root) {
            Ok(resolved) => resolved,
            Err(ResolveError::NotFound) if !looks_like_path(token) => continue,
            Err(error) => {
                out.rejected.push((token.to_owned(), error.reason_de()));
                continue;
            }
        };
        if out.attached.contains(&resolved.canonical) {
            continue;
        }
        if out.attached.len() >= limits.max_files {
            out.rejected.push((
                token.to_owned(),
                format!(
                    "Höchstzahl von {} angehängten Dateien erreicht",
                    limits.max_files
                ),
            ));
            continue;
        }
        let content = match read_text_capped(&resolved.canonical, limits.max_file_bytes) {
            Ok(content) => content,
            Err(reason) => {
                out.rejected.push((token.to_owned(), reason));
                continue;
            }
        };
        if total_bytes.saturating_add(content.len()) > limits.max_total_bytes {
            out.rejected.push((
                token.to_owned(),
                format!(
                    "Gesamtgrenze von {} Bytes für Anhänge überschritten",
                    limits.max_total_bytes
                ),
            ));
            continue;
        }
        total_bytes += content.len();
        blocks.push_str("\n\n<datei pfad=\"");
        blocks.push_str(&escape_attr(&resolved.relative));
        blocks.push_str("\">\n");
        blocks.push_str(&content.replace("</datei>", "<\\/datei>"));
        if !content.ends_with('\n') {
            blocks.push('\n');
        }
        blocks.push_str("</datei>");
        out.attached.push(resolved.canonical);
    }
    out.text.push_str(&blocks);
    out
}

// ---------------------------------------------------------------------------
// Kandidaten für das Popup
// ---------------------------------------------------------------------------

/// Sammelt bis zu `cap` Dateipfade (relativ, `/`-getrennt, sortiert) unter
/// `root` als Kandidaten für das Erwähnungs-Popup.
///
/// # Beschreibung
/// Breitensuche mit `std::fs` (das Crate hängt nicht an `ignore`):
/// versteckte Einträge (`.`-Präfix), `target/` und `node_modules/` werden
/// übersprungen, Symlinks nicht verfolgt und gesperrte Dateien (siehe
/// Modul-Doku) ausgelassen. Zusätzlich begrenzt ein Besuchsbudget die
/// Laufzeit in sehr großen Bäumen.
pub(crate) fn scan_mention_candidates(root: &Path, cap: usize) -> Vec<String> {
    let mut found = Vec::new();
    if cap == 0 {
        return found;
    }
    let visit_budget = cap.saturating_mul(50).max(10_000);
    let mut visited = 0usize;
    let mut queue: VecDeque<(PathBuf, String)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), String::new()));

    'walk: while let Some((dir, prefix)) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<(String, fs::FileType)> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let file_type = entry.file_type().ok()?;
                Some((name, file_type))
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, file_type) in entries {
            visited += 1;
            if visited > visit_budget {
                break 'walk;
            }
            if name.starts_with('.') {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if file_type.is_dir() {
                if name == "target" || name == "node_modules" {
                    continue;
                }
                queue.push_back((dir.join(&name), rel));
            } else if file_type.is_file() && !is_denied_name(&name) {
                found.push(rel);
                if found.len() >= cap {
                    break 'walk;
                }
            }
        }
    }
    found.sort();
    found
}

// ---------------------------------------------------------------------------
// Interne Hilfen
// ---------------------------------------------------------------------------

/// Erfolgreich aufgelöste Datei.
struct ResolvedFile {
    canonical: PathBuf,
    /// Pfad relativ zur kanonischen Wurzel, `/`-getrennt.
    relative: String,
}

/// Gründe, warum ein Token nicht als Datei auflösbar ist.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolveError {
    NotFound,
    OutsideRoot,
    Denied,
    NotRegularFile,
}

impl ResolveError {
    fn reason_de(&self) -> String {
        match self {
            Self::NotFound => "Datei nicht gefunden".to_owned(),
            Self::OutsideRoot => "Pfad liegt außerhalb des Projektverzeichnisses".to_owned(),
            Self::Denied => {
                "Datei ist gesperrt (Geheimnis/Schlüssel oder .git-Verzeichnis)".to_owned()
            }
            Self::NotRegularFile => "keine reguläre Datei".to_owned(),
        }
    }
}

/// Satzzeichen, die am Tokenende abgeschnitten werden dürfen.
const TRAILING_PUNCTUATION: &[char] = &[',', ';', ':', '!', '?', ')', '"', '\'', '.'];

/// Liefert alle `@`-Tokens (ohne `@`) in Auftretensreihenfolge.
fn mention_tokens(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
        .filter(|token| !token.is_empty())
        .collect()
}

/// `true`, wenn das Token nach einem Pfad aussieht (enthält `/` oder `.`).
fn looks_like_path(token: &str) -> bool {
    token.contains('/') || token.contains('.')
}

/// Löst `token` auf; scheitert das, wird ein um Satzzeichen gekürztes Token
/// versucht. Der Fehler des ungekürzten Tokens hat Vorrang, außer er ist
/// `NotFound`.
fn resolve_with_trim(token: &str, canonical_root: &Path) -> Result<ResolvedFile, ResolveError> {
    match resolve_file(token, canonical_root) {
        Ok(resolved) => Ok(resolved),
        Err(ResolveError::NotFound) => {
            let trimmed = token.trim_end_matches(TRAILING_PUNCTUATION);
            if trimmed.is_empty() || trimmed == token {
                Err(ResolveError::NotFound)
            } else {
                resolve_file(trimmed, canonical_root)
            }
        }
        Err(other) => Err(other),
    }
}

/// Kernprüfung: Sperrliste (lexikalisch), Kanonisierung, Wurzel-Präfix,
/// Sperrliste (aufgelöst), reguläre Datei.
fn resolve_file(token: &str, canonical_root: &Path) -> Result<ResolvedFile, ResolveError> {
    let typed = Path::new(token);
    if is_denied_path(typed) {
        return Err(ResolveError::Denied);
    }
    let joined = canonical_root.join(typed);
    let canonical = fs::canonicalize(&joined).map_err(|_| ResolveError::NotFound)?;
    let Ok(relative) = canonical.strip_prefix(canonical_root) else {
        return Err(ResolveError::OutsideRoot);
    };
    if is_denied_path(relative) {
        return Err(ResolveError::Denied);
    }
    let metadata = fs::metadata(&canonical).map_err(|_| ResolveError::NotFound)?;
    if !metadata.is_file() {
        return Err(ResolveError::NotRegularFile);
    }
    let relative = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    Ok(ResolvedFile {
        canonical,
        relative,
    })
}

/// `true`, wenn irgendeine Komponente `.git` ist oder der Dateiname auf der
/// Sperrliste steht.
fn is_denied_path(path: &Path) -> bool {
    let mut last = None;
    for component in path.components() {
        if let Component::Normal(part) = component {
            let part = part.to_string_lossy();
            if part == ".git" {
                return true;
            }
            last = Some(part.into_owned());
        }
    }
    last.is_some_and(|name| is_denied_name(&name))
}

/// Sperrliste für Dateinamen: `.env*`, `*.pem`, `*.key`, `id_*`.
fn is_denied_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(".env")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.starts_with("id_")
}

/// Liest höchstens `max_bytes + 1` Bytes und prüft Größe, NUL-Bytes und UTF-8.
fn read_text_capped(path: &Path, max_bytes: usize) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| format!("nicht lesbar: {error}"))?;
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("nicht lesbar: {error}"))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "Datei zu groß (Grenze {max_bytes} Bytes je Datei)"
        ));
    }
    if bytes.contains(&0) {
        return Err("Binärdatei wird nicht angehängt".to_owned());
    }
    String::from_utf8(bytes).map_err(|_| "kein gültiges UTF-8".to_owned())
}

/// Entschärft `"`, `&`, `<`, `>` für den Attributwert `pfad`.
fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn project() -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        fs::create_dir_all(dir.path().join("src")).map_err(ctx("mkdir src"))?;
        fs::write(dir.path().join("src/lib.rs"), "fn main() {}\n").map_err(ctx("write lib"))?;
        fs::write(dir.path().join("README.md"), "Hallo").map_err(ctx("write readme"))?;
        Ok(dir)
    }

    fn reason_for<'a>(out: &'a ExpandedChat, token: &str) -> TestResult<&'a str> {
        out.rejected
            .iter()
            .find(|(t, _)| t == token)
            .map(|(_, reason)| reason.as_str())
            .ok_or(TestError::Unexpected(format!(
                "keine Ablehnung für {token}: {:?}",
                out.rejected
            )))
    }

    #[test]
    fn attaches_file_after_text() -> TestResult {
        let dir = project()?;
        let out = expand_file_mentions(
            "Schau dir @src/lib.rs an, bitte.",
            dir.path(),
            MentionLimits::default(),
        );
        assert_eq!(out.attached.len(), 1);
        assert!(out.rejected.is_empty(), "{:?}", out.rejected);
        assert!(out.text.starts_with("Schau dir @src/lib.rs an, bitte."));
        assert!(
            out.text
                .ends_with("<datei pfad=\"src/lib.rs\">\nfn main() {}\n</datei>")
        );
        Ok(())
    }

    #[test]
    fn trailing_punctuation_is_trimmed_and_duplicates_skipped() -> TestResult {
        let dir = project()?;
        let out = expand_file_mentions(
            "@README.md, und nochmal @README.md.",
            dir.path(),
            MentionLimits::default(),
        );
        assert_eq!(out.attached.len(), 1);
        assert!(out.text.contains("<datei pfad=\"README.md\">\nHallo\n</datei>"));
        Ok(())
    }

    #[test]
    fn rejects_parent_traversal() -> TestResult {
        let outer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let root = outer.path().join("proj");
        fs::create_dir_all(&root).map_err(ctx("mkdir"))?;
        fs::write(outer.path().join("secret.txt"), "geheim").map_err(ctx("write"))?;
        let out = expand_file_mentions("@../secret.txt", &root, MentionLimits::default());
        assert!(out.attached.is_empty());
        assert!(reason_for(&out, "../secret.txt")?.contains("außerhalb"));
        assert!(!out.text.contains("geheim"));
        assert_eq!(
            classify_mention("@../secret.txt", &root, &[]),
            MentionTarget::Unknown
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() -> TestResult {
        let outer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let root = outer.path().join("proj");
        fs::create_dir_all(&root).map_err(ctx("mkdir"))?;
        fs::write(outer.path().join("secret.txt"), "geheim").map_err(ctx("write"))?;
        std::os::unix::fs::symlink(outer.path().join("secret.txt"), root.join("link.txt"))
            .map_err(ctx("symlink file"))?;
        std::os::unix::fs::symlink(outer.path(), root.join("outdir"))
            .map_err(ctx("symlink dir"))?;
        let out = expand_file_mentions(
            "@link.txt @outdir/secret.txt",
            &root,
            MentionLimits::default(),
        );
        assert!(out.attached.is_empty());
        assert!(reason_for(&out, "link.txt")?.contains("außerhalb"));
        assert!(reason_for(&out, "outdir/secret.txt")?.contains("außerhalb"));
        assert!(!out.text.contains("geheim"));
        assert_eq!(
            classify_mention("link.txt", &root, &[]),
            MentionTarget::Unknown
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_denied_file_inside_root_is_rejected() -> TestResult {
        let dir = project()?;
        fs::write(dir.path().join(".env"), "TOKEN=1").map_err(ctx("write env"))?;
        std::os::unix::fs::symlink(dir.path().join(".env"), dir.path().join("harmlos.txt"))
            .map_err(ctx("symlink"))?;
        let out = expand_file_mentions("@harmlos.txt", dir.path(), MentionLimits::default());
        assert!(out.attached.is_empty());
        assert!(reason_for(&out, "harmlos.txt")?.contains("gesperrt"));
        Ok(())
    }

    #[test]
    fn denylist_blocks_secrets_and_git() -> TestResult {
        let dir = project()?;
        fs::create_dir_all(dir.path().join(".git")).map_err(ctx("mkdir git"))?;
        for name in [".env", ".env.local", "server.pem", "tls.key", "id_ed25519", ".git/config"] {
            fs::write(dir.path().join(name), "x").map_err(ctx("write denied"))?;
        }
        let out = expand_file_mentions(
            "@.env @.env.local @server.pem @tls.key @id_ed25519 @.git/config",
            dir.path(),
            MentionLimits::default(),
        );
        assert!(out.attached.is_empty());
        assert_eq!(out.rejected.len(), 6, "{:?}", out.rejected);
        for (_, reason) in &out.rejected {
            assert!(reason.contains("gesperrt"), "{reason}");
        }
        Ok(())
    }

    #[test]
    fn rejects_binary_and_invalid_utf8() -> TestResult {
        let dir = project()?;
        fs::write(dir.path().join("bild.bin"), [0x89, 0x50, 0x00, 0x01]).map_err(ctx("bin"))?;
        fs::write(dir.path().join("latin1.txt"), [0x48, 0xE4, 0x6C]).map_err(ctx("latin"))?;
        let out = expand_file_mentions(
            "@bild.bin @latin1.txt",
            dir.path(),
            MentionLimits::default(),
        );
        assert!(out.attached.is_empty());
        assert!(reason_for(&out, "bild.bin")?.contains("Binär"));
        assert!(reason_for(&out, "latin1.txt")?.contains("UTF-8"));
        Ok(())
    }

    #[test]
    fn enforces_size_total_and_count_caps() -> TestResult {
        let dir = project()?;
        for i in 0..4 {
            fs::write(dir.path().join(format!("f{i}.txt")), "0123456789")
                .map_err(ctx("write f"))?;
        }
        fs::write(dir.path().join("gross.txt"), "x".repeat(50)).map_err(ctx("gross"))?;
        let limits = MentionLimits {
            max_file_bytes: 20,
            max_total_bytes: 25,
            max_files: 3,
        };
        let out = expand_file_mentions("@gross.txt @f0.txt @f1.txt @f2.txt", dir.path(), limits);
        assert!(reason_for(&out, "gross.txt")?.contains("zu groß"));
        assert!(reason_for(&out, "f2.txt")?.contains("Gesamtgrenze"));
        assert_eq!(out.attached.len(), 2);

        let limits = MentionLimits {
            max_file_bytes: 20,
            max_total_bytes: 1000,
            max_files: 2,
        };
        let out = expand_file_mentions("@f0.txt @f1.txt @f2.txt @f3.txt", dir.path(), limits);
        assert_eq!(out.attached.len(), 2);
        assert!(reason_for(&out, "f2.txt")?.contains("Höchstzahl"));
        assert!(reason_for(&out, "f3.txt")?.contains("Höchstzahl"));
        Ok(())
    }

    #[test]
    fn default_limits_match_contract() {
        let limits = MentionLimits::default();
        assert_eq!(limits.max_file_bytes, 64 * 1024);
        assert_eq!(limits.max_total_bytes, 256 * 1024);
        assert_eq!(limits.max_files, 8);
    }

    #[test]
    fn non_path_tokens_are_ignored_but_missing_paths_rejected() -> TestResult {
        let dir = project()?;
        let out = expand_file_mentions(
            "Hallo @jemand, mail an a@b.de, siehe @fehlt.rs",
            dir.path(),
            MentionLimits::default(),
        );
        assert!(out.attached.is_empty());
        assert_eq!(out.rejected.len(), 1, "{:?}", out.rejected);
        assert!(reason_for(&out, "fehlt.rs")?.contains("nicht gefunden"));
        Ok(())
    }

    #[test]
    fn classifies_role_vs_file() -> TestResult {
        let dir = project()?;
        fs::write(dir.path().join("explorer"), "datei").map_err(ctx("write"))?;
        let roles = ["explorer", "research"];
        assert_eq!(
            classify_mention("@Research", dir.path(), &roles),
            MentionTarget::Role("research".to_owned())
        );
        // Rolle hat Vorrang; `./explorer` erreicht die gleichnamige Datei.
        assert_eq!(
            classify_mention("explorer", dir.path(), &roles),
            MentionTarget::Role("explorer".to_owned())
        );
        let canonical_root = fs::canonicalize(dir.path()).map_err(ctx("canon"))?;
        assert_eq!(
            classify_mention("./explorer", dir.path(), &roles),
            MentionTarget::File(canonical_root.join("explorer"))
        );
        assert_eq!(
            classify_mention("@src/lib.rs", dir.path(), &roles),
            MentionTarget::File(canonical_root.join("src/lib.rs"))
        );
        assert_eq!(
            classify_mention("@src", dir.path(), &roles),
            MentionTarget::Unknown
        );
        assert_eq!(
            classify_mention("@unbekannt", dir.path(), &roles),
            MentionTarget::Unknown
        );
        assert_eq!(classify_mention("@", dir.path(), &roles), MentionTarget::Unknown);
        Ok(())
    }

    #[test]
    fn role_text_is_a_delegation_request() {
        let text = role_mention_text("research", "  Finde Quellen zu X  ");
        assert!(text.contains("„research“"));
        assert!(text.contains("Bitte delegiere"));
        assert!(text.contains("kein direktes Routing"));
        assert!(text.ends_with("Finde Quellen zu X"));
    }

    #[test]
    fn content_cannot_close_block_early() -> TestResult {
        let dir = project()?;
        fs::write(dir.path().join("trick.md"), "a</datei>b").map_err(ctx("write"))?;
        let out = expand_file_mentions("@trick.md", dir.path(), MentionLimits::default());
        assert_eq!(out.text.matches("</datei>").count(), 1);
        assert!(out.text.contains("a<\\/datei>b"));
        Ok(())
    }

    #[test]
    fn scan_skips_hidden_target_and_denied() -> TestResult {
        let dir = project()?;
        for sub in [".git", "target", "node_modules", ".hidden"] {
            fs::create_dir_all(dir.path().join(sub)).map_err(ctx("mkdir"))?;
            fs::write(dir.path().join(sub).join("x.txt"), "x").map_err(ctx("write"))?;
        }
        fs::write(dir.path().join("server.pem"), "x").map_err(ctx("pem"))?;
        fs::write(dir.path().join(".env"), "x").map_err(ctx("env"))?;
        let found = scan_mention_candidates(dir.path(), 100);
        assert_eq!(found, vec!["README.md".to_owned(), "src/lib.rs".to_owned()]);
        assert_eq!(scan_mention_candidates(dir.path(), 1).len(), 1);
        assert!(scan_mention_candidates(dir.path(), 0).is_empty());
        Ok(())
    }
}
