//! Landlock-Selbstbeschränkung beim Start — hart, nicht degradierend.
//!
//! # Die harte Entscheidung
//! `harw-sentinel` (Knoten AW2-19, `src/sandbox.rs`) **degradiert** bei
//! fehlender Landlock-Unterstützung nur: es läuft ohne die zusätzliche
//! Schranke weiter, weil sein Berechtigungsumfang von vornherein klein ist
//! und ein Sicherheitssammler, der gar nicht erst startet, jede Beobachtung
//! verliert, die er sonst geliefert hätte. Für **diese** Sonde gilt das
//! Gegenteil: sie läuft mit `CAP_SYS_ADMIN`, und Landlock ist hier nicht
//! eine von mehreren Verteidigungslinien, sondern die einzige, die den
//! deklarierten `ReadScope` gegen das tatsächliche Berechtigungsvermögen
//! des Prozesses durchsetzt. Ohne sie zu starten hieße, die Verteidigung
//! wegzulassen und trotzdem das Risiko (volle `CAP_SYS_ADMIN`-Rechte) zu
//! tragen. [`enforce_read_scope`] liefert deshalb `Err`, sobald der
//! erreichte Ausgang irgendetwas außer vollständiger Durchsetzung ist —
//! `main` behandelt das als harten Startfehler, kein Weiterlaufen.
//!
//! # Warum diese Sonde Landlock heute bauen kann, obwohl der ursprüngliche
//! Auftrag das Gegenteil vermutete
//! Der Arbeitsauftrag dieses Knotens ging davon aus, Landlock sei „ohne
//! `unsafe` nicht erreichbar" — dieselbe Vermutung wie bei fanotify. Das
//! stimmt für fanotify (siehe [`crate::source`]-Moduldoku), aber nicht mehr
//! für Landlock: Der gleichzeitig gelandete Knoten AW2-19
//! (`harw-sentinel/src/sandbox.rs`) hat bereits eine sichere, ohne
//! `unsafe` auskommende Landlock-Bindung in diesen Workspace eingebracht
//! (`landlock = "0.4.7"`, laut deren eigener Dokumentation eine „safe
//! abstraction for the Landlock system calls"). Diese Sonde prüft das
//! selbst nach, statt eine veraltete Vermutung fortzuschreiben, und
//! übernimmt dieselbe Abhängigkeit mit derselben, bereits im Workspace
//! begründeten Versionsangabe (siehe `Cargo.toml`-Kommentar). **Anders als
//! fanotify** ist Landlock damit kein „nicht gebaut, gemeldet"-Fall mehr,
//! sondern eine gebaute, geprüfte Funktion.
//!
//! # Ohne `unsafe`
//! Wie `harw-sentinel/src/sandbox.rs`: jeder hier aufgerufene Baustein der
//! `landlock`-Crate (`Ruleset`, `RulesetCreated`, `PathBeneath`, `PathFd`,
//! `.restrict_self()`) ist laut deren eigener Dokumentation eine sichere
//! Funktion. Diese Implementierung wurde gegen die auf
//! `docs.rs/landlock/0.4.7` nachgelesene API geschrieben, aber **nie gegen
//! einen echten Kernel oder Compiler ausgeführt** — dieser Knoten darf kein
//! `cargo` ausführen und kein echtes Landlock binden (zentrale,
//! sequenzielle Verifikation; Aufgabenstellung: „binde kein echtes
//! Landlock").
//!
//! # Nur `FullyEnforced` zählt als Erfolg
//! `landlock::RulesetStatus` kennt drei Ausgänge: `FullyEnforced`,
//! `PartiallyEnforced` (älterer Kernel, niedrigere ABI-Stufe — ein Teil der
//! Regeln greift) und `NotEnforced` (der Kernel unterstützt Landlock
//! überhaupt nicht). Anders als `harw-sentinel`, das `PartiallyEnforced`
//! als brauchbaren Teilschutz akzeptiert, behandelt diese Sonde jeden
//! Ausgang außer `FullyEnforced` — einschließlich `PartiallyEnforced` und
//! jedes Fehlers beim Aufbau der Regel — als „Landlock nicht verfügbar".
//! Für einen Prozess, dessen **einzige** Schranke Landlock ist, ist ein
//! teilweiser Schutz kein Schutz, dem man vertrauen sollte: welche der
//! angeforderten Regeln genau fehlt, ist von hier aus nicht unterscheidbar.
//!
//! # Fail-closed bei nicht öffenbaren Wurzeln
//! Lässt sich eine Wurzel aus `scope.roots()` nicht als `PathFd` öffnen
//! (fehlt, keine Berechtigung), wird sie aus der Regel ausgelassen und
//! geloggt, statt den gesamten Versuch abzubrechen — genau wie
//! `harw-sentinel::sandbox::open_rule`. Das ist die sichere Richtung: eine
//! ausgelassene Wurzel wird nach `restrict_self()` **unlesbar**, nie
//! zusätzlich erlaubt. Eine fehlende Wurzel ist bei den Vorgaben normal
//! (`/var/home` gibt es nur auf ostree-Hosts) und steht nur auf `debug`;
//! jeder andere Fehler steht auf `warn`, ohne Pfad (Journal-Hygiene, PL-100
//! WP0.6), der Pfad auf `debug`.
//!
//! # `/proc` lesend (PL-100 WP1.6, auid)
//! `harw_dod_fsmon::loginuid::resolve_loginuid` liest nach der
//! Selbstbeschränkung `/proc/<pid>/loginuid` — die einzige Quelle für
//! `Actor::auid`, die ein `sudo` nicht umschreibt. Ohne Leseregel für
//! `--proc-root` scheiterte jeder dieser Zugriffe still, und jedes Ereignis
//! trug `auid = None`. [`enforce_read_scope`] nimmt deshalb zusätzliche
//! Lesewurzeln entgegen (`main` übergibt `--proc-root`). Landlock gewährt
//! damit nur Lesen; fremde `/proc/<pid>/mem`/`environ` bleiben über die
//! ptrace-Prüfung des Kernels geschützt (die Unit gibt kein
//! `CAP_SYS_PTRACE`).
//!
//! # Datei-Wurzeln
//! Eine konfigurierte Wurzel darf eine Datei sein (z. B.
//! `/etc/ld.so.preload`). Für eine Datei nimmt die Regel nur die
//! Dateirechte von `AccessFs::from_read` (`ReadFile`); Verzeichnisrechte auf
//! einer Datei wiese der Kernel ab, und die `landlock`-Crate meldete dann
//! nur `PartiallyEnforced` — für diese Sonde ein Startabbruch.
//!
//! # Exportierte Typen
//! Keine — nur die Funktion [`enforce_read_scope`].
//!
//! # Nebenläufigkeit
//! [`enforce_read_scope`] muss vor jedem weiteren Thread aus dem
//! Hauptthread aufgerufen werden — eine Landlock-Regel gilt prozessweit für
//! alle danach entstehenden Threads.
//!
//! # Fehler
//! [`crate::error::ProbeError::LandlockUnavailable`], siehe oben.
//!
//! # Examples
//! ```rust,ignore
//! use crate::landlock::enforce_read_scope;
//! use harw_dod_cap::ReadScope;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
//! let _ = enforce_read_scope(&scope, &[PathBuf::from("/proc")]);
//! ```

use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};

use harw_dod_cap::ReadScope;
use landlock::{
    ABI, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, RestrictionStatus,
    Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};

use crate::error::ProbeError;

/// Erzwingt den übergebenen Lesebereich über Landlock, bevor die Sonde ein
/// einziges Ereignis liest.
///
/// # Description
/// Baut ein `landlock::Ruleset` mit `CompatLevel::BestEffort` (damit ein
/// älterer Kernel `PartiallyEnforced` statt eines harten `Err` aus der
/// `landlock`-Crate selbst liefert — diese Funktion entscheidet danach
/// selbst, siehe Moduldoku, dass auch das nicht genügt), nimmt für jede
/// erreichbare Wurzel aus `scope.roots()` eine lesende `PathBeneath`-Regel
/// unter [`landlock::ABI::V1`] auf und ruft `restrict_self()` auf. Nur
/// [`RulesetStatus::FullyEnforced`] gilt als Erfolg.
///
/// # Arguments
/// - `scope` (`&harw_dod_cap::ReadScope`): der Bereich, auf den sich diese
///   Sonde beschränken soll.
/// - `extra_read_roots` (`&[PathBuf]`): weitere nur lesbare Wurzeln, die
///   kein Teil des gemeldeten Bereichs sind (`--proc-root` für die
///   loginuid, siehe Moduldoku).
///
/// # Returns
/// `Ok(())` ausschließlich bei `RulesetStatus::FullyEnforced`.
///
/// # Errors
/// - [`ProbeError::LandlockUnavailable`]: bei jedem Aufbaufehler
///   (`handle_access`, `create`, `add_rules`, `restrict_self`) und bei
///   jedem Ausgang außer `FullyEnforced`.
pub fn enforce_read_scope(
    scope: &ReadScope,
    extra_read_roots: &[PathBuf],
) -> Result<(), ProbeError> {
    let abi = ABI::V1;

    let ruleset = Ruleset::default().set_compatibility(CompatLevel::BestEffort);
    let ruleset = ruleset
        .handle_access(AccessFs::from_read(abi))
        .map_err(|error| {
            tracing::error!(error = %error, "landlock handle_access failed; refusing to start");
            ProbeError::LandlockUnavailable
        })?;

    let created = ruleset
        .create()
        .map_err(|error| {
            tracing::error!(error = %error, "landlock ruleset create failed; refusing to start");
            ProbeError::LandlockUnavailable
        })?
        .set_compatibility(CompatLevel::BestEffort);

    let rules: Vec<Result<PathBeneath<PathFd>, landlock::RulesetError>> = scope
        .roots()
        .chain(extra_read_roots.iter().map(PathBuf::as_path))
        .filter_map(|root| open_rule(root, abi))
        .collect();

    let created = created.add_rules(rules).map_err(|error| {
        tracing::error!(error = %error, "landlock add_rules failed; refusing to start");
        ProbeError::LandlockUnavailable
    })?;

    let status: RestrictionStatus = created.restrict_self().map_err(|error| {
        tracing::error!(error = %error, "landlock restrict_self failed; refusing to start");
        ProbeError::LandlockUnavailable
    })?;

    if status.ruleset == RulesetStatus::FullyEnforced {
        tracing::info!("landlock read scope fully enforced");
        Ok(())
    } else {
        tracing::error!(
            status = ?status.ruleset,
            "landlock did not fully enforce the read scope; refusing to start"
        );
        Err(ProbeError::LandlockUnavailable)
    }
}

/// Baut, falls möglich, eine lesende `PathBeneath`-Regel für eine einzelne
/// Wurzel.
///
/// # Returns
/// `None`, wenn der Pfad nicht als `PathFd` geöffnet werden kann (fehlt,
/// keine Berechtigung) — geloggt, aber kein Abbruch des gesamten Aufbaus
/// (siehe Moduldoku, Abschnitt „Fail-closed"). Sonst `Some(Ok(rule))` mit
/// [`read_access_for`].
fn open_rule(root: &Path, abi: ABI) -> Option<Result<PathBeneath<PathFd>, landlock::RulesetError>> {
    match PathFd::new(root) {
        Ok(fd) => {
            let access = read_access_for(&fd, abi);
            Some(Ok(PathBeneath::new(fd, access)))
        }
        Err(error) => {
            if matches!(root.try_exists(), Ok(false)) {
                tracing::debug!(
                    path = %root.display(),
                    "landlock root absent on this host; skipped"
                );
            } else {
                // Journal-Hygiene: kein Pfad auf `warn` (die Fehlermeldung
                // der Crate nennt ihn), beides auf `debug`.
                tracing::warn!("a landlock read root could not be opened; it will not be readable");
                tracing::debug!(
                    path = %root.display(),
                    error = %error,
                    "landlock read root could not be opened"
                );
            }
            None
        }
    }
}

/// Die Leserechte einer Regel: `AccessFs::from_read` für ein Verzeichnis,
/// davon nur die Dateirechte für eine Datei (siehe Moduldoku, „Datei-Wurzeln").
/// Ist der Typ nicht feststellbar, bleibt es bei `from_read` (dann meldet die
/// Crate höchstens `PartiallyEnforced`, und die Sonde startet nicht — die
/// sichere Richtung).
fn read_access_for(fd: &PathFd, abi: ABI) -> BitFlags<AccessFs> {
    let is_file = fd
        .as_fd()
        .try_clone_to_owned()
        .ok()
        .map(std::fs::File::from)
        .and_then(|file| file.metadata().ok())
        .is_some_and(|meta| !meta.is_dir());
    if is_file {
        AccessFs::from_read(abi) & AccessFs::from_file(abi)
    } else {
        AccessFs::from_read(abi)
    }
}

// Kein Test ruft `enforce_read_scope` auf (echtes `restrict_self`; nach
// Aufgabenstellung untersagt: "binde kein echtes Landlock"). Geprüft ist nur
// die reine Rechtewahl `read_access_for`: `PathFd::new` öffnet dafür einen
// gewöhnlichen `O_PATH`-Deskriptor und bindet keine Regel (dieselbe Grenze
// wie `harw-sentinel::sandbox::prepare_roots` in dessen Tests). Die übrige
// Entscheidungslogik (Fail-closed bei nicht öffenbaren Wurzeln, „nur
// `FullyEnforced` zählt als Erfolg") ist in der Moduldoku begründet.
#[cfg(test)]
mod tests {
    use landlock::{ABI, AccessFs, PathFd};

    use super::read_access_for;
    use crate::test_support::{TestResult, ctx};

    /// PL-100 WP1.6: eine Datei-Wurzel bekommt nur Dateirechte, ein
    /// Verzeichnis alle Leserechte.
    #[test]
    fn test_read_access_for_a_file_root_keeps_only_file_rights() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let file = dir.path().join("ld.so.preload");
        std::fs::write(&file, b"").map_err(ctx("write file"))?;

        let dir_fd = PathFd::new(dir.path()).map_err(ctx("dir fd"))?;
        assert_eq!(
            read_access_for(&dir_fd, ABI::V1),
            AccessFs::from_read(ABI::V1)
        );

        let file_fd = PathFd::new(&file).map_err(ctx("file fd"))?;
        let file_access = read_access_for(&file_fd, ABI::V1);
        assert_eq!(
            file_access,
            AccessFs::from_read(ABI::V1) & AccessFs::from_file(ABI::V1)
        );
        assert!(file_access.contains(AccessFs::ReadFile));
        assert!(!file_access.contains(AccessFs::ReadDir));
        Ok(())
    }
}
