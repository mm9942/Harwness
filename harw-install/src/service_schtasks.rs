//! Windows-Task-Scheduler-Service-Manager (`schtasks`).
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! `harw-install / src/service.rs` (Unterpunkt `service_schtasks.rs`).
//!
//! # Verantwortung
//! Dieses Modul besitzt die Windows-spezifische Implementierung von
//! [`crate::service::ServiceManager`] auf Basis des Task Schedulers. Es rendert
//! eine einfache Task-Beschreibung (XML) rein (ohne I/O) und führt Installation,
//! Statusabfrage und Deinstallation über das `schtasks`-Kommando aus. Der Code
//! ist auf allen Plattformen kompilierbar; sinnvoll ausführbar nur unter Windows.
//! Die Deinstallation ist idempotent: `schtasks /End` beendet eine laufende
//! Instanz (Fehlschlag wird ignoriert), `schtasks /Delete … /F` entfernt den
//! Task (ein nicht vorhandener Task gilt als Erfolg); optional räumt
//! [`SchtasksServiceManager::uninstall_with_runtime_dir`] die nach dem Dienst
//! benannten Einträge im `run/`-Verzeichnis auf.
//!
//! # Exportierte Typen
//! - [`SchtasksServiceManager`]: die konkrete Manager-Implementierung.
//!
//! # Fehlerbehandlung
//! Alle seiteneffekt-behafteten Operationen liefern
//! [`crate::error::ServiceError`].
//!
//! # Concurrency
//! [`SchtasksServiceManager`] ist zustandslos, `Send + Sync`. `install`/
//! `uninstall`/`status` starten kurzlebige Subprozesse und warten synchron auf
//! deren Ende.
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::service::{ServiceManager, ServiceSpec};
//! use harw_install::service_schtasks::SchtasksServiceManager;
//! use std::path::PathBuf;
//!
//! let mgr = SchtasksServiceManager::new();
//! let spec = ServiceSpec {
//!     name: "harw".to_owned(),
//!     exec: vec!["C:/harw/harw.exe".to_owned(), "serve".to_owned()],
//!     working_dir: PathBuf::from("C:/harw"),
//!     env: vec![],
//!     restart_sec: 5,
//! };
//! println!("{}", mgr.render_unit(&spec));
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::ServiceError;
use crate::service::{ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};

/// Windows-Task-Scheduler-Service-Manager.
///
/// # Description
/// Implementiert [`ServiceManager`] für Windows über `schtasks`. Der Task wird
/// per `schtasks /Create` mit dem in `spec.exec` gerenderten Kommando angelegt.
///
/// # Concurrency
/// Zustandslos, `Send + Sync`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SchtasksServiceManager;

impl SchtasksServiceManager {
    /// Erzeugt einen neuen [`SchtasksServiceManager`].
    ///
    /// # Returns
    /// Eine zustandslose Instanz.
    pub fn new() -> Self {
        SchtasksServiceManager
    }

    /// Führt `schtasks <args…>` aus und liefert die rohe Ausgabe.
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn der Aufruf gar nicht startet.
    fn schtasks(args: &[&str]) -> Result<std::process::Output, ServiceError> {
        Command::new("schtasks")
            .args(args)
            .output()
            .map_err(|e| ServiceError::Command {
                command: format!("schtasks {}", args.join(" ")),
                detail: e.to_string(),
            })
    }

    /// Deinstalliert den Task und räumt zusätzlich Laufzeitdateien auf.
    ///
    /// # Description
    /// Wie [`ServiceManager::uninstall`] (`schtasks /End`, `schtasks /Delete`);
    /// danach werden in `run_dir` die nach dem Dienst benannten Laufzeit-
    /// einträge `<name>.pid` und `<name>.sock` entfernt (siehe
    /// `runtime_entries`). Das Verzeichnis selbst und alle anderen Einträge
    /// bleiben unangetastet. Fehlende Dateien gelten als bereits entfernt.
    ///
    /// Hinweis: `install` dieses Backends schreibt selbst nichts nach `run/`;
    /// die Einträge stammen ggf. vom gestarteten Dienst bzw. vom PID-Rückfall
    /// der CLI (`<home>/run/<name>.pid`) und würden sonst verwaist liegen
    /// bleiben.
    ///
    /// # Arguments
    /// - `name` (`&str`): Dienstname (= Task-Name).
    /// - `run_dir` (`&Path`): Laufzeitverzeichnis, typisch `<harw-home>/run`.
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn `schtasks` nicht startbar ist oder
    ///   `/Delete` mit einem anderen Fehler als „Task nicht gefunden" scheitert.
    /// - [`ServiceError::Io`]: wenn ein vorhandener Eintrag nicht löschbar ist.
    pub fn uninstall_with_runtime_dir(
        &self,
        name: &str,
        run_dir: &Path,
    ) -> Result<(), ServiceError> {
        self.uninstall(name)?;
        remove_files_if_present(&runtime_entries(run_dir, name))
    }
}

impl ServiceManager for SchtasksServiceManager {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Schtasks
    }

    fn render_unit(&self, spec: &ServiceSpec) -> String {
        render_schtasks_task(spec)
    }

    fn install(&self, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let command_line = spec.exec.join(" ");
        let out = Self::schtasks(&[
            "/Create",
            "/TN",
            &spec.name,
            "/TR",
            &command_line,
            "/SC",
            "ONLOGON",
            "/RL",
            "HIGHEST",
            "/F",
        ])?;
        if !out.status.success() {
            return Err(ServiceError::Command {
                command: format!("schtasks /Create /TN {}", spec.name),
                detail: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(())
    }

    fn status(&self, name: &str) -> Result<ServiceStatus, ServiceError> {
        let out = Self::schtasks(&["/Query", "/TN", name])?;
        if !out.status.success() {
            return Ok(ServiceStatus::NotInstalled);
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains("Running") {
            Ok(ServiceStatus::Running)
        } else {
            Ok(ServiceStatus::Stopped)
        }
    }

    /// Beendet und löscht den Task (idempotent).
    ///
    /// Zuerst `schtasks /End /TN <name>`; scheitert das (Task läuft nicht oder
    /// existiert nicht), wird das ignoriert. Danach `schtasks /Delete /TN
    /// <name> /F`; meldet `schtasks`, dass der Task nicht gefunden wurde, gilt
    /// das als Erfolg.
    fn uninstall(&self, name: &str) -> Result<(), ServiceError> {
        let end = end_args(name);
        let end_refs: Vec<&str> = end.iter().map(String::as_str).collect();
        // Exit-Status bewusst ignoriert: ein nicht laufender Task ist kein Fehler.
        let _ = Self::schtasks(&end_refs)?;

        let delete = delete_args(name);
        let delete_refs: Vec<&str> = delete.iter().map(String::as_str).collect();
        let out = Self::schtasks(&delete_refs)?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if is_task_missing(&stderr) || is_task_missing(&stdout) {
            return Ok(());
        }
        Err(ServiceError::Command {
            command: format!("schtasks {}", delete.join(" ")),
            detail: stderr.into_owned(),
        })
    }
}

/// Argumente für `schtasks /End /TN <name>` (rein, ohne Programmnamen).
///
/// # Arguments
/// - `name` (`&str`): Task-Name.
fn end_args(name: &str) -> Vec<String> {
    vec!["/End".to_owned(), "/TN".to_owned(), name.to_owned()]
}

/// Argumente für `schtasks /Delete /TN <name> /F` (rein, ohne Programmnamen).
///
/// # Arguments
/// - `name` (`&str`): Task-Name.
fn delete_args(name: &str) -> Vec<String> {
    vec![
        "/Delete".to_owned(),
        "/TN".to_owned(),
        name.to_owned(),
        "/F".to_owned(),
    ]
}

/// Erkennt, ob eine `schtasks`-Fehlermeldung nur „Task nicht gefunden"
/// bedeutet (rein).
///
/// # Description
/// `schtasks` endet bei jedem Fehler mit Exit-Code 1; ein fehlender Task ist
/// nur am Text erkennbar („The system cannot find the file specified." bzw.
/// „The specified task name … does not exist in the system."; deutsch „Das
/// System kann die angegebene Datei nicht finden.").
///
/// # Arguments
/// - `output` (`&str`): Standardfehler bzw. -ausgabe von `schtasks`.
fn is_task_missing(output: &str) -> bool {
    let lower = output.to_lowercase();
    [
        "cannot find",
        "does not exist",
        "nicht finden",
        "nicht gefunden",
        "ist nicht vorhanden",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Laufzeiteinträge eines Dienstes in `run_dir` (rein): `<name>.pid` und
/// `<name>.sock`.
///
/// # Arguments
/// - `run_dir` (`&Path`): Laufzeitverzeichnis, typisch `<harw-home>/run`.
/// - `name` (`&str`): Dienstname.
fn runtime_entries(run_dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![
        run_dir.join(format!("{name}.pid")),
        run_dir.join(format!("{name}.sock")),
    ]
}

/// Löscht die angegebenen Dateien; fehlende Dateien sind kein Fehler.
///
/// # Errors
/// - [`ServiceError::Io`]: beim ersten Löschfehler außer `NotFound`.
fn remove_files_if_present(paths: &[PathBuf]) -> Result<(), ServiceError> {
    for path in paths {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ServiceError::Io {
                    path: path.display().to_string(),
                    source: e,
                });
            }
        }
    }
    Ok(())
}

/// Rendert eine einfache Task-Scheduler-Beschreibung (XML) rein (ohne I/O).
///
/// # Description
/// Erzeugt ein kompaktes Task-XML mit `Command` (= `spec.exec[0]`),
/// `Arguments` (Rest von `spec.exec`), `WorkingDirectory` und einer
/// `RestartOnFailure`-Angabe basierend auf `spec.restart_sec`.
///
/// # Arguments
/// - `spec` (`&ServiceSpec`): Dienstbeschreibung.
///
/// # Returns
/// Den Task-XML-Text als `String`.
fn render_schtasks_task(spec: &ServiceSpec) -> String {
    let command = spec.exec.first().map(String::as_str).unwrap_or("");
    let arguments = if spec.exec.len() > 1 {
        spec.exec[1..].join(" ")
    } else {
        String::new()
    };
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n");
    out.push_str("<Task version=\"1.2\">\n");
    out.push_str("  <RegistrationInfo>\n");
    out.push_str(&format!(
        "    <Description>harw service {}</Description>\n",
        xml_escape(&spec.name)
    ));
    out.push_str("  </RegistrationInfo>\n");
    out.push_str("  <Settings>\n");
    out.push_str("    <RestartOnFailure>\n");
    out.push_str(&format!(
        "      <Interval>PT{}S</Interval>\n",
        spec.restart_sec
    ));
    out.push_str("      <Count>999</Count>\n");
    out.push_str("    </RestartOnFailure>\n");
    out.push_str("  </Settings>\n");
    out.push_str("  <Actions>\n");
    out.push_str("    <Exec>\n");
    out.push_str(&format!(
        "      <Command>{}</Command>\n",
        xml_escape(command)
    ));
    if !arguments.is_empty() {
        out.push_str(&format!(
            "      <Arguments>{}</Arguments>\n",
            xml_escape(&arguments)
        ));
    }
    out.push_str(&format!(
        "      <WorkingDirectory>{}</WorkingDirectory>\n",
        xml_escape(&spec.working_dir.display().to_string())
    ));
    out.push_str("    </Exec>\n");
    out.push_str("  </Actions>\n");
    out.push_str("</Task>\n");
    out
}

/// Ersetzt die für XML-Textinhalte reservierten Zeichen durch Entitäten.
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn spec() -> ServiceSpec {
        ServiceSpec {
            name: "harw".to_owned(),
            exec: vec!["C:/harw/harw.exe".to_owned(), "serve".to_owned()],
            working_dir: PathBuf::from("C:/harw"),
            env: vec![],
            restart_sec: 11,
        }
    }

    #[test]
    fn test_render_unit_contains_command() {
        let task = render_schtasks_task(&spec());
        assert!(task.contains("<Command>C:/harw/harw.exe</Command>"));
    }

    #[test]
    fn test_render_unit_contains_arguments() {
        let task = render_schtasks_task(&spec());
        assert!(task.contains("<Arguments>serve</Arguments>"));
    }

    #[test]
    fn test_render_unit_contains_restart_sec() {
        let task = render_schtasks_task(&spec());
        assert!(task.contains("PT11S"));
    }

    #[test]
    fn test_render_unit_contains_working_directory() {
        let task = render_schtasks_task(&spec());
        assert!(task.contains("<WorkingDirectory>C:/harw</WorkingDirectory>"));
    }

    #[test]
    fn test_kind_is_schtasks() {
        assert_eq!(SchtasksServiceManager::new().kind(), ServiceKind::Schtasks);
    }

    /// Legt ein eindeutiges temporäres Verzeichnis an.
    fn temp_dir(tag: &str) -> TestResult<PathBuf> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "harw-schtasks-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).map_err(ctx("temp dir anlegen"))?;
        Ok(dir)
    }

    #[test]
    fn test_end_args_target_task_name() {
        assert_eq!(
            end_args("harw"),
            vec!["/End".to_owned(), "/TN".to_owned(), "harw".to_owned()]
        );
    }

    #[test]
    fn test_delete_args_force_delete_task_name() {
        assert_eq!(
            delete_args("harw"),
            vec![
                "/Delete".to_owned(),
                "/TN".to_owned(),
                "harw".to_owned(),
                "/F".to_owned(),
            ]
        );
    }

    #[test]
    fn test_is_task_missing_recognises_messages() {
        assert!(is_task_missing(
            "ERROR: The system cannot find the file specified.\r\n"
        ));
        assert!(is_task_missing(
            "ERROR: The specified task name \"\\harw\" does not exist in the system.\r\n"
        ));
        assert!(is_task_missing(
            "FEHLER: Das System kann die angegebene Datei nicht finden.\r\n"
        ));
        assert!(!is_task_missing("ERROR: Access is denied.\r\n"));
        assert!(!is_task_missing(""));
    }

    #[test]
    fn test_runtime_entries_are_named_after_service() {
        let entries = runtime_entries(Path::new("/h/.harw/run"), "harw-gateway");
        assert_eq!(
            entries,
            vec![
                PathBuf::from("/h/.harw/run/harw-gateway.pid"),
                PathBuf::from("/h/.harw/run/harw-gateway.sock"),
            ]
        );
    }

    #[test]
    fn test_remove_files_if_present_removes_owned_and_keeps_others() -> TestResult {
        let home = temp_dir("remove")?;
        let run_dir = home.join("run");
        std::fs::create_dir_all(&run_dir).map_err(ctx("run anlegen"))?;
        std::fs::write(run_dir.join("harw.pid"), "42").map_err(ctx("pid"))?;
        std::fs::write(run_dir.join("harw.sock"), "").map_err(ctx("sock"))?;
        std::fs::write(run_dir.join("other.pid"), "7").map_err(ctx("fremd"))?;

        let targets = runtime_entries(&run_dir, "harw");
        remove_files_if_present(&targets).map_err(ctx("erstes Entfernen"))?;

        assert!(!run_dir.join("harw.pid").exists());
        assert!(!run_dir.join("harw.sock").exists());
        assert!(run_dir.join("other.pid").exists());
        assert!(run_dir.exists());

        // Idempotent: ein zweiter Lauf auf fehlende Dateien ist kein Fehler.
        remove_files_if_present(&targets).map_err(ctx("zweites Entfernen"))?;

        std::fs::remove_dir_all(&home).map_err(ctx("aufräumen"))?;
        Ok(())
    }

    #[test]
    fn test_remove_files_if_present_reports_non_notfound_error() -> TestResult {
        let home = temp_dir("dir-error")?;
        // Ein Verzeichnis lässt sich nicht per remove_file löschen.
        let blocker = home.join("harw.pid");
        std::fs::create_dir_all(&blocker).map_err(ctx("blocker anlegen"))?;
        let result = remove_files_if_present(&[blocker]);
        assert!(matches!(result, Err(ServiceError::Io { .. })));
        std::fs::remove_dir_all(&home).map_err(ctx("aufräumen"))?;
        Ok(())
    }
}
