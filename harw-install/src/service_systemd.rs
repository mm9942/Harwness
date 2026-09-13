//! systemd-User-Service-Manager (Linux).
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! `harw-install / src/service.rs` (Unterpunkt `service_systemd.rs`).
//!
//! # Verantwortung
//! Dieses Modul besitzt die Linux-spezifische Implementierung von
//! [`crate::service::ServiceManager`] auf Basis von systemd-User-Units. Es
//! rendert eine `.service`-Unit rein (ohne I/O) und führt Installation,
//! Statusabfrage und Deinstallation über `systemctl --user` aus.
//!
//! # Exportierte Typen
//! - [`SystemdServiceManager`]: die konkrete Manager-Implementierung.
//!
//! # Fehlerbehandlung
//! Alle seiteneffekt-behafteten Operationen liefern
//! [`crate::error::ServiceError`].
//!
//! # Concurrency
//! [`SystemdServiceManager`] ist zustandslos, `Send + Sync`. `install`/
//! `uninstall`/`status` starten kurzlebige Subprozesse und warten synchron auf
//! deren Ende.
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::service::{ServiceManager, ServiceSpec};
//! use harw_install::service_systemd::SystemdServiceManager;
//! use std::path::PathBuf;
//!
//! let mgr = SystemdServiceManager::new();
//! let spec = ServiceSpec {
//!     name: "harw".to_owned(),
//!     exec: vec!["/usr/bin/harw".to_owned(), "serve".to_owned()],
//!     working_dir: PathBuf::from("/var/lib/harw"),
//!     env: vec![],
//!     restart_sec: 5,
//! };
//! println!("{}", mgr.render_unit(&spec));
//! ```

use std::path::PathBuf;
use std::process::Command;

use crate::error::ServiceError;
use crate::service::{ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};

/// systemd-User-Service-Manager.
///
/// # Description
/// Implementiert [`ServiceManager`] für Linux über `systemctl --user`. Die
/// Unit-Datei wird unter `~/.config/systemd/user/<name>.service` abgelegt.
///
/// # Concurrency
/// Zustandslos, `Send + Sync`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemdServiceManager;

impl SystemdServiceManager {
    /// Erzeugt einen neuen [`SystemdServiceManager`].
    ///
    /// # Returns
    /// Eine zustandslose Instanz.
    pub fn new() -> Self {
        SystemdServiceManager
    }

    /// Berechnet den Pfad der Unit-Datei für einen Dienstnamen.
    ///
    /// # Errors
    /// - [`ServiceError::Io`]: wenn das Home-Verzeichnis nicht bestimmbar ist.
    fn unit_path(name: &str) -> Result<PathBuf, ServiceError> {
        let home = home_dir().ok_or_else(|| ServiceError::Io {
            path: "$HOME".to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Home-Verzeichnis nicht bestimmbar",
            ),
        })?;
        Ok(home
            .join(".config")
            .join("systemd")
            .join("user")
            .join(format!("{name}.service")))
    }

    /// Führt `systemctl --user <args…>` aus und prüft den Exit-Status.
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn der Aufruf scheitert oder != 0 endet.
    fn systemctl(args: &[&str]) -> Result<std::process::Output, ServiceError> {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .map_err(|e| ServiceError::Command {
                command: format!("systemctl --user {}", args.join(" ")),
                detail: e.to_string(),
            })?;
        Ok(output)
    }
}

impl ServiceManager for SystemdServiceManager {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Systemd
    }

    fn render_unit(&self, spec: &ServiceSpec) -> String {
        render_systemd_unit(spec)
    }

    fn install(&self, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let path = Self::unit_path(&spec.name)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ServiceError::Io {
                path: parent.display().to_string(),
                source: e,
            })?;
        }
        std::fs::write(&path, self.render_unit(spec)).map_err(|e| ServiceError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let out = Self::systemctl(&["enable", "--now", &spec.name])?;
        if !out.status.success() {
            return Err(ServiceError::Command {
                command: format!("systemctl --user enable --now {}", spec.name),
                detail: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(())
    }

    fn status(&self, name: &str) -> Result<ServiceStatus, ServiceError> {
        let out = Self::systemctl(&["is-active", name])?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let state = stdout.trim();
        match state {
            "active" => Ok(ServiceStatus::Running),
            "inactive" | "failed" | "deactivating" | "activating" | "reloading" => {
                Ok(ServiceStatus::Stopped)
            }
            _ => Ok(ServiceStatus::NotInstalled),
        }
    }

    fn uninstall(&self, name: &str) -> Result<(), ServiceError> {
        let out = Self::systemctl(&["disable", "--now", name])?;
        if !out.status.success() {
            return Err(ServiceError::Command {
                command: format!("systemctl --user disable --now {name}"),
                detail: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let path = Self::unit_path(name)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ServiceError::Io {
                path: path.display().to_string(),
                source: e,
            }),
        }
    }
}

/// Rendert eine systemd-User-Unit rein (ohne I/O).
///
/// # Description
/// Erzeugt die `[Unit]`/`[Service]`/`[Install]`-Abschnitte aus `spec`:
/// `ExecStart` aus `spec.exec`, `WorkingDirectory`, je eine `Environment=`-Zeile
/// pro Variable, `Restart=always`, `RestartSec=<restart_sec>` und
/// `WantedBy=default.target`.
///
/// # Arguments
/// - `spec` (`&ServiceSpec`): Dienstbeschreibung.
///
/// # Returns
/// Den vollständigen Unit-Text als `String`.
fn render_systemd_unit(spec: &ServiceSpec) -> String {
    let exec_start = spec.exec.join(" ");
    let mut out = String::new();
    out.push_str("[Unit]\n");
    out.push_str(&format!("Description=harw service {}\n", spec.name));
    out.push_str("After=network.target\n\n");
    out.push_str("[Service]\n");
    out.push_str("Type=simple\n");
    out.push_str(&format!("ExecStart={exec_start}\n"));
    out.push_str(&format!(
        "WorkingDirectory={}\n",
        spec.working_dir.display()
    ));
    for (key, value) in &spec.env {
        out.push_str(&format!("Environment={key}={value}\n"));
    }
    out.push_str("Restart=always\n");
    out.push_str(&format!("RestartSec={}\n\n", spec.restart_sec));
    out.push_str("[Install]\n");
    out.push_str("WantedBy=default.target\n");
    out
}

/// Bestimmt das Home-Verzeichnis aus den Umgebungsvariablen.
///
/// Nutzt `HOME` (Unix) und fällt auf `USERPROFILE` (Windows) zurück.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ServiceSpec {
        ServiceSpec {
            name: "harw".to_owned(),
            exec: vec!["/usr/bin/harw".to_owned(), "serve".to_owned()],
            working_dir: PathBuf::from("/var/lib/harw"),
            env: vec![("RUST_LOG".to_owned(), "info".to_owned())],
            restart_sec: 7,
        }
    }

    #[test]
    fn test_render_unit_contains_exec_start_command() {
        let unit = render_systemd_unit(&spec());
        assert!(unit.contains("ExecStart=/usr/bin/harw serve"));
    }

    #[test]
    fn test_render_unit_contains_restart_sec() {
        let unit = render_systemd_unit(&spec());
        assert!(unit.contains("RestartSec=7"));
    }

    #[test]
    fn test_render_unit_contains_environment_line() {
        let unit = render_systemd_unit(&spec());
        assert!(unit.contains("Environment=RUST_LOG=info"));
    }

    #[test]
    fn test_render_unit_contains_wanted_by_and_working_dir() {
        let unit = render_systemd_unit(&spec());
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("WorkingDirectory=/var/lib/harw"));
        assert!(unit.contains("Restart=always"));
    }

    #[test]
    fn test_kind_is_systemd() {
        assert_eq!(SystemdServiceManager::new().kind(), ServiceKind::Systemd);
    }
}
