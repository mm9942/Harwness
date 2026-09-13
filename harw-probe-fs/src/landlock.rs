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
//! zusätzlich erlaubt.
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
//! let _ = enforce_read_scope(&scope);
//! ```

use std::path::Path;

use harw_dod_cap::ReadScope;
use landlock::{
    ABI, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, RestrictionStatus,
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
///
/// # Returns
/// `Ok(())` ausschließlich bei `RulesetStatus::FullyEnforced`.
///
/// # Errors
/// - [`ProbeError::LandlockUnavailable`]: bei jedem Aufbaufehler
///   (`handle_access`, `create`, `add_rules`, `restrict_self`) und bei
///   jedem Ausgang außer `FullyEnforced`.
pub fn enforce_read_scope(scope: &ReadScope) -> Result<(), ProbeError> {
    let abi = ABI::V1;

    let ruleset = Ruleset::default().set_compatibility(CompatLevel::BestEffort);
    let ruleset = ruleset.handle_access(AccessFs::from_read(abi)).map_err(|error| {
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

    let rules: Vec<Result<PathBeneath<PathFd>, landlock::RulesetError>> =
        scope.roots().filter_map(|root| open_rule(root, abi)).collect();

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
/// (siehe Moduldoku, Abschnitt „Fail-closed"). Sonst `Some(Ok(rule))`.
fn open_rule(root: &Path, abi: ABI) -> Option<Result<PathBeneath<PathFd>, landlock::RulesetError>> {
    match PathFd::new(root) {
        Ok(fd) => Some(Ok(PathBeneath::new(fd, AccessFs::from_read(abi)))),
        Err(error) => {
            tracing::warn!(
                path = %root.display(),
                error = %error,
                "landlock path unavailable while building the read scope rule; this root will not be readable"
            );
            None
        }
    }
}

// Bewusst kein `#[cfg(test)] mod tests` in dieser Datei: jeder denkbare Test
// müsste entweder `enforce_read_scope` (echtes `restrict_self`) oder
// `open_rule` (das seinerseits `PathFd::new` aufruft) ausführen. Ein
// `PathFd::new`-Aufruf öffnet zwar nur einen gewöhnlichen Dateideskriptor und
// bindet keine Landlock-Regel, aber dieser Knoten hält sich an dieselbe,
// striktere Disziplin wie `harw-sentinel::sandbox` (dort ebenfalls kein Test,
// der irgendeine Funktion der `landlock`-Crate aufruft) — nach
// Aufgabenstellung ausdrücklich untersagt ("binde kein echtes Landlock"). Die
// Entscheidungslogik dieser Datei (Fail-closed bei nicht öffenbaren Wurzeln,
// „nur `FullyEnforced` zählt als Erfolg") ist in der Moduldoku begründet statt
// durch einen Test belegt, der einen Kernel-Zustand voraussetzen würde.
