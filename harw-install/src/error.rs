//! Fehlertypen für das `harw-install`-Crate.
//!
//! # Zweck
//! Zentrale, handgeschriebene Error-Enums für den Setup- und Install-Lebenszyklus
//! gemäß Contract-Master `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! „Crate `harw-install` / `src/error.rs`".
//!
//! # Verantwortung
//! Dieses Modul besitzt die Fehlerdefinitionen aller Install-Teilbelange und
//! delegiert nichts an fremde Fehler-Crates (`anyhow`/`thiserror` sind verboten).
//! Jeder Belang trägt einen eigenen Enum, damit die aufrufende Schicht präzise
//! auf den jeweiligen Fehlerraum reagieren kann.
//!
//! # Exportierte Typen
//! - [`InstallError`] / [`InstallResult`] — Installations- und Kontexterkennung.
//! - [`ServiceError`] / [`ServiceResult`] — Service-Manager (systemd/launchd/schtasks).
//! - [`DoctorError`] / [`DoctorResult`] — Diagnose-Checks.
//! - [`UpdateError`] / [`UpdateResult`] — Update-/Versionsprüfung (JSON).
//! - [`MigrationError`] / [`MigrationResult`] — Konfigurationsmigrationen (TOML).
//! - [`PathError`] / [`PathResult`] — Pfad-Sandboxing.
//!
//! # Nebenläufigkeit
//! Alle Typen sind einfache Werttypen, `Send + Sync`, und tragen keinen
//! gemeinsam veränderlichen Zustand. Sie sind gefahrlos über Threads
//! übertragbar.
//!
//! # Fehlerkonvention
//! Jeder Enum implementiert `Display`, `Debug` (delegiert an `Display`),
//! [`std::error::Error`] mit `source()` für gewrappte Ursachen und `From`-Impls
//! für die getragenen Fremdfehler (`std::io::Error`, `serde_json::Error`,
//! `toml_edit::TomlError`).
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::error::{PathError, PathResult};
//!
//! fn reject() -> PathResult<()> {
//!     Err(PathError::Traversal { input: "../etc/passwd".to_owned() })
//! }
//! assert!(reject().is_err());
//! ```

use std::error::Error as StdError;
use std::fmt;

// ---------------------------------------------------------------------------
// InstallError
// ---------------------------------------------------------------------------

/// Fehler bei Installation, Kontexterkennung und Uninstall-Ausführung.
///
/// # Description
/// Deckt I/O-Fehler beim Schreiben/Entfernen von Artefakten sowie das
/// Fehlschlagen der Installationsmethoden-Erkennung ab.
///
/// # Concurrency
/// `Send + Sync`; kein gemeinsam veränderlicher Zustand.
pub enum InstallError {
    /// Ein-/Ausgabefehler bei einer Datei-/Verzeichnisoperation.
    Io {
        /// Betroffener Pfad (menschenlesbar).
        path: String,
        /// Zugrunde liegender I/O-Fehler.
        source: std::io::Error,
    },
    /// Die Installationsmethode konnte nicht bestimmt werden.
    Detect {
        /// Beschreibung, warum die Erkennung scheiterte.
        reason: String,
    },
    /// Das aktuelle Programm-Executable konnte nicht ermittelt werden.
    CurrentExe {
        /// Zugrunde liegender I/O-Fehler von `std::env::current_exe`.
        source: std::io::Error,
    },
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::Io { path, source } => {
                write!(f, "I/O-Fehler an Pfad '{path}': {source}")
            }
            InstallError::Detect { reason } => {
                write!(f, "Installationsmethode nicht erkennbar: {reason}")
            }
            InstallError::CurrentExe { source } => {
                write!(f, "aktuelles Executable nicht ermittelbar: {source}")
            }
        }
    }
}

impl fmt::Debug for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl StdError for InstallError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            InstallError::Io { source, .. } => Some(source),
            InstallError::CurrentExe { source } => Some(source),
            InstallError::Detect { .. } => None,
        }
    }
}

impl From<std::io::Error> for InstallError {
    fn from(source: std::io::Error) -> Self {
        InstallError::Io {
            path: String::new(),
            source,
        }
    }
}

/// Ergebnis-Alias für [`InstallError`].
pub type InstallResult<T> = Result<T, InstallError>;

// ---------------------------------------------------------------------------
// ServiceError
// ---------------------------------------------------------------------------

/// Fehler beim Installieren/Abfragen/Entfernen von Systemdiensten.
///
/// # Description
/// Umfasst nicht unterstützte Plattformen, I/O beim Schreiben von Unit-Dateien
/// und das Fehlschlagen aufgerufener Service-Kommandos.
///
/// # Concurrency
/// `Send + Sync`; kein gemeinsam veränderlicher Zustand.
pub enum ServiceError {
    /// Auf dieser Plattform ist keine Service-Verwaltung verfügbar.
    Unsupported {
        /// Kurzbeschreibung der Plattform/Situation.
        detail: String,
    },
    /// I/O-Fehler beim Schreiben/Löschen einer Unit-/Plist-/Task-Datei.
    Io {
        /// Betroffener Pfad (menschenlesbar).
        path: String,
        /// Zugrunde liegender I/O-Fehler.
        source: std::io::Error,
    },
    /// Ein externes Service-Kommando lieferte einen Fehlerstatus.
    Command {
        /// Aufgerufenes Kommando (z. B. `systemctl --user enable`).
        command: String,
        /// Erklärung/Standardfehler des Kommandos.
        detail: String,
    },
    /// Der angefragte Dienst wurde nicht gefunden.
    NotFound {
        /// Dienstname.
        name: String,
    },
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::Unsupported { detail } => {
                write!(f, "Service-Verwaltung nicht unterstützt: {detail}")
            }
            ServiceError::Io { path, source } => {
                write!(f, "I/O-Fehler an Pfad '{path}': {source}")
            }
            ServiceError::Command { command, detail } => {
                write!(f, "Service-Kommando '{command}' fehlgeschlagen: {detail}")
            }
            ServiceError::NotFound { name } => {
                write!(f, "Dienst '{name}' nicht gefunden")
            }
        }
    }
}

impl fmt::Debug for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl StdError for ServiceError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            ServiceError::Io { source, .. } => Some(source),
            ServiceError::Unsupported { .. }
            | ServiceError::Command { .. }
            | ServiceError::NotFound { .. } => None,
        }
    }
}

impl From<std::io::Error> for ServiceError {
    fn from(source: std::io::Error) -> Self {
        ServiceError::Io {
            path: String::new(),
            source,
        }
    }
}

/// Ergebnis-Alias für [`ServiceError`].
pub type ServiceResult<T> = Result<T, ServiceError>;

// ---------------------------------------------------------------------------
// DoctorError
// ---------------------------------------------------------------------------

/// Fehler beim Ausführen von Diagnose-Checks.
///
/// # Description
/// Diagnose-Checks sind grundsätzlich fehlertolerant; dieser Enum deckt die
/// seltenen Fälle ab, in denen ein Check gar nicht ausführbar ist.
///
/// # Concurrency
/// `Send + Sync`; kein gemeinsam veränderlicher Zustand.
pub enum DoctorError {
    /// I/O-Fehler bei einer für den Check nötigen Sonde.
    Io {
        /// Betroffener Pfad (menschenlesbar).
        path: String,
        /// Zugrunde liegender I/O-Fehler.
        source: std::io::Error,
    },
    /// Der Check konnte aufgrund einer fehlenden Voraussetzung nicht laufen.
    Unavailable {
        /// Check-Id.
        check: String,
        /// Grund der Nichtverfügbarkeit.
        reason: String,
    },
}

impl fmt::Display for DoctorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DoctorError::Io { path, source } => {
                write!(f, "I/O-Fehler an Pfad '{path}': {source}")
            }
            DoctorError::Unavailable { check, reason } => {
                write!(f, "Check '{check}' nicht ausführbar: {reason}")
            }
        }
    }
}

impl fmt::Debug for DoctorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl StdError for DoctorError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            DoctorError::Io { source, .. } => Some(source),
            DoctorError::Unavailable { .. } => None,
        }
    }
}

impl From<std::io::Error> for DoctorError {
    fn from(source: std::io::Error) -> Self {
        DoctorError::Io {
            path: String::new(),
            source,
        }
    }
}

/// Ergebnis-Alias für [`DoctorError`].
pub type DoctorResult<T> = Result<T, DoctorError>;

// ---------------------------------------------------------------------------
// UpdateError
// ---------------------------------------------------------------------------

/// Fehler beim Lesen/Schreiben der Versions-/Update-Metadaten.
///
/// # Description
/// Deckt I/O auf `~/.harw/version.json` und Parsing-/Serialisierungsfehler des
/// JSON-Inhalts ab.
///
/// # Concurrency
/// `Send + Sync`; kein gemeinsam veränderlicher Zustand.
pub enum UpdateError {
    /// I/O-Fehler beim Zugriff auf die Versionsdatei.
    Io {
        /// Betroffener Pfad (menschenlesbar).
        path: String,
        /// Zugrunde liegender I/O-Fehler.
        source: std::io::Error,
    },
    /// JSON konnte nicht (de-)serialisiert werden.
    Parse {
        /// Zugrunde liegender `serde_json`-Fehler.
        source: serde_json::Error,
    },
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::Io { path, source } => {
                write!(f, "I/O-Fehler an Pfad '{path}': {source}")
            }
            UpdateError::Parse { source } => {
                write!(f, "Versionsdaten nicht lesbar (JSON): {source}")
            }
        }
    }
}

impl fmt::Debug for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl StdError for UpdateError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            UpdateError::Io { source, .. } => Some(source),
            UpdateError::Parse { source } => Some(source),
        }
    }
}

impl From<std::io::Error> for UpdateError {
    fn from(source: std::io::Error) -> Self {
        UpdateError::Io {
            path: String::new(),
            source,
        }
    }
}

impl From<serde_json::Error> for UpdateError {
    fn from(source: serde_json::Error) -> Self {
        UpdateError::Parse { source }
    }
}

/// Ergebnis-Alias für [`UpdateError`].
pub type UpdateResult<T> = Result<T, UpdateError>;

// ---------------------------------------------------------------------------
// MigrationError
// ---------------------------------------------------------------------------

/// Fehler beim Anwenden von Konfigurationsmigrationen.
///
/// # Description
/// Deckt I/O auf `config.toml`/Backups, TOML-Parsing sowie unbekannte oder
/// nicht anwendbare Versionsschritte ab.
///
/// # Concurrency
/// `Send + Sync`; kein gemeinsam veränderlicher Zustand.
pub enum MigrationError {
    /// I/O-Fehler beim Lesen/Schreiben der Konfigurationsdatei oder Backups.
    Io {
        /// Betroffener Pfad (menschenlesbar).
        path: String,
        /// Zugrunde liegender I/O-Fehler.
        source: std::io::Error,
    },
    /// Das TOML-Dokument konnte nicht geparst werden.
    Parse {
        /// Zugrunde liegender `toml_edit`-Fehler.
        source: toml_edit::TomlError,
    },
    /// Für die vorgefundene Konfigurationsversion existiert kein Migrationspfad.
    Version {
        /// Version, die in der Datei gefunden wurde.
        found: u32,
        /// Zielversion, auf die migriert werden sollte.
        expected: u32,
    },
    /// Eine einzelne Migration schlug bei der Anwendung fehl.
    Apply {
        /// Ausgangsversion der Migration.
        from_version: u32,
        /// Fehlerbeschreibung.
        reason: String,
    },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrationError::Io { path, source } => {
                write!(f, "I/O-Fehler an Pfad '{path}': {source}")
            }
            MigrationError::Parse { source } => {
                write!(f, "Konfiguration nicht lesbar (TOML): {source}")
            }
            MigrationError::Version { found, expected } => {
                write!(
                    f,
                    "kein Migrationspfad von Konfigurationsversion {found} nach {expected}"
                )
            }
            MigrationError::Apply {
                from_version,
                reason,
            } => {
                write!(
                    f,
                    "Migration ab Version {from_version} fehlgeschlagen: {reason}"
                )
            }
        }
    }
}

impl fmt::Debug for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl StdError for MigrationError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            MigrationError::Io { source, .. } => Some(source),
            MigrationError::Parse { source } => Some(source),
            MigrationError::Version { .. } | MigrationError::Apply { .. } => None,
        }
    }
}

impl From<std::io::Error> for MigrationError {
    fn from(source: std::io::Error) -> Self {
        MigrationError::Io {
            path: String::new(),
            source,
        }
    }
}

impl From<toml_edit::TomlError> for MigrationError {
    fn from(source: toml_edit::TomlError) -> Self {
        MigrationError::Parse { source }
    }
}

/// Ergebnis-Alias für [`MigrationError`].
pub type MigrationResult<T> = Result<T, MigrationError>;

// ---------------------------------------------------------------------------
// PathError
// ---------------------------------------------------------------------------

/// Fehler beim Auflösen von Pfaden innerhalb einer Sandbox-Wurzel.
///
/// # Description
/// `PathScope` lehnt Traversal (`..`), absolute Pfade und Symlink-Ausbrüche ab;
/// dieser Enum beschreibt die jeweilige Ablehnung.
///
/// # Concurrency
/// `Send + Sync`; kein gemeinsam veränderlicher Zustand.
pub enum PathError {
    /// Der Eingabepfad enthält einen Traversal-Versuch (`..`).
    Traversal {
        /// Ursprünglicher, abgelehnter Eingabepfad.
        input: String,
    },
    /// Ein absoluter Pfad wurde übergeben, wo nur relative erlaubt sind.
    Absolute {
        /// Ursprünglicher, abgelehnter Eingabepfad.
        input: String,
    },
    /// Der aufgelöste Pfad verlässt über einen Symlink die Sandbox-Wurzel.
    Escape {
        /// Ursprünglicher Eingabepfad.
        input: String,
        /// Sandbox-Wurzel, die nicht verlassen werden darf.
        root: String,
    },
    /// I/O-Fehler beim Auflösen (z. B. `canonicalize`).
    Io {
        /// Betroffener Pfad (menschenlesbar).
        path: String,
        /// Zugrunde liegender I/O-Fehler.
        source: std::io::Error,
    },
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Traversal { input } => {
                write!(f, "Pfad-Traversal abgelehnt: '{input}'")
            }
            PathError::Absolute { input } => {
                write!(f, "absoluter Pfad nicht erlaubt: '{input}'")
            }
            PathError::Escape { input, root } => {
                write!(f, "Pfad '{input}' verlässt Sandbox-Wurzel '{root}'")
            }
            PathError::Io { path, source } => {
                write!(f, "I/O-Fehler an Pfad '{path}': {source}")
            }
        }
    }
}

impl fmt::Debug for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl StdError for PathError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            PathError::Io { source, .. } => Some(source),
            PathError::Traversal { .. } | PathError::Absolute { .. } | PathError::Escape { .. } => {
                None
            }
        }
    }
}

impl From<std::io::Error> for PathError {
    fn from(source: std::io::Error) -> Self {
        PathError::Io {
            path: String::new(),
            source,
        }
    }
}

/// Ergebnis-Alias für [`PathError`].
pub type PathResult<T> = Result<T, PathError>;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn io_err() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::NotFound, "fehlt")
    }

    #[test]
    fn test_install_error_display_detect() {
        let e = InstallError::Detect {
            reason: "kein Marker".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "Installationsmethode nicht erkennbar: kein Marker"
        );
    }

    #[test]
    fn test_install_error_source_present_for_io() {
        let e: InstallError = io_err().into();
        assert!(e.source().is_some());
    }

    #[test]
    fn test_service_error_display_unsupported() {
        let e = ServiceError::Unsupported {
            detail: "Plan 9".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "Service-Verwaltung nicht unterstützt: Plan 9"
        );
    }

    #[test]
    fn test_doctor_error_display_unavailable() {
        let e = DoctorError::Unavailable {
            check: "bwrap".to_owned(),
            reason: "nicht installiert".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "Check 'bwrap' nicht ausführbar: nicht installiert"
        );
    }

    #[test]
    fn test_update_error_display_and_from_serde() -> TestResult {
        let parse_result = serde_json::from_str::<u32>("nope");
        let Err(serde_err) = parse_result else {
            return Err(TestError::Unexpected(
                "ungültiges JSON muss als Err geparst werden".to_owned(),
            ));
        };
        let e: UpdateError = serde_err.into();
        assert!(
            e.to_string()
                .starts_with("Versionsdaten nicht lesbar (JSON):")
        );
        assert!(e.source().is_some());
        Ok(())
    }

    #[test]
    fn test_migration_error_display_version() {
        let e = MigrationError::Version {
            found: 1,
            expected: 3,
        };
        assert_eq!(
            e.to_string(),
            "kein Migrationspfad von Konfigurationsversion 1 nach 3"
        );
    }

    #[test]
    fn test_migration_error_from_toml() -> TestResult {
        let parse_result = "a = = 1".parse::<toml_edit::DocumentMut>();
        let Err(toml_err) = parse_result else {
            return Err(TestError::Unexpected(
                "ungültiges TOML muss als Err geparst werden".to_owned(),
            ));
        };
        let e: MigrationError = toml_err.into();
        assert!(
            e.to_string()
                .starts_with("Konfiguration nicht lesbar (TOML):")
        );
        assert!(e.source().is_some());
        Ok(())
    }

    #[test]
    fn test_path_error_display_traversal() {
        let e = PathError::Traversal {
            input: "../x".to_owned(),
        };
        assert_eq!(e.to_string(), "Pfad-Traversal abgelehnt: '../x'");
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let e = PathError::Absolute {
            input: "/etc".to_owned(),
        };
        assert_eq!(format!("{e:?}"), format!("{e}"));
    }
}
