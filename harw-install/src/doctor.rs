//! Diagnose-Checks („doctor") für die harw-Installation.
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! `harw-install / src/doctor.rs`.
//!
//! # Verantwortung
//! Dieses Modul besitzt die read-only Selbstdiagnose der Laufzeit- und
//! Installationsumgebung. Jeder Einzel-Check kapselt genau eine Prüfung und
//! liefert ein [`CheckOutcome`]. Das Modul delegiert die Umgebungserkennung an
//! [`crate::platform`] und die Service-Manager-Auswahl an
//! [`crate::service::detect_service_manager`]. Es führt keinerlei mutierende
//! Aktionen am System aus.
//!
//! # Exportierte Typen
//! - [`CheckOutcome`]: Ergebnis eines einzelnen Checks (Ok/Warn/Fail).
//! - [`DoctorCheck`]: Trait für einen benannten, ausführbaren Check.
//! - [`default_checks`]: baut die Standard-Check-Liste.
//! - [`RuntimeCompositionEvidence`]: beobachtbare Evidenz der Tool-Komposition.
//! - [`run_all`]: führt eine Check-Liste aus und sammelt die Ergebnisse.
//!
//! # Fehlerbehandlung
//! Checks können nicht per `Result` fehlschlagen; ein negatives Prüfergebnis
//! wird als [`CheckOutcome::Warn`] oder [`CheckOutcome::Fail`] modelliert. Es
//! gibt daher keinen Error-Typ in diesem Modul.
//!
//! # Concurrency
//! Alle Check-Structs sind `Send + Sync`. Die Ausführung ist read-only
//! (Environment-Snapshot, Datei-Metadaten) und hält keine Locks. Der Aufruf ist
//! von mehreren Threads aus sicher.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//! use harw_install::doctor::{default_checks, run_all};
//!
//! let checks = default_checks(Path::new("/home/u/.harw"));
//! for (id, outcome) in run_all(&checks) {
//!     println!("{id}: {outcome:?}");
//! }
//! ```

use std::path::{Path, PathBuf};

use crate::platform::{Os, Platform};
use crate::service::{ServiceKind, detect_service_manager};

/// Ergebnis eines einzelnen Diagnose-Checks.
///
/// # Description
/// Modelliert den Ausgang einer Prüfung mit einer menschenlesbaren Nachricht.
/// `Ok` steht für einen bestandenen Check, `Warn` für einen nicht-kritischen
/// Mangel und `Fail` für einen kritischen Fehler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Check bestanden; die Nachricht beschreibt den positiven Befund.
    Ok(String),
    /// Nicht-kritischer Mangel; die Nachricht beschreibt die Einschränkung.
    Warn(String),
    /// Kritischer Fehler; die Nachricht beschreibt das Problem.
    Fail(String),
}

/// Ein benannter, read-only ausführbarer Diagnose-Check.
///
/// # Description
/// Implementierungen kapseln genau eine Prüfung. `id` liefert einen stabilen
/// Bezeichner (für Ausgabe/Filterung), `run` führt die Prüfung fehlertolerant
/// aus und liefert ein [`CheckOutcome`].
///
/// # Concurrency
/// Implementierungen müssen read-only und von mehreren Threads aus sicher
/// aufrufbar sein.
pub trait DoctorCheck {
    /// Liefert den stabilen Bezeichner dieses Checks.
    ///
    /// # Returns
    /// Ein `&str`, z. B. `"system/os"` oder `"sandbox/bwrap"`.
    fn id(&self) -> &str;

    /// Führt die Prüfung read-only aus.
    ///
    /// # Returns
    /// Ein [`CheckOutcome`] mit menschenlesbarer Begründung.
    ///
    /// # Concurrency
    /// Read-only; sicher aus mehreren Threads aufrufbar.
    fn run(&self) -> CheckOutcome;
}

/// Prüft das erkannte Betriebssystem via [`Platform::detect`].
///
/// # Description
/// Liefert für explizit unterstützte Systeme [`CheckOutcome::Ok`] mit dem
/// erkannten OS-Namen; ein [`Os::Other`] wird als [`CheckOutcome::Warn`]
/// gemeldet, da nicht explizit unterstützt.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCheck;

impl DoctorCheck for SystemCheck {
    /// Liefert den Bezeichner `"system/os"`.
    fn id(&self) -> &str {
        "system/os"
    }

    /// Erkennt das Betriebssystem und meldet dessen Namen.
    fn run(&self) -> CheckOutcome {
        let platform = Platform::detect();
        let name = os_name(platform.os);
        match platform.os {
            Os::Other => CheckOutcome::Warn(format!("nicht unterstütztes Betriebssystem: {name}")),
            _ => CheckOutcome::Ok(format!("Betriebssystem: {name}")),
        }
    }
}

/// Liefert einen menschenlesbaren OS-Namen.
fn os_name(os: Os) -> &'static str {
    match os {
        Os::Linux => "Linux",
        Os::MacOs => "macOS",
        Os::Windows => "Windows",
        Os::Other => "Other",
    }
}

/// Prüft, ob das Sandbox-Tool `bwrap` im `PATH` auffindbar ist.
///
/// # Description
/// Durchsucht die `PATH`-Umgebungsvariable nach einer ausführbaren `bwrap`-
/// Datei. Bei Fund [`CheckOutcome::Ok`], sonst [`CheckOutcome::Warn`]. Ohne
/// ein ausführbares `bwrap` ist Bubblewrap-Isolation nicht verfügbar.
#[derive(Debug, Clone, Copy, Default)]
pub struct SandboxCheck;

impl DoctorCheck for SandboxCheck {
    /// Liefert den Bezeichner `"sandbox/bwrap"`.
    fn id(&self) -> &str {
        "sandbox/bwrap"
    }

    /// Sucht `bwrap` im `PATH`.
    fn run(&self) -> CheckOutcome {
        match find_executable_in_path("bwrap") {
            Some(path) => CheckOutcome::Ok(format!("bwrap gefunden: {}", path.display())),
            None => CheckOutcome::Warn(
                "Bubblewrap-Isolation nicht verfügbar: bwrap ist nicht ausführbar im PATH"
                    .to_owned(),
            ),
        }
    }
}

/// Sucht ein ausführbares Programm namens `bin` in den `PATH`-Einträgen.
///
/// Liefert den ersten ausführbaren Kandidatenpfad, sonst `None`. Fehlertolerant:
/// ein fehlendes `PATH` ergibt `None`. Unter Unix reicht eine vorhandene Datei
/// nicht aus; mindestens ein Execute-Bit muss gesetzt sein.
fn find_executable_in_path(bin: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(bin);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        true
    }
}

/// Beobachtbare, von der vertrauenswürdigen Composition Root gelieferte
/// Evidenz über eine Tool-Komposition.
///
/// `harw-install` konstruiert keine Sessions und darf deshalb weder einen
/// `SpawnContext` noch einen Approval-Handler erraten. Die
/// aufrufende Composition Root kann nur Fakten einsetzen, die sie tatsächlich
/// beim Aufbau der Laufzeit beobachtet hat. Nicht gelieferte Fakten bleiben
/// explizit unbekannt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeCompositionEvidence {
    /// Die Namen der Tools, die der Komposition tatsächlich angeboten werden.
    /// `None` bedeutet: Die Tool-Fläche wurde nicht konfiguriert oder nicht
    /// an die Diagnose übergeben.
    pub advertised_tools: Option<Vec<String>>,
    /// Ob dieselbe Composition Root einen vertrauenswürdigen `SpawnContext`
    /// an die Session gebunden hat.
    pub has_trusted_spawn_context: Option<bool>,
    /// Ob dieselbe Composition Root eine Approval-Grenze für mutierende oder
    /// Shell-Tools registriert hat.
    pub has_approval_boundary: Option<bool>,
}

impl RuntimeCompositionEvidence {
    /// Erzeugt Evidenz ohne konfigurierte Runtime-Composition.
    #[must_use]
    pub fn not_configured() -> Self {
        Self::default()
    }
}

/// Prüft, ob eine beobachtete Tool-Komposition für Shell- und Schreibtools an
/// die nötigen Laufzeitgrenzen gebunden ist.
///
/// Ohne vom Composition Root gelieferte Evidenz wird bewusst kein positiver
/// Sicherheitsbefund ausgegeben: `harw-install` kann dann nur
/// "unbekannt/nicht konfiguriert" melden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeCompositionCheck {
    evidence: RuntimeCompositionEvidence,
}

impl RuntimeCompositionCheck {
    /// Erzeugt einen Check für die gelieferte Runtime-Composition-Evidenz.
    #[must_use]
    pub fn new(evidence: RuntimeCompositionEvidence) -> Self {
        Self { evidence }
    }
}

impl DoctorCheck for RuntimeCompositionCheck {
    fn id(&self) -> &str {
        "runtime/tool-boundaries"
    }

    fn run(&self) -> CheckOutcome {
        let Some(tools) = &self.evidence.advertised_tools else {
            return CheckOutcome::Warn(
                "Runtime-Tool-Komposition nicht konfiguriert: SpawnContext- und Approval-Grenze unbekannt"
                    .to_owned(),
            );
        };

        if !tools.iter().any(|tool| is_sensitive_tool(tool)) {
            return CheckOutcome::Ok(
                "keine Shell- oder Schreibtools in der beobachteten Tool-Komposition".to_owned(),
            );
        }

        match (
            self.evidence.has_trusted_spawn_context,
            self.evidence.has_approval_boundary,
        ) {
            (Some(false), _) => CheckOutcome::Fail(
                "Shell-/Schreibtools werden angeboten, aber kein vertrauenswürdiger SpawnContext ist gebunden"
                    .to_owned(),
            ),
            (_, Some(false)) => CheckOutcome::Fail(
                "Shell-/Schreibtools werden angeboten, aber keine Approval-Grenze ist gebunden"
                    .to_owned(),
            ),
            (Some(true), Some(true)) => CheckOutcome::Ok(
                "Shell-/Schreibtools sind an vertrauenswürdigen SpawnContext und Approval-Grenze gebunden"
                    .to_owned(),
            ),
            _ => CheckOutcome::Warn(
                "Shell-/Schreibtools werden angeboten, aber SpawnContext- oder Approval-Evidenz ist unbekannt/nicht konfiguriert"
                    .to_owned(),
            ),
        }
    }
}

fn is_sensitive_tool(tool: &str) -> bool {
    matches!(tool, "shell.exec" | "fs.write" | "filesystem.write")
}

/// Prüft den erkannten Service-Manager via [`detect_service_manager`].
///
/// # Description
/// Meldet die Art des Service-Managers ([`ServiceKind`]). Ein
/// [`ServiceKind::Unsupported`] wird als [`CheckOutcome::Warn`] gemeldet, alle
/// konkreten Manager als [`CheckOutcome::Ok`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ServiceManagerCheck;

impl DoctorCheck for ServiceManagerCheck {
    /// Liefert den Bezeichner `"service/manager"`.
    fn id(&self) -> &str {
        "service/manager"
    }

    /// Erkennt den Service-Manager der aktuellen Plattform.
    fn run(&self) -> CheckOutcome {
        let platform = Platform::detect();
        let manager = detect_service_manager(&platform);
        match manager.kind() {
            ServiceKind::Systemd => CheckOutcome::Ok("Service-Manager: systemd".to_owned()),
            ServiceKind::Launchd => CheckOutcome::Ok("Service-Manager: launchd".to_owned()),
            ServiceKind::Schtasks => CheckOutcome::Ok("Service-Manager: schtasks".to_owned()),
            ServiceKind::Unsupported => {
                CheckOutcome::Warn("kein unterstützter Service-Manager verfügbar".to_owned())
            }
        }
    }
}

/// Prüft die Existenz des harw-Home-Verzeichnisses.
///
/// # Description
/// Meldet [`CheckOutcome::Ok`], wenn das Verzeichnis existiert, sonst
/// [`CheckOutcome::Fail`], da nachgelagerte Checks darauf aufbauen.
#[derive(Debug, Clone)]
pub struct HomeExistsCheck {
    /// Pfad zum harw-Home-Verzeichnis (`~/.harw`).
    home: PathBuf,
}

impl HomeExistsCheck {
    /// Erzeugt einen [`HomeExistsCheck`] für das gegebene Home-Verzeichnis.
    ///
    /// # Arguments
    /// - `home` (`&Path`): Pfad zum harw-Home-Verzeichnis; wird kopiert.
    ///
    /// # Returns
    /// Einen neuen [`HomeExistsCheck`].
    pub fn new(home: &Path) -> Self {
        HomeExistsCheck {
            home: home.to_path_buf(),
        }
    }
}

impl DoctorCheck for HomeExistsCheck {
    /// Liefert den Bezeichner `"home/exists"`.
    fn id(&self) -> &str {
        "home/exists"
    }

    /// Prüft, ob das Home-Verzeichnis als Verzeichnis existiert.
    fn run(&self) -> CheckOutcome {
        if self.home.is_dir() {
            CheckOutcome::Ok(format!(
                "Home-Verzeichnis vorhanden: {}",
                self.home.display()
            ))
        } else {
            CheckOutcome::Fail(format!("Home-Verzeichnis fehlt: {}", self.home.display()))
        }
    }
}

/// Prüft die Berechtigungen des harw-Home-Verzeichnisses.
///
/// # Description
/// Verlangt unter Unix den Modus `0700` (nur Eigentümer). Abweichende Modi
/// werden als [`CheckOutcome::Warn`] gemeldet. Existiert das Verzeichnis nicht,
/// resultiert ein [`CheckOutcome::Warn`]. Auf Nicht-Unix-Systemen kann der Modus
/// nicht geprüft werden und der Check meldet [`CheckOutcome::Ok`] als „nicht
/// anwendbar".
#[derive(Debug, Clone)]
pub struct HomePermsCheck {
    /// Pfad zum harw-Home-Verzeichnis (`~/.harw`).
    home: PathBuf,
}

impl HomePermsCheck {
    /// Erzeugt einen [`HomePermsCheck`] für das gegebene Home-Verzeichnis.
    ///
    /// # Arguments
    /// - `home` (`&Path`): Pfad zum harw-Home-Verzeichnis; wird kopiert.
    ///
    /// # Returns
    /// Einen neuen [`HomePermsCheck`].
    pub fn new(home: &Path) -> Self {
        HomePermsCheck {
            home: home.to_path_buf(),
        }
    }
}

impl DoctorCheck for HomePermsCheck {
    /// Liefert den Bezeichner `"home/perms"`.
    fn id(&self) -> &str {
        "home/perms"
    }

    /// Prüft unter Unix den Modus `0700` des Home-Verzeichnisses.
    fn run(&self) -> CheckOutcome {
        if !self.home.exists() {
            return CheckOutcome::Warn(format!(
                "Home-Verzeichnis fehlt, Berechtigungen nicht prüfbar: {}",
                self.home.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match std::fs::metadata(&self.home) {
                Ok(meta) => {
                    let mode = meta.permissions().mode() & 0o777;
                    if mode == 0o700 {
                        CheckOutcome::Ok(format!(
                            "Home-Berechtigungen korrekt (0700): {}",
                            self.home.display()
                        ))
                    } else {
                        CheckOutcome::Warn(format!(
                            "Home-Berechtigungen {mode:04o}, erwartet 0700: {}",
                            self.home.display()
                        ))
                    }
                }
                Err(err) => CheckOutcome::Warn(format!(
                    "Home-Metadaten nicht lesbar ({err}): {}",
                    self.home.display()
                )),
            }
        }
        #[cfg(not(unix))]
        {
            CheckOutcome::Ok(format!(
                "Berechtigungsprüfung auf dieser Plattform nicht anwendbar: {}",
                self.home.display()
            ))
        }
    }
}

/// Baut die Standard-Check-Liste für die harw-Diagnose.
///
/// # Description
/// Stellt die Reihenfolge System → Sandbox → Runtime-Tool-Grenzen →
/// Service-Manager → Home-Existenz → Home-Berechtigungen zusammen. Die
/// Default-Composition übergibt keine Runtime-Evidenz; der Tool-Grenzen-Check
/// meldet deshalb ausdrücklich "unbekannt/nicht konfiguriert".
///
/// # Arguments
/// - `home` (`&Path`): Pfad zum harw-Home-Verzeichnis (`~/.harw`).
///
/// # Returns
/// Ein `Vec<Box<dyn DoctorCheck>>` mit den Standard-Checks.
///
/// # Concurrency
/// Reine Konstruktion; keine I/O, keine Locks.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_install::doctor::default_checks;
///
/// let checks = default_checks(Path::new("/home/u/.harw"));
/// assert_eq!(checks.len(), 6);
/// ```
pub fn default_checks(home: &Path) -> Vec<Box<dyn DoctorCheck>> {
    vec![
        Box::new(SystemCheck),
        Box::new(SandboxCheck),
        Box::new(RuntimeCompositionCheck::new(
            RuntimeCompositionEvidence::not_configured(),
        )),
        Box::new(ServiceManagerCheck),
        Box::new(HomeExistsCheck::new(home)),
        Box::new(HomePermsCheck::new(home)),
    ]
}

/// Führt alle übergebenen Checks aus und sammelt deren Ergebnisse.
///
/// # Description
/// Ruft für jeden Check `run()` auf und paart das Ergebnis mit dessen `id()`.
/// Die Reihenfolge der Eingabe bleibt erhalten.
///
/// # Arguments
/// - `checks` (`&[Box<dyn DoctorCheck>]`): auszuführende Checks (geliehen).
///
/// # Returns
/// Ein `Vec<(String, CheckOutcome)>` mit je einem Eintrag pro Check.
///
/// # Concurrency
/// Führt die Checks sequenziell im aufrufenden Thread aus; read-only.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_install::doctor::{default_checks, run_all};
///
/// let checks = default_checks(Path::new("/home/u/.harw"));
/// let results = run_all(&checks);
/// assert_eq!(results.len(), 6);
/// ```
pub fn run_all(checks: &[Box<dyn DoctorCheck>]) -> Vec<(String, CheckOutcome)> {
    checks
        .iter()
        .map(|check| (check.id().to_owned(), check.run()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein Test-Check, der ein festes Ergebnis liefert.
    struct FixedCheck {
        id: &'static str,
        outcome: CheckOutcome,
    }

    impl DoctorCheck for FixedCheck {
        fn id(&self) -> &str {
            self.id
        }

        fn run(&self) -> CheckOutcome {
            self.outcome.clone()
        }
    }

    #[test]
    fn test_check_outcome_variants_distinct() {
        let ok = CheckOutcome::Ok("a".to_owned());
        let warn = CheckOutcome::Warn("a".to_owned());
        let fail = CheckOutcome::Fail("a".to_owned());
        assert_ne!(ok, warn);
        assert_ne!(warn, fail);
        assert_ne!(ok, fail);
    }

    #[test]
    fn test_check_outcome_matches_pattern() {
        let outcome = CheckOutcome::Warn("x".to_owned());
        match outcome {
            CheckOutcome::Warn(msg) => assert_eq!(msg, "x"),
            other => panic!("erwartete Warn, erhielt {other:?}"),
        }
    }

    #[test]
    fn test_run_all_length_matches_input() {
        let checks: Vec<Box<dyn DoctorCheck>> = vec![
            Box::new(FixedCheck {
                id: "a",
                outcome: CheckOutcome::Ok("ok".to_owned()),
            }),
            Box::new(FixedCheck {
                id: "b",
                outcome: CheckOutcome::Fail("bad".to_owned()),
            }),
        ];
        let results = run_all(&checks);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "a");
        assert_eq!(results[0].1, CheckOutcome::Ok("ok".to_owned()));
        assert_eq!(results[1].0, "b");
    }

    #[test]
    fn test_run_all_empty_is_empty() {
        let checks: Vec<Box<dyn DoctorCheck>> = Vec::new();
        assert!(run_all(&checks).is_empty());
    }

    #[test]
    fn test_default_checks_len_is_six() {
        let checks = default_checks(Path::new("/tmp/harw-doctor-test"));
        assert_eq!(checks.len(), 6);
    }

    #[test]
    fn default_checks_report_missing_runtime_evidence_as_unknown() {
        let checks = default_checks(Path::new("/tmp/harw-doctor-test"));
        let result = checks
            .iter()
            .find(|check| check.id() == "runtime/tool-boundaries")
            .expect("default checks include the runtime boundary check")
            .run();

        assert!(
            matches!(result, CheckOutcome::Warn(message) if message.contains("nicht konfiguriert"))
        );
    }

    #[test]
    fn test_system_check_id_stable() {
        assert_eq!(SystemCheck.id(), "system/os");
    }

    #[test]
    fn test_home_exists_check_missing_fails() {
        let check = HomeExistsCheck::new(Path::new("/nonexistent/harw/home/xyz"));
        match check.run() {
            CheckOutcome::Fail(_) => {}
            other => panic!("erwartete Fail, erhielt {other:?}"),
        }
    }

    #[test]
    fn runtime_composition_without_evidence_is_explicitly_unknown() {
        let check = RuntimeCompositionCheck::new(RuntimeCompositionEvidence::not_configured());

        assert_eq!(
            check.run(),
            CheckOutcome::Warn(
                "Runtime-Tool-Komposition nicht konfiguriert: SpawnContext- und Approval-Grenze unbekannt"
                    .to_owned(),
            )
        );
    }

    #[test]
    fn runtime_composition_fails_when_sensitive_tools_lack_spawn_context() {
        let check = RuntimeCompositionCheck::new(RuntimeCompositionEvidence {
            advertised_tools: Some(vec!["fs.read".to_owned(), "shell.exec".to_owned()]),
            has_trusted_spawn_context: Some(false),
            has_approval_boundary: Some(true),
        });

        assert!(
            matches!(check.run(), CheckOutcome::Fail(message) if message.contains("SpawnContext"))
        );
    }

    #[test]
    fn runtime_composition_fails_when_sensitive_tools_lack_approval_boundary() {
        let check = RuntimeCompositionCheck::new(RuntimeCompositionEvidence {
            advertised_tools: Some(vec!["fs.write".to_owned()]),
            has_trusted_spawn_context: Some(true),
            has_approval_boundary: Some(false),
        });

        assert!(
            matches!(check.run(), CheckOutcome::Fail(message) if message.contains("Approval-Grenze"))
        );
    }

    #[test]
    fn runtime_composition_warns_when_sensitive_tool_boundary_evidence_is_partial() {
        let check = RuntimeCompositionCheck::new(RuntimeCompositionEvidence {
            advertised_tools: Some(vec!["filesystem.write".to_owned()]),
            has_trusted_spawn_context: Some(true),
            has_approval_boundary: None,
        });

        assert!(
            matches!(check.run(), CheckOutcome::Warn(message) if message.contains("unbekannt/nicht konfiguriert"))
        );
    }

    #[test]
    fn runtime_composition_accepts_observed_sensitive_tool_boundaries() {
        let check = RuntimeCompositionCheck::new(RuntimeCompositionEvidence {
            advertised_tools: Some(vec!["shell.exec".to_owned(), "fs.write".to_owned()]),
            has_trusted_spawn_context: Some(true),
            has_approval_boundary: Some(true),
        });

        assert!(matches!(check.run(), CheckOutcome::Ok(message) if message.contains("gebunden")));
    }

    #[test]
    fn runtime_composition_allows_a_tool_surface_without_sensitive_tools() {
        let check = RuntimeCompositionCheck::new(RuntimeCompositionEvidence {
            advertised_tools: Some(vec!["fs.read".to_owned(), "fs.search".to_owned()]),
            has_trusted_spawn_context: None,
            has_approval_boundary: None,
        });

        assert!(
            matches!(check.run(), CheckOutcome::Ok(message) if message.contains("keine Shell- oder Schreibtools"))
        );
    }

    #[test]
    fn sensitive_tool_detection_covers_supported_shell_and_write_names() {
        for tool in ["shell.exec", "fs.write", "filesystem.write"] {
            assert!(
                is_sensitive_tool(tool),
                "{tool} must require runtime guards"
            );
        }
        assert!(!is_sensitive_tool("fs.read"));
    }
}
