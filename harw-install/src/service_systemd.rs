//! systemd-User-Service-Manager (Linux).
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! `harw-install / src/service.rs` (Unterpunkt `service_systemd.rs`).
//!
//! # Verantwortung
//! Dieses Modul besitzt die Linux-spezifische Implementierung von
//! [`crate::service::ServiceManager`] auf Basis von systemd-User-Units. Es
//! rendert eine `.service`-Unit rein (ohne I/O) und führt Installation,
//! Statusabfrage und Deinstallation über `systemctl --user` aus. Die
//! Deinstallation ist idempotent: `systemctl --user disable --now` (eine nicht
//! geladene bzw. nicht existierende Unit wird ignoriert), danach wird die
//! Unit-Datei gelöscht und `systemctl --user daemon-reload` ausgeführt;
//! optional räumt [`SystemdServiceManager::uninstall_with_runtime_dir`] die
//! nach dem Dienst benannten Einträge im `run/`-Verzeichnis auf.
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

use std::path::{Path, PathBuf};
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
        Ok(unit_path_in(&home, name))
    }

    /// Führt `systemctl --user <args…>` aus (ohne Prüfung des Exit-Status).
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn der Prozess nicht startbar ist.
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

    /// Deinstalliert den Dienst und räumt zusätzlich Laufzeitdateien auf.
    ///
    /// # Description
    /// Wie [`ServiceManager::uninstall`] (`systemctl --user disable --now`,
    /// Unit-Datei löschen, `daemon-reload`); danach werden in `run_dir` die
    /// nach dem Dienst benannten Laufzeiteinträge `<name>.pid` und
    /// `<name>.sock` entfernt (siehe `runtime_entries`). Das Verzeichnis selbst
    /// und alle anderen Einträge bleiben unangetastet. Fehlende Dateien gelten
    /// als bereits entfernt.
    ///
    /// Hinweis: `install` dieses Backends schreibt selbst nichts nach `run/`;
    /// die Einträge stammen ggf. vom gestarteten Dienst bzw. vom PID-Rückfall
    /// der CLI (`<home>/run/<name>.pid`) und würden sonst verwaist liegen
    /// bleiben.
    ///
    /// # Arguments
    /// - `name` (`&str`): Dienstname (= Unit-Name ohne `.service`).
    /// - `run_dir` (`&Path`): Laufzeitverzeichnis, typisch `<harw-home>/run`.
    ///
    /// # Errors
    /// - [`ServiceError::Command`]: wenn `disable --now` mit einem anderen
    ///   Fehler als „nicht geladen/existiert nicht" scheitert oder
    ///   `daemon-reload` fehlschlägt.
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

    /// Deaktiviert und stoppt die Unit, entfernt die Unit-Datei und lädt die
    /// systemd-Konfiguration neu (idempotent).
    ///
    /// `systemctl --user disable --now <name>`; ist die Unit nicht geladen
    /// bzw. existiert sie nicht, wird das ignoriert. Danach wird
    /// `~/.config/systemd/user/<name>.service` gelöscht (fehlt sie, ist das
    /// kein Fehler) und `systemctl --user daemon-reload` ausgeführt; dessen
    /// Scheitern ist ein Fehler.
    fn uninstall(&self, name: &str) -> Result<(), ServiceError> {
        let path = Self::unit_path(name)?;
        let args = disable_args(name);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = Self::systemctl(&arg_refs)?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() && !is_unit_missing(out.status.code(), &stderr) {
            return Err(ServiceError::Command {
                command: format!("systemctl --user {}", args.join(" ")),
                detail: stderr.into_owned(),
            });
        }
        remove_files_if_present(&[path])?;
        let reload = daemon_reload_args();
        let reload_refs: Vec<&str> = reload.iter().map(String::as_str).collect();
        let out = Self::systemctl(&reload_refs)?;
        if !out.status.success() {
            return Err(ServiceError::Command {
                command: format!("systemctl --user {}", reload.join(" ")),
                detail: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(())
    }
}

/// Pfad der Unit-Datei `<home>/.config/systemd/user/<name>.service` (rein).
///
/// # Arguments
/// - `home` (`&Path`): Home-Verzeichnis des Nutzers.
/// - `name` (`&str`): Dienstname (= Unit-Name ohne `.service`).
fn unit_path_in(home: &Path, name: &str) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{name}.service"))
}

/// Argumente für `systemctl --user disable --now <name>` (rein, ohne
/// Programmnamen und ohne `--user`).
///
/// # Arguments
/// - `name` (`&str`): Dienstname (= Unit-Name ohne `.service`).
fn disable_args(name: &str) -> Vec<String> {
    vec!["disable".to_owned(), "--now".to_owned(), name.to_owned()]
}

/// Argumente für `systemctl --user daemon-reload` (rein, ohne Programmnamen
/// und ohne `--user`).
fn daemon_reload_args() -> Vec<String> {
    vec!["daemon-reload".to_owned()]
}

/// Erkennt, ob ein fehlgeschlagenes `systemctl --user disable --now` nur
/// „Unit nicht geladen/existiert nicht" bedeutet (rein).
///
/// # Description
/// `disable` meldet eine fehlende Unit-Datei mit „Unit file … does not
/// exist", `stop` eine nicht geladene Unit mit „Unit … not loaded" bzw.
/// Exit-Code 5 (LSB „program is not installed").
///
/// # Arguments
/// - `code` (`Option<i32>`): Exit-Code des Prozesses.
/// - `stderr` (`&str`): Standardfehler von `systemctl`.
fn is_unit_missing(code: Option<i32>, stderr: &str) -> bool {
    if code == Some(5) {
        return true;
    }
    let lower = stderr.to_ascii_lowercase();
    ["does not exist", "not loaded", "not found", "no such file"]
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

/// Rendert eine systemd-User-Unit rein (ohne I/O).
///
/// # Description
/// Erzeugt die `[Unit]`/`[Service]`/`[Install]`-Abschnitte aus `spec`:
/// `ExecStart` aus `spec.exec`, `WorkingDirectory`, je eine `Environment=`-Zeile
/// pro Variable, `Restart=always`, `RestartSec=<restart_sec>` und
/// `WantedBy=default.target`.
///
/// `ExecStart` und `Environment` werden nach `systemd.syntax(7)` zitiert:
/// jedes `ExecStart`-Wort und jede `Environment=`-Zuweisung wird einzeln über
/// [`quote_systemd_value`] in doppelte Anführungszeichen gesetzt. Damit
/// spaltet ein Leerzeichen im Programmpfad `ExecStart` nicht in ein anderes
/// Kommando auf, `%` (Spezifizierer) und `$` (Variablenexpansion) werden
/// verdoppelt statt expandiert, und ein Zeilenumbruch im Wert erzeugt keine
/// zusätzliche physische Zeile (und damit keine zusätzliche Direktive) in
/// der Unit-Datei.
///
/// # Arguments
/// - `spec` (`&ServiceSpec`): Dienstbeschreibung.
///
/// # Returns
/// Den vollständigen Unit-Text als `String`.
fn render_systemd_unit(spec: &ServiceSpec) -> String {
    let exec_start = spec
        .exec
        .iter()
        .map(|word| quote_systemd_value(word))
        .collect::<Vec<_>>()
        .join(" ");
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
        let assignment = format!("{key}={value}");
        out.push_str(&format!(
            "Environment={}\n",
            quote_systemd_value(&assignment)
        ));
    }
    out.push_str("Restart=always\n");
    out.push_str(&format!("RestartSec={}\n\n", spec.restart_sec));
    out.push_str("[Install]\n");
    out.push_str("WantedBy=default.target\n");
    out
}

/// Escaped einen Rohwert für die Verwendung innerhalb einer doppelt
/// zitierten systemd-Zuweisung (rein, ohne umschließende Anführungszeichen).
///
/// # Description
/// systemd expandiert `%`-Spezifizierer und, in `ExecStart=`/`Environment=`,
/// `$`-Variablen unabhängig von Anführungszeichen; ein unzitiertes
/// Leerzeichen trennt Wörter in `ExecStart=`. Diese Funktion verdoppelt `%`
/// und `$`, escaped `\` und `"` C-artig (wie von `systemd.syntax(7)`
/// erwartet) und ersetzt Steuerzeichen — inklusive `\n`, `\r`, `\t` — durch
/// die passende Escape-Sequenz (benannt bzw. `\xHH`). So kann ein Rohwert
/// niemals eine neue physische Zeile oder eine zusätzliche Direktive im
/// Unit-File erzeugen; C-artige Escape-Sequenzen bewahren aber den
/// ursprünglichen Zeicheninhalt beim Parsen durch systemd.
///
/// # Arguments
/// - `raw` (`&str`): unverarbeiteter Wert, z. B. ein Pfadsegment oder eine
///   `KEY=value`-Zuweisung.
///
/// # Returns
/// Den escapten Wert ohne umschließende Anführungszeichen.
fn escape_systemd_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let mut buf = [0u8; 4];
                for byte in c.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("\\x{byte:02x}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Zitiert einen Rohwert für eine systemd-Unit-Zeile (rein).
///
/// # Description
/// Wendet [`escape_systemd_value`] an und umschließt das Ergebnis mit
/// doppelten Anführungszeichen (`"..."`), wie es `ExecStart=`- und
/// `Environment=`-Zeilen nach `systemd.syntax(7)` erwarten.
///
/// # Arguments
/// - `raw` (`&str`): unverarbeiteter Wert.
///
/// # Returns
/// Den Wert in doppelten Anführungszeichen.
fn quote_systemd_value(raw: &str) -> String {
    format!("\"{}\"", escape_systemd_value(raw))
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
        assert!(unit.contains("ExecStart=\"/usr/bin/harw\" \"serve\""));
    }

    #[test]
    fn test_render_unit_contains_restart_sec() {
        let unit = render_systemd_unit(&spec());
        assert!(unit.contains("RestartSec=7"));
    }

    #[test]
    fn test_render_unit_contains_environment_line() {
        let unit = render_systemd_unit(&spec());
        assert!(unit.contains("Environment=\"RUST_LOG=info\""));
    }

    #[test]
    fn test_render_unit_quotes_exec_words_with_spaces() {
        let mut s = spec();
        s.exec = vec!["/opt/My Apps/harw".to_owned(), "serve".to_owned()];
        let unit = render_systemd_unit(&s);
        assert!(unit.contains("ExecStart=\"/opt/My Apps/harw\" \"serve\"\n"));
    }

    #[test]
    fn test_render_unit_quotes_environment_assignment_with_spaces() {
        let mut s = spec();
        s.env = vec![("HARW_HOME".to_owned(), "/home/a b/.harw".to_owned())];
        let unit = render_systemd_unit(&s);
        assert!(unit.contains("Environment=\"HARW_HOME=/home/a b/.harw\"\n"));
    }

    #[test]
    fn test_render_unit_doubles_percent_specifier_in_exec_and_env() {
        let mut s = spec();
        s.exec = vec!["/opt/100%done/harw".to_owned()];
        s.env = vec![("KEY".to_owned(), "50%off".to_owned())];
        let unit = render_systemd_unit(&s);
        assert!(unit.contains("ExecStart=\"/opt/100%%done/harw\"\n"));
        assert!(unit.contains("Environment=\"KEY=50%%off\"\n"));
    }

    #[test]
    fn test_render_unit_doubles_dollar_variable_in_env() {
        let mut s = spec();
        s.env = vec![("PATH".to_owned(), "$HOME/bin".to_owned())];
        let unit = render_systemd_unit(&s);
        assert!(unit.contains("Environment=\"PATH=$$HOME/bin\"\n"));
    }

    #[test]
    fn test_render_unit_prevents_line_injection_via_newline_in_env_value() -> TestResult {
        let mut s = spec();
        s.env = vec![(
            "EVIL".to_owned(),
            "x\n[Service]\nExecStart=/bin/evil".to_owned(),
        )];
        let unit = render_systemd_unit(&s);
        let line = unit
            .lines()
            .find(|l| l.starts_with("Environment=\"EVIL="))
            .ok_or(TestError::Missing("Environment=EVIL-Zeile"))?;
        assert!(line.contains("x\\n[Service]\\nExecStart=/bin/evil"));
        // Der eingebettete Zeilenumbruch darf keine zusätzliche
        // `ExecStart=`-Zeile im Unit-Text erzeugen.
        assert_eq!(unit.matches("ExecStart=").count(), 1);
        Ok(())
    }

    #[test]
    fn test_escape_systemd_value_escapes_backslash_and_quote() {
        assert_eq!(escape_systemd_value("a\"b\\c"), "a\\\"b\\\\c");
    }

    #[test]
    fn test_escape_systemd_value_escapes_control_chars() {
        assert_eq!(escape_systemd_value("a\nb\tc\rd"), "a\\nb\\tc\\rd");
        assert_eq!(escape_systemd_value("a\u{7}b"), "a\\x07b");
    }

    #[test]
    fn test_quote_systemd_value_wraps_in_double_quotes() {
        assert_eq!(quote_systemd_value("plain"), "\"plain\"");
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

    /// Legt ein eindeutiges temporäres Verzeichnis an.
    fn temp_dir(tag: &str) -> TestResult<PathBuf> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("harw-systemd-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(ctx("temp dir anlegen"))?;
        Ok(dir)
    }

    #[test]
    fn test_unit_path_in_uses_systemd_user_dir() {
        assert_eq!(
            unit_path_in(Path::new("/home/u"), "harw"),
            PathBuf::from("/home/u/.config/systemd/user/harw.service")
        );
    }

    #[test]
    fn test_disable_args_disable_and_stop_unit() {
        assert_eq!(
            disable_args("harw"),
            vec!["disable".to_owned(), "--now".to_owned(), "harw".to_owned()]
        );
    }

    #[test]
    fn test_daemon_reload_args() {
        assert_eq!(daemon_reload_args(), vec!["daemon-reload".to_owned()]);
    }

    #[test]
    fn test_is_unit_missing_recognises_codes_and_messages() {
        assert!(is_unit_missing(Some(5), ""));
        assert!(is_unit_missing(
            Some(1),
            "Failed to disable unit: Unit file harw.service does not exist.\n"
        ));
        assert!(is_unit_missing(
            Some(1),
            "Failed to stop harw.service: Unit harw.service not loaded.\n"
        ));
        assert!(!is_unit_missing(
            Some(1),
            "Failed to connect to bus: No medium found\n"
        ));
        assert!(!is_unit_missing(None, ""));
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
        let unit = unit_path_in(&home, "harw");
        let unit_dir = unit.parent().ok_or(TestError::Missing("unit parent"))?;
        std::fs::create_dir_all(unit_dir).map_err(ctx("unit dir anlegen"))?;
        std::fs::write(&unit, "x").map_err(ctx("unit schreiben"))?;
        let run_dir = home.join("run");
        std::fs::create_dir_all(&run_dir).map_err(ctx("run anlegen"))?;
        std::fs::write(run_dir.join("harw.pid"), "42").map_err(ctx("pid"))?;
        std::fs::write(run_dir.join("harw.sock"), "").map_err(ctx("sock"))?;
        std::fs::write(run_dir.join("other.pid"), "7").map_err(ctx("fremd"))?;

        let mut targets = vec![unit.clone()];
        targets.extend(runtime_entries(&run_dir, "harw"));
        remove_files_if_present(&targets).map_err(ctx("erstes Entfernen"))?;

        assert!(!unit.exists());
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
