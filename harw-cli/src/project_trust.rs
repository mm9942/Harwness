//! `harw project` — Projekt-Freigabe (Trust) für ein Verzeichnis verwalten.
//!
//! Spiegelt `<home>/trusted-projects.toml` (siehe [`harw_home::trust`]) auf
//! die drei Aktionen aus [`crate::cli::ProjectAction`]: `trust`, `untrust`,
//! `status`. Der Root-Space wird wie überall in `harw-cli` über
//! [`crate::home::resolve_home`] aufgelöst (`--home` vor `HARW_HOME`); das
//! Projekt-Verzeichnis ist das gegebene `DIR` oder, ohne Angabe, das
//! aktuelle Arbeitsverzeichnis. Kanonisierung und Digest-Vergleich
//! übernimmt vollständig `harw_home::trust` — dieses Modul formatiert nur
//! die Nutzerausgabe.
//!
//! # Nebenläufigkeit
//! Zustandslos; jeder Aufruf lädt/schreibt den Trust-Store einmal über
//! `harw_home::trust`. Keine eigene Synchronisation gegen konkurrente
//! `harw project`-Prozesse.
//!
//! # Fehler
//! Alle Fehler werden als `String` mit Kontext (Pfad, Ursache) zurückgegeben
//! — konsistent mit den übrigen `harw-cli`-Subcommand-Läufern
//! (z. B. [`crate::chat::run_chat`]).
//!
//! # Examples
//! ```ignore
//! // `harw-cli` is a binary crate, so this internal module is not
//! // importable as a library path; `run` is wired from `main.rs`'s
//! // dispatch for `Command::Project`.
//! project_trust::run(home_override, action)?;
//! ```

use std::path::{Path, PathBuf};

use harw_home::TrustStatus;

use crate::cli::ProjectAction;

/// Führt `harw project <action>` aus und druckt das Ergebnis auf `stdout`.
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter `--home`-Wert; siehe
///   [`crate::home::resolve_home`].
/// - `action` ([`ProjectAction`]): `Trust`, `Untrust` oder `Status`.
///
/// # Errors
/// `String` mit Kontext, wenn die Home-Auflösung, das Lesen/Schreiben des
/// Trust-Stores oder die Digest-Berechnung scheitert.
pub(crate) fn run(home_override: Option<PathBuf>, action: ProjectAction) -> Result<(), String> {
    let home = crate::home::resolve_home(home_override)?;
    let output = execute(&home, action)?;
    println!("{output}");
    Ok(())
}

/// Testbarer Kern von [`run`]: liefert die Ausgabezeile statt sie zu drucken.
///
/// # Arguments
/// - `home` (`&Path`): bereits aufgelöster Root-Space.
/// - `action` ([`ProjectAction`]): auszuführende Trust-Aktion.
///
/// # Returns
/// Die für den Nutzer bestimmte Ergebniszeile:
/// - Trust: `trusted: <root> (digest <digest>)`
/// - Untrust: `removed: <root>` bzw. `not trusted: <root>`
/// - Status: `Trusted: <root>` / `Untrusted: <root>` / `Changed: <root>`
///
/// # Errors
/// - Kanonisierungsfehler des Projekt-Roots (z. B. Verzeichnis existiert
///   nicht) als `String`.
/// - Jeder [`harw_home::HomeError`] aus `trust_project`/`untrust_project`/
///   `project_trust_status`, in Kontext gesetzt.
fn execute(home: &Path, action: ProjectAction) -> Result<String, String> {
    match action {
        ProjectAction::Trust { path } => {
            let root = project_root(path)?;
            let record = harw_home::trust_project(home, &root)
                .map_err(|error| format!("trust fehlgeschlagen für {}: {error}", root.display()))?;
            Ok(format!(
                "trusted: {} (digest {})",
                record.canonical_root.display(),
                record.digest
            ))
        }
        ProjectAction::Untrust { path } => {
            let root = project_root(path)?;
            let removed = harw_home::untrust_project(home, &root).map_err(|error| {
                format!("untrust fehlgeschlagen für {}: {error}", root.display())
            })?;
            let display_root = display_root(&root);
            if removed {
                Ok(format!("removed: {}", display_root.display()))
            } else {
                Ok(format!("not trusted: {}", display_root.display()))
            }
        }
        ProjectAction::Status { path } => {
            let root = project_root(path)?;
            let status = harw_home::project_trust_status(home, &root).map_err(|error| {
                format!("status fehlgeschlagen für {}: {error}", root.display())
            })?;
            let label = match status {
                TrustStatus::Trusted => "Trusted",
                TrustStatus::Untrusted => "Untrusted",
                TrustStatus::Changed => "Changed",
            };
            let display_root = display_root(&root);
            Ok(format!("{label}: {}", display_root.display()))
        }
    }
}

/// Löst das Ziel-Verzeichnis auf: `path` oder das aktuelle Arbeitsverzeichnis.
///
/// # Errors
/// `String`, wenn `std::env::current_dir` scheitert (z. B. Verzeichnis
/// gelöscht, keine Zugriffsrechte).
fn project_root(path: Option<PathBuf>) -> Result<PathBuf, String> {
    match path {
        Some(path) => Ok(path),
        None => std::env::current_dir()
            .map_err(|error| format!("aktuelles Arbeitsverzeichnis nicht auflösbar: {error}")),
    }
}

/// Kanonisiert `root` für die Anzeige; fällt auf den gegebenen Pfad zurück,
/// wenn er (mehr) nicht existiert — spiegelt den Schlüssel-Fallback aus
/// [`harw_home::untrust_project`].
fn display_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("create temporary HARW home");
        harw_home::ensure_home(home.path()).expect("scaffold home");
        home
    }

    fn temp_project() -> tempfile::TempDir {
        let project = tempfile::tempdir().expect("create temporary project directory");
        std::fs::create_dir(project.path().join(".harw")).expect("create .harw");
        project
    }

    #[test]
    fn test_run_trust_then_status_reports_trusted() {
        let home = temp_home();
        let project = temp_project();

        let trust_output = execute(
            home.path(),
            ProjectAction::Trust {
                path: Some(project.path().to_path_buf()),
            },
        )
        .expect("trust succeeds");
        assert!(trust_output.starts_with("trusted: "));
        assert!(trust_output.contains("(digest blake3:"));

        let status_output = execute(
            home.path(),
            ProjectAction::Status {
                path: Some(project.path().to_path_buf()),
            },
        )
        .expect("status succeeds");
        let canonical = std::fs::canonicalize(project.path()).expect("canonicalize project");
        assert_eq!(status_output, format!("Trusted: {}", canonical.display()));
    }

    #[test]
    fn test_run_untrust_unknown_reports_false() {
        let home = temp_home();
        let project = temp_project();

        let output = execute(
            home.path(),
            ProjectAction::Untrust {
                path: Some(project.path().to_path_buf()),
            },
        )
        .expect("untrust of an unknown project still succeeds");

        let canonical = std::fs::canonicalize(project.path()).expect("canonicalize project");
        assert_eq!(output, format!("not trusted: {}", canonical.display()));
    }
}
