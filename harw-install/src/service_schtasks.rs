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

    fn uninstall(&self, name: &str) -> Result<(), ServiceError> {
        let out = Self::schtasks(&["/Delete", "/TN", name, "/F"])?;
        if !out.status.success() {
            return Err(ServiceError::Command {
                command: format!("schtasks /Delete /TN {name}"),
                detail: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(())
    }
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
    use std::path::PathBuf;

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
}
