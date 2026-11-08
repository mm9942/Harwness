//! `Cargo.lock`-Teilmenge: gesperrte crates.io-Pakete mit Name und Version.
//!
//! # Zweck
//! Liest aus einem `Cargo.lock` genau das, was der Abgleich braucht: je
//! `[[package]]`-Eintrag den Namen, die Version und die Quelle. Alles andere
//! (`checksum`, `dependencies`, `[metadata]`, die Formatversion) wird beim
//! Deserialisieren ignoriert, damit jede Lockfile-Fassung (v1 bis v4)
//! lesbar bleibt.
//!
//! # Welche Einträge zählen
//! Nur Pakete aus der crates.io-Registry ([`CRATES_IO_SOURCES`], Git- und
//! Sparse-Index). Das ist dieselbe Auswahl wie bei `cargo audit`
//! (`rustsec::Database::query_vulnerabilities`): ein Pfad-Paket (ohne
//! `source`, etwa ein Workspace-Member), ein Git-Paket oder ein Paket einer
//! fremden Registry kann zufällig wie eine crates.io-Crate heißen, ist aber
//! nicht diese Crate — eine RustSec-Meldung darauf wäre ein Fehlalarm.
//!
//! Mehrere Einträge mit demselben Namen und verschiedenen Versionen bleiben
//! alle erhalten (ein `Cargo.lock` sperrt eine Crate oft in mehreren
//! Fassungen); geprüft wird jede einzeln.
//!
//! # Ungültige Einträge
//! Ein crates.io-Eintrag mit unlesbarer Version oder einem Namen außerhalb
//! von `[A-Za-z0-9_-]` (höchstens [`MAX_CRATE_NAME_LEN`] Zeichen) wird
//! übersprungen und in [`ParsedLockfile::invalid_entries`] gezählt. Der Name
//! wird später zu einem Glob-Muster; ein `*`, `?`, `/` oder `..` darin
//! könnte sonst fremde Verzeichnisse der Datenbank erreichen.
//!
//! # Nebenläufigkeit
//! Reine Funktionen und Werttypen; `Send + Sync`.
//!
//! # Fehler
//! [`SourceFault::LockfileShape`], wenn der Inhalt kein TOML der erwarteten
//! Form ist. Inhaltsfrei.
//!
//! # Examples
//! ```rust,ignore
//! // Crate-intern.
//! let parsed = parse_lockfile("version = 4\n")?;
//! assert!(parsed.crates.is_empty());
//! ```

use semver::Version;
use serde::Deserialize;

use crate::fault::SourceFault;

/// Die `source`-Werte, unter denen `Cargo.lock` ein crates.io-Paket führt:
/// der klassische Git-Index und der Sparse-Index.
pub(crate) const CRATES_IO_SOURCES: [&str; 2] = [
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
];

/// Höchstlänge eines Crate-Namens (crates.io erlaubt 64 Zeichen).
pub(crate) const MAX_CRATE_NAME_LEN: usize = 64;

/// Ein gesperrtes crates.io-Paket.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LockedCrate {
    /// Der Crate-Name, geprüft durch [`is_valid_crate_name`].
    pub(crate) name: String,
    /// Die gesperrte Version (inklusive Build-Metadaten, falls vorhanden).
    pub(crate) version: Version,
}

/// Ergebnis von [`parse_lockfile`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParsedLockfile {
    /// Die gültigen crates.io-Pakete in Dateireihenfolge.
    pub(crate) crates: Vec<LockedCrate>,
    /// crates.io-Einträge mit ungültigem Namen oder unlesbarer Version.
    pub(crate) invalid_entries: usize,
}

// Wurzel eines `Cargo.lock`; unbekannte Schlüssel werden ignoriert.
#[derive(Deserialize)]
struct RawLockfile {
    #[serde(default)]
    package: Vec<RawPackage>,
}

// Ein `[[package]]`-Eintrag; nur die drei gebrauchten Felder.
#[derive(Deserialize)]
struct RawPackage {
    name: String,
    version: String,
    #[serde(default)]
    source: Option<String>,
}

/// Liest die crates.io-Pakete aus dem Inhalt eines `Cargo.lock`.
///
/// # Arguments
/// - `content` (`&str`): der vollständige Dateiinhalt.
///
/// # Returns
/// Die gültigen crates.io-Pakete und die Zahl ungültiger crates.io-Einträge
/// (siehe Moduldoku).
///
/// # Errors
/// [`SourceFault::LockfileShape`]: kein gültiges TOML, oder ein
/// `[[package]]`-Eintrag ohne `name`/`version`.
///
/// # Concurrency
/// Rein; von jedem Thread aufrufbar.
pub(crate) fn parse_lockfile(content: &str) -> Result<ParsedLockfile, SourceFault> {
    let raw: RawLockfile = toml::from_str(content).map_err(|_| SourceFault::LockfileShape)?;

    let mut parsed = ParsedLockfile::default();
    for package in raw.package {
        if !package.source.as_deref().is_some_and(is_crates_io_source) {
            continue;
        }
        let valid_name = is_valid_crate_name(&package.name);
        match Version::parse(&package.version) {
            Ok(version) if valid_name => parsed.crates.push(LockedCrate {
                name: package.name,
                version,
            }),
            _ => parsed.invalid_entries = parsed.invalid_entries.saturating_add(1),
        }
    }
    Ok(parsed)
}

/// Ob `source` eine der [`CRATES_IO_SOURCES`] ist.
///
/// # Arguments
/// - `source` (`&str`): der `source`-Wert eines Lockfile-Eintrags oder einer
///   Advisory.
///
/// # Returns
/// `true` für den crates.io-Git- oder Sparse-Index.
pub(crate) fn is_crates_io_source(source: &str) -> bool {
    CRATES_IO_SOURCES.contains(&source)
}

/// Ob `name` als Crate-Name und damit als Glob-Komponente taugt.
///
/// # Arguments
/// - `name` (`&str`): der Name aus dem Lockfile.
///
/// # Returns
/// `true` für 1 bis [`MAX_CRATE_NAME_LEN`] Zeichen aus `[A-Za-z0-9_-]`.
pub(crate) fn is_valid_crate_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_CRATE_NAME_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{CRATES_IO, TestResult, ctx, lockfile_text};

    fn version(text: &str) -> TestResult<Version> {
        Version::parse(text).map_err(ctx("Version parsen"))
    }

    #[test]
    fn test_keeps_crates_io_packages_and_skips_path_and_git_packages() -> TestResult {
        let content = lockfile_text(&[
            ("mein-projekt", "0.1.0", None),
            ("tokio", "1.8.0", Some(CRATES_IO)),
            (
                "serde",
                "1.0.0",
                Some("git+https://github.com/serde-rs/serde?rev=abc#abc"),
            ),
            ("bytes", "1.0.0", Some("sparse+https://index.crates.io/")),
        ]);
        let parsed = parse_lockfile(&content).map_err(ctx("Lockfile parsen"))?;
        assert_eq!(
            parsed.crates,
            vec![
                LockedCrate {
                    name: "tokio".to_owned(),
                    version: version("1.8.0")?,
                },
                LockedCrate {
                    name: "bytes".to_owned(),
                    version: version("1.0.0")?,
                },
            ]
        );
        assert_eq!(parsed.invalid_entries, 0);
        Ok(())
    }

    #[test]
    fn test_duplicate_names_with_different_versions_are_all_kept() -> TestResult {
        let content = lockfile_text(&[
            ("syn", "2.0.0", Some(CRATES_IO)),
            ("syn", "1.0.109", Some(CRATES_IO)),
        ]);
        let parsed = parse_lockfile(&content).map_err(ctx("Lockfile parsen"))?;
        let versions: Vec<String> = parsed
            .crates
            .iter()
            .map(|locked| locked.version.to_string())
            .collect();
        assert_eq!(versions, ["2.0.0", "1.0.109"]);
        Ok(())
    }

    #[test]
    fn test_invalid_entries_are_counted_and_skipped() -> TestResult {
        let content = lockfile_text(&[
            ("gut", "1.0.0", Some(CRATES_IO)),
            ("kaputt", "eins", Some(CRATES_IO)),
            ("../crates/x", "1.0.0", Some(CRATES_IO)),
            ("stern*", "1.0.0", Some(CRATES_IO)),
            ("", "1.0.0", Some(CRATES_IO)),
            // Ein Pfad-Paket mit kaputter Version zählt nicht: es wird nie geprüft.
            ("lokal", "nicht-semver", None),
        ]);
        let parsed = parse_lockfile(&content).map_err(ctx("Lockfile parsen"))?;
        assert_eq!(parsed.crates.len(), 1);
        assert_eq!(parsed.invalid_entries, 4);
        Ok(())
    }

    #[test]
    fn test_build_metadata_is_kept_in_the_version() -> TestResult {
        let content = lockfile_text(&[("toml", "1.1.6+spec-1.1.0", Some(CRATES_IO))]);
        let parsed = parse_lockfile(&content).map_err(ctx("Lockfile parsen"))?;
        let first = parsed
            .crates
            .first()
            .ok_or_else(crate::test_support::missing("erster Eintrag"))?;
        assert_eq!(first.version.to_string(), "1.1.6+spec-1.1.0");
        Ok(())
    }

    #[test]
    fn test_lockfile_without_packages_is_empty() -> TestResult {
        let parsed = parse_lockfile("version = 4\n").map_err(ctx("Lockfile parsen"))?;
        assert_eq!(parsed, ParsedLockfile::default());
        let parsed = parse_lockfile("").map_err(ctx("leeres Lockfile parsen"))?;
        assert_eq!(parsed, ParsedLockfile::default());
        Ok(())
    }

    #[test]
    fn test_unknown_keys_and_tables_are_ignored() -> TestResult {
        let content = format!(
            "version = 3\n\n[[package]]\nname = \"tokio\"\nversion = \"1.8.0\"\nsource = \"{CRATES_IO}\"\nchecksum = \"00ff\"\ndependencies = [\"bytes\"]\n\n[metadata]\nirgendwas = \"wert\"\n"
        );
        let parsed = parse_lockfile(&content).map_err(ctx("Lockfile parsen"))?;
        assert_eq!(parsed.crates.len(), 1);
        Ok(())
    }

    #[test]
    fn test_malformed_toml_is_rejected_without_content() {
        let marker = "GEHEIMER-INHALT-4711";
        let parsed = parse_lockfile(&format!("[[package]\nname = \"{marker}\""));
        assert_eq!(parsed, Err(SourceFault::LockfileShape));
        assert!(!SourceFault::LockfileShape.to_string().contains(marker));
    }

    #[test]
    fn test_package_without_version_is_a_shape_error() {
        let parsed = parse_lockfile("[[package]]\nname = \"tokio\"\n");
        assert_eq!(parsed, Err(SourceFault::LockfileShape));
    }

    #[test]
    fn test_crate_name_validation() {
        assert!(is_valid_crate_name("tokio"));
        assert!(is_valid_crate_name("serde_json"));
        assert!(is_valid_crate_name("tracing-subscriber"));
        assert!(!is_valid_crate_name(""));
        assert!(!is_valid_crate_name("a/b"));
        assert!(!is_valid_crate_name(".."));
        assert!(!is_valid_crate_name("RUSTSEC-*"));
        assert!(!is_valid_crate_name("na?e"));
        assert!(!is_valid_crate_name("t\u{202E}okio"));
        assert!(!is_valid_crate_name(
            &"a".repeat(MAX_CRATE_NAME_LEN.saturating_add(1))
        ));
        assert!(is_valid_crate_name(&"a".repeat(MAX_CRATE_NAME_LEN)));
    }

    #[test]
    fn test_crates_io_sources() {
        assert!(is_crates_io_source(CRATES_IO));
        assert!(is_crates_io_source("sparse+https://index.crates.io/"));
        assert!(!is_crates_io_source(
            "registry+https://example.invalid/index"
        ));
        assert!(!is_crates_io_source(""));
    }
}
