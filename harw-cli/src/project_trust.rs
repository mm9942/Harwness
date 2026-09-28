//! `harw project` — Projekt-Freigabe (Trust) für ein Verzeichnis verwalten.
//!
//! Spiegelt `<home>/trusted-projects.toml` (siehe [`harw_home::trust`]) auf
//! die drei Aktionen aus [`crate::cli::ProjectAction`]: `trust`, `untrust`,
//! `status`. Der Root-Space wird wie überall in `harw-cli` über
//! [`crate::home::resolve_home`] aufgelöst (`--home` vor `HARW_HOME`); das
//! Startverzeichnis ist das gegebene `DIR` oder, ohne Angabe, das aktuelle
//! Arbeitsverzeichnis. Von dort aus wird der tatsächliche Projekt-Root über
//! [`harw_home::discover_project`] ermittelt (Aufstieg zum nächsten `.git`
//! bzw. konfigurierten Marker) — exakt der Root, den auch die Config-Schicht
//! (`harw_home::paths::config_layers_report_at`) für ihre Trust-Prüfung
//! verwendet. So arbeiten `trust`/`untrust`/`status` aus einem
//! Unterverzeichnis eines Repos auf demselben Root wie die Config-Ladung,
//! statt versehentlich einen leeren `.harw`-Stand im Unterverzeichnis
//! anzulegen bzw. abzufragen. Kanonisierung und Digest-Vergleich übernimmt
//! vollständig `harw_home::trust` — dieses Modul formatiert nur die
//! Nutzerausgabe.
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
/// - Fehler bei der Root-Auflösung (`std::env::current_dir` bzw.
///   [`harw_home::discover_project`], z. B. wenn `DIR` aus einem anderen
///   Grund als "existiert nicht" nicht auflösbar ist) als `String`.
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

/// Löst den Projekt-Root auf: Aufstieg von `path` (oder, ohne Angabe, dem
/// aktuellen Arbeitsverzeichnis) über [`harw_home::discover_project`] zum
/// nächsten Marker (Default `.git`).
///
/// Das ist bewusst derselbe Aufruf wie in der Config-Schicht
/// (`config_layers_report_in` in `harw_home::paths`, dort mit `&[]` = den
/// Default-Markern): ohne geladene Config kennt auch dieses Modul keine
/// konfigurierten `project_root_markers` — beide Stellen weichen also
/// identisch vom Profil ab, statt hier eine andere Root-Definition als die
/// Config-Ladung zu verwenden. Ein Aufruf aus einem Unterverzeichnis eines
/// Repos liefert damit den Repo-Root, nicht das Unterverzeichnis.
///
/// Existiert `path`/das Arbeitsverzeichnis nicht mehr (etwa ein bereits
/// gelöschtes Projekt, das per früherem absoluten Pfad ausgetragen werden
/// soll), wird der gegebene Pfad unverändert zurückgegeben: `untrust_project`
/// behandelt diesen Fall selbst (siehe dort); `trust`/`status` scheitern für
/// einen solchen Pfad ohnehin unabhängig von dieser Funktion.
///
/// # Errors
/// `String`, wenn `std::env::current_dir` scheitert, oder wenn die
/// Root-Suche aus einem anderen Grund als "Pfad existiert nicht" scheitert
/// (z. B. keine Zugriffsrechte auf einen Vorfahren).
fn project_root(path: Option<PathBuf>) -> Result<PathBuf, String> {
    let cwd = match path {
        Some(path) => path,
        None => std::env::current_dir()
            .map_err(|error| format!("aktuelles Arbeitsverzeichnis nicht auflösbar: {error}"))?,
    };
    match harw_home::discover_project(&cwd, &[]) {
        Ok(project) => Ok(project.root),
        Err(harw_home::HomeError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(cwd)
        }
        Err(error) => Err(format!(
            "Projekt-Root nicht auflösbar für {}: {error}",
            cwd.display()
        )),
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
    use crate::test_support::{TestResult, ctx};

    fn temp_home() -> TestResult<tempfile::TempDir> {
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;
        harw_home::ensure_home(home.path()).map_err(ctx("scaffold home"))?;
        Ok(home)
    }

    fn temp_project() -> TestResult<tempfile::TempDir> {
        let project = tempfile::tempdir().map_err(ctx("create temporary project directory"))?;
        std::fs::create_dir(project.path().join(".harw")).map_err(ctx("create .harw"))?;
        Ok(project)
    }

    /// Wie [`temp_project`], aber mit `.git`-Marker (echter Repo-Root) und
    /// einer `src`-Unterverzeichnis-Ebene für die Subdir-Regressionstests.
    fn temp_repo() -> TestResult<tempfile::TempDir> {
        let repo = tempfile::tempdir().map_err(ctx("create temporary repo directory"))?;
        std::fs::create_dir(repo.path().join(".git")).map_err(ctx("create .git"))?;
        std::fs::create_dir(repo.path().join(".harw")).map_err(ctx("create .harw"))?;
        std::fs::create_dir(repo.path().join("src")).map_err(ctx("create src subdirectory"))?;
        Ok(repo)
    }

    #[test]
    fn test_run_trust_then_status_reports_trusted() -> TestResult {
        let home = temp_home()?;
        let project = temp_project()?;

        let trust_output = execute(
            home.path(),
            ProjectAction::Trust {
                path: Some(project.path().to_path_buf()),
            },
        )
        .map_err(ctx("trust succeeds"))?;
        assert!(trust_output.starts_with("trusted: "));
        assert!(trust_output.contains("(digest blake3:"));

        let status_output = execute(
            home.path(),
            ProjectAction::Status {
                path: Some(project.path().to_path_buf()),
            },
        )
        .map_err(ctx("status succeeds"))?;
        let canonical =
            std::fs::canonicalize(project.path()).map_err(ctx("canonicalize project"))?;
        assert_eq!(status_output, format!("Trusted: {}", canonical.display()));
        Ok(())
    }

    #[test]
    fn test_run_untrust_unknown_reports_false() -> TestResult {
        let home = temp_home()?;
        let project = temp_project()?;

        let output = execute(
            home.path(),
            ProjectAction::Untrust {
                path: Some(project.path().to_path_buf()),
            },
        )
        .map_err(ctx("untrust of an unknown project still succeeds"))?;

        let canonical =
            std::fs::canonicalize(project.path()).map_err(ctx("canonicalize project"))?;
        assert_eq!(output, format!("not trusted: {}", canonical.display()));
        Ok(())
    }

    #[test]
    fn test_run_status_from_subdirectory_reports_trusted_at_repo_root() -> TestResult {
        let home = temp_home()?;
        let repo = temp_repo()?;

        execute(
            home.path(),
            ProjectAction::Trust {
                path: Some(repo.path().to_path_buf()),
            },
        )
        .map_err(ctx("trust at repo root succeeds"))?;

        // `status` aus dem Unterverzeichnis `src/` heraus muss denselben Root
        // wie die Config-Schicht prüfen — nicht das (untrustete) `src/`.
        let status_output = execute(
            home.path(),
            ProjectAction::Status {
                path: Some(repo.path().join("src")),
            },
        )
        .map_err(ctx("status from subdirectory succeeds"))?;

        let canonical_repo = std::fs::canonicalize(repo.path()).map_err(ctx("canonicalize repo"))?;
        assert_eq!(
            status_output,
            format!("Trusted: {}", canonical_repo.display())
        );
        Ok(())
    }

    #[test]
    fn test_run_untrust_from_subdirectory_removes_repo_root_trust() -> TestResult {
        let home = temp_home()?;
        let repo = temp_repo()?;

        execute(
            home.path(),
            ProjectAction::Trust {
                path: Some(repo.path().to_path_buf()),
            },
        )
        .map_err(ctx("trust at repo root succeeds"))?;

        // `untrust` aus `src/` heraus muss den Repo-Root-Eintrag entfernen,
        // nicht fälschlich "not trusted" für `src/` melden.
        let untrust_output = execute(
            home.path(),
            ProjectAction::Untrust {
                path: Some(repo.path().join("src")),
            },
        )
        .map_err(ctx("untrust from subdirectory succeeds"))?;

        let canonical_repo = std::fs::canonicalize(repo.path()).map_err(ctx("canonicalize repo"))?;
        assert_eq!(
            untrust_output,
            format!("removed: {}", canonical_repo.display())
        );

        let status_output = execute(
            home.path(),
            ProjectAction::Status {
                path: Some(repo.path().to_path_buf()),
            },
        )
        .map_err(ctx("status after untrust succeeds"))?;
        assert_eq!(
            status_output,
            format!("Untrusted: {}", canonical_repo.display())
        );
        Ok(())
    }

    #[test]
    fn test_run_untrust_of_deleted_project_still_removes_by_former_path() -> TestResult {
        let home = temp_home()?;
        let project = temp_project()?;
        let canonical_project =
            std::fs::canonicalize(project.path()).map_err(ctx("canonicalize project"))?;

        execute(
            home.path(),
            ProjectAction::Trust {
                path: Some(project.path().to_path_buf()),
            },
        )
        .map_err(ctx("trust succeeds"))?;

        // Das Projekt wird gelöscht, bevor es ausgetragen wird: `project_root`
        // darf die Root-Suche in diesem Fall nicht hart scheitern lassen,
        // sondern muss (wie zuvor) den früheren kanonischen Pfad an
        // `untrust_project` weitergeben, das den NotFound-Fall selbst
        // behandelt.
        drop(project);

        let output = execute(
            home.path(),
            ProjectAction::Untrust {
                path: Some(canonical_project.clone()),
            },
        )
        .map_err(ctx(
            "untrust of a deleted project by its former path still succeeds",
        ))?;
        assert_eq!(output, format!("removed: {}", canonical_project.display()));
        Ok(())
    }
}
