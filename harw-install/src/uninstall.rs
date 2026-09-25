//! Uninstall-Planung und -Ausführung für `harw-install`.
//!
//! # Zweck
//! Berechnet einen fehlertoleranten Aufräumplan für die Deinstallation gemäß
//! Contract-Master `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! „Crate `harw-install` / `src/uninstall.rs`" und führt ihn optional aus.
//!
//! # Verantwortung
//! Dieses Modul besitzt die Zuordnung von [`UninstallScope`]s auf konkrete
//! Datei-/Verzeichnispfade ([`plan`]) sowie deren Entfernung ([`execute`]).
//! Die Planberechnung ist von der Ausführung strikt getrennt: [`plan`] ist eine
//! reine Berechnung (liest höchstens read-only Verzeichnislistings des
//! Home-Baums), [`execute`] löscht ausschließlich.
//!
//! # Exportierte Typen
//! - [`UninstallScope`] — welche Artefaktklasse entfernt werden soll.
//! - [`CleanupPlan`] — geplante Entfernungen und bewusst erhaltene Pfade.
//! - [`plan`] — Scope-Liste → [`CleanupPlan`].
//! - [`execute`] — [`CleanupPlan`] anwenden (mit `dry_run`-Schalter).
//!
//! # Nebenläufigkeit
//! Alle Typen sind einfache Werttypen (`Send + Sync`) ohne gemeinsam
//! veränderlichen Zustand. [`execute`] führt Datei-I/O sequentiell aus und ist
//! nicht auf parallele Aufrufe auf denselben Pfaden ausgelegt.
//!
//! # Fehlertypen
//! [`execute`] liefert [`crate::error::InstallError`] (I/O beim Entfernen).
//! [`plan`] ist fehlerfrei.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//! use harw_install::uninstall::{plan, execute, UninstallScope};
//!
//! let cleanup = plan(Path::new("/home/u/.harw"), &[UninstallScope::State]);
//! // Trockenlauf entfernt nichts:
//! execute(&cleanup, true).expect("dry-run schlägt nie fehl");
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::InstallError;

/// Klasse von Artefakten, die bei der Deinstallation entfernt werden soll.
///
/// # Description
/// Jede Variante entspricht einem Aufräum-Belang. Der Enum ist
/// `#[non_exhaustive]`, damit künftige Belange additiv ergänzt werden können,
/// ohne abhängige Crates zu brechen.
///
/// # Concurrency
/// `Copy`-Werttyp, `Send + Sync`; gefahrlos über Threads teilbar.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallScope {
    /// Registrierte Systemdienst-Einheiten (systemd-Unit, launchd-Plist).
    Service,
    /// Zustands-/Laufzeitdaten im Home-Baum (Cache, Logs, State) — ohne
    /// `profiles`/`workspace`.
    State,
    /// Arbeitsverzeichnisse je Profil (`profiles/*/workspace`).
    Workspace,
    /// Installiertes Programm-Binary (`~/.local/bin/harw`).
    Binary,
}

/// Ergebnis der Planberechnung: was entfernt und was erhalten wird.
///
/// # Description
/// `removals` listet Pfade, die [`execute`] zu löschen versucht (Reihenfolge
/// stabil, dedupliziert). `preserved` dokumentiert bewusst ausgenommene Pfade —
/// nützlich für Nutzerausgabe und Tests, wird von [`execute`] nicht angefasst.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`; kein gemeinsam veränderlicher Zustand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupPlan {
    /// Zu entfernende Datei-/Verzeichnispfade (Reihenfolge stabil).
    pub removals: Vec<PathBuf>,
    /// Bewusst erhaltene Pfade (nur informativ).
    pub preserved: Vec<PathBuf>,
}

/// Bekannte Zustands-Unterverzeichnisse des Home-Baums (Reihenfolge stabil).
///
/// Diese Namen bilden den State-Scope, falls kein Verzeichnislisting verfügbar
/// ist. Sie schließen `profiles`/`workspace` bewusst aus.
const KNOWN_STATE_DIRS: &[&str] = &["cache", "logs", "state", "run", "tmp"];

/// Top-Level-Namen, die der State-Scope niemals entfernt (Nutzerdaten).
const STATE_EXCLUDED: &[&str] = &["profiles", "workspace"];

/// Berechnet den Aufräumplan für die gewählten Scopes.
///
/// # Description
/// Reine Berechnung: es wird nichts gelöscht. Zur Auflösung dynamischer Pfade
/// (Home-Unterverzeichnisse, `profiles/*/workspace`) darf read-only auf den
/// Verzeichnisbaum zugegriffen werden; fehlende Verzeichnisse führen zu einem
/// stabilen Fallback (bekannte Namen) statt zu einem Fehler.
///
/// Scope-Zuordnung:
/// - [`UninstallScope::State`]: alle Home-Unterverzeichnisse außer `profiles`
///   und `workspace`. Ist [`UninstallScope::Workspace`] **nicht** gewählt,
///   werden die ausgenommenen `profiles`/`workspace`-Pfade in `preserved`
///   vermerkt.
/// - [`UninstallScope::Workspace`]: `profiles/*/workspace` je Profil.
/// - [`UninstallScope::Service`]: bekannte Service-Unit-Pfade (systemd/launchd),
///   abgeleitet aus dem OS-Home (`home.parent()`).
/// - [`UninstallScope::Binary`]: `~/.local/bin/harw` und `harw-agent-runner`
///   (aus dem OS-Home), der Agenten-Build-Cache `cache/agent-builds` und der
///   Installationsvermerk `install.toml` (#22).
///
/// # Arguments
/// - `home` (`&Path`): Wurzel des harw-Home-Verzeichnisses (typisch `~/.harw`).
/// - `scopes` (`&[UninstallScope]`): zu berücksichtigende Belange.
///
/// # Returns
/// Einen [`CleanupPlan`] mit deduplizierten `removals` und `preserved`.
///
/// # Concurrency
/// Führt keine Mutation aus; parallele Aufrufe sind unbedenklich.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_install::uninstall::{plan, UninstallScope};
///
/// let p = plan(Path::new("/home/u/.harw"), &[UninstallScope::Binary]);
/// assert!(!p.removals.is_empty());
/// ```
pub fn plan(home: &Path, scopes: &[UninstallScope]) -> CleanupPlan {
    let mut removals: Vec<PathBuf> = Vec::new();
    let mut preserved: Vec<PathBuf> = Vec::new();

    let workspace_selected = scopes.contains(&UninstallScope::Workspace);
    // OS-Home = Elternverzeichnis des harw-Home (z. B. `$HOME` bei `~/.harw`).
    let os_home = home.parent().unwrap_or(home);

    for scope in scopes {
        match scope {
            UninstallScope::State => {
                collect_state(home, workspace_selected, &mut removals, &mut preserved);
            }
            UninstallScope::Workspace => {
                collect_workspace(home, &mut removals);
            }
            UninstallScope::Service => {
                collect_service(os_home, &mut removals);
            }
            UninstallScope::Binary => {
                let bin = os_home.join(".local").join("bin");
                push_unique(&mut removals, bin.join("harw"));
                // #22: the agent runner installed next to `harw`, and the
                // native agent-build cache it feeds (regenerable, can grow
                // to gigabytes; `harw agent clean` trims it while installed).
                push_unique(&mut removals, bin.join("harw-agent-runner"));
                push_unique(&mut removals, home.join("cache").join("agent-builds"));
                push_unique(&mut removals, home.join("install.toml"));
            }
        }
    }

    CleanupPlan {
        removals,
        preserved,
    }
}

/// Sammelt State-Scope-Pfade (Home-Unterverzeichnisse außer `profiles`/`workspace`).
///
/// Liest read-only das Home-Verzeichnis; fällt auf [`KNOWN_STATE_DIRS`] zurück,
/// wenn das Listing nicht verfügbar ist.
fn collect_state(
    home: &Path,
    workspace_selected: bool,
    removals: &mut Vec<PathBuf>,
    preserved: &mut Vec<PathBuf>,
) {
    match fs::read_dir(home) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect();
            names.sort();
            for name in &names {
                if STATE_EXCLUDED.contains(&name.as_str()) {
                    if !workspace_selected {
                        push_unique(preserved, home.join(name));
                    }
                } else {
                    push_unique(removals, home.join(name));
                }
            }
        }
        Err(_) => {
            // Fallback: bekannte Zustandsverzeichnisse und ausgenommene Pfade.
            for name in KNOWN_STATE_DIRS {
                push_unique(removals, home.join(name));
            }
            if !workspace_selected {
                for name in STATE_EXCLUDED {
                    push_unique(preserved, home.join(name));
                }
            }
        }
    }
}

/// Sammelt Workspace-Scope-Pfade (`profiles/*/workspace`).
///
/// Liest read-only `home/profiles`; ohne Listing bleibt der Scope leer, da
/// keine Profile bekannt sind.
fn collect_workspace(home: &Path, removals: &mut Vec<PathBuf>) {
    let profiles_dir = home.join("profiles");
    if let Ok(entries) = fs::read_dir(&profiles_dir) {
        let mut profile_paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.path())
            .collect();
        profile_paths.sort();
        for profile in profile_paths {
            push_unique(removals, profile.join("workspace"));
        }
    }
}

/// Sammelt bekannte Service-Unit-Pfade abgeleitet aus dem OS-Home.
///
/// Fügt sowohl den systemd- als auch den launchd-Pfad hinzu; nicht existente
/// Pfade sind unschädlich, da [`execute`] fehlertolerant löscht.
fn collect_service(os_home: &Path, removals: &mut Vec<PathBuf>) {
    push_unique(
        removals,
        os_home
            .join(".config")
            .join("systemd")
            .join("user")
            .join("harw.service"),
    );
    push_unique(
        removals,
        os_home
            .join("Library")
            .join("LaunchAgents")
            .join("dev.harw.harw.plist"),
    );
}

/// Hängt `path` an `target` an, sofern noch nicht vorhanden (stabile Dedup).
fn push_unique(target: &mut Vec<PathBuf>, path: PathBuf) {
    if !target.contains(&path) {
        target.push(path);
    }
}

/// Führt einen [`CleanupPlan`] aus.
///
/// # Description
/// Bei `dry_run == true` wird nichts entfernt; die Funktion liefert `Ok(())`.
/// Andernfalls wird jeder Pfad in `plan.removals` entfernt: Verzeichnisse
/// rekursiv (`remove_dir_all`), Dateien via `remove_file`. Bereits fehlende
/// Pfade (`NotFound`) gelten als Erfolg (idempotent). `plan.preserved` wird nie
/// angefasst.
///
/// # Arguments
/// - `plan` (`&CleanupPlan`): der auszuführende Aufräumplan.
/// - `dry_run` (`bool`): wenn `true`, wird ausschließlich validiert, nichts
///   gelöscht.
///
/// # Returns
/// `Ok(())`, wenn alle Entfernungen (oder der Trockenlauf) erfolgreich waren.
///
/// # Errors
/// - [`InstallError::Io`]: wenn das Entfernen eines vorhandenen Pfades mit einem
///   anderen Fehler als `NotFound` scheitert; der betroffene Pfad ist im Fehler
///   enthalten.
///
/// # Concurrency
/// Sequentielle Datei-I/O; nicht für konkurrierende Aufrufe auf denselben
/// Pfaden gedacht.
///
/// # Examples
/// ```rust,no_run
/// use harw_install::uninstall::{CleanupPlan, execute};
///
/// let empty = CleanupPlan { removals: vec![], preserved: vec![] };
/// assert!(execute(&empty, false).is_ok());
/// ```
pub fn execute(plan: &CleanupPlan, dry_run: bool) -> Result<(), InstallError> {
    if dry_run {
        return Ok(());
    }

    for path in &plan.removals {
        remove_path(path)?;
    }
    Ok(())
}

/// Entfernt einen einzelnen Pfad idempotent.
///
/// Verzeichnisse werden rekursiv gelöscht, Dateien einzeln. Ein `NotFound`
/// gilt als Erfolg. Bei existierenden Pfaden, die keine Verzeichnisse sind,
/// wird `remove_file` verwendet.
fn remove_path(path: &Path) -> Result<(), InstallError> {
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    match result {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(InstallError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::fs as stdfs;

    /// Legt ein eindeutiges temporäres Home-Verzeichnis an.
    fn temp_home(tag: &str) -> TestResult<PathBuf> {
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        base.push(format!("harw-uninstall-{tag}-{nanos}"));
        stdfs::create_dir_all(&base).map_err(ctx("temp home anlegen"))?;
        Ok(base)
    }

    #[test]
    fn test_plan_state_removes_non_profile_dirs_and_preserves_data() -> TestResult {
        let home = temp_home("state")?;
        for d in ["cache", "logs", "profiles", "workspace"] {
            stdfs::create_dir_all(home.join(d)).map_err(ctx("dir"))?;
        }

        let p = plan(&home, &[UninstallScope::State]);

        assert!(p.removals.contains(&home.join("cache")));
        assert!(p.removals.contains(&home.join("logs")));
        assert!(!p.removals.contains(&home.join("profiles")));
        assert!(!p.removals.contains(&home.join("workspace")));
        assert!(p.preserved.contains(&home.join("profiles")));
        assert!(p.preserved.contains(&home.join("workspace")));

        stdfs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_plan_state_with_workspace_does_not_preserve() -> TestResult {
        let home = temp_home("state-ws")?;
        for d in ["cache", "profiles", "workspace"] {
            stdfs::create_dir_all(home.join(d)).map_err(ctx("dir"))?;
        }

        let p = plan(&home, &[UninstallScope::State, UninstallScope::Workspace]);

        assert!(p.preserved.is_empty());
        stdfs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_plan_workspace_enumerates_profiles() -> TestResult {
        let home = temp_home("ws")?;
        stdfs::create_dir_all(home.join("profiles").join("default").join("workspace"))
            .map_err(ctx("dir"))?;
        stdfs::create_dir_all(home.join("profiles").join("alt").join("workspace"))
            .map_err(ctx("dir"))?;

        let p = plan(&home, &[UninstallScope::Workspace]);

        assert!(
            p.removals
                .contains(&home.join("profiles").join("default").join("workspace"))
        );
        assert!(
            p.removals
                .contains(&home.join("profiles").join("alt").join("workspace"))
        );
        stdfs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_plan_binary_targets_local_bin() -> TestResult {
        let home = temp_home("bin")?.join(".harw");
        let p = plan(&home, &[UninstallScope::Binary]);
        let os_home = home.parent().ok_or(TestError::Missing("parent"))?;
        assert!(
            p.removals
                .contains(&os_home.join(".local").join("bin").join("harw"))
        );
        assert!(
            p.removals
                .contains(&os_home.join(".local").join("bin").join("harw-agent-runner"))
        );
        assert!(
            p.removals
                .contains(&home.join("cache").join("agent-builds"))
        );
        assert!(p.removals.contains(&home.join("install.toml")));
        Ok(())
    }

    #[test]
    fn test_plan_service_targets_unit_paths() {
        let home = PathBuf::from("/home/tester/.harw");
        let p = plan(&home, &[UninstallScope::Service]);
        assert!(p.removals.contains(&PathBuf::from(
            "/home/tester/.config/systemd/user/harw.service"
        )));
        assert!(p.removals.contains(&PathBuf::from(
            "/home/tester/Library/LaunchAgents/dev.harw.harw.plist"
        )));
    }

    #[test]
    fn test_plan_deduplicates_removals() {
        let home = PathBuf::from("/home/tester/.harw");
        let p = plan(&home, &[UninstallScope::Binary, UninstallScope::Binary]);
        let count = p
            .removals
            .iter()
            .filter(|path| path.ends_with("harw"))
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_execute_dry_run_removes_nothing() -> TestResult {
        let home = temp_home("dry")?;
        let victim = home.join("cache");
        stdfs::create_dir_all(&victim).map_err(ctx("dir"))?;

        let p = plan(&home, &[UninstallScope::State]);
        let result = execute(&p, true);

        assert!(result.is_ok());
        assert!(victim.exists(), "dry-run darf nichts löschen");
        stdfs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_execute_real_removes_dirs() -> TestResult {
        let home = temp_home("real")?;
        let victim = home.join("cache");
        stdfs::create_dir_all(&victim).map_err(ctx("dir"))?;
        stdfs::create_dir_all(home.join("profiles")).map_err(ctx("dir"))?;

        let p = plan(&home, &[UninstallScope::State]);
        let result = execute(&p, false);

        assert!(result.is_ok());
        assert!(!victim.exists(), "cache muss entfernt sein");
        assert!(home.join("profiles").exists(), "profiles bleibt erhalten");
        stdfs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn test_execute_missing_path_is_ok() -> TestResult {
        let home = temp_home("missing")?;
        stdfs::remove_dir_all(&home).ok();
        let plan = CleanupPlan {
            removals: vec![home.join("does-not-exist")],
            preserved: vec![],
        };
        assert!(execute(&plan, false).is_ok());
        Ok(())
    }
}
