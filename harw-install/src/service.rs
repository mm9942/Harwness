//! Service-Manager-Abstraktion für den Install-/Betriebs-Lebenszyklus.
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! `harw-install / src/service.rs`.
//!
//! # Verantwortung
//! Dieses Modul besitzt die plattformneutrale Abstraktion zur Verwaltung von
//! Hintergrunddiensten: die Datentypen [`ServiceKind`], [`ServiceSpec`] und
//! [`ServiceStatus`], das Verhaltens-Trait [`ServiceManager`] sowie die Fabrik
//! [`detect_service_manager`]. Die konkreten, plattformspezifischen
//! Implementierungen liegen in `service_systemd.rs`, `service_launchd.rs` und
//! `service_schtasks.rs`. Der [`UnsupportedServiceManager`] bildet den
//! Fallback für nicht unterstützte Plattformen.
//!
//! # Exportierte Typen
//! - [`ServiceKind`]: Klassifikation der Service-Backend-Technologie.
//! - [`ServiceSpec`]: deklarative Beschreibung eines zu installierenden Dienstes.
//! - [`ServiceStatus`]: Laufzeitzustand eines Dienstes.
//! - [`ServiceManager`]: Trait, das install/status/uninstall/render_unit vereint.
//! - [`UnsupportedServiceManager`]: Fallback für `Os::Other`.
//!
//! # Fehlerbehandlung
//! Fehlgeschlagene Operationen liefern [`crate::error::ServiceError`].
//! [`detect_service_manager`] selbst kann nicht fehlschlagen.
//!
//! # Concurrency
//! Alle Typen sind `Send + Sync` und tragen keinen gemeinsam veränderlichen
//! Zustand. Der zurückgegebene `Box<dyn ServiceManager>` kapselt nur
//! unveränderliche Konfiguration.
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::platform::Platform;
//! use harw_install::service::detect_service_manager;
//!
//! let mgr = detect_service_manager(&Platform::detect());
//! println!("{:?}", mgr.kind());
//! ```

use std::path::PathBuf;

use crate::error::ServiceError;
use crate::platform::{Os, Platform};
use crate::service_launchd::LaunchdServiceManager;
use crate::service_schtasks::SchtasksServiceManager;
use crate::service_systemd::SystemdServiceManager;

/// Klassifikation der zugrunde liegenden Service-Backend-Technologie.
///
/// # Description
/// Kennzeichnet, welche Plattform-Mechanik ein [`ServiceManager`] bedient.
/// `Unsupported` steht für Plattformen ohne integrierte Dienstverwaltung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    /// systemd-User-Units (Linux).
    Systemd,
    /// launchd LaunchAgents (macOS).
    Launchd,
    /// Windows Task Scheduler (`schtasks`).
    Schtasks,
    /// Keine unterstützte Service-Verwaltung.
    Unsupported,
}

/// Deklarative Beschreibung eines zu installierenden Dienstes.
///
/// # Description
/// Enthält alle Angaben, die eine konkrete `render_unit`-Implementierung
/// benötigt, um eine Unit-/Plist-/Task-Beschreibung rein (ohne I/O) zu
/// erzeugen.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    /// Eindeutiger Dienstname (auch Dateibasisname der Unit/Plist/Task).
    pub name: String,
    /// Auszuführendes Kommando als Argumentvektor (`exec[0]` = Programmpfad).
    pub exec: Vec<String>,
    /// Arbeitsverzeichnis des Dienstes.
    pub working_dir: PathBuf,
    /// Zusätzliche Umgebungsvariablen als `(Schlüssel, Wert)`-Paare.
    pub env: Vec<(String, String)>,
    /// Wartezeit in Sekunden vor einem Neustart nach Absturz.
    pub restart_sec: u32,
}

/// Laufzeitzustand eines Dienstes.
///
/// # Description
/// Ergebnis einer Statusabfrage über [`ServiceManager::status`].
#[derive(Debug, Clone)]
pub enum ServiceStatus {
    /// Der Dienst ist installiert und aktiv.
    Running,
    /// Der Dienst ist installiert, aber nicht aktiv.
    Stopped,
    /// Der Dienst ist nicht installiert.
    NotInstalled,
}

/// Verhaltens-Trait zur Verwaltung eines Hintergrunddienstes.
///
/// # Description
/// Vereint das reine Rendern der Unit-Beschreibung mit den seiteneffekt-
/// behafteten Operationen Installation, Statusabfrage und Deinstallation.
/// Implementierungen sind je Plattform in eigenen Modulen definiert.
///
/// # Concurrency
/// Implementierungen sind zustandslos bzw. tragen nur unveränderliche
/// Konfiguration und sind damit `Send + Sync`.
pub trait ServiceManager {
    /// Liefert die Backend-Klassifikation dieses Managers.
    ///
    /// # Returns
    /// Den passenden [`ServiceKind`].
    fn kind(&self) -> ServiceKind;

    /// Rendert die plattformspezifische Unit-/Plist-/Task-Beschreibung.
    ///
    /// # Description
    /// REINE Funktion ohne I/O: erzeugt aus `spec` den vollständigen Textinhalt
    /// der Dienstbeschreibung.
    ///
    /// # Arguments
    /// - `spec` (`&ServiceSpec`): Dienstbeschreibung.
    ///
    /// # Returns
    /// Den gerenderten Beschreibungstext als `String`.
    fn render_unit(&self, spec: &ServiceSpec) -> String;

    /// Installiert den Dienst (schreibt Datei, aktiviert ihn).
    ///
    /// # Errors
    /// - [`ServiceError::Io`]: beim Schreiben der Beschreibungsdatei.
    /// - [`ServiceError::Command`]: wenn ein externes Kommando fehlschlägt.
    /// - [`ServiceError::Unsupported`]: auf nicht unterstützten Plattformen.
    fn install(&self, spec: &ServiceSpec) -> Result<(), ServiceError>;

    /// Fragt den Laufzeitzustand des Dienstes ab.
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn die Statusabfrage nicht ausführbar ist.
    /// - [`ServiceError::Unsupported`]: auf nicht unterstützten Plattformen.
    fn status(&self, name: &str) -> Result<ServiceStatus, ServiceError>;

    /// Deinstalliert den Dienst (deaktiviert ihn, entfernt die Datei).
    ///
    /// # Errors
    /// - [`ServiceError::Io`]: beim Löschen der Beschreibungsdatei.
    /// - [`ServiceError::Command`]: wenn ein externes Kommando fehlschlägt.
    /// - [`ServiceError::Unsupported`]: auf nicht unterstützten Plattformen.
    fn uninstall(&self, name: &str) -> Result<(), ServiceError>;
}

/// Wählt anhand der Plattform den passenden [`ServiceManager`].
///
/// # Description
/// Linux → [`SystemdServiceManager`], macOS → [`LaunchdServiceManager`],
/// Windows → [`SchtasksServiceManager`], sonst [`UnsupportedServiceManager`].
///
/// # Arguments
/// - `p` (`&Platform`): erkannte Laufzeitumgebung.
///
/// # Returns
/// Ein `Box<dyn ServiceManager>` für die aktuelle Plattform.
///
/// # Concurrency
/// Reine Fabrikfunktion; hält keine Locks, startet keine Threads.
///
/// # Examples
/// ```rust,no_run
/// use harw_install::platform::Platform;
/// use harw_install::service::detect_service_manager;
///
/// let mgr = detect_service_manager(&Platform::detect());
/// let _ = mgr.kind();
/// ```
pub fn detect_service_manager(p: &Platform) -> Box<dyn ServiceManager> {
    match p.os {
        Os::Linux => Box::new(SystemdServiceManager::new()),
        Os::MacOs => Box::new(LaunchdServiceManager::new()),
        Os::Windows => Box::new(SchtasksServiceManager::new()),
        Os::Other => Box::new(UnsupportedServiceManager::new()),
    }
}

/// Fallback-Manager für Plattformen ohne integrierte Dienstverwaltung.
///
/// # Description
/// Liefert bei [`install`](ServiceManager::install)/[`status`](ServiceManager::status)/
/// [`uninstall`](ServiceManager::uninstall) stets [`ServiceError::Unsupported`].
/// `render_unit` gibt einen erklärenden Kommentar zurück.
///
/// # Concurrency
/// Zustandslos, `Send + Sync`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedServiceManager;

impl UnsupportedServiceManager {
    /// Erzeugt einen neuen [`UnsupportedServiceManager`].
    ///
    /// # Returns
    /// Eine zustandslose Instanz.
    pub fn new() -> Self {
        UnsupportedServiceManager
    }

    /// Baut den einheitlichen [`ServiceError::Unsupported`]-Fehler.
    fn unsupported() -> ServiceError {
        ServiceError::Unsupported {
            detail: "keine unterstützte Service-Verwaltung auf dieser Plattform".to_owned(),
        }
    }
}

impl ServiceManager for UnsupportedServiceManager {
    fn kind(&self) -> ServiceKind {
        ServiceKind::Unsupported
    }

    fn render_unit(&self, spec: &ServiceSpec) -> String {
        format!(
            "# nicht unterstützte Plattform für Dienst '{}'\n",
            spec.name
        )
    }

    fn install(&self, _spec: &ServiceSpec) -> Result<(), ServiceError> {
        Err(Self::unsupported())
    }

    fn status(&self, _name: &str) -> Result<ServiceStatus, ServiceError> {
        Err(Self::unsupported())
    }

    fn uninstall(&self, _name: &str) -> Result<(), ServiceError> {
        Err(Self::unsupported())
    }
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
            restart_sec: 5,
        }
    }

    #[test]
    fn test_detect_service_manager_linux_is_systemd() {
        let p = Platform {
            os: Os::Linux,
            container: false,
            wsl: false,
            termux: false,
            managed: false,
        };
        assert_eq!(detect_service_manager(&p).kind(), ServiceKind::Systemd);
    }

    #[test]
    fn test_detect_service_manager_macos_is_launchd() {
        let p = Platform {
            os: Os::MacOs,
            container: false,
            wsl: false,
            termux: false,
            managed: false,
        };
        assert_eq!(detect_service_manager(&p).kind(), ServiceKind::Launchd);
    }

    #[test]
    fn test_detect_service_manager_windows_is_schtasks() {
        let p = Platform {
            os: Os::Windows,
            container: false,
            wsl: false,
            termux: false,
            managed: false,
        };
        assert_eq!(detect_service_manager(&p).kind(), ServiceKind::Schtasks);
    }

    #[test]
    fn test_detect_service_manager_other_is_unsupported() {
        let p = Platform {
            os: Os::Other,
            container: false,
            wsl: false,
            termux: false,
            managed: false,
        };
        assert_eq!(detect_service_manager(&p).kind(), ServiceKind::Unsupported);
    }

    #[test]
    fn test_unsupported_install_returns_unsupported_error() {
        let mgr = UnsupportedServiceManager::new();
        let err = mgr.install(&spec()).unwrap_err();
        assert!(matches!(err, ServiceError::Unsupported { .. }));
    }

    #[test]
    fn test_unsupported_status_and_uninstall_return_unsupported() {
        let mgr = UnsupportedServiceManager::new();
        assert!(matches!(
            mgr.status("harw").unwrap_err(),
            ServiceError::Unsupported { .. }
        ));
        assert!(matches!(
            mgr.uninstall("harw").unwrap_err(),
            ServiceError::Unsupported { .. }
        ));
    }

    #[test]
    fn test_unsupported_render_unit_mentions_name() {
        let mgr = UnsupportedServiceManager::new();
        assert!(mgr.render_unit(&spec()).contains("harw"));
    }
}
