//! Root-Space- und Profil-Pfadauflösung für `harw`.
//!
//! Spiegelt das codex-Muster (`utils/home-dir::find_codex_home`): eine
//! Env-Override-Variable, ein `~`-Default, und **kein eager `mkdir`** — das
//! Verzeichnis wird erst beim Scaffolding/Schreiben (`crate::scaffold`)
//! angelegt. Dieses Modul löst nur Pfade auf und liest höchstens die
//! `active_profile`-Zeigerdatei.
//!
//! # Env-Overrides
//! - `HARW_HOME`: absoluter Pfad des Root-Space (Default `~/.harw`). Muss,
//!   falls gesetzt, auf ein existierendes Verzeichnis zeigen.
//! - `HARW_PROFILE`: Name des aktiven Profils (überschreibt `active_profile`).
//!
//! # Verzeichnisse des Ausbauprogramms AW0–AW7
//! Mehrere Teilsysteme des Ausbauprogramms legen Dateien unterhalb des
//! Root-Space ab; ihre Pfade sind hier benannt, damit der Speicherort aus dem
//! Funktionsnamen hervorgeht statt aus fremdem Quellcode erschlossen werden
//! zu müssen. Wie alle Pfade in diesem Modul werden sie nur aufgelöst, nicht
//! angelegt:
//! - [`telemetry_dir`]: rotierender JSONL-Sink (Knoten AW0-01) plus
//!   `.blake3`-Beidateien je abgeschlossener Datei.
//! - [`freeze_dir`]: Datensätze des `FreezeStore` (Knoten AW5-05), Zustand im
//!   Dateisuffix wie bei `harw-session-store`s `ChildLeaseStore`.
//! - [`ring_snapshot_dir`]: eingefrorene Ringpuffer-Snapshots des Sentinels
//!   (Knoten AW2-18) als `SecurityEvidence`-Datensätze.
//! - [`scan_reports_dir`]: von `harw-dod-scanreport` (Knoten AW2-16) nur
//!   **gelesene** Berichte fremder Scanner.
//! - [`lens_store_dir`]: inhaltsadressierter Chunk- und Indexspeicher
//!   (Knoten AW3-06).
//! - [`visibility_index_dir`]: pro Sichtbarkeit ein getrennter physischer
//!   Index (kein Filter auf einem gemeinsamen Index).

use std::path::{Path, PathBuf};

use crate::error::{HomeError, HomeResult};

/// Env-Var, die den Root-Space überschreibt.
pub const HARW_HOME_ENV: &str = "HARW_HOME";
/// Env-Var, die das aktive Profil überschreibt.
pub const HARW_PROFILE_ENV: &str = "HARW_PROFILE";
/// Standard-Profilname, wenn keiner konfiguriert ist.
pub const DEFAULT_PROFILE: &str = "default";
/// Verzeichnisname des Root-Space unter `$HOME`.
pub const HOME_DIR_NAME: &str = ".harw";

/// Löst den Root-Space-Pfad auf, ohne ihn anzulegen.
///
/// # Returns
/// Den absoluten `~/.harw`-Pfad (oder den Wert von `HARW_HOME`).
///
/// # Errors
/// - [`HomeError::HomeNotADirectory`]: `HARW_HOME` ist gesetzt, zeigt aber auf
///   eine existierende Nicht-Verzeichnis-Datei.
/// - [`HomeError::NoHomeDirectory`]: `HARW_HOME` ist leer und `$HOME` (bzw. die
///   plattformübliche Home-Auflösung) liefert nichts.
pub fn home_dir() -> HomeResult<PathBuf> {
    match std::env::var_os(HARW_HOME_ENV).filter(|value| !value.is_empty()) {
        Some(raw) => {
            let path = PathBuf::from(raw);
            if path.exists() && !path.is_dir() {
                return Err(HomeError::HomeNotADirectory { path });
            }
            Ok(path)
        }
        None => {
            let base = user_home_directory().ok_or(HomeError::NoHomeDirectory)?;
            Ok(base.join(HOME_DIR_NAME))
        }
    }
}

/// Ermittelt den Namen des aktiven Profils.
///
/// Präzedenz: `HARW_PROFILE` → Inhalt der `active_profile`-Datei → `default`.
/// Diese Funktion liest höchstens eine kleine Textdatei und legt nichts an.
#[must_use]
pub fn active_profile_name(home: &Path) -> String {
    if let Some(name) = std::env::var(HARW_PROFILE_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        return name;
    }
    std::fs::read_to_string(active_profile_path(home))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_PROFILE.to_owned())
}

/// Pfad der `active_profile`-Zeigerdatei.
#[must_use]
pub fn active_profile_path(home: &Path) -> PathBuf {
    home.join("active_profile")
}

/// Pfad der `auth.toml` (chmod 600) auf Root-Ebene.
#[must_use]
pub fn auth_path(home: &Path) -> PathBuf {
    home.join("auth.toml")
}

/// Pfad der `installation_id`-Datei (stabile Identität).
#[must_use]
pub fn installation_id_path(home: &Path) -> PathBuf {
    home.join("installation_id")
}

/// Datei mit der persistenten Eingabe-Historie der TUI.
///
/// # Description
/// Eine Zeile je abgesendeter Eingabe, älteste zuerst. Zeilenumbrüche
/// innerhalb eines Eintrags sind escaped, damit ein mehrzeiliger Prompt eine
/// Zeile bleibt. Der Inhalt ist reiner Komfort und jederzeit löschbar.
///
/// # Arguments
/// - `home` (`&Path`): das harw-Home-Verzeichnis.
///
/// # Returns
/// Den Pfad `<home>/input_history`.
#[must_use]
pub fn input_history_path(home: &Path) -> PathBuf {
    home.join("input_history")
}

/// Regenerierbares Cache-Verzeichnis.
#[must_use]
pub fn cache_dir(home: &Path) -> PathBuf {
    home.join("cache")
}

/// Durable-Job-Verzeichnis (bestehendes `JobStore`-Layout).
#[must_use]
pub fn jobs_dir(home: &Path) -> PathBuf {
    home.join("jobs")
}

/// Verzeichnis der persistierten Plan-Graphen.
///
/// Root-bezogen (analog zu [`jobs_dir`]/[`cache_dir`], nicht profilbezogen):
/// Aufrufer, die einen profil-lokalen Plan-Space brauchen, reichen bereits
/// den per [`profile_dir`] aufgelösten Pfad als `home` hinein — genau wie es
/// `harw-cli` heute schon mit `jobs_dir`/`JobStore` handhabt.
#[must_use]
pub fn plans_dir(home: &Path) -> PathBuf {
    home.join("plans")
}

/// Verzeichnis der persistierten Goals (Desired State, überlebt
/// Plan-Revisionen).
///
/// Root-bezogen (analog zu [`jobs_dir`]/[`cache_dir`], nicht profilbezogen):
/// siehe [`plans_dir`] zur Begründung dieser Konvention.
#[must_use]
pub fn goals_dir(home: &Path) -> PathBuf {
    home.join("goals")
}

/// Verzeichnis des rotierenden Telemetrie-Sinks (Knoten AW0-01).
///
/// # Description
/// Der JSONL-Sink aus Knoten AW0-01 hängt hier rotierende Log-Dateien an und
/// legt für jede abgeschlossene Datei eine `.blake3`-Beidatei mit dem
/// Integritätshash der Datei ab. Alleiniger Schreiber ist der
/// Telemetrie-Sink; dieses Modul löst den Pfad nur auf und legt ihn nicht an
/// (siehe Modul-Doku).
///
/// # Returns
/// Den Pfad `home/telemetry`.
///
/// # Errors
/// Keine — reine Pfadauflösung, kein Dateisystemzugriff.
///
/// # Examples
/// ```rust
/// use std::path::PathBuf;
/// let home = PathBuf::from("/tmp/harw-test-home");
/// let telemetry = harw_home::paths::telemetry_dir(&home);
/// assert_eq!(telemetry, home.join("telemetry"));
/// ```
#[must_use]
pub fn telemetry_dir(home: &Path) -> PathBuf {
    home.join("telemetry")
}

/// Verzeichnis des `FreezeStore` (Knoten AW5-05).
///
/// # Description
/// Der `FreezeStore` aus Knoten AW5-05 legt hier einen Datensatz je Freeze
/// ab; der Zustand steht — wie bei `harw-session-store`s `ChildLeaseStore`
/// (`harw-session-store/src/child_lease.rs`) — im Dateisuffix
/// (`.active.json` für offene, `.resolved.json` für abgeschlossene Freezes).
/// Dieses Modul legt das Verzeichnis nicht an; das übernimmt der
/// `FreezeStore` beim ersten Schreiben.
///
/// # Returns
/// Den Pfad `home/freeze`.
///
/// # Errors
/// Keine — reine Pfadauflösung, kein Dateisystemzugriff.
///
/// # Examples
/// ```rust
/// use std::path::PathBuf;
/// let home = PathBuf::from("/tmp/harw-test-home");
/// let freeze = harw_home::paths::freeze_dir(&home);
/// assert_eq!(freeze, home.join("freeze"));
/// ```
#[must_use]
pub fn freeze_dir(home: &Path) -> PathBuf {
    home.join("freeze")
}

/// Verzeichnis der Ring-Snapshots des Sentinels (Knoten AW2-18).
///
/// # Description
/// Der Sentinel aus Knoten AW2-18 friert seinen Ringpuffer periodisch
/// hierhin ein und schreibt jeden Snapshot als `SecurityEvidence`-Datensatz.
/// Alleiniger Schreiber ist der Sentinel; dieses Modul legt das Verzeichnis
/// nicht an.
///
/// # Returns
/// Den Pfad `home/ring_snapshots`.
///
/// # Errors
/// Keine — reine Pfadauflösung, kein Dateisystemzugriff.
///
/// # Examples
/// ```rust
/// use std::path::PathBuf;
/// let home = PathBuf::from("/tmp/harw-test-home");
/// let snapshots = harw_home::paths::ring_snapshot_dir(&home);
/// assert_eq!(snapshots, home.join("ring_snapshots"));
/// ```
#[must_use]
pub fn ring_snapshot_dir(home: &Path) -> PathBuf {
    home.join("ring_snapshots")
}

/// Verzeichnis der Fremdscanner-Berichte (Knoten AW2-16, `harw-dod-scanreport`).
///
/// # Description
/// `harw-dod-scanreport` (Knoten AW2-16) liest hier Berichte fremder Scanner
/// ein. Dieses Verzeichnis wird von externen Werkzeugen befüllt — weder
/// `harw-dod-scanreport` noch dieses Crate schreiben hierher, beide sind
/// ausschließlich lesend. Genau deshalb bildet `harw-dod-scanreport` keine
/// eigene Kommandozeile zum Erzeugen dieser Berichte.
///
/// # Returns
/// Den Pfad `home/scan_reports`.
///
/// # Errors
/// Keine — reine Pfadauflösung, kein Dateisystemzugriff.
///
/// # Examples
/// ```rust
/// use std::path::PathBuf;
/// let home = PathBuf::from("/tmp/harw-test-home");
/// let reports = harw_home::paths::scan_reports_dir(&home);
/// assert_eq!(reports, home.join("scan_reports"));
/// ```
#[must_use]
pub fn scan_reports_dir(home: &Path) -> PathBuf {
    home.join("scan_reports")
}

/// Verzeichnis des Lens-Content-Stores (Knoten AW3-06).
///
/// # Description
/// Der inhaltsadressierte Chunk- und Indexspeicher aus Knoten AW3-06 legt
/// hier seine Chunks und Indexdateien ab. Alleiniger Schreiber ist der
/// Lens-Store; dieses Modul legt das Verzeichnis nicht an.
///
/// # Returns
/// Den Pfad `home/lens_store`.
///
/// # Errors
/// Keine — reine Pfadauflösung, kein Dateisystemzugriff.
///
/// # Examples
/// ```rust
/// use std::path::PathBuf;
/// let home = PathBuf::from("/tmp/harw-test-home");
/// let lens_store = harw_home::paths::lens_store_dir(&home);
/// assert_eq!(lens_store, home.join("lens_store"));
/// ```
#[must_use]
pub fn lens_store_dir(home: &Path) -> PathBuf {
    home.join("lens_store")
}

/// Verzeichnis des physischen Index einer Sichtbarkeit.
///
/// # Description
/// Sichtbarkeit wird in diesem Programm als getrennter physischer Index
/// umgesetzt, nicht als Filter auf einem gemeinsamen Index — der
/// Sichtbarkeitsname steht deshalb als eigene Pfadkomponente im Ergebnis.
/// Die Zeichenprüfung nutzt bewusst dieselbe Traversal-Schutz-Regel wie
/// [`profile_dir`] ([`is_valid_profile_name`]), statt eine zweite zu
/// schreiben: `visibility` darf keinen Pfadseparator und kein `..` enthalten.
///
/// # Returns
/// Den Pfad `home/index/<visibility>`.
///
/// # Errors
/// [`HomeError::InvalidVisibilityName`], wenn `visibility` Zeichen außerhalb
/// von `[A-Za-z0-9_-]` enthält (Traversal-Schutz).
///
/// # Examples
/// ```rust
/// use std::path::PathBuf;
/// let home = PathBuf::from("/tmp/harw-test-home");
/// let index = harw_home::paths::visibility_index_dir(&home, "internal").unwrap();
/// assert_eq!(index, home.join("index").join("internal"));
/// assert!(harw_home::paths::visibility_index_dir(&home, "../escape").is_err());
/// ```
pub fn visibility_index_dir(home: &Path, visibility: &str) -> HomeResult<PathBuf> {
    if !is_valid_profile_name(visibility) {
        return Err(HomeError::InvalidVisibilityName {
            name: visibility.to_owned(),
        });
    }
    Ok(home.join("index").join(visibility))
}

/// Verzeichnis eines benannten Profils.
///
/// # Errors
/// [`HomeError::InvalidProfileName`], wenn `name` Zeichen außerhalb von
/// `[A-Za-z0-9_-]` enthält (Traversal-Schutz).
pub fn profile_dir(home: &Path, name: &str) -> HomeResult<PathBuf> {
    if !is_valid_profile_name(name) {
        return Err(HomeError::InvalidProfileName {
            name: name.to_owned(),
        });
    }
    Ok(home.join("profiles").join(name))
}

/// Baut die Layer-Reihenfolge für [`harw_config::discover_config`] in
/// aufsteigender Präzedenz: globaler Root, aktives Profil, repo-lokales
/// `.harw` (falls im aktuellen Arbeitsverzeichnis vorhanden).
///
/// # Returns
/// Einen Vektor absoluter Pfade; nicht existente Layer überspringt
/// `discover_config` selbst.
///
/// # Errors
/// [`HomeError::InvalidProfileName`], wenn der aktive Profilname ungültig ist.
pub fn config_layers(home: &Path) -> HomeResult<Vec<PathBuf>> {
    let profile = active_profile_name(home);
    let mut layers = vec![home.to_path_buf(), profile_dir(home, &profile)?];
    let repo_local = PathBuf::from(HOME_DIR_NAME);
    if repo_local.is_dir() {
        // Nur hinzufügen, wenn der repo-lokale `.harw` NICHT dieselbe Location
        // wie der globale Home-Layer ist. Das passiert, wenn `harw` im
        // `$HOME`-Verzeichnis gestartet wird — der relative Pfad `.harw`
        // resolved dann zu `$HOME/.harw`, wird als weiterer Layer angefügt
        // und überschreibt das Profile (letzter Layer gewinnt).
        let repo_local_abs = std::fs::canonicalize(&repo_local).unwrap_or(repo_local.clone());
        let home_abs = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        if repo_local_abs != home_abs {
            layers.push(repo_local);
        }
    }
    Ok(layers)
}

/// Prüft, ob `name` ein sicherer Profil-Identifier ist.
#[must_use]
pub fn is_valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Plattformunabhängige Home-Verzeichnis-Auflösung ohne externe Crate.
///
/// Nutzt `$HOME` (Unix) bzw. `%USERPROFILE%`/`%HOMEDRIVE%%HOMEPATH%` (Windows).
fn user_home_directory() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    match (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
        (Some(drive), Some(path)) => {
            let mut combined = PathBuf::from(drive);
            combined.push(path);
            Some(combined)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_dir_ends_with_plans_component_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let plans = plans_dir(&home);
        assert!(plans.ends_with("plans"));
        assert!(plans.starts_with(&home));
        assert_eq!(plans, home.join("plans"));
    }

    #[test]
    fn goals_dir_ends_with_goals_component_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let goals = goals_dir(&home);
        assert!(goals.ends_with("goals"));
        assert!(goals.starts_with(&home));
        assert_eq!(goals, home.join("goals"));
    }

    #[test]
    fn plans_dir_and_goals_dir_are_deterministic_and_distinct() {
        let home = PathBuf::from("/tmp/harw-test-home");
        // Wiederholte Aufrufe müssen denselben Pfad liefern (keine
        // versteckte Zustandsabhängigkeit, kein Zufall).
        assert_eq!(plans_dir(&home), plans_dir(&home));
        assert_eq!(goals_dir(&home), goals_dir(&home));
        // Plans und Goals dürfen sich nicht denselben Speicherort teilen —
        // Goals überleben Plan-Revisionen und müssen unabhängig löschbar sein.
        assert_ne!(plans_dir(&home), goals_dir(&home));
    }

    #[test]
    fn plans_dir_and_goals_dir_are_root_relative_like_jobs_dir() {
        let home = PathBuf::from("/tmp/harw-test-home");
        // Gleiche Konvention wie `jobs_dir`/`cache_dir`: direkt unterhalb von
        // `home`, nicht unterhalb eines Profils.
        assert_eq!(plans_dir(&home).parent(), Some(home.as_path()));
        assert_eq!(goals_dir(&home).parent(), Some(home.as_path()));
    }

    #[test]
    fn telemetry_dir_ends_with_telemetry_component_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let telemetry = telemetry_dir(&home);
        assert!(telemetry.ends_with("telemetry"));
        assert!(telemetry.starts_with(&home));
        assert_eq!(telemetry, home.join("telemetry"));
    }

    #[test]
    fn freeze_dir_ends_with_freeze_component_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let freeze = freeze_dir(&home);
        assert!(freeze.ends_with("freeze"));
        assert!(freeze.starts_with(&home));
        assert_eq!(freeze, home.join("freeze"));
    }

    #[test]
    fn ring_snapshot_dir_ends_with_ring_snapshots_component_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let snapshots = ring_snapshot_dir(&home);
        assert!(snapshots.ends_with("ring_snapshots"));
        assert!(snapshots.starts_with(&home));
        assert_eq!(snapshots, home.join("ring_snapshots"));
    }

    #[test]
    fn scan_reports_dir_ends_with_scan_reports_component_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let reports = scan_reports_dir(&home);
        assert!(reports.ends_with("scan_reports"));
        assert!(reports.starts_with(&home));
        assert_eq!(reports, home.join("scan_reports"));
    }

    #[test]
    fn lens_store_dir_ends_with_lens_store_component_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let lens_store = lens_store_dir(&home);
        assert!(lens_store.ends_with("lens_store"));
        assert!(lens_store.starts_with(&home));
        assert_eq!(lens_store, home.join("lens_store"));
    }

    #[test]
    fn visibility_index_dir_places_visibility_name_below_index_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let index = visibility_index_dir(&home, "internal").unwrap();
        assert!(index.starts_with(&home));
        assert_eq!(index, home.join("index").join("internal"));
    }

    #[test]
    fn visibility_index_dir_rejects_path_separator() {
        let home = PathBuf::from("/tmp/harw-test-home");
        assert!(matches!(
            visibility_index_dir(&home, "a/b"),
            Err(HomeError::InvalidVisibilityName { .. })
        ));
        assert!(matches!(
            visibility_index_dir(&home, "public/../secret"),
            Err(HomeError::InvalidVisibilityName { .. })
        ));
    }

    #[test]
    fn visibility_index_dir_rejects_dot_dot_traversal() {
        let home = PathBuf::from("/tmp/harw-test-home");
        assert!(matches!(
            visibility_index_dir(&home, ".."),
            Err(HomeError::InvalidVisibilityName { .. })
        ));
    }

    #[test]
    fn aw_program_directories_are_pairwise_distinct() {
        let home = PathBuf::from("/tmp/harw-test-home");
        // Jedes der sechs Teilsysteme braucht einen eigenen Speicherort;
        // keine zwei dürfen auf denselben Pfad kollidieren.
        let dirs = [
            telemetry_dir(&home),
            freeze_dir(&home),
            ring_snapshot_dir(&home),
            scan_reports_dir(&home),
            lens_store_dir(&home),
            visibility_index_dir(&home, "internal").unwrap(),
        ];
        for (i, left) in dirs.iter().enumerate() {
            for (j, right) in dirs.iter().enumerate() {
                if i != j {
                    assert_ne!(left, right, "dirs at index {i} and {j} must differ");
                }
            }
        }
    }
}
