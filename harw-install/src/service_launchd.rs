//! launchd-LaunchAgent-Service-Manager (macOS).
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! `harw-install / src/service.rs` (Unterpunkt `service_launchd.rs`).
//!
//! # Verantwortung
//! Dieses Modul besitzt die macOS-spezifische Implementierung von
//! [`crate::service::ServiceManager`] auf Basis von launchd-LaunchAgents. Es
//! rendert ein LaunchAgent-Plist (XML) rein (ohne I/O) und führt Installation,
//! Statusabfrage und Deinstallation über `launchctl` aus.
//!
//! # Exportierte Typen
//! - [`LaunchdServiceManager`]: die konkrete Manager-Implementierung.
//!
//! # Fehlerbehandlung
//! Alle seiteneffekt-behafteten Operationen liefern
//! [`crate::error::ServiceError`].
//!
//! # Concurrency
//! [`LaunchdServiceManager`] ist zustandslos, `Send + Sync`. `install`/
//! `uninstall`/`status` starten kurzlebige Subprozesse und warten synchron auf
//! deren Ende.
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::service::{ServiceManager, ServiceSpec};
//! use harw_install::service_launchd::LaunchdServiceManager;
//! use std::path::PathBuf;
//!
//! let mgr = LaunchdServiceManager::new();
//! let spec = ServiceSpec {
//!     name: "harw".to_owned(),
//!     exec: vec!["/usr/local/bin/harw".to_owned(), "serve".to_owned()],
//!     working_dir: PathBuf::from("/Users/u/harw"),
//!     env: vec![],
//!     restart_sec: 5,
//! };
//! println!("{}", mgr.render_unit(&spec));
//! ```

use std::path::PathBuf;
use std::process::Command;

use crate::error::ServiceError;
use crate::service::{ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};

/// launchd-LaunchAgent-Service-Manager.
///
/// # Description
/// Implementiert [`ServiceManager`] für macOS über `launchctl`. Das Plist wird
/// unter `~/Library/LaunchAgents/<name>.plist` abgelegt.
///
/// # Concurrency
/// Zustandslos, `Send + Sync`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LaunchdServiceManager;

impl LaunchdServiceManager {
    /// Erzeugt einen neuen [`LaunchdServiceManager`].
    ///
    /// # Returns
    /// Eine zustandslose Instanz.
    pub fn new() -> Self {
        LaunchdServiceManager
    }

    /// Berechnet den Pfad des LaunchAgent-Plist für einen Dienstnamen.
    ///
    /// # Errors
    /// - [`ServiceError::Io`]: wenn das Home-Verzeichnis nicht bestimmbar ist.
    fn plist_path(name: &str) -> Result<PathBuf, ServiceError> {
        let home = home_dir().ok_or_else(|| ServiceError::Io {
            path: "$HOME".to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Home-Verzeichnis nicht bestimmbar",
            ),
        })?;
        Ok(home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{name}.plist")))
    }

    /// Führt `launchctl <args…>` aus und prüft den Exit-Status.
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn der Aufruf scheitert oder != 0 endet.
    fn launchctl(args: &[&str]) -> Result<std::process::Output, ServiceError> {
        Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|e| ServiceError::Command {
                command: format!("launchctl {}", args.join(" ")),
                detail: e.to_string(),
            })
    }
}

impl ServiceManager for LaunchdServiceManager {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Launchd
    }

    fn render_unit(&self, spec: &ServiceSpec) -> String {
        render_launchd_plist(spec)
    }

    fn install(&self, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let path = Self::plist_path(&spec.name)?;
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
        let path_str = path.display().to_string();
        let out = Self::launchctl(&["load", "-w", &path_str])?;
        if !out.status.success() {
            return Err(ServiceError::Command {
                command: format!("launchctl load -w {path_str}"),
                detail: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(())
    }

    fn status(&self, name: &str) -> Result<ServiceStatus, ServiceError> {
        let out = Self::launchctl(&["list"])?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains(name) {
            Ok(ServiceStatus::Running)
        } else {
            Ok(ServiceStatus::NotInstalled)
        }
    }

    fn uninstall(&self, name: &str) -> Result<(), ServiceError> {
        let path = Self::plist_path(name)?;
        let path_str = path.display().to_string();
        let out = Self::launchctl(&["unload", "-w", &path_str])?;
        if !out.status.success() {
            return Err(ServiceError::Command {
                command: format!("launchctl unload -w {path_str}"),
                detail: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ServiceError::Io {
                path: path_str,
                source: e,
            }),
        }
    }
}

/// Rendert ein launchd-LaunchAgent-Plist rein (ohne I/O).
///
/// # Description
/// Erzeugt ein XML-Plist mit `Label` (= `spec.name`), `ProgramArguments` (aus
/// `spec.exec`), `WorkingDirectory`, optionalen `EnvironmentVariables`,
/// `KeepAlive` (true) und `RunAtLoad` (true).
///
/// # Arguments
/// - `spec` (`&ServiceSpec`): Dienstbeschreibung.
///
/// # Returns
/// Den vollständigen Plist-Text als `String`.
fn render_launchd_plist(spec: &ServiceSpec) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    out.push_str("<plist version=\"1.0\">\n");
    out.push_str("<dict>\n");
    out.push_str("  <key>Label</key>\n");
    out.push_str(&format!("  <string>{}</string>\n", xml_escape(&spec.name)));
    out.push_str("  <key>ProgramArguments</key>\n");
    out.push_str("  <array>\n");
    for arg in &spec.exec {
        out.push_str(&format!("    <string>{}</string>\n", xml_escape(arg)));
    }
    out.push_str("  </array>\n");
    out.push_str("  <key>WorkingDirectory</key>\n");
    out.push_str(&format!(
        "  <string>{}</string>\n",
        xml_escape(&spec.working_dir.display().to_string())
    ));
    if !spec.env.is_empty() {
        out.push_str("  <key>EnvironmentVariables</key>\n");
        out.push_str("  <dict>\n");
        for (key, value) in &spec.env {
            out.push_str(&format!("    <key>{}</key>\n", xml_escape(key)));
            out.push_str(&format!("    <string>{}</string>\n", xml_escape(value)));
        }
        out.push_str("  </dict>\n");
    }
    out.push_str("  <key>KeepAlive</key>\n");
    out.push_str("  <true/>\n");
    out.push_str("  <key>RunAtLoad</key>\n");
    out.push_str("  <true/>\n");
    out.push_str(&format!(
        "  <key>ThrottleInterval</key>\n  <integer>{}</integer>\n",
        spec.restart_sec
    ));
    out.push_str("</dict>\n");
    out.push_str("</plist>\n");
    out
}

/// Ersetzt die für XML-Textinhalte reservierten Zeichen durch Entitäten.
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
            name: "com.harw.agent".to_owned(),
            exec: vec!["/usr/local/bin/harw".to_owned(), "serve".to_owned()],
            working_dir: PathBuf::from("/Users/u/harw"),
            env: vec![("RUST_LOG".to_owned(), "info".to_owned())],
            restart_sec: 9,
        }
    }

    #[test]
    fn test_render_unit_contains_label() {
        let plist = render_launchd_plist(&spec());
        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("<string>com.harw.agent</string>"));
    }

    #[test]
    fn test_render_unit_contains_command_argument() {
        let plist = render_launchd_plist(&spec());
        assert!(plist.contains("<string>/usr/local/bin/harw</string>"));
        assert!(plist.contains("<string>serve</string>"));
    }

    #[test]
    fn test_render_unit_contains_restart_sec() {
        let plist = render_launchd_plist(&spec());
        assert!(plist.contains("<integer>9</integer>"));
    }

    #[test]
    fn test_render_unit_contains_run_at_load_and_keepalive() {
        let plist = render_launchd_plist(&spec());
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn test_kind_is_launchd() {
        assert_eq!(LaunchdServiceManager::new().kind(), ServiceKind::Launchd);
    }
}
