//! Landlock-Selbstbeschränkung beim Start — hart, nicht degradierend.
//!
//! # Die harte Entscheidung
//! `harw-sentinel` (`src/sandbox.rs`) **degradiert** bei fehlender
//! Landlock-Unterstützung nur: es läuft ohne die zusätzliche Schranke
//! weiter, weil sein Berechtigungsumfang von vornherein klein ist und ein
//! Sicherheitssammler, der gar nicht erst startet, jede Beobachtung
//! verliert, die er sonst geliefert hätte. Für `harw-warden` gilt das
//! Gegenteil, und das ist Entscheidung Nr. 4 des Plans: dieser Prozess ist
//! **privilegiert** — er schreibt in cgroup-v2-Kontrolldateien, um
//! Prozessbäume einzufrieren und zu beenden. Für einen privilegierten
//! Prozess ist eine nicht durchsetzbare Selbstbeschränkung ein **harter
//! Startfehler**, kein akzeptabler Teilbetrieb: ein Warden, der ohne
//! Landlock liefe, hätte vollen Dateisystemzugriff, obwohl er nachweislich
//! nur `/sys/fs/cgroup` braucht — die Selbstbeschränkung ist hier nicht
//! eine von mehreren Verteidigungslinien, sondern die einzige, die den
//! deklarierten Zugriff gegen das tatsächliche Berechtigungsvermögen des
//! Prozesses durchsetzt. Diese Datei folgt deshalb `harw-probe-fs`s
//! Muster (dort aus demselben Grund: ein Prozess mit `CAP_SYS_ADMIN`), nicht
//! `harw-sentinel`s.
//!
//! # Lese- **und** Schreibzugriff, anders als `harw-probe-fs`
//! `harw-probe-fs` erzwingt nur `AccessFs::from_read` — es beobachtet nur.
//! `harw-warden` **schreibt** (`cgroup.freeze`, `cgroup.kill`, siehe
//! `harw_dod_warden::executor::CgroupV2Executor`) — die Regel muss deshalb
//! `AccessFs::from_read(abi) | AccessFs::from_write(abi)` verlangen, sonst
//! scheitert jede echte Ausführung nach einem sonst erfolgreichen Start an
//! einem von Landlock selbst verweigerten `write()`.
//!
//! # Ein Wurzelverzeichnis, kein optionaler Satz
//! `harw-probe-fs::landlock::enforce_read_scope` lässt eine nicht
//! öffenbare Wurzel aus mehreren aus (fail-closed: diese eine Wurzel wird
//! danach unlesbar, der Start läuft trotzdem weiter) — dort ist das
//! sicher, weil andere Wurzeln aus demselben `ReadScope` noch funktionieren.
//! `harw-warden` hat genau **eine** Wurzel (`--cgroup-root`), und sie ist
//! nicht optional: ohne sie kann dieser Prozess buchstäblich keine seiner
//! vier Aktionen ausführen. Ist sie nicht als `PathFd` öffenbar, ist das
//! deshalb hier **kein** stiller Teilerfolg, sondern derselbe harte
//! Startfehler wie jeder andere Landlock-Fehlschlag — die Alternative
//! (eine leere Regel binden, die die Wurzel nach `restrict_self()`
//! vollständig unlesbar machte) wäre eine stille Degradation der
//! eigentlichen Aufgabe dieses Prozesses, die Entscheidung Nr. 4 gerade
//! ausschließt.
//!
//! # Ohne `unsafe`
//! Wie `harw-sentinel::sandbox` und `harw-probe-fs::landlock`: jeder hier
//! aufgerufene Baustein der `landlock`-Crate (`Ruleset`, `RulesetCreated`,
//! `PathBeneath`, `PathFd`, `.restrict_self()`) ist laut deren eigener
//! Dokumentation eine sichere Funktion. Diese Implementierung wurde gegen
//! dieselbe, bereits im Workspace verwendete Version (0.4.7) geschrieben,
//! aber **nie gegen einen echten Kernel ausgeführt** — dieser Knoten darf
//! kein `cargo` ausführen und keine echte Landlock-Regel binden.
//!
//! # Nur `FullyEnforced` zählt als Erfolg
//! Wie `harw-probe-fs`, anders als `harw-sentinel`: `PartiallyEnforced`
//! (älterer Kernel) und `NotEnforced` gelten beide als „nicht verfügbar".
//! Für einen Prozess, dessen einzige Schranke Landlock ist, ist ein
//! teilweiser Schutz kein Schutz, dem man vertrauen sollte.
//!
//! # Exportierte Typen
//! Keine — nur die Funktion [`enforce_cgroup_root`].
//!
//! # Nebenläufigkeit
//! [`enforce_cgroup_root`] muss vor jedem weiteren Thread aus dem
//! Hauptthread aufgerufen werden — eine Landlock-Regel gilt prozessweit für
//! alle danach entstehenden Threads (insbesondere für jeden von
//! [`crate::ipc::serve_forever`] gestarteten Verbindungs-Thread).
//!
//! # Fehler
//! [`crate::error::WardenBinError::LandlockUnavailable`], siehe oben.

use std::path::Path;

use landlock::{
    ABI, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, RestrictionStatus,
    Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};

use crate::error::WardenBinError;

/// Erzwingt Lese- und Schreibzugriff auf `cgroup_root`, sonst nichts, bevor
/// dieser Prozess auch nur eine Anfrage annimmt.
///
/// # Description
/// Baut ein `landlock::Ruleset` mit `CompatLevel::BestEffort` (damit ein
/// älterer Kernel `PartiallyEnforced` statt eines harten `Err` aus der
/// `landlock`-Crate selbst liefert — diese Funktion entscheidet danach
/// trotzdem selbst, siehe Moduldoku, dass auch das nicht genügt), nimmt für
/// `cgroup_root` eine lesend-schreibende `PathBeneath`-Regel unter
/// [`landlock::ABI::V1`] auf und ruft `restrict_self()` auf. Nur
/// [`RulesetStatus::FullyEnforced`] gilt als Erfolg; ist `cgroup_root`
/// selbst nicht als `PathFd` öffenbar, ist das ebenfalls ein Fehlschlag
/// dieser Funktion (siehe Moduldoku, Abschnitt „Ein Wurzelverzeichnis, kein
/// optionaler Satz").
///
/// # Arguments
/// - `cgroup_root` (`&Path`): das Wurzelverzeichnis, auf das sich dieser
///   Prozess beschränken soll (Produktion: `/sys/fs/cgroup`).
///
/// # Returns
/// `Ok(())` ausschließlich bei `RulesetStatus::FullyEnforced`.
///
/// # Errors
/// - [`WardenBinError::LandlockUnavailable`]: bei jedem Aufbaufehler, bei
///   einem nicht öffenbaren `cgroup_root`, und bei jedem Ausgang außer
///   `FullyEnforced`.
pub fn enforce_cgroup_root(cgroup_root: &Path) -> Result<(), WardenBinError> {
    let abi = ABI::V1;
    let access = AccessFs::from_read(abi) | AccessFs::from_write(abi);

    let ruleset = Ruleset::default().set_compatibility(CompatLevel::BestEffort);
    let ruleset = ruleset.handle_access(access).map_err(|error| {
        tracing::error!(error = %error, "landlock handle_access failed; refusing to start");
        WardenBinError::LandlockUnavailable
    })?;

    let created = ruleset
        .create()
        .map_err(|error| {
            tracing::error!(error = %error, "landlock ruleset create failed; refusing to start");
            WardenBinError::LandlockUnavailable
        })?
        .set_compatibility(CompatLevel::BestEffort);

    let root_fd = PathFd::new(cgroup_root).map_err(|error| {
        tracing::error!(
            path = %cgroup_root.display(),
            error = %error,
            "cgroup root not openable while building the landlock rule; refusing to start"
        );
        WardenBinError::LandlockUnavailable
    })?;
    let rule: PathBeneath<PathFd> = PathBeneath::new(root_fd, access);

    let created = created.add_rules([Ok::<_, landlock::RulesetError>(rule)]).map_err(|error| {
        tracing::error!(error = %error, "landlock add_rules failed; refusing to start");
        WardenBinError::LandlockUnavailable
    })?;

    let status: RestrictionStatus = created.restrict_self().map_err(|error| {
        tracing::error!(error = %error, "landlock restrict_self failed; refusing to start");
        WardenBinError::LandlockUnavailable
    })?;

    if status.ruleset == RulesetStatus::FullyEnforced {
        tracing::info!(root = %cgroup_root.display(), "landlock cgroup root fully enforced");
        Ok(())
    } else {
        tracing::error!(
            status = ?status.ruleset,
            "landlock did not fully enforce the cgroup root; refusing to start"
        );
        Err(WardenBinError::LandlockUnavailable)
    }
}

// Bewusst kein `#[cfg(test)] mod tests` in dieser Datei: jeder denkbare Test
// müsste entweder `enforce_cgroup_root` (echtes `restrict_self`) oder
// `PathFd::new` (öffnet einen echten Dateideskriptor) ausführen — nach
// Aufgabenstellung ausdrücklich untersagt ("binde keine echte
// Landlock-Regel"). Dieselbe Disziplin wie `harw-sentinel::sandbox` und
// `harw-probe-fs::landlock`, die aus demselben Grund ebenfalls keinen Test
// tragen. Die Entscheidungslogik dieser Datei (Lese- und Schreibzugriff,
// ein nicht öffenbares Wurzelverzeichnis ist ein harter Fehlschlag, nur
// `FullyEnforced` zählt) ist in der Moduldoku begründet statt durch einen
// Test belegt, der einen Kernel-Zustand voraussetzen würde.
