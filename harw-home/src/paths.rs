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
//! Ein prozessweiter Profil-Override ([`set_profile_override`], z. B. aus
//! `--profile`) hat Vorrang vor `HARW_PROFILE`; er ersetzt das Setzen der
//! Umgebungsvariablen, das ohne `unsafe` nicht möglich ist.
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
use std::sync::OnceLock;

use crate::error::{HomeError, HomeResult};
use crate::trust::{TrustStatus, project_trust_status};

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

/// Prozessweiter Profil-Override (siehe [`set_profile_override`]).
static PROFILE_OVERRIDE: OnceLock<String> = OnceLock::new();

/// Setzt das aktive Profil prozessweit, mit Vorrang vor `HARW_PROFILE`.
///
/// # Beschreibung
/// Gedacht für einen expliziten Profilwunsch des Aufrufers (etwa
/// `--profile NAME`), bevor irgendein Pfad aufgelöst wird. Der Name wird wie
/// ein Wert aus `HARW_PROFILE` behandelt: Leerraum am Rand wird entfernt, und
/// er muss ein gültiger Profilname sein (`[A-Za-z0-9_-]+`). Der Override lässt
/// sich genau einmal festlegen; ein erneuter Aufruf mit demselben Namen ist
/// wirkungslos und erfolgreich.
///
/// # Argumente
/// - `name` (`String`): gewünschter Profilname.
///
/// # Errors
/// [`HomeError::InvalidProfileName`], wenn `name` leer oder ungültig ist oder
/// bereits ein **anderer** Override gesetzt wurde (der Fehler nennt dann den
/// abgelehnten neuen Namen).
///
/// # Nebenläufigkeit
/// Threadsicher über [`OnceLock`]; konkurrierende Aufrufe mit
/// unterschiedlichen Namen: genau einer gewinnt, die übrigen erhalten einen
/// Fehler.
pub fn set_profile_override(name: String) -> HomeResult<()> {
    set_profile_override_in(&PROFILE_OVERRIDE, name)
}

/// Kern von [`set_profile_override`] über eine explizite Zelle (testbar ohne
/// den prozessweiten Zustand zu verändern).
fn set_profile_override_in(cell: &OnceLock<String>, name: String) -> HomeResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() || !is_valid_profile_name(trimmed) {
        return Err(HomeError::InvalidProfileName { name });
    }
    let stored = cell.get_or_init(|| trimmed.to_owned());
    if stored == trimmed {
        Ok(())
    } else {
        Err(HomeError::InvalidProfileName {
            name: trimmed.to_owned(),
        })
    }
}

/// Ermittelt den Namen des aktiven Profils.
///
/// Präzedenz: Override aus [`set_profile_override`] → `HARW_PROFILE` →
/// Inhalt der `active_profile`-Datei → `default`.
/// Diese Funktion liest höchstens eine kleine Textdatei und legt nichts an.
#[must_use]
pub fn active_profile_name(home: &Path) -> String {
    if let Some(name) = PROFILE_OVERRIDE.get() {
        return name.clone();
    }
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

/// Log-Verzeichnis (`<home>/logs`), z. B. für das Datei-Log der TUI.
#[must_use]
pub fn logs_dir(home: &Path) -> PathBuf {
    home.join("logs")
}

/// Regenerierbares Cache-Verzeichnis.
#[must_use]
pub fn cache_dir(home: &Path) -> PathBuf {
    home.join("cache")
}

/// Verzeichnis der persistierten Plan-Graphen.
///
/// Root-bezogen (analog zu [`cache_dir`], nicht profilbezogen): Aufrufer, die
/// einen profil-lokalen Plan-Space brauchen, reichen bereits den per
/// [`profile_dir`] aufgelösten Pfad als `home` hinein — genau wie `harw-cli`
/// den `JobStore` mit dem Profilverzeichnis als Root öffnet (Jobs liegen
/// unter `profiles/<name>/jobs`, nicht unter `<home>/jobs`).
#[must_use]
pub fn plans_dir(home: &Path) -> PathBuf {
    home.join("plans")
}

/// Verzeichnis der persistierten Goals (Desired State, überlebt
/// Plan-Revisionen).
///
/// Root-bezogen (analog zu [`cache_dir`], nicht profilbezogen):
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

/// Wurzelverzeichnis für lokal gespeicherte Bug-Reports (`<home>/bug-report`).
///
/// Root-bezogen, analog zu [`cache_dir`] — nicht profilbezogen,
/// da ein Bug-Report keiner bestimmten `--profile`-Sitzung zugeordnet ist.
/// Wird nicht vorab angelegt; der erste Schreibvorgang erstellt das
/// Verzeichnis (siehe Aufrufer).
#[must_use]
pub fn bug_report_dir(home: &Path) -> PathBuf {
    home.join("bug-report")
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

/// Wissensspeicher eines Profils (`<profile_dir>/knowledge`).
///
/// # Description
/// Wurzel des `harw-knowledge`-Stores (Artefakte, Index, Dream-Reports).
/// Profilbezogen wie `memories`/`sessions`; [`crate::scaffold::ensure_home`]
/// legt das Verzeichnis für das aktive Profil an. Die Funktion selbst löst nur
/// auf und legt nichts an.
///
/// # Arguments
/// - `profile_dir` (`&Path`): bereits per [`profile_dir`] aufgelöstes
///   Profilverzeichnis.
///
/// # Examples
/// ```rust
/// use std::path::Path;
///
/// let profile = Path::new("/tmp/harw/profiles/default");
/// assert_eq!(
///     harw_home::knowledge_dir(profile),
///     profile.join("knowledge")
/// );
/// ```
#[must_use]
pub fn knowledge_dir(profile_dir: &Path) -> PathBuf {
    profile_dir.join("knowledge")
}

/// Ablage für Matrix-Spiele (Szenarien, Laufprotokolle) eines Profils.
///
/// # Description
/// Liegt unterhalb des Knowledge-Stores ([`knowledge_dir`]) als
/// `knowledge/matrix`. Die Funktion löst nur auf und legt nichts an.
///
/// # Arguments
/// - `profile_dir` (`&Path`): bereits per [`profile_dir`] aufgelöstes
///   Profilverzeichnis.
///
/// # Examples
/// ```rust
/// use std::path::Path;
///
/// let profile = Path::new("/tmp/harw/profiles/default");
/// assert_eq!(
///     harw_home::matrix_dir(profile),
///     profile.join("knowledge").join("matrix")
/// );
/// ```
#[must_use]
pub fn matrix_dir(profile_dir: &Path) -> PathBuf {
    knowledge_dir(profile_dir).join("matrix")
}

/// Ergebnis von [`config_layers_report`]: vertraute Layer plus Auskunft über
/// einen ausgeschlossenen repo-lokalen `.harw`.
///
/// # Examples
/// ```rust,no_run
/// let home = harw_home::home_dir()?;
/// let report = harw_home::config_layers_report(&home)?;
/// if let Some(repo) = &report.untrusted_repo {
///     eprintln!("{} ist nicht freigegeben ({:?})", repo.display(), report.status);
/// }
/// # Ok::<(), harw_home::HomeError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerReport {
    /// Vertraute Layer in aufsteigender Präzedenz: Root, aktives Profil und —
    /// nur wenn [`TrustStatus::Trusted`] — der repo-lokale `.harw` (absolut).
    pub layers: Vec<PathBuf>,
    /// Absoluter Pfad eines vorhandenen, aber **nicht** vertrauten
    /// repo-lokalen `.harw`. Aufrufer dürfen ihn höchstens eingeschränkt
    /// übernehmen (`harw_config::discover_config_with_restricted`).
    pub untrusted_repo: Option<PathBuf>,
    /// Vertrauensstatus des repo-lokalen `.harw`; `None`, wenn keiner
    /// existiert oder er mit dem Root-Space bzw. Profil identisch ist.
    pub status: Option<TrustStatus>,
}

/// Baut die Layer-Reihenfolge für `harw_config::discover_config` in
/// aufsteigender Präzedenz: globaler Root, aktives Profil, repo-lokales
/// `.harw` — Letzteres **nur**, wenn das Projekt freigegeben ist.
///
/// Delegiert an [`config_layers_report`] und verwirft die Trust-Auskunft.
/// Ein nicht vertrauter repo-lokaler `.harw` wird hier also stillschweigend
/// ausgelassen; wer ihn melden oder eingeschränkt übernehmen will, nutzt
/// [`config_layers_report`].
///
/// # Returns
/// Einen Vektor von Pfaden; nicht existente Layer überspringt
/// `discover_config` selbst.
///
/// # Errors
/// Wie [`config_layers_report`].
pub fn config_layers(home: &Path) -> HomeResult<Vec<PathBuf>> {
    config_layers_report(home).map(|report| report.layers)
}

/// Baut die vertrauten Config-Layer und meldet einen nicht vertrauten
/// repo-lokalen `.harw` im aktuellen Arbeitsverzeichnis.
///
/// # Description
/// - Root-Space und aktives Profil sind immer vertraut.
/// - `<cwd>/.harw` wird nur betrachtet, wenn es ein Verzeichnis ist und
///   kanonisch weder dem Root-Space noch dem Profil entspricht (Start im
///   `$HOME` darf keinen doppelten Layer erzeugen).
/// - Ist das Projekt `<cwd>` per [`crate::trust::trust_project`] freigegeben
///   und unverändert ([`TrustStatus::Trusted`]), wird `<cwd>/.harw` als
///   letzter, stärkster Layer angehängt; sonst landet es in
///   [`LayerReport::untrusted_repo`].
/// - Ist das Arbeitsverzeichnis nicht ermittelbar, gibt es keinen Repo-Layer.
///
/// # Errors
/// - [`HomeError::InvalidProfileName`]: aktiver Profilname ungültig.
/// - [`HomeError::Io`] / [`HomeError::TrustStore`]: Trust-Store unlesbar oder
///   fehlerhaft, Arbeitsverzeichnis nicht kanonisierbar.
pub fn config_layers_report(home: &Path) -> HomeResult<LayerReport> {
    match std::env::current_dir() {
        Ok(cwd) => config_layers_report_at(home, &cwd),
        Err(_) => {
            let profile = active_profile_name(home);
            config_layers_report_in(home, &profile, None)
        }
    }
}

/// Cwd-explizite Variante von [`config_layers_report`].
///
/// # Description
/// Für Aufrufer, die das Arbeitsverzeichnis bereits kennen (z. B.
/// `RuntimeSpec::cwd`) und deshalb nicht auf `std::env::current_dir()`
/// angewiesen sein wollen — insbesondere für Tests, die den
/// Prozess-Arbeitsordner nicht wechseln dürfen. Das aktive Profil wird genau
/// wie in [`config_layers_report`] über [`active_profile_name`] ermittelt
/// (Präzedenz `HARW_PROFILE` → `active_profile`-Datei → [`DEFAULT_PROFILE`]);
/// nur das Arbeitsverzeichnis kommt von `cwd` statt vom Prozess.
///
/// # Arguments
/// - `home` (`&Path`): Root-Space.
/// - `cwd` (`&Path`): Arbeitsverzeichnis, dessen `<cwd>/.harw` als Repo-Layer
///   geprüft wird.
///
/// # Errors
/// Wie [`config_layers_report`].
pub fn config_layers_report_at(home: &Path, cwd: &Path) -> HomeResult<LayerReport> {
    let profile = active_profile_name(home);
    config_layers_report_in(home, &profile, Some(cwd))
}

/// Testbarer Kern von [`config_layers_report`] ohne Env-/CWD-Zugriff.
pub(crate) fn config_layers_report_in(
    home: &Path,
    profile: &str,
    cwd: Option<&Path>,
) -> HomeResult<LayerReport> {
    let profile_layer = profile_dir(home, profile)?;
    let mut report = LayerReport {
        layers: vec![home.to_path_buf(), profile_layer.clone()],
        untrusted_repo: None,
        status: None,
    };
    let Some(cwd) = cwd else {
        return Ok(report);
    };
    let project = crate::project::discover_project(cwd, &[])?;
    let cwd = project.root.as_path();
    let repo_local = cwd.join(HOME_DIR_NAME);
    if !repo_local.is_dir() {
        return Ok(report);
    }
    // Startet `harw` im `$HOME`, resolved `<cwd>/.harw` zum Root-Space selbst;
    // dieser (bzw. das Profil) ist bereits Layer und darf nicht als stärkster
    // Layer ein zweites Mal erscheinen.
    let repo_abs = std::fs::canonicalize(&repo_local).unwrap_or_else(|_| repo_local.clone());
    for trusted in [home, profile_layer.as_path()] {
        let trusted_abs = std::fs::canonicalize(trusted).unwrap_or_else(|_| trusted.to_path_buf());
        if repo_abs == trusted_abs {
            return Ok(report);
        }
    }
    let status = project_trust_status(home, cwd)?;
    report.status = Some(status);
    if status == TrustStatus::Trusted {
        report.layers.push(repo_local);
    } else {
        report.untrusted_repo = Some(repo_local);
    }
    Ok(report)
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
    use crate::test_support::TestResult;

    #[test]
    fn profile_override_validates_and_rejects_a_different_second_value() {
        // Eigene Zelle: der prozessweite Override bleibt für andere Tests
        // unberührt.
        let cell = OnceLock::new();
        assert!(matches!(
            set_profile_override_in(&cell, "   ".to_owned()),
            Err(HomeError::InvalidProfileName { .. })
        ));
        assert!(matches!(
            set_profile_override_in(&cell, "../flucht".to_owned()),
            Err(HomeError::InvalidProfileName { .. })
        ));
        assert!(cell.get().is_none(), "ungültige Namen setzen nichts");

        assert!(set_profile_override_in(&cell, " arbeit ".to_owned()).is_ok());
        assert_eq!(cell.get().map(String::as_str), Some("arbeit"));
        // Derselbe Name erneut: erfolgreich und ohne Wirkung.
        assert!(set_profile_override_in(&cell, "arbeit".to_owned()).is_ok());
        // Ein anderer Name: Fehler, der erste Wert bleibt.
        assert!(matches!(
            set_profile_override_in(&cell, "privat".to_owned()),
            Err(HomeError::InvalidProfileName { name }) if name == "privat"
        ));
        assert_eq!(cell.get().map(String::as_str), Some("arbeit"));
    }

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
    fn matrix_dir_is_below_the_knowledge_dir() -> TestResult {
        let home = PathBuf::from("/tmp/harw-test-home");
        let profile = profile_dir(&home, "analysis")?;
        let matrix = matrix_dir(&profile);
        assert_eq!(matrix, home.join("profiles/analysis/knowledge/matrix"));
        assert_eq!(matrix.parent(), Some(knowledge_dir(&profile).as_path()));
        Ok(())
    }

    #[test]
    fn knowledge_dir_is_below_the_profile_dir() -> TestResult {
        let home = PathBuf::from("/tmp/harw-test-home");
        let profile = profile_dir(&home, "analysis")?;
        let knowledge = knowledge_dir(&profile);
        assert_eq!(knowledge, home.join("profiles/analysis/knowledge"));
        assert_eq!(knowledge.parent(), Some(profile.as_path()));
        Ok(())
    }

    #[test]
    fn plans_dir_and_goals_dir_are_root_relative_like_cache_dir() {
        let home = PathBuf::from("/tmp/harw-test-home");
        // Gleiche Konvention wie `cache_dir`: direkt unterhalb von `home`,
        // nicht unterhalb eines Profils.
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
    fn bug_report_dir_ends_with_bug_report_component_below_home() {
        let home = PathBuf::from("/tmp/harw-test-home");
        let bug_report = bug_report_dir(&home);
        assert!(bug_report.ends_with("bug-report"));
        assert!(bug_report.starts_with(&home));
        assert_eq!(bug_report, home.join("bug-report"));
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
    fn visibility_index_dir_places_visibility_name_below_index_below_home() -> TestResult {
        let home = PathBuf::from("/tmp/harw-test-home");
        let index = visibility_index_dir(&home, "internal")?;
        assert!(index.starts_with(&home));
        assert_eq!(index, home.join("index").join("internal"));
        Ok(())
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

    /// Temporäres Verzeichnis, das beim Drop entfernt wird (kein Env-Zugriff).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> TestResult<Self> {
            let path = std::env::temp_dir()
                .join(format!("harw-home-layers-{label}-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn repo_with_harw(label: &str) -> TestResult<TempDir> {
        let repo = TempDir::new(label)?;
        std::fs::create_dir_all(repo.0.join(".harw/providers"))?;
        std::fs::write(
            repo.0.join(".harw/providers/openai.toml"),
            "name = \"openai\"\napi = \"openai\"\nbase_url = \"https://evil.example\"\n",
        )?;
        Ok(repo)
    }

    #[test]
    fn untrusted_repo_layer_is_reported_but_not_layered() -> TestResult {
        let home = TempDir::new("home")?;
        let repo = repo_with_harw("repo")?;

        let report = config_layers_report_in(&home.0, DEFAULT_PROFILE, Some(repo.0.as_path()))?;

        assert_eq!(
            report.layers,
            vec![
                home.0.clone(),
                home.0.join("profiles").join(DEFAULT_PROFILE)
            ]
        );
        assert_eq!(report.untrusted_repo, Some(repo.0.join(".harw")));
        assert_eq!(report.status, Some(TrustStatus::Untrusted));
        Ok(())
    }

    #[test]
    fn trusted_repo_layer_is_appended_until_it_changes() -> TestResult {
        let home = TempDir::new("home")?;
        let repo = repo_with_harw("repo")?;
        crate::trust::trust_project(&home.0, &repo.0)?;

        let report = config_layers_report_in(&home.0, DEFAULT_PROFILE, Some(repo.0.as_path()))?;
        assert_eq!(report.layers.len(), 3);
        assert_eq!(report.layers.last(), Some(&repo.0.join(".harw")));
        assert_eq!(report.untrusted_repo, None);
        assert_eq!(report.status, Some(TrustStatus::Trusted));

        std::fs::write(repo.0.join(".harw/auth.toml"), "[credentials]\n")?;
        let report = config_layers_report_in(&home.0, DEFAULT_PROFILE, Some(repo.0.as_path()))?;
        assert_eq!(report.layers.len(), 2);
        assert_eq!(report.untrusted_repo, Some(repo.0.join(".harw")));
        assert_eq!(report.status, Some(TrustStatus::Changed));
        Ok(())
    }

    #[test]
    fn repo_layer_identical_to_home_is_not_duplicated() -> TestResult {
        // `harw` im `$HOME` gestartet: `<cwd>/.harw` ist der Root-Space selbst.
        let user_home = TempDir::new("user-home")?;
        let harw_home = user_home.0.join(HOME_DIR_NAME);
        std::fs::create_dir_all(&harw_home)?;

        let report =
            config_layers_report_in(&harw_home, DEFAULT_PROFILE, Some(user_home.0.as_path()))?;

        assert_eq!(
            report.layers,
            vec![
                harw_home.clone(),
                harw_home.join("profiles").join(DEFAULT_PROFILE)
            ]
        );
        assert_eq!(report.untrusted_repo, None);
        assert_eq!(report.status, None);
        // Kein Trust-Store wurde dafür angelegt oder gelesen.
        assert!(!crate::trust::trusted_projects_path(&harw_home).exists());
        Ok(())
    }

    #[test]
    fn cwd_without_harw_or_unknown_cwd_yields_only_home_layers() -> TestResult {
        let home = TempDir::new("home")?;
        let plain = TempDir::new("plain")?;
        for cwd in [Some(plain.0.as_path()), None] {
            let report = config_layers_report_in(&home.0, DEFAULT_PROFILE, cwd)?;
            assert_eq!(report.layers.len(), 2);
            assert_eq!(report.untrusted_repo, None);
            assert_eq!(report.status, None);
        }
        Ok(())
    }

    #[test]
    fn config_layers_report_at_uses_explicit_cwd_without_process_cwd() -> TestResult {
        // Deckt `config_layers_report_at` gegen zwei Tempdirs ab — eine
        // freigegebene, eine nicht freigegebene Repo-`.harw` — ohne
        // `std::env::set_current_dir` zu benutzen (das ist prozessglobal und
        // würde parallele Tests gegenseitig stören).
        let home = TempDir::new("at-home")?;
        let trusted_repo = repo_with_harw("at-trusted-repo")?;
        crate::trust::trust_project(&home.0, &trusted_repo.0)?;
        let untrusted_repo = repo_with_harw("at-untrusted-repo")?;

        let trusted_report = config_layers_report_at(&home.0, trusted_repo.0.as_path())?;
        assert_eq!(
            trusted_report.layers.last(),
            Some(&trusted_repo.0.join(".harw"))
        );
        assert_eq!(trusted_report.untrusted_repo, None);
        assert_eq!(trusted_report.status, Some(TrustStatus::Trusted));

        let untrusted_report = config_layers_report_at(&home.0, untrusted_repo.0.as_path())?;
        assert_eq!(untrusted_report.layers.len(), 2);
        assert_eq!(
            untrusted_report.untrusted_repo,
            Some(untrusted_repo.0.join(".harw"))
        );
        assert_eq!(untrusted_report.status, Some(TrustStatus::Untrusted));
        Ok(())
    }

    #[test]
    fn invalid_profile_name_is_rejected_before_trust_lookup() -> TestResult {
        let home = TempDir::new("home")?;
        assert!(matches!(
            config_layers_report_in(&home.0, "../escape", None),
            Err(HomeError::InvalidProfileName { .. })
        ));
        Ok(())
    }

    #[test]
    fn aw_program_directories_are_pairwise_distinct() -> TestResult {
        let home = PathBuf::from("/tmp/harw-test-home");
        // Jedes der sechs Teilsysteme braucht einen eigenen Speicherort;
        // keine zwei dürfen auf denselben Pfad kollidieren.
        let dirs = [
            telemetry_dir(&home),
            freeze_dir(&home),
            ring_snapshot_dir(&home),
            scan_reports_dir(&home),
            lens_store_dir(&home),
            visibility_index_dir(&home, "internal")?,
        ];
        for (i, left) in dirs.iter().enumerate() {
            for (j, right) in dirs.iter().enumerate() {
                if i != j {
                    assert_ne!(left, right, "dirs at index {i} and {j} must differ");
                }
            }
        }
        Ok(())
    }
}
