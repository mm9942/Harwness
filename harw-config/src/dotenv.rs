//! `.env`-Datei-Parser und Env-Layer für harwness-eigene Credential-Quellen.
//!
//! # Zweck
//! Nutzer legen Credentials in `~/.harw/.env` (und optional in
//! `~/.harw/profiles/<aktiv>/.env`) ab statt in ihrer Shell-Konfiguration
//! (`~/.zshrc`, `~/.bashrc`). Der Env-Layer wird als [`BTreeMap`] in
//! [`crate::discovery::ResolvedConfig`] gehalten und **niemals** in die
//! Prozess-Umgebung geschrieben — `std::env::set_var` ist in Edition 2024
//! `unsafe` und daher verboten.
//!
//! # Layer-Semantik
//! - Root-Layer: `<home>/.env`
//! - Profil-Layer: `<home>/profiles/<aktiv>/.env`
//! - **Profil überschreibt Root** (letzte Zuweisung gewinnt).
//!
//! # Auflösungs-Präzedenz für `env:`-Refs
//! Prozess-Umgebung gewinnt, falls die Variable dort gesetzt ist; andernfalls
//! greift der Env-Layer. Siehe [`resolve_env_ref`].
//!
//! # Sicherheit
//! Secret-Werte werden niemals geloggt. Ist `<home>/.env` für Gruppe oder
//! Welt lesbar, gibt [`check_dotenv_permissions`] eine Warnung aus.
//!
//! # Schlüssel
//! Exportierte öffentliche Typen:
//! - [`load_dotenv`] — Lädt eine einzelne `.env`-Datei in eine `BTreeMap`.
//! - [`load_env_layer`] — Lädt Root + Profil und mergt die Schichten.
//! - [`resolve_env_ref`] — Löst einen Variablennamen prozess-first, dann layer-fallback auf.
//! - [`check_dotenv_permissions`] — Warnt bei zu offenen Datei-Rechten.
//!
//! # Concurrency
//! Alle Funktionen sind zustandslos; der zurückgegebene [`BTreeMap`] ist
//! `Send + Sync`.
//!
//! # Spec
//! Spezifikation: harwness-Feature „harw-dotenv-credential-source".

use std::collections::BTreeMap;
use std::path::Path;

/// Lädt eine `.env`-Datei in eine geordnete Map.
///
/// # Description
/// Parst Zeilen der Form `KEY=VALUE` (Shell-Syntax-Subset):
/// - Zeilen, die mit `#` beginnen (nach führenden Leerzeichen), sind Kommentare.
/// - Ein führendes `export ` wird stillschweigend entfernt.
/// - Umschließende einfache (`'`) oder doppelte (`"`) Anführungszeichen am
///   VALUE werden getrimmt (nur vollständige Paare).
/// - Leere Zeilen und Zeilen ohne `=` werden übersprungen.
/// - Fehlerhafte Einträge werden übersprungen (Funktion panikt niemals).
///
/// Wenn die Datei nicht existiert, wird eine leere Map zurückgegeben.
///
/// # Arguments
/// - `path` (`&Path`): Pfad zur `.env`-Datei.
///
/// # Returns
/// Eine [`BTreeMap<String, String>`] mit allen gültig geparsten Schlüssel-Wert-Paaren.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_config::dotenv::load_dotenv;
///
/// let layer = load_dotenv(Path::new("/home/user/.harw/.env"));
/// ```
#[must_use]
pub fn load_dotenv(path: &Path) -> BTreeMap<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return BTreeMap::new(),
    };
    parse_dotenv_text(&text)
}

/// Parst `.env`-Text aus einem String-Slice.
///
/// # Description
/// Interne Hilfsfunktion, die den rohen Datei-Inhalt in Schlüssel-Wert-Paare
/// zerlegt. Öffentlich damit Tests direkt gegen den Parser schreiben können.
///
/// # Arguments
/// - `text` (`&str`): Roher Datei-Inhalt.
///
/// # Returns
/// Geordnete [`BTreeMap`] aller gültig geparsten Paare.
///
/// # Examples
/// ```rust
/// use harw_config::dotenv::parse_dotenv_text;
///
/// let map = parse_dotenv_text("KEY=value\n# Kommentar\nOTHER='hello'");
/// assert_eq!(map.get("KEY").map(String::as_str), Some("value"));
/// assert_eq!(map.get("OTHER").map(String::as_str), Some("hello"));
/// ```
#[must_use]
pub fn parse_dotenv_text(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();

        // Leerzeilen und Kommentare überspringen.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Führendes `export ` entfernen.
        let line_stripped = trimmed
            .strip_prefix("export ")
            .unwrap_or(trimmed)
            .trim_start();

        // Aufteilen am ersten `=`.
        let Some((key, value)) = line_stripped.split_once('=') else {
            continue;
        };

        let key = key.trim();
        let value = value.trim();

        // Schlüssel muss nicht-leer und ein gültiger Identifier sein.
        if key.is_empty() {
            continue;
        }

        let value = strip_quotes(value);
        map.insert(key.to_owned(), value.to_owned());
    }
    map
}

/// Entfernt umschließende einfache oder doppelte Anführungszeichen.
///
/// Nur vollständige Paare werden entfernt; ein einzelnes Anführungszeichen
/// am Anfang ohne passendes Ende bleibt unverändert.
fn strip_quotes(value: &str) -> &str {
    if (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
        || (value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2)
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Lädt Root-Layer und Profil-Layer und mergt sie (Profil überschreibt Root).
///
/// # Description
/// Lädt in dieser Reihenfolge:
/// 1. `layers[0]/.env` (Root-Home, z. B. `~/.harw/.env`)
/// 2. `layers[1]/.env` (aktives Profil, z. B. `~/.harw/profiles/default/.env`)
///
/// Weitere Layer (Index ≥ 2, z. B. Repo-lokales `.harw`) werden ebenfalls
/// geladen und überschreiben vorherige Werte. Das entspricht der Präzedenz von
/// [`crate::discovery::discover_config`].
///
/// Ist ein Layer-Pfad nicht vorhanden, wird er stillschweigend übersprungen.
///
/// # Arguments
/// - `layers` (`&[std::path::PathBuf]`): Aufsteigende Präzedenzliste wie von
///   `harw_home::config_layers` erzeugt.
///
/// # Returns
/// Gemergte [`BTreeMap<String, String>`] mit allen Schlüssel-Wert-Paaren.
///
/// # Examples
/// ```rust,no_run
/// use std::path::PathBuf;
/// use harw_config::dotenv::load_env_layer;
///
/// let layers = vec![PathBuf::from("/home/user/.harw")];
/// let env = load_env_layer(&layers);
/// ```
#[must_use]
pub fn load_env_layer(layers: &[std::path::PathBuf]) -> BTreeMap<String, String> {
    let mut merged: BTreeMap<String, String> = BTreeMap::new();
    for layer in layers {
        let dotenv_path = layer.join(".env");
        let layer_map = load_dotenv(&dotenv_path);
        merged.extend(layer_map);
    }
    merged
}

/// Löst einen Variablennamen auf: Prozess-Umgebung gewinnt, andernfalls Env-Layer.
///
/// # Description
/// Implementiert die least-surprising dotenv-Semantik: wenn die Variable im
/// laufenden Prozess gesetzt ist (und nicht leer ist), wird ihr Wert
/// zurückgegeben. Andernfalls wird der Env-Layer konsultiert. Ist die Variable
/// in keiner Quelle vorhanden, wird `None` zurückgegeben.
///
/// Secret-Werte werden niemals geloggt.
///
/// # Arguments
/// - `name` (`&str`): Variablenname, z. B. `"OPENAI_API_KEY"`.
/// - `env_layer` (`&BTreeMap<String, String>`): Geladener Env-Layer aus
///   `~/.harw/.env`.
///
/// # Returns
/// `Some(value)` wenn die Variable in Prozess-Umgebung oder Env-Layer gefunden
/// wurde; `None` wenn sie in keiner Quelle vorhanden ist.
///
/// # Concurrency
/// Zustandslos und `Send + Sync`. Liest `std::env::var` ohne Mutation.
///
/// # Examples
/// ```rust
/// use std::collections::BTreeMap;
/// use harw_config::dotenv::resolve_env_ref;
///
/// let mut layer = BTreeMap::new();
/// layer.insert("MY_KEY".to_owned(), "from-dotenv".to_owned());
///
/// // Wenn Prozess-Env nicht gesetzt, greift der Layer.
/// // (Im Test wird keine Prozess-Env-Variable MY_KEY erwartet.)
/// ```
#[must_use]
pub fn resolve_env_ref(name: &str, env_layer: &BTreeMap<String, String>) -> Option<String> {
    // Prozess-Umgebung hat Priorität.
    if let Ok(value) = std::env::var(name) {
        if !value.trim().is_empty() {
            return Some(value);
        }
    }
    // Fallback auf den Env-Layer.
    env_layer.get(name).cloned()
}

/// Prüft die Dateirechte einer `.env`-Datei und gibt eine Warnung aus, wenn
/// die Datei für Gruppe oder Welt lesbar ist.
///
/// # Description
/// Sicherheits-Hygiene: Credential-Dateien sollten ausschließlich für den
/// Besitzer lesbar sein (chmod 600). Ist die Datei nicht vorhanden oder ist
/// die Plattform nicht Unix, wird nichts ausgegeben.
///
/// # Arguments
/// - `path` (`&Path`): Pfad zur zu prüfenden `.env`-Datei.
///
/// # Concurrency
/// Zustandslos; gibt nur auf stderr aus.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_config::dotenv::check_dotenv_permissions;
///
/// check_dotenv_permissions(Path::new("/home/user/.harw/.env"));
/// ```
pub fn check_dotenv_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mode = meta.permissions().mode();
            // Bits: group-read (0o040) oder world-read (0o004)
            if mode & 0o044 != 0 {
                eprintln!(
                    "harw: Warnung: '{}' ist für Gruppe oder Welt lesbar (Modus {:04o}). \
                     Bitte `chmod 600 {}` ausführen, um Credentials zu schützen.",
                    path.display(),
                    mode & 0o777,
                    path.display(),
                );
            }
        }
    }
    // Auf Nicht-Unix-Plattformen ist diese Prüfung nicht anwendbar.
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    // --- Parser-Tests ---

    #[test]
    fn test_parse_simple_key_value() {
        let map = parse_dotenv_text("KEY=value\n");
        assert_eq!(map.get("KEY").map(String::as_str), Some("value"));
    }

    #[test]
    fn test_parse_ignores_comment_lines() {
        let map = parse_dotenv_text("# Kommentar\nKEY=value");
        assert!(!map.contains_key("# Kommentar"));
        assert_eq!(map.get("KEY").map(String::as_str), Some("value"));
    }

    #[test]
    fn test_parse_strips_double_quotes() {
        let map = parse_dotenv_text(r#"KEY="hello world""#);
        assert_eq!(map.get("KEY").map(String::as_str), Some("hello world"));
    }

    #[test]
    fn test_parse_strips_single_quotes() {
        let map = parse_dotenv_text("KEY='hello world'");
        assert_eq!(map.get("KEY").map(String::as_str), Some("hello world"));
    }

    #[test]
    fn test_parse_strips_export_prefix() {
        let map = parse_dotenv_text("export MY_VAR=secret");
        assert_eq!(map.get("MY_VAR").map(String::as_str), Some("secret"));
    }

    #[test]
    fn test_parse_skips_empty_lines() {
        let map = parse_dotenv_text("\n\nKEY=val\n\n");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn test_parse_skips_lines_without_equals() {
        let map = parse_dotenv_text("BROKEN_LINE\nGOOD=ok");
        assert!(!map.contains_key("BROKEN_LINE"));
        assert_eq!(map.get("GOOD").map(String::as_str), Some("ok"));
    }

    #[test]
    fn test_parse_value_with_equals_sign() {
        // Wert darf `=` enthalten — nur am ersten `=` splitten.
        let map = parse_dotenv_text("KEY=a=b=c");
        assert_eq!(map.get("KEY").map(String::as_str), Some("a=b=c"));
    }

    #[test]
    fn test_parse_inline_comment_not_stripped() {
        // Wir unterstützen KEINE Inline-Kommentare (zu fehleranfällig für Passwörter).
        let map = parse_dotenv_text("KEY=value # kommentar");
        assert_eq!(
            map.get("KEY").map(String::as_str),
            Some("value # kommentar")
        );
    }

    #[test]
    fn test_parse_mismatched_quotes_not_stripped() {
        let map = parse_dotenv_text("KEY=\"unmatched");
        // Kein vollständiges Paar → unverändert übernehmen.
        assert_eq!(map.get("KEY").map(String::as_str), Some("\"unmatched"));
    }

    // --- resolve_env_ref Tests ---

    #[test]
    fn test_resolve_env_ref_falls_back_to_layer_when_process_env_absent() {
        // Wir verwenden einen unwahrscheinlichen Variablennamen, der sicher
        // nicht in der Prozess-Umgebung gesetzt ist.
        let var = "HARW_TEST_DOTENV_ABSENT_VARIABLE_XYZ_42";
        let mut layer = BTreeMap::new();
        layer.insert(var.to_owned(), "from-dotenv".to_owned());

        let result = resolve_env_ref(var, &layer);
        assert_eq!(result.as_deref(), Some("from-dotenv"));
    }

    #[test]
    fn test_resolve_env_ref_returns_none_when_absent_everywhere() {
        let var = "HARW_TEST_DOTENV_TOTALLY_MISSING_VAR_XYZ_99";
        let layer = BTreeMap::new();
        let result = resolve_env_ref(var, &layer);
        assert!(result.is_none());
    }

    #[test]
    fn test_load_dotenv_returns_empty_map_when_file_missing() {
        let path = std::path::Path::new("/tmp/harw_test_nonexistent_dotenv_file_xyz.env");
        let map = load_dotenv(path);
        assert!(map.is_empty());
    }

    // --- load_env_layer Tests ---

    #[test]
    fn test_load_env_layer_profile_overrides_root() -> TestResult {
        use std::io::Write;

        let dir = std::env::temp_dir();
        let root_env = dir.join("harw_test_root_layer.env");
        let profile_env = dir.join("harw_test_profile_layer.env");

        // Root definiert KEY=root
        {
            let mut f = std::fs::File::create(&root_env).map_err(ctx("root_env anlegen"))?;
            writeln!(f, "KEY=root").map_err(ctx("KEY=root schreiben"))?;
            writeln!(f, "ONLY_ROOT=yes").map_err(ctx("ONLY_ROOT=yes schreiben"))?;
        }
        // Profil definiert KEY=profile
        {
            let mut f = std::fs::File::create(&profile_env).map_err(ctx("profile_env anlegen"))?;
            writeln!(f, "KEY=profile").map_err(ctx("KEY=profile schreiben"))?;
        }

        // Wir modellieren zwei "Layer"-Verzeichnisse als Elternverzeichnisse der .env-Dateien.
        // Da load_env_layer `layer.join(".env")` ruft, brauchen wir Verzeichnisse mit .env-Dateien.
        let root_dir = dir.join("harw_test_root_layerdir");
        let profile_dir_path = dir.join("harw_test_profile_layerdir");
        std::fs::create_dir_all(&root_dir).map_err(ctx("root_dir anlegen"))?;
        std::fs::create_dir_all(&profile_dir_path).map_err(ctx("profile_dir anlegen"))?;

        std::fs::copy(&root_env, root_dir.join(".env")).map_err(ctx("root .env kopieren"))?;
        std::fs::copy(&profile_env, profile_dir_path.join(".env"))
            .map_err(ctx("profile .env kopieren"))?;

        let layers = vec![root_dir.clone(), profile_dir_path.clone()];
        let merged = load_env_layer(&layers);

        // Profil überschreibt Root.
        assert_eq!(merged.get("KEY").map(String::as_str), Some("profile"));
        // Root-only Variable bleibt erhalten.
        assert_eq!(merged.get("ONLY_ROOT").map(String::as_str), Some("yes"));

        // Aufräumen.
        let _ = std::fs::remove_file(root_env);
        let _ = std::fs::remove_file(profile_env);
        let _ = std::fs::remove_dir_all(root_dir);
        let _ = std::fs::remove_dir_all(profile_dir_path);
        Ok(())
    }
}
