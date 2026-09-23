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
//! nur `/sys/fs/cgroup` (lesend-schreibend) und das `nft`-Programm samt
//! Laufzeitbibliotheken (nur lesend/ausführend, siehe unten) braucht — die
//! Selbstbeschränkung ist hier nicht eine von mehreren Verteidigungslinien, sondern die einzige, die den
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
//! # Schreibzugriff nur auf die cgroup-Wurzel
//! Seit `IsolateNetwork` über [`crate::isolation::NftNetworkIsolator`]
//! läuft, führt dieser Prozess das `nft`-Programm aus
//! ([`crate::isolation::DEFAULT_NFT_BINARY`]). Die Landlock-Domäne vererbt
//! sich über `execve` auf das Kindprogramm; ohne weitere Regeln scheiterte
//! schon das `execve` selbst, spätestens aber das Laden des dynamischen
//! Laders und der geteilten Bibliotheken mit `EACCES`. Die Regel gewährt
//! deshalb zusätzlich — **ausschließlich** `AccessFs::from_read(abi)`
//! (`Execute | ReadFile | ReadDir`), **niemals** Schreibrechte — Zugriff auf
//! die in [`read_exec_paths`] aufgezählten Pfade: das `nft`-Programm, die
//! Bibliotheks- und Laderverzeichnisse (`/usr/lib`, `/lib`, `/lib64`,
//! `/usr/lib64` — `Execute` ist dort nötig, weil der Kernel den
//! ELF-Interpreter `ld-linux*.so` wie ein Programm öffnet),
//! `/etc/ld.so.cache` und nftables' Konfiguration (`/etc/nftables.conf`,
//! `/etc/nftables`, `/etc/nftables.d`). Der Schreibzugriff bleibt exakt wie
//! zuvor auf `cgroup_root` beschränkt.
//!
//! Anders als die cgroup-Wurzel sind diese Pfade **optional**: je nach
//! Distribution fehlt z. B. `/lib64` oder `/etc/nftables.d`. Ein nicht
//! öffenbarer Zusatzpfad wird übersprungen (`landlock::path_beneath_rules`
//! lässt ihn aus) statt den Start zu verhindern — fail-closed, denn ein
//! ausgelassener Pfad bleibt nach `restrict_self()` schlicht unzugänglich;
//! fehlt tatsächlich etwas, das `nft` braucht, scheitert erst die jeweilige
//! Isolation mit einem gemeldeten Fehler, nicht der ganze Warden. Für
//! reguläre Dateien (`nft`, `ld.so.cache`) beschneidet
//! `path_beneath_rules` die Rechte auf die für Dateien gültigen
//! (`Execute | ReadFile`) — ein `ReadDir` auf einer Datei ergäbe sonst
//! `PartiallyEnforced` und damit einen harten Startfehler.
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
//! `PathBeneath`, `PathFd`, `path_beneath_rules`, `.restrict_self()`) ist
//! laut deren eigener Dokumentation eine sichere Funktion. Diese Implementierung wurde gegen
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
//! Keine — nur die Funktionen [`enforce_cgroup_root`],
//! [`enforce_warden_scope`] und die reine Hilfsfunktion [`read_exec_paths`].
//!
//! # Nebenläufigkeit
//! [`enforce_cgroup_root`] bzw. [`enforce_warden_scope`] muss vor jedem weiteren Thread aus dem
//! Hauptthread aufgerufen werden — eine Landlock-Regel gilt prozessweit für
//! alle danach entstehenden Threads (insbesondere für jeden von
//! [`crate::ipc::serve_forever`] gestarteten Verbindungs-Thread).
//!
//! # Fehler
//! [`crate::error::WardenBinError::LandlockUnavailable`], siehe oben.

use std::path::{Path, PathBuf};

use landlock::{
    ABI, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus, path_beneath_rules,
};

use crate::error::WardenBinError;
use crate::isolation::DEFAULT_NFT_BINARY;

/// Bibliotheks- und Laderverzeichnisse, die `nft` (und sein dynamischer
/// Lader) zum Start lesen bzw. ausführen muss; fehlende werden übersprungen.
const LIBRARY_DIRS: [&str; 4] = ["/usr/lib", "/lib", "/lib64", "/usr/lib64"];

/// Cache des dynamischen Laders (`ld.so`), nur lesend.
const LD_SO_CACHE: &str = "/etc/ld.so.cache";

/// Mögliche Konfigurationspfade von nftables, nur lesend; fehlende werden
/// übersprungen.
const NFTABLES_CONFIG_PATHS: [&str; 3] = ["/etc/nftables.conf", "/etc/nftables", "/etc/nftables.d"];

/// Zählt die Pfade auf, auf die dieser Prozess zusätzlich zur cgroup-Wurzel
/// ausschließlich lesend/ausführend zugreifen darf.
///
/// # Description
/// Reine Funktion ohne Dateisystemzugriff: sie liefert Kandidaten, das
/// Überspringen nicht vorhandener Pfade erledigt der Aufrufer
/// ([`enforce_warden_scope`] über `landlock::path_beneath_rules`).
/// Reihenfolge: `nft_binary`, [`LIBRARY_DIRS`], [`LD_SO_CACHE`],
/// [`NFTABLES_CONFIG_PATHS`]. Ein **nicht absoluter** `nft_binary` (ein
/// bloßer Programmname, den `Command` über `PATH` auflöst) wird
/// ausgelassen: `PathFd::new` löste ihn relativ zum Arbeitsverzeichnis auf
/// und gewährte damit Zugriff auf einen beliebigen, falschen Pfad.
/// Doppelte Einträge (z. B. `nft_binary` gleich einem Bibliothekspfad)
/// werden entfernt.
///
/// # Arguments
/// - `nft_binary` (`&Path`): Pfad des `nft`-Programms, das
///   [`crate::isolation::NftNetworkIsolator`] ausführt.
///
/// # Returns
/// Die Kandidatenliste in obiger Reihenfolge, ohne Duplikate.
#[must_use]
pub fn read_exec_paths(nft_binary: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> =
        Vec::with_capacity(1 + LIBRARY_DIRS.len() + 1 + NFTABLES_CONFIG_PATHS.len());
    let candidates = std::iter::once(nft_binary)
        .filter(|path| path.is_absolute())
        .chain(LIBRARY_DIRS.iter().map(Path::new))
        .chain(std::iter::once(Path::new(LD_SO_CACHE)))
        .chain(NFTABLES_CONFIG_PATHS.iter().map(Path::new));
    for candidate in candidates {
        if !paths.iter().any(|existing| existing.as_path() == candidate) {
            paths.push(candidate.to_path_buf());
        }
    }
    paths
}

/// Wie [`enforce_warden_scope`] mit [`DEFAULT_NFT_BINARY`] — dem Programm,
/// das der produktive [`crate::isolation::NftNetworkIsolator`] ausführt.
///
/// # Arguments
/// - `cgroup_root` (`&Path`): siehe [`enforce_warden_scope`].
///
/// # Returns
/// Siehe [`enforce_warden_scope`].
///
/// # Errors
/// Siehe [`enforce_warden_scope`].
pub fn enforce_cgroup_root(cgroup_root: &Path) -> Result<(), WardenBinError> {
    enforce_warden_scope(cgroup_root, Path::new(DEFAULT_NFT_BINARY))
}

/// Erzwingt Lese- und Schreibzugriff auf `cgroup_root` sowie reinen
/// Lese-/Ausführungszugriff auf `nft` und dessen Laufzeitpfade, sonst
/// nichts, bevor dieser Prozess auch nur eine Anfrage annimmt.
///
/// # Description
/// Baut ein `landlock::Ruleset` mit `CompatLevel::BestEffort` (damit ein
/// älterer Kernel `PartiallyEnforced` statt eines harten `Err` aus der
/// `landlock`-Crate selbst liefert — diese Funktion entscheidet danach
/// trotzdem selbst, siehe Moduldoku, dass auch das nicht genügt), nimmt für
/// `cgroup_root` eine lesend-schreibende `PathBeneath`-Regel unter
/// [`landlock::ABI::V1`] auf, dazu für jeden öffenbaren Pfad aus
/// [`read_exec_paths`] eine rein lesende (`Execute | ReadFile | ReadDir`,
/// für Dateien beschnitten auf `Execute | ReadFile`), und ruft
/// `restrict_self()` auf. Nur [`RulesetStatus::FullyEnforced`] gilt als
/// Erfolg; ist `cgroup_root` selbst nicht als `PathFd` öffenbar, ist das
/// ebenfalls ein Fehlschlag dieser Funktion (siehe Moduldoku, Abschnitt
/// „Ein Wurzelverzeichnis, kein optionaler Satz"). Nicht vorhandene
/// Zusatzpfade werden übersprungen (Moduldoku, Abschnitt „Schreibzugriff
/// nur auf die cgroup-Wurzel").
///
/// # Arguments
/// - `cgroup_root` (`&Path`): das Wurzelverzeichnis, auf das sich der
///   Schreibzugriff dieses Prozesses beschränken soll (Produktion:
///   `/sys/fs/cgroup`).
/// - `nft_binary` (`&Path`): das `nft`-Programm, das der Isolator ausführt
///   (Produktion: [`DEFAULT_NFT_BINARY`]); muss absolut sein, sonst wird es
///   ausgelassen (siehe [`read_exec_paths`]).
///
/// # Returns
/// `Ok(())` ausschließlich bei `RulesetStatus::FullyEnforced`.
///
/// # Errors
/// - [`WardenBinError::LandlockUnavailable`]: bei jedem Aufbaufehler, bei
///   einem nicht öffenbaren `cgroup_root`, und bei jedem Ausgang außer
///   `FullyEnforced`.
pub fn enforce_warden_scope(cgroup_root: &Path, nft_binary: &Path) -> Result<(), WardenBinError> {
    let abi = ABI::V1;
    let read_access = AccessFs::from_read(abi);
    let access = read_access | AccessFs::from_write(abi);

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

    let created = created
        .add_rules([Ok::<_, landlock::RulesetError>(rule)])
        .map_err(|error| {
            tracing::error!(error = %error, "landlock add_rules failed; refusing to start");
            WardenBinError::LandlockUnavailable
        })?;

    let extra_paths = read_exec_paths(nft_binary);
    if !nft_binary.is_absolute() {
        tracing::warn!(
            nft = %nft_binary.display(),
            "nft binary path is not absolute; no landlock rule for it, isolation will likely fail"
        );
    }
    for path in &extra_paths {
        if PathFd::new(path).is_err() {
            tracing::debug!(path = %path.display(), "optional landlock read path not openable; skipped");
        }
    }
    // `path_beneath_rules` lässt nicht öffenbare Pfade selbst aus und
    // beschneidet die Rechte regulärer Dateien auf `Execute | ReadFile`.
    let created = created.add_rules(path_beneath_rules(&extra_paths, read_access)).map_err(|error| {
        tracing::error!(error = %error, "landlock add_rules for nft paths failed; refusing to start");
        WardenBinError::LandlockUnavailable
    })?;

    let status: RestrictionStatus = created.restrict_self().map_err(|error| {
        tracing::error!(error = %error, "landlock restrict_self failed; refusing to start");
        WardenBinError::LandlockUnavailable
    })?;

    if status.ruleset == RulesetStatus::FullyEnforced {
        tracing::info!(
            root = %cgroup_root.display(),
            nft = %nft_binary.display(),
            "landlock cgroup root and nft read scope fully enforced"
        );
        Ok(())
    } else {
        tracing::error!(
            status = ?status.ruleset,
            "landlock did not fully enforce the warden scope; refusing to start"
        );
        Err(WardenBinError::LandlockUnavailable)
    }
}

// Getestet wird nur die reine Pfadliste [`read_exec_paths`]:
// `enforce_warden_scope`/`enforce_cgroup_root` selbst würden eine echte
// Landlock-Regel binden (`restrict_self`) — nach Aufgabenstellung
// ausdrücklich untersagt, dieselbe Disziplin wie `harw-sentinel::sandbox`
// und `harw-probe-fs::landlock`. Deren Entscheidungslogik (Schreibzugriff nur
// auf die Wurzel, ein nicht öffenbares Wurzelverzeichnis ist ein harter
// Fehlschlag, nur `FullyEnforced` zählt) ist in der Moduldoku begründet.
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{LD_SO_CACHE, LIBRARY_DIRS, NFTABLES_CONFIG_PATHS, read_exec_paths};
    use crate::isolation::DEFAULT_NFT_BINARY;
    use crate::test_support::{TestError, TestResult};

    fn expected_tail() -> Vec<PathBuf> {
        LIBRARY_DIRS
            .iter()
            .chain(std::iter::once(&LD_SO_CACHE))
            .chain(NFTABLES_CONFIG_PATHS.iter())
            .map(PathBuf::from)
            .collect()
    }

    #[test]
    fn test_read_exec_paths_starts_with_nft_binary_then_fixed_paths() -> TestResult {
        let paths = read_exec_paths(Path::new(DEFAULT_NFT_BINARY));
        let first = paths
            .first()
            .ok_or(TestError::Missing("nft binary entry"))?;
        if first.as_path() != Path::new("/usr/sbin/nft") {
            return Err(TestError::Unexpected(format!(
                "first entry {}",
                first.display()
            )));
        }
        let tail: Vec<PathBuf> = paths.iter().skip(1).cloned().collect();
        if tail != expected_tail() {
            return Err(TestError::Unexpected(format!("tail {tail:?}")));
        }
        Ok(())
    }

    #[test]
    fn test_read_exec_paths_covers_loader_cache_and_nftables_config() -> TestResult {
        let paths = read_exec_paths(Path::new("/opt/nft/bin/nft"));
        for required in [
            "/opt/nft/bin/nft",
            "/usr/lib",
            "/lib",
            "/lib64",
            "/usr/lib64",
            "/etc/ld.so.cache",
            "/etc/nftables.conf",
            "/etc/nftables",
            "/etc/nftables.d",
        ] {
            if !paths
                .iter()
                .any(|path| path.as_path() == Path::new(required))
            {
                return Err(TestError::Unexpected(format!(
                    "missing {required} in {paths:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_read_exec_paths_never_contains_cgroup_mount() -> TestResult {
        let paths = read_exec_paths(Path::new(DEFAULT_NFT_BINARY));
        if paths.iter().any(|path| path.starts_with("/sys")) {
            return Err(TestError::Unexpected(format!(
                "read scope reaches /sys: {paths:?}"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_read_exec_paths_skips_relative_nft_binary() -> TestResult {
        for relative in ["nft", "bin/nft", ""] {
            let paths = read_exec_paths(Path::new(relative));
            if paths != expected_tail() {
                return Err(TestError::Unexpected(format!(
                    "{relative:?} gave {paths:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_read_exec_paths_deduplicates() -> TestResult {
        let paths = read_exec_paths(Path::new("/usr/lib"));
        let count = paths
            .iter()
            .filter(|path| path.as_path() == Path::new("/usr/lib"))
            .count();
        if count != 1 {
            return Err(TestError::Unexpected(format!(
                "/usr/lib appears {count} times"
            )));
        }
        if paths != expected_tail() {
            return Err(TestError::Unexpected(format!("unexpected list {paths:?}")));
        }
        Ok(())
    }
}
