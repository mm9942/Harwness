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
//! nur `/sys/fs/cgroup` und sein StateDirectory (lesend-schreibend), im
//! Indexverzeichnis der Eindämmungsmarker Marker anlegen und entfernen und —
//! nur bei erlaubter Netzaktion — das `nft`-Programm samt
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
//! # Das StateDirectory (PL-100 WP 3.1)
//! Nonce-Ledger, Aktions-Ledger und Hold-Timer liegen im StateDirectory
//! (`--state-dir`, Vorgabe `/var/lib/harw-warden`). Der Warden hängt dort zur
//! Laufzeit an und ersetzt Dateien atomar (`<name>.tmp` → `rename` im selben
//! Verzeichnis). Es bekommt deshalb dieselbe lesend-schreibende Regel wie die
//! cgroup-Wurzel. Unter ABI V1 bleibt ein Umbenennen **über**
//! Verzeichnisgrenzen hinweg verboten (`REFER` ist dort nie erlaubt); der
//! Warden benennt nur innerhalb des StateDirectory um. Wie die cgroup-Wurzel
//! ist das StateDirectory **Pflicht**: ist es nicht öffenbar, startet der
//! Warden nicht.
//!
//! # Das Indexverzeichnis der Eindämmungsmarker (Owner-Review 4505015305)
//! Je eingefrorener cgroup legt der Warden eine leere Datei
//! `<holds>/<Kernel-ID>` an (`--holds-dir`, Vorgabe
//! `/run/harw-warden/holds`, `harw_dod_warden::containment`); harw taut eine
//! markierte cgroup nie auf (`job.resume`). Die Regel für dieses Verzeichnis
//! ist **enger** als die der beiden Wurzeln: nur [`holds_index_access`] —
//! auflisten (`ReadDir`), eine reguläre Datei anlegen und zum Schreiben
//! öffnen (`MakeReg`, `WriteFile`), eine Datei entfernen (`RemoveFile`). Kein
//! Symlink, kein Verzeichnis, kein Gerät, kein Lesen von Markerinhalten,
//! kein Ausführen. Die Regel gilt nur unterhalb von `holds`; der
//! root-eigene Elternordner `/run/harw-warden` bleibt für den Warden
//! unbeschreibbar. `flock` und `fstatat` regelt Landlock (ABI V1) nicht.
//! Wie StateDirectory und cgroup-Wurzel ist das Verzeichnis **Pflicht**
//! (tmpfiles legt es an; fehlt es, startet der Warden nicht, wie auch
//! systemd ohne den Pfad aus `ReadWritePaths=` nicht startet).
//!
//! # Schreibzugriff nur auf cgroup-Wurzel, StateDirectory und Marker
//! Erlaubt die Policy eine Netzaktion, führt dieser Prozess über
//! [`crate::isolation::NftCommandBackend`] das `nft`-Programm aus
//! ([`crate::isolation::DEFAULT_NFT_BINARY`]). Die Landlock-Domäne vererbt
//! sich über `execve` auf das Kindprogramm; ohne weitere Regeln scheiterte
//! schon das `execve` selbst, spätestens aber das Laden des dynamischen
//! Laders und der geteilten Bibliotheken mit `EACCES`. Was das Backend
//! braucht, meldet es selbst ([`crate::isolation::SandboxNeeds`]):
//!
//! - **ausschließlich** `AccessFs::from_read(abi)` (`Execute | ReadFile |
//!   ReadDir`), **niemals** Schreibrechte, für die in [`read_exec_paths`]
//!   aufgezählten Pfade: das `nft`-Programm, die Bibliotheks- und
//!   Laderverzeichnisse (`/usr/lib`, `/lib`, `/lib64`, `/usr/lib64` —
//!   `Execute` ist dort nötig, weil der Kernel den ELF-Interpreter
//!   `ld-linux*.so` wie ein Programm öffnet), `/etc/ld.so.cache` und
//!   nftables' Konfiguration (`/etc/nftables.conf`, `/etc/nftables`,
//!   `/etc/nftables.d`);
//! - Lesen und Schreiben **einzelner Dateien**: `/dev/null`, das
//!   `std::process::Stdio::null()` für Standard-Ein- und -Ausgabe des
//!   Kindes öffnet. Für Dateien beschneidet `path_beneath_rules` die Rechte
//!   auf `Execute | WriteFile | ReadFile`; ein Verzeichnis ist hier nie
//!   gemeint.
//!
//! Ohne erlaubte Netzaktion (Vorgabe: nur Freeze/Thaw) ist der Bedarf leer:
//! dann bekommt der Warden weder Ausführungsrechte noch `/dev/null`. Der
//! Schreibzugriff auf Verzeichnisse bleibt immer auf `cgroup_root`, das
//! StateDirectory und (nur Marker anlegen und entfernen) das
//! Indexverzeichnis beschränkt. Policy, Schlüssel und `/etc/passwd` liest der
//! Warden **vor** dieser Beschränkung; danach sind sie unerreichbar.
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
//! Keine — nur die Funktion [`enforce_runtime_scope`] und die reinen
//! Hilfsfunktionen [`read_exec_paths`] und [`holds_index_access`].
//!
//! # Nebenläufigkeit
//! [`enforce_runtime_scope`] muss vor jedem weiteren Thread aus dem
//! Hauptthread aufgerufen werden — eine Landlock-Regel gilt prozessweit für
//! alle danach entstehenden Threads (insbesondere für jeden von
//! [`crate::ipc::serve_forever`] gestarteten Verbindungs-Thread). Danach
//! folgt der seccomp-Filter (`crate::seccomp`), der `landlock_*` nicht mehr
//! erlaubt.
//!
//! # Fehler
//! [`crate::error::WardenBinError::LandlockUnavailable`], siehe oben.

use std::path::{Path, PathBuf};

use landlock::{
    ABI, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, PathFdError,
    RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, path_beneath_rules,
};

use crate::error::WardenBinError;
use crate::isolation::SandboxNeeds;

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
/// ([`enforce_runtime_scope`] über `landlock::path_beneath_rules`).
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
///   [`crate::isolation::NftCommandBackend`] ausführt.
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

/// Die Rechte unterhalb des Indexverzeichnisses der Eindämmungsmarker.
///
/// # Description
/// Genau, was das Marker-Protokoll und der Abgleich beim Start brauchen
/// (`harw_dod_warden::containment`, `harw_dod_warden::reconcile`): das
/// Verzeichnis öffnen und auflisten (`ReadDir`), einen Marker anlegen
/// (`MakeReg`, `O_CREAT | O_EXCL`) und dabei zum Schreiben öffnen
/// (`WriteFile`), einen Marker entfernen (`RemoveFile`). Siehe Moduldoku,
/// Abschnitt „Das Indexverzeichnis der Eindämmungsmarker“.
///
/// # Returns
/// Die Rechte (eine Teilmenge der unter ABI V1 behandelten).
#[must_use]
pub fn holds_index_access() -> BitFlags<AccessFs> {
    AccessFs::ReadDir | AccessFs::WriteFile | AccessFs::MakeReg | AccessFs::RemoveFile
}

/// Erzwingt Lese- und Schreibzugriff auf `cgroup_root` und `state_dir`,
/// Marker-Rechte im Indexverzeichnis `holds_dir` sowie den Bedarf des
/// Netfilter-Backends, sonst nichts, bevor dieser Prozess auch nur eine
/// Anfrage annimmt.
///
/// # Description
/// Baut ein `landlock::Ruleset` mit `CompatLevel::BestEffort` (damit ein
/// älterer Kernel `PartiallyEnforced` statt eines harten `Err` aus der
/// `landlock`-Crate selbst liefert — diese Funktion entscheidet danach
/// trotzdem selbst, siehe Moduldoku, dass auch das nicht genügt), nimmt für
/// `cgroup_root` und für `state_dir` je eine lesend-schreibende
/// `PathBeneath`-Regel unter [`landlock::ABI::V1`] auf, für `holds_dir`
/// eine mit nur [`holds_index_access`], dazu für jeden
/// öffenbaren Pfad aus `needs.read_exec_paths` eine rein lesende
/// (`Execute | ReadFile | ReadDir`, für Dateien beschnitten auf
/// `Execute | ReadFile`) und für jede öffenbare Datei aus
/// `needs.read_write_files` eine lesend-schreibende (beschnitten auf die
/// Dateirechte), und ruft `restrict_self()` auf. Nur
/// [`RulesetStatus::FullyEnforced`] gilt als Erfolg; ist `cgroup_root`,
/// `state_dir` oder `holds_dir` nicht als `PathFd` öffenbar, ist das
/// ebenfalls ein Fehlschlag dieser Funktion (siehe Moduldoku, Abschnitt „Ein
/// Wurzelverzeichnis, kein optionaler Satz"). Nicht vorhandene Zusatzpfade
/// werden übersprungen (Moduldoku, Abschnitt „Schreibzugriff nur auf
/// cgroup-Wurzel, StateDirectory und Marker").
///
/// # Arguments
/// - `cgroup_root` (`&Path`): das Wurzelverzeichnis, auf das sich der
///   Schreibzugriff dieses Prozesses beschränken soll (Produktion:
///   `/sys/fs/cgroup`).
/// - `state_dir` (`&Path`): das StateDirectory (Produktion:
///   `/var/lib/harw-warden`).
/// - `holds_dir` (`&Path`): das Indexverzeichnis der Eindämmungsmarker
///   (Produktion: `/run/harw-warden/holds`).
/// - `needs` (`&SandboxNeeds`): Bedarf des Netfilter-Backends (leer, wenn
///   die Policy keine Netzaktion erlaubt).
///
/// # Returns
/// `Ok(())` ausschließlich bei `RulesetStatus::FullyEnforced`.
///
/// # Errors
/// - [`WardenBinError::LandlockUnavailable`]: bei jedem Aufbaufehler, bei
///   einem nicht öffenbaren `cgroup_root`, `state_dir` oder `holds_dir`, und
///   bei jedem Ausgang außer `FullyEnforced`.
pub fn enforce_runtime_scope(
    cgroup_root: &Path,
    state_dir: &Path,
    holds_dir: &Path,
    needs: &SandboxNeeds,
) -> Result<(), WardenBinError> {
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
        // `PathFdError` nennt den Pfad: der volle Fehler nur auf `debug`.
        tracing::debug!(error = %error, "cgroup root that could not be opened");
        tracing::error!(
            kind = ?open_error_kind(&error),
            "cgroup root not openable while building the landlock rule; refusing to start"
        );
        WardenBinError::LandlockUnavailable
    })?;
    let rule: PathBeneath<PathFd> = PathBeneath::new(root_fd, access);
    let state_fd = PathFd::new(state_dir).map_err(|error| {
        tracing::debug!(error = %error, "state directory that could not be opened");
        tracing::error!(
            kind = ?open_error_kind(&error),
            "state directory not openable while building the landlock rule; refusing to start"
        );
        WardenBinError::LandlockUnavailable
    })?;
    let state_rule: PathBeneath<PathFd> = PathBeneath::new(state_fd, access);
    let holds_fd = PathFd::new(holds_dir).map_err(|error| {
        tracing::debug!(error = %error, "containment holds directory that could not be opened");
        tracing::error!(
            kind = ?open_error_kind(&error),
            "containment holds directory not openable while building the landlock rule; refusing to start"
        );
        WardenBinError::LandlockUnavailable
    })?;
    let holds_rule: PathBeneath<PathFd> = PathBeneath::new(holds_fd, holds_index_access());

    let created = created
        .add_rules([
            Ok::<_, landlock::RulesetError>(rule),
            Ok::<_, landlock::RulesetError>(state_rule),
            Ok::<_, landlock::RulesetError>(holds_rule),
        ])
        .map_err(|error| {
            tracing::error!(error = %error, "landlock add_rules failed; refusing to start");
            WardenBinError::LandlockUnavailable
        })?;

    for path in needs.read_exec_paths.iter().chain(&needs.read_write_files) {
        if PathFd::new(path).is_err() {
            tracing::debug!(path = %path.display(), "optional landlock path not openable; skipped");
        }
    }
    // `path_beneath_rules` lässt nicht öffenbare Pfade selbst aus und
    // beschneidet die Rechte regulärer Dateien auf die Dateirechte.
    let created = created
        .add_rules(path_beneath_rules(&needs.read_exec_paths, read_access))
        .map_err(|error| {
            tracing::error!(error = %error, "landlock add_rules for backend read paths failed; refusing to start");
            WardenBinError::LandlockUnavailable
        })?;
    let created = created
        .add_rules(path_beneath_rules(&needs.read_write_files, access))
        .map_err(|error| {
            tracing::error!(error = %error, "landlock add_rules for backend files failed; refusing to start");
            WardenBinError::LandlockUnavailable
        })?;

    let status: RestrictionStatus = created.restrict_self().map_err(|error| {
        tracing::error!(error = %error, "landlock restrict_self failed; refusing to start");
        WardenBinError::LandlockUnavailable
    })?;

    if status.ruleset == RulesetStatus::FullyEnforced {
        tracing::info!(
            root = %cgroup_root.display(),
            state = %state_dir.display(),
            holds = %holds_dir.display(),
            backend_read_paths = needs.read_exec_paths.len(),
            backend_files = needs.read_write_files.len(),
            "landlock cgroup root, state directory, containment index and backend scope fully enforced"
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

/// Returns the `io::ErrorKind` behind a `PathFdError` (never its path).
///
/// # Description
/// `landlock::PathFdError` zeigt in `Display` den Pfad
/// (`failed to open "<pfad>": …`). Auf `error`-Stufe steht deshalb nur die
/// Fehlerart; ein unbekannter (künftiger) Variantentyp wird `Other`.
///
/// # Arguments
/// - `error` (`&PathFdError`): der Öffnungsfehler.
///
/// # Returns
/// Die Fehlerart des zugrunde liegenden `open()`.
fn open_error_kind(error: &PathFdError) -> std::io::ErrorKind {
    match error {
        PathFdError::OpenCall { source, .. } => source.kind(),
        _ => std::io::ErrorKind::Other,
    }
}

// Getestet werden nur die reine Pfadliste [`read_exec_paths`] und
// [`open_error_kind`] (`PathFd::new` öffnet nur, bindet keine Regel):
// `enforce_runtime_scope` selbst würde eine echte
// Landlock-Regel binden (`restrict_self`) — nach Aufgabenstellung
// ausdrücklich untersagt, dieselbe Disziplin wie `harw-sentinel::sandbox`
// und `harw-probe-fs::landlock`. Deren Entscheidungslogik (Schreibzugriff nur
// auf die Wurzel, ein nicht öffenbares Wurzelverzeichnis ist ein harter
// Fehlschlag, nur `FullyEnforced` zählt) ist in der Moduldoku begründet.
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        LD_SO_CACHE, LIBRARY_DIRS, NFTABLES_CONFIG_PATHS, holds_index_access, open_error_kind,
        read_exec_paths,
    };
    use crate::isolation::DEFAULT_NFT_BINARY;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_open_error_kind_reports_the_kind_without_the_path() -> TestResult {
        let missing = Path::new("/nonexistent/harw-warden-landlock-test");
        let Err(error) = landlock::PathFd::new(missing) else {
            return Err(TestError::Unexpected(
                "a missing path must not be openable".to_owned(),
            ));
        };
        assert_eq!(open_error_kind(&error), std::io::ErrorKind::NotFound);
        // Genau deshalb steht auf `error`-Stufe nur die Fehlerart.
        assert!(error.to_string().contains("harw-warden-landlock-test"));
        Ok(())
    }

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

    // Nur Marker anlegen, öffnen und entfernen, das Verzeichnis auflisten;
    // alles innerhalb der unter ABI V1 behandelten Rechte (sonst
    // `PartiallyEnforced`, ein harter Startfehler).
    #[test]
    fn test_holds_index_access_only_creates_lists_and_removes_markers() {
        use landlock::{ABI, AccessFs};

        let access = holds_index_access();
        for needed in [
            AccessFs::ReadDir,
            AccessFs::WriteFile,
            AccessFs::MakeReg,
            AccessFs::RemoveFile,
        ] {
            assert!(access.contains(needed), "{needed:?} missing");
        }
        for forbidden in [
            AccessFs::Execute,
            AccessFs::ReadFile,
            AccessFs::RemoveDir,
            AccessFs::MakeDir,
            AccessFs::MakeSym,
            AccessFs::MakeChar,
            AccessFs::MakeBlock,
            AccessFs::MakeFifo,
            AccessFs::MakeSock,
            AccessFs::Refer,
            AccessFs::Truncate,
        ] {
            assert!(!access.contains(forbidden), "{forbidden:?} granted");
        }
        let handled = AccessFs::from_read(ABI::V1) | AccessFs::from_write(ABI::V1);
        assert!(handled.contains(access));
        assert_eq!(access.len(), 4);
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
