//! launchd-LaunchAgent-Service-Manager (macOS).
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! `harw-install / src/service.rs` (Unterpunkt `service_launchd.rs`).
//!
//! # Verantwortung
//! Dieses Modul besitzt die macOS-spezifische Implementierung von
//! [`crate::service::ServiceManager`] auf Basis von launchd-LaunchAgents. Es
//! rendert ein LaunchAgent-Plist (XML) rein (ohne I/O) und führt Installation,
//! Statusabfrage und Deinstallation über `launchctl` aus. Die Deinstallation
//! ist idempotent: `launchctl bootout` (ein nicht geladener Agent wird
//! ignoriert), danach wird das Plist gelöscht; optional räumt
//! [`LaunchdServiceManager::uninstall_with_runtime_dir`] die nach dem Dienst
//! benannten Einträge im `run/`-Verzeichnis auf.
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

use std::path::{Path, PathBuf};
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
        Ok(plist_path_in(&home, name))
    }

    /// Führt `launchctl <args…>` aus (ohne Prüfung des Exit-Status).
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn der Prozess nicht startbar ist.
    fn launchctl(args: &[&str]) -> Result<std::process::Output, ServiceError> {
        Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|e| ServiceError::Command {
                command: format!("launchctl {}", args.join(" ")),
                detail: e.to_string(),
            })
    }

    /// Ermittelt die numerische UID des aufrufenden Nutzers über `id -u`.
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn `id -u` scheitert oder keine Zahl liefert.
    fn current_uid() -> Result<u32, ServiceError> {
        let out = Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| ServiceError::Command {
                command: "id -u".to_owned(),
                detail: e.to_string(),
            })?;
        if !out.status.success() {
            return Err(ServiceError::Command {
                command: "id -u".to_owned(),
                detail: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        parse_uid(&String::from_utf8_lossy(&out.stdout))
    }

    /// Deinstalliert den Dienst und räumt zusätzlich Laufzeitdateien auf.
    ///
    /// # Description
    /// Wie [`ServiceManager::uninstall`] (`launchctl bootout`, Plist löschen);
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
    /// - `name` (`&str`): Dienstname (= launchd-Label).
    /// - `run_dir` (`&Path`): Laufzeitverzeichnis, typisch `<harw-home>/run`.
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn `launchctl bootout` mit einem anderen
    ///   Fehler als „nicht geladen" scheitert oder die UID nicht bestimmbar ist.
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

    /// Entlädt den LaunchAgent und entfernt sein Plist (idempotent).
    ///
    /// `launchctl bootout gui/<uid>/<name>` beendet den Dienst dauerhaft (trotz
    /// `KeepAlive`); ist der Agent nicht geladen, wird das ignoriert. Danach
    /// wird `~/Library/LaunchAgents/<name>.plist` gelöscht; fehlt es, ist das
    /// kein Fehler.
    fn uninstall(&self, name: &str) -> Result<(), ServiceError> {
        let path = Self::plist_path(name)?;
        let uid = Self::current_uid()?;
        let args = bootout_args(uid, name);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = Self::launchctl(&arg_refs)?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() && !is_not_loaded(out.status.code(), &stderr) {
            return Err(ServiceError::Command {
                command: format!("launchctl {}", args.join(" ")),
                detail: stderr.into_owned(),
            });
        }
        remove_files_if_present(&[path])
    }
}

/// Pfad des LaunchAgent-Plist `<home>/Library/LaunchAgents/<name>.plist` (rein).
///
/// # Arguments
/// - `home` (`&Path`): Home-Verzeichnis des Nutzers.
/// - `name` (`&str`): Dienstname (= launchd-Label).
fn plist_path_in(home: &Path, name: &str) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{name}.plist"))
}

/// Argumente für `launchctl bootout gui/<uid>/<name>` (rein, ohne Programmnamen).
///
/// # Arguments
/// - `uid` (`u32`): UID des angemeldeten Nutzers (GUI-Domain).
/// - `name` (`&str`): Dienstname (= launchd-Label).
fn bootout_args(uid: u32, name: &str) -> Vec<String> {
    vec!["bootout".to_owned(), format!("gui/{uid}/{name}")]
}

/// Erkennt, ob ein fehlgeschlagenes `launchctl bootout` nur „nicht geladen"
/// bedeutet (rein).
///
/// # Description
/// launchd meldet einen nicht geladenen Dienst je nach macOS-Version mit
/// Exit-Code 3 (`ESRCH`, „No such process") oder 113 („Could not find
/// specified service"); ältere Versionen nur über den Fehlertext.
///
/// # Arguments
/// - `code` (`Option<i32>`): Exit-Code des Prozesses.
/// - `stderr` (`&str`): Standardfehler von `launchctl`.
fn is_not_loaded(code: Option<i32>, stderr: &str) -> bool {
    if matches!(code, Some(3 | 113)) {
        return true;
    }
    let lower = stderr.to_ascii_lowercase();
    [
        "no such process",
        "could not find specified service",
        "not loaded",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Liest die numerische UID aus der Ausgabe von `id -u` (rein).
///
/// # Errors
/// - [`ServiceError::Command`]: wenn die Ausgabe keine gültige UID ist.
fn parse_uid(stdout: &str) -> Result<u32, ServiceError> {
    stdout.trim().parse().map_err(|e| ServiceError::Command {
        command: "id -u".to_owned(),
        detail: format!("UID nicht lesbar ({:?}): {e}", stdout.trim()),
    })
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
    use crate::test_support::{TestError, TestResult, ctx};

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

    /// Legt ein eindeutiges temporäres Verzeichnis an.
    fn temp_dir(tag: &str) -> TestResult<PathBuf> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("harw-launchd-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(ctx("temp dir anlegen"))?;
        Ok(dir)
    }

    #[test]
    fn test_plist_path_in_uses_launch_agents_dir() {
        let p = plist_path_in(Path::new("/Users/u"), "com.harw.agent");
        assert_eq!(
            p,
            PathBuf::from("/Users/u/Library/LaunchAgents/com.harw.agent.plist")
        );
    }

    #[test]
    fn test_bootout_args_target_gui_domain_label() {
        assert_eq!(
            bootout_args(501, "com.harw.agent"),
            vec!["bootout".to_owned(), "gui/501/com.harw.agent".to_owned()]
        );
    }

    #[test]
    fn test_is_not_loaded_recognises_codes_and_messages() {
        assert!(is_not_loaded(Some(3), ""));
        assert!(is_not_loaded(Some(113), ""));
        assert!(is_not_loaded(
            Some(5),
            "Boot-out failed: 3: No such process\n"
        ));
        assert!(is_not_loaded(Some(1), "Could not find specified service\n"));
        assert!(!is_not_loaded(Some(1), "Operation not permitted\n"));
        assert!(!is_not_loaded(None, ""));
    }

    #[test]
    fn test_parse_uid_accepts_trimmed_number_and_rejects_garbage() -> TestResult {
        assert_eq!(parse_uid(" 501\n").map_err(ctx("uid"))?, 501);
        assert!(parse_uid("abc").is_err());
        Ok(())
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
        let plist = plist_path_in(&home, "com.harw.agent");
        let agents = plist.parent().ok_or(TestError::Missing("plist parent"))?;
        std::fs::create_dir_all(agents).map_err(ctx("LaunchAgents anlegen"))?;
        std::fs::write(&plist, "x").map_err(ctx("plist schreiben"))?;
        let run_dir = home.join("run");
        std::fs::create_dir_all(&run_dir).map_err(ctx("run anlegen"))?;
        std::fs::write(run_dir.join("com.harw.agent.pid"), "42").map_err(ctx("pid"))?;
        std::fs::write(run_dir.join("other.pid"), "7").map_err(ctx("fremd"))?;

        let mut targets = vec![plist.clone()];
        targets.extend(runtime_entries(&run_dir, "com.harw.agent"));
        remove_files_if_present(&targets).map_err(ctx("erstes Entfernen"))?;

        assert!(!plist.exists());
        assert!(!run_dir.join("com.harw.agent.pid").exists());
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
        let blocker = home.join("com.harw.agent.pid");
        std::fs::create_dir_all(&blocker).map_err(ctx("blocker anlegen"))?;
        let result = remove_files_if_present(&[blocker]);
        assert!(matches!(result, Err(ServiceError::Io { .. })));
        std::fs::remove_dir_all(&home).map_err(ctx("aufräumen"))?;
        Ok(())
    }
}
