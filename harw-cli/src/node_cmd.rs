//! `harw node status`: `[session_listener]`, Bind-Adresse, Dateirechte,
//! Gerätezahlen und Probleme des Own-Cloud-Listeners (PL-94, Zyklus N1).
//!
//! # Beschreibung
//! Rein lesend. Der Auth-Hub wird weder gelesen noch angesprochen und es wird
//! kein Schlüssel erzeugt (Schlüssel-Bootstrap und -Prüfung gehören zu
//! `harw node init`/`doctor`, Zyklus N2). Das Zustandsverzeichnis ist
//! `<profil>/session-host/remote/` wie bei `harw gateway`.

use std::path::{Path, PathBuf};

use harw_config::SessionListenerSection;
use harw_node_listener::DeviceRegistry;
use serde_json::json;

use crate::cli::{GlobalArgs, NodeAction};
use crate::device_cmd::clean;
use crate::output::Printer;
use crate::session_listener::{PEERS_FILE, parse_peers, parse_tier, plan, resolve_listen};

/// Dateiname der Geräte-Registry (siehe `harw-node-listener`).
const REGISTRY_FILE: &str = harw_node_listener::identity::REGISTRY_FILE;

/// Führt `harw node …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn das Home nicht auflösbar oder die Ausgabe
/// nicht schreibbar ist. Eine nicht ladbare Konfiguration ist *kein* Fehler,
/// sondern ein gemeldetes Problem.
pub(crate) fn run(g: &GlobalArgs, action: NodeAction) -> Result<(), String> {
    match action {
        NodeAction::Status => {
            let home = crate::home::resolve_home(g.home.clone())?;
            let dir = crate::session_serve_remote::remote_state_dir(&home)?;
            let loaded = harw_home::config_layers(&home)
                .map_err(|error| error.to_string())
                .and_then(|layers| {
                    harw_config::discover_config(&layers).map_err(|error| error.to_string())
                })
                .map(|config| config.harness.session_listener);
            let report = match loaded {
                Ok(section) => status(Some(&section), None, &dir),
                Err(error) => status(None, Some(error), &dir),
            };
            Printer::new(g.output()).value(&report.text(), report.json())
        }
    }
}

/// Schwere eines Problems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    /// Der Listener startet nicht oder kann nicht arbeiten.
    Error,
    /// Läuft, ist aber unsicher, unvollständig oder verdächtig.
    Warning,
}

/// Ein Befund von `harw node status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Problem {
    pub(crate) level: Level,
    pub(crate) message: String,
}

/// Rechte und Existenz einer Zustandsdatei.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileView {
    pub(crate) path: PathBuf,
    pub(crate) exists: bool,
    /// Unix-Modus (`& 0o777`); `None` ohne Datei oder auf Nicht-Unix.
    pub(crate) mode: Option<u32>,
}

impl FileView {
    fn of(path: PathBuf) -> Self {
        let meta = std::fs::metadata(&path);
        let exists = meta.is_ok();
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            meta.ok().map(|m| m.permissions().mode() & 0o777)
        };
        #[cfg(not(unix))]
        let mode = None;
        Self { path, exists, mode }
    }

    /// Schreibbar für Gruppe oder alle.
    pub(crate) fn loosely_writable(&self) -> bool {
        self.mode.is_some_and(|mode| mode & 0o022 != 0)
    }

    fn describe(&self) -> String {
        match (self.exists, self.mode) {
            (false, _) => "nicht vorhanden".to_owned(),
            (true, Some(mode)) => format!("Modus {mode:04o}"),
            (true, None) => "vorhanden".to_owned(),
        }
    }
}

/// Der Zustand für `harw node status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusReport {
    pub(crate) remote_dir: PathBuf,
    /// `None`, wenn die Konfiguration nicht ladbar war.
    pub(crate) listener: Option<SessionListenerSection>,
    /// `Some(true)` bei Loopback-Bind; `None`, wenn die Adresse nicht parst.
    pub(crate) bind_loopback: Option<bool>,
    pub(crate) registry: FileView,
    pub(crate) peers: FileView,
    pub(crate) pinned_peers: usize,
    pub(crate) devices_active: usize,
    pub(crate) devices_revoked: usize,
    pub(crate) problems: Vec<Problem>,
}

/// Sammelt den Zustand. `section` ist die geladene `[session_listener]`-
/// Sektion; `config_error` der Grund, falls sie nicht ladbar war.
pub(crate) fn status(
    section: Option<&SessionListenerSection>,
    config_error: Option<String>,
    remote_dir: &Path,
) -> StatusReport {
    let mut problems = Vec::new();
    let mut add = |level, message: String| problems.push(Problem { level, message });

    if let Some(error) = config_error {
        add(
            Level::Error,
            format!("Konfiguration nicht ladbar, [session_listener] unbekannt: {error}"),
        );
    }

    let enabled = section.is_some_and(|s| s.enabled);
    let bind_loopback = section.and_then(|s| {
        s.listen
            .parse::<std::net::SocketAddr>()
            .ok()
            .map(|addr| addr.ip().is_loopback())
    });
    if let Some(s) = section {
        let prefix = if enabled {
            ""
        } else {
            "(Listener ist deaktiviert) "
        };
        // Die Startregeln des Gateways: bei aktivem Listener die volle Planung,
        // sonst nur Adresse und Tier (node_id ist dann noch nicht nötig).
        let check = if enabled {
            plan(s).map(|_| ())
        } else {
            resolve_listen(s)
                .map(|_| ())
                .and_then(|()| parse_tier(&s.tier).map(|_| ()))
        };
        if let Err(error) = check {
            add(
                if enabled {
                    Level::Error
                } else {
                    Level::Warning
                },
                format!("{prefix}{error}"),
            );
        }
        if enabled && bind_loopback == Some(false) && s.allow_non_loopback {
            add(
                Level::Warning,
                format!(
                    "Listener bindet an `{}` (nicht Loopback) und ist damit von außen erreichbar.",
                    s.listen
                ),
            );
        }
    }

    // Registry.
    let registry_file = FileView::of(remote_dir.join(REGISTRY_FILE));
    let registry = DeviceRegistry::new(remote_dir);
    let (records, rejected) = match registry.scan() {
        Ok(scan) => (scan.records, scan.rejected),
        Err(error) => {
            add(
                Level::Error,
                format!("Geräte-Registry nicht lesbar: {error}"),
            );
            (Vec::new(), Vec::new())
        }
    };
    let devices_active = records.iter().filter(|r| !r.revoked).count();
    let devices_revoked = records.len() - devices_active;
    if registry_file.loosely_writable() {
        add(
            Level::Warning,
            format!(
                "{} ist für Gruppe/Alle schreibbar ({}); jede schreibberechtigte Person kann Geräte freischalten. Empfohlen: chmod 600.",
                registry_file.path.display(),
                registry_file.describe()
            ),
        );
    }
    for line in &rejected {
        add(
            Level::Warning,
            format!(
                "{REGISTRY_FILE} Zeile {} wird vom Parser verworfen und gewährt nichts: {}",
                line.line,
                clean(&line.text).chars().take(80).collect::<String>()
            ),
        );
    }
    for (what, ids) in [
        (
            "node_id",
            records
                .iter()
                .map(|r| r.node_id.as_str())
                .collect::<Vec<_>>(),
        ),
        (
            "Gerät",
            records
                .iter()
                .map(|r| r.device.as_str())
                .collect::<Vec<_>>(),
        ),
    ] {
        let mut seen: Vec<&str> = Vec::new();
        for id in &ids {
            if ids.iter().filter(|other| *other == id).count() > 1 && !seen.contains(id) {
                seen.push(id);
                add(
                    Level::Warning,
                    format!(
                        "{what} '{}' kommt mehrfach in {REGISTRY_FILE} vor; mehrdeutige Einträge werden abgelehnt.",
                        clean(id)
                    ),
                );
            }
        }
    }
    if enabled && devices_active == 0 {
        add(
            Level::Warning,
            "Kein aktives Gerät enrollt: niemand kann sich verbinden.".to_owned(),
        );
    }
    if let Some(device) = section.and_then(|s| s.approval_device.as_deref()) {
        match records.iter().find(|r| r.device.as_str() == device) {
            None => add(
                Level::Warning,
                format!(
                    "approval_device '{}' ist nicht enrollt; Freigaben bleiben geparkt.",
                    clean(device)
                ),
            ),
            Some(r) if r.revoked => add(
                Level::Warning,
                format!(
                    "approval_device '{}' ist widerrufen; Freigaben bleiben geparkt.",
                    clean(device)
                ),
            ),
            Some(r) if !r.approve_optin => add(
                Level::Warning,
                format!(
                    "approval_device '{}' hat kein `approve`-Opt-in in {REGISTRY_FILE}; Freigaben bleiben geparkt.",
                    clean(device)
                ),
            ),
            Some(_) => {}
        }
    }

    // Peers.
    let peers_file = FileView::of(remote_dir.join(PEERS_FILE));
    let (pinned_peers, skipped) = match std::fs::read_to_string(&peers_file.path) {
        Ok(text) => {
            let (peers, skipped) = parse_peers(&text);
            (peers.len(), skipped)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (0, 0),
        Err(error) => {
            add(Level::Error, format!("{PEERS_FILE} nicht lesbar: {error}"));
            (0, 0)
        }
    };
    if peers_file.loosely_writable() {
        add(
            Level::Warning,
            format!(
                "{} ist für Gruppe/Alle schreibbar ({}); jede schreibberechtigte Person kann Schlüssel pinnen. Empfohlen: chmod 600.",
                peers_file.path.display(),
                peers_file.describe()
            ),
        );
    }
    if skipped > 0 {
        add(
            Level::Warning,
            format!("{skipped} Zeile(n) in {PEERS_FILE} sind ungültig und werden ignoriert."),
        );
    }
    if enabled && pinned_peers == 0 {
        add(
            Level::Warning,
            format!(
                "Kein Peer-Schlüssel in {PEERS_FILE} gepinnt: kein Gerät kann sich authentifizieren."
            ),
        );
    }

    StatusReport {
        remote_dir: remote_dir.to_path_buf(),
        listener: section.cloned(),
        bind_loopback,
        registry: registry_file,
        peers: peers_file,
        pinned_peers,
        devices_active,
        devices_revoked,
        problems,
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "ja" } else { "nein" }
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::Error => "Fehler",
        Level::Warning => "Warnung",
    }
}

impl StatusReport {
    /// Menschenlesbarer Bericht.
    pub(crate) fn text(&self) -> String {
        let mut out = vec!["[session_listener]".to_owned()];
        match &self.listener {
            Some(s) => {
                let bind = match self.bind_loopback {
                    Some(true) => "ja",
                    Some(false) => "nein (von außen erreichbar)",
                    None => "unbekannt (Adresse nicht lesbar)",
                };
                out.push(format!("  enabled:            {}", yes_no(s.enabled)));
                out.push(format!("  listen:             {}", clean(&s.listen)));
                out.push(format!("  loopback:           {bind}"));
                out.push(format!(
                    "  allow_non_loopback: {}",
                    yes_no(s.allow_non_loopback)
                ));
                out.push(format!(
                    "  node_id:            {}",
                    s.node_id.as_deref().map_or("-".to_owned(), clean)
                ));
                out.push(format!("  tier:               {}", clean(&s.tier)));
                out.push(format!(
                    "  approval_device:    {}",
                    s.approval_device.as_deref().map_or("-".to_owned(), clean)
                ));
            }
            None => out.push("  (nicht ladbar)".to_owned()),
        }
        out.push(String::new());
        out.push(format!("Zustand: {}", self.remote_dir.display()));
        out.push(format!(
            "  {REGISTRY_FILE}: {} ({})",
            self.registry.path.display(),
            self.registry.describe()
        ));
        out.push(format!(
            "  {PEERS_FILE}:   {} ({}, gepinnte Peers: {})",
            self.peers.path.display(),
            self.peers.describe(),
            self.pinned_peers
        ));
        out.push(format!(
            "  Geräte: {} aktiv, {} widerrufen",
            self.devices_active, self.devices_revoked
        ));
        out.push(
            "  Node-Schlüssel (Auth-Hub): wird hier nicht geprüft (kommt mit `harw node doctor`)"
                .to_owned(),
        );
        out.push(String::new());
        if self.problems.is_empty() {
            out.push("Probleme: keine".to_owned());
        } else {
            out.push(format!("Probleme ({}):", self.problems.len()));
            for p in &self.problems {
                out.push(format!("  - {}: {}", level_name(p.level), p.message));
            }
        }
        out.join("\n")
    }

    /// Maschinenlesbarer Bericht.
    pub(crate) fn json(&self) -> serde_json::Value {
        let file = |f: &FileView| {
            json!({
                "path": f.path,
                "exists": f.exists,
                "mode": f.mode.map(|m| format!("{m:04o}")),
                "group_or_world_writable": f.loosely_writable(),
            })
        };
        let listener = self.listener.as_ref().map(|s| {
            json!({
                "enabled": s.enabled,
                "listen": s.listen,
                "loopback": self.bind_loopback,
                "allow_non_loopback": s.allow_non_loopback,
                "node_id": s.node_id,
                "tier": s.tier,
                "approval_device": s.approval_device,
            })
        });
        json!({
            "session_listener": listener,
            "state_dir": self.remote_dir,
            "registry": file(&self.registry),
            "peers": file(&self.peers),
            "pinned_peers": self.pinned_peers,
            "devices": { "active": self.devices_active, "revoked": self.devices_revoked },
            "problems": self.problems.iter().map(|p| json!({
                "level": match p.level { Level::Error => "error", Level::Warning => "warning" },
                "message": p.message,
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    const REGISTRY: &str = "node-a|dev-1|acme|observer|active|phone\n\
        node-b|dev-2|acme|operator|revoked|old\n\
        broken line\n";

    fn dir_with(registry: Option<&str>, peers: Option<&str>) -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        if let Some(text) = registry {
            std::fs::write(dir.path().join(REGISTRY_FILE), text).map_err(ctx("registry"))?;
        }
        if let Some(text) = peers {
            std::fs::write(dir.path().join(PEERS_FILE), text).map_err(ctx("peers"))?;
        }
        Ok(dir)
    }

    fn enabled(listen: &str, allow: bool) -> SessionListenerSection {
        SessionListenerSection {
            enabled: true,
            listen: listen.to_owned(),
            allow_non_loopback: allow,
            node_id: Some("gateway-a".to_owned()),
            ..SessionListenerSection::default()
        }
    }

    fn messages(report: &StatusReport) -> String {
        report
            .problems
            .iter()
            .map(|p| p.message.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn default_state_has_no_files_and_is_disabled_loopback() -> TestResult {
        let dir = dir_with(None, None)?;
        let report = status(Some(&SessionListenerSection::default()), None, dir.path());
        assert_eq!(report.bind_loopback, Some(true));
        assert_eq!((report.devices_active, report.devices_revoked), (0, 0));
        assert!(!report.registry.exists && !report.peers.exists);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert!(report.text().contains("Probleme: keine"));
        assert!(report.text().contains("enabled:            nein"));
        Ok(())
    }

    #[test]
    fn counts_devices_and_reports_the_broken_registry_line() -> TestResult {
        let dir = dir_with(Some(REGISTRY), None)?;
        let report = status(Some(&SessionListenerSection::default()), None, dir.path());
        assert_eq!((report.devices_active, report.devices_revoked), (1, 1));
        let text = messages(&report);
        assert!(text.contains("Zeile 3"), "{text}");
        assert_eq!(report.json()["devices"]["revoked"], json!(1));
        Ok(())
    }

    #[test]
    fn a_non_loopback_bind_is_detected() -> TestResult {
        let dir = dir_with(Some(REGISTRY), None)?;
        // Opt-in given: it starts, but it is flagged as reachable from outside.
        let open = status(Some(&enabled("0.0.0.0:7443", true)), None, dir.path());
        assert_eq!(open.bind_loopback, Some(false));
        assert!(messages(&open).contains("von außen erreichbar"));
        assert!(open.text().contains("nein (von außen erreichbar)"));
        // No opt-in: the gateway would refuse to start.
        let refused = status(Some(&enabled("0.0.0.0:7443", false)), None, dir.path());
        assert!(
            refused
                .problems
                .iter()
                .any(|p| p.level == Level::Error && p.message.contains("allow_non_loopback"))
        );
        // Loopback is not flagged.
        let local = status(Some(&enabled("127.0.0.1:7443", false)), None, dir.path());
        assert_eq!(local.bind_loopback, Some(true));
        assert!(!messages(&local).contains("von außen"));
        Ok(())
    }

    #[test]
    fn enabled_without_node_id_is_an_error_but_disabled_is_not() -> TestResult {
        let dir = dir_with(None, None)?;
        let mut section = enabled("127.0.0.1:7443", false);
        section.node_id = None;
        let report = status(Some(&section), None, dir.path());
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.level == Level::Error && p.message.contains("node_id"))
        );
        section.enabled = false;
        assert!(status(Some(&section), None, dir.path()).problems.is_empty());
        Ok(())
    }

    #[test]
    fn an_unloadable_config_is_a_reported_problem() -> TestResult {
        let dir = dir_with(None, None)?;
        let report = status(None, Some("kaputt".to_owned()), dir.path());
        assert!(report.listener.is_none());
        assert!(messages(&report).contains("kaputt"));
        assert_eq!(report.json()["session_listener"], json!(null));
        Ok(())
    }

    #[test]
    fn approval_device_must_be_enrolled_active_and_opted_in() -> TestResult {
        let dir = dir_with(Some(REGISTRY), None)?;
        for (device, needle) in [
            ("ghost", "nicht enrollt"),
            ("dev-2", "widerrufen"),
            ("dev-1", "approve"),
        ] {
            let mut section = enabled("127.0.0.1:7443", false);
            section.approval_device = Some(device.to_owned());
            let report = status(Some(&section), None, dir.path());
            assert!(
                messages(&report).contains(needle),
                "{device}: {}",
                messages(&report)
            );
        }
        Ok(())
    }

    #[test]
    fn duplicates_are_reported() -> TestResult {
        let dir = dir_with(
            Some("n|dup|t|observer|active|a\nm|dup|t|observer|active|b\n"),
            None,
        )?;
        let report = status(Some(&SessionListenerSection::default()), None, dir.path());
        assert!(messages(&report).contains("Gerät 'dup' kommt mehrfach"));
        Ok(())
    }

    #[test]
    fn peers_are_counted_and_bad_lines_flagged() -> TestResult {
        let key = "ab".repeat(1952);
        let peers = format!("phone|{key}\nnonsense\n");
        let dir = dir_with(Some(REGISTRY), Some(&peers))?;
        let report = status(Some(&enabled("127.0.0.1:7443", false)), None, dir.path());
        assert_eq!(report.pinned_peers, 1);
        assert!(messages(&report).contains("1 Zeile(n) in node-peers.conf"));
        // Enabled with no pins at all.
        let empty = dir_with(Some(REGISTRY), None)?;
        let none = status(Some(&enabled("127.0.0.1:7443", false)), None, empty.path());
        assert!(messages(&none).contains("Kein Peer-Schlüssel"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn group_or_world_writable_files_are_flagged() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = dir_with(Some(REGISTRY), Some("# none\n"))?;
        let set = |name: &str, mode: u32| -> TestResult {
            std::fs::set_permissions(dir.path().join(name), std::fs::Permissions::from_mode(mode))
                .map_err(ctx("chmod"))
        };
        set(REGISTRY_FILE, 0o600)?;
        set(PEERS_FILE, 0o600)?;
        let tight = status(Some(&SessionListenerSection::default()), None, dir.path());
        assert_eq!(tight.registry.mode, Some(0o600));
        assert!(!messages(&tight).contains("schreibbar"));

        set(REGISTRY_FILE, 0o666)?;
        set(PEERS_FILE, 0o620)?;
        let loose = status(Some(&SessionListenerSection::default()), None, dir.path());
        let text = messages(&loose);
        assert!(
            text.contains("node-devices.conf ist für Gruppe/Alle schreibbar"),
            "{text}"
        );
        assert!(
            text.contains("node-peers.conf ist für Gruppe/Alle schreibbar"),
            "{text}"
        );
        assert_eq!(
            loose.json()["registry"]["group_or_world_writable"],
            json!(true)
        );
        assert_eq!(loose.json()["registry"]["mode"], json!("0666"));
        // Readable-by-others alone is not a "writable" warning.
        set(REGISTRY_FILE, 0o644)?;
        set(PEERS_FILE, 0o644)?;
        let readable = status(Some(&SessionListenerSection::default()), None, dir.path());
        assert!(!messages(&readable).contains("schreibbar"));
        Ok(())
    }

    #[test]
    fn run_status_works_against_a_temp_home() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let global = GlobalArgs {
            home: Some(home.path().to_path_buf()),
            ..GlobalArgs::default()
        };
        run(&global, NodeAction::Status).map_err(TestError::Unexpected)
    }
}
