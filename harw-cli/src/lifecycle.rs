//! Handler für die Betriebs-Lebenszyklus-Subcommands: `update`, `service`,
//! `catalog`, `uninstall`, sowie die Prozess-Signale der Daemons.
//!
//! # Verantwortung
//! - Dünne Verdrahtung auf `harw-install`/`harw-model-catalog`; die Logik lebt
//!   in den jeweiligen Descriptor-Modulen.
//! - `harw service install` legt je eine Dienst-Unit für `harw serve`
//!   (MCP-Listener + Job-Worker) und `harw gateway` an (G-065). Unter systemd
//!   werden die User-Units geschrieben, neu geladen und per
//!   `systemctl --user enable --now` aktiviert.
//! - `harw gateway <aktion>` steuert nur den Gateway-Dienst über ein erkanntes
//!   Backend: systemd-User-Unit (`systemctl --user`), launchd-LaunchAgent
//!   (`launchctl`, macOS) oder als portabler Rückfall einen losgelösten
//!   Hintergrundprozess mit PID-Datei `<home>/run/harw-gateway.pid` und Log
//!   `<home>/logs/harw-gateway.log`. Alle externen Aufrufe werden von reinen
//!   Funktionen als Argumentvektoren gebaut (testbar ohne Ausführung);
//!   `HARW_GATEWAY_BACKEND` erzwingt ein Backend.
//! - [`ShutdownSignals`]: SIGTERM/SIGINT als Auslöser des geordneten Shutdowns
//!   von `harw serve` (G-022, `crate::serve_until`).
//!
//! # Nebenläufigkeit
//! [`ShutdownSignals::install`] muss innerhalb einer Tokio-Runtime mit
//! aktiviertem IO-Treiber laufen; die Signal-Registrierung ist prozessweit.
//! Alle übrigen Funktionen sind synchron; der Zustand des Gateway-Rückfalls
//! liegt ausschließlich in dessen PID-Datei (gleichzeitige Start-/Stopp-Aufrufe
//! werden nicht gegeneinander gesperrt).
//!
//! # Fehler
//! Fehler werden als `String` an `main::dispatch` gereicht (Exit-Code 2).

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};

use harw_extension_api::approval_mode::ApprovalMode;
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_install::{
    Platform, ServiceKind, ServiceManager, ServiceSpec, UninstallScope, UpdateChecker,
    detect_service_manager,
};

use crate::cli::{GatewayAction, ServiceAction};
use crate::home::resolve_home;

/// Rendert die `harw-install`-Health-Checks (System/Sandbox/Runtime/Service/Home).
pub fn health(home_override: Option<PathBuf>) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let checks = health_checks(&home)?;
    println!("--- Health-Checks ---");
    for (id, outcome) in harw_install::run_all(&checks) {
        let (mark, detail) = match outcome {
            harw_install::CheckOutcome::Ok(detail) => ("ok  ", detail),
            harw_install::CheckOutcome::Warn(detail) => ("warn", detail),
            harw_install::CheckOutcome::Fail(detail) => ("FAIL", detail),
        };
        println!("[{mark}] {id}: {detail}");
    }
    Ok(())
}

/// Builds the install checks with evidence from this CLI's real composition
/// root. `harw-install` deliberately cannot construct or infer this evidence
/// itself because it does not own the registry or session boundary.
///
/// A failed runtime assembly does not abort `harw doctor` as a whole: the
/// failure is logged and the remaining checks (system, sandbox, service
/// manager, home) still run with "not configured" runtime-composition
/// evidence. Silently swallowing the assembly error would hide a real
/// misconfiguration; aborting all diagnosis over it would hide everything
/// else `doctor` could otherwise report (C2).
fn health_checks(home: &Path) -> Result<Vec<Box<dyn harw_install::DoctorCheck>>, String> {
    let evidence = runtime_composition_evidence(home).unwrap_or_else(|error| {
        tracing::warn!(error = %error, "doctor runtime assembly failed");
        harw_install::doctor::RuntimeCompositionEvidence::not_configured()
    });
    Ok(vec![
        Box::new(harw_install::doctor::SystemCheck),
        Box::new(harw_install::doctor::SandboxCheck),
        Box::new(harw_install::doctor::RuntimeCompositionCheck::new(evidence)),
        Box::new(harw_install::doctor::ServiceManagerCheck),
        Box::new(harw_install::doctor::HomeExistsCheck::new(home)),
        Box::new(harw_install::doctor::HomePermsCheck::new(home)),
    ])
}

/// Observes the tool composition, the trusted spawn context, and the approval
/// boundary that `harw doctor`'s own runtime entry
/// ([`crate::runtime_entry::doctor_assembly`]) actually assembles for `home`.
/// A failed assembly is returned rather than converted into positive or
/// guessed evidence (the caller, [`health_checks`], decides what an assembly
/// failure means for the rest of `doctor`).
fn runtime_composition_evidence(
    home: &Path,
) -> Result<harw_install::doctor::RuntimeCompositionEvidence, String> {
    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    let assembly = crate::runtime_entry::doctor_assembly(home, &cwd)?;
    let rights = assembly.rights_snapshot();

    let approval_actor_present = assembly.spawn_context().approval_actor.is_some();
    let sandbox_root = assembly.sandbox().workspace().canonical_root();
    let sandbox_bound_to_project = sandbox_root == assembly.project().project_root.as_path();
    let (has_trusted_spawn_context, has_approval_boundary) = evaluate_evidence(
        &rights.approval_chain,
        assembly.approval_mode().get(),
        approval_actor_present,
        sandbox_bound_to_project,
    );

    Ok(harw_install::doctor::RuntimeCompositionEvidence {
        advertised_tools: Some(rights.tools),
        has_trusted_spawn_context: Some(has_trusted_spawn_context),
        has_approval_boundary: Some(has_approval_boundary),
    })
}

/// Pure evaluation of the two boolean composition-evidence flags from
/// already-observed facts about a [`harw_runtime::RuntimeAssembly`] — kept
/// free of the assembly type itself so the fail combinations below are
/// testable without assembling a real runtime (C13).
///
/// A previous version treated the mere presence of *some* approval handler,
/// or of *some* `approval_actor`, as sufficient evidence of a boundary — but
/// [`harw_runtime::approval::ApprovalChain::snapshot`] always carries the
/// built-in default policy, and every CLI/TUI principal always resolves an
/// `approval_actor`, so both flags were `true` unconditionally and observed
/// nothing. The criteria here instead require a fact that can actually be
/// absent:
/// - `has_approval_boundary`: the chain carries the **configured** policy
///   (`ApprovalHandlerKind::ConfigPolicy`, only present when
///   `[policy].require_approval_for` is non-empty) *and* the run's approval
///   mode is not [`ApprovalMode::FullAccess`] (which waves every call through
///   regardless of the chain).
/// - `has_trusted_spawn_context`: the root sandbox is bound to the same,
///   independently detected project root (`assembly.sandbox()` vs.
///   `assembly.project()`) *and* an `approval_actor` is attached.
///
/// # Arguments
/// - `approval_chain`: [`harw_runtime::spec::RightsSnapshot::approval_chain`].
/// - `approval_mode`: the run's current [`ApprovalMode`].
/// - `approval_actor_present`: whether `assembly.spawn_context().approval_actor`
///   is `Some`.
/// - `sandbox_bound_to_project`: whether the root sandbox's canonical
///   workspace root equals the detected project root.
///
/// # Returns
/// `(has_trusted_spawn_context, has_approval_boundary)`.
fn evaluate_evidence(
    approval_chain: &[(&'static str, ApprovalHandlerKind)],
    approval_mode: ApprovalMode,
    approval_actor_present: bool,
    sandbox_bound_to_project: bool,
) -> (bool, bool) {
    let has_config_policy = approval_chain
        .iter()
        .any(|(_, kind)| *kind == ApprovalHandlerKind::ConfigPolicy);
    let has_approval_boundary = has_config_policy && approval_mode != ApprovalMode::FullAccess;
    let has_trusted_spawn_context = sandbox_bound_to_project && approval_actor_present;

    (has_trusted_spawn_context, has_approval_boundary)
}

/// `harw update [--check]`: liest `version.json`.
pub fn update(home_override: Option<PathBuf>, check: bool) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let checker = UpdateChecker::new(&home);

    match checker.read().map_err(|error| error.to_string())? {
        Some(info) => println!(
            "letzte Prüfung: {} · neueste bekannte Version: {}",
            info.last_checked_at, info.latest_version
        ),
        None => println!("noch keine Versionsdaten (version.json fehlt)"),
    }

    if check {
        return Err(
            "Versionsprüfung nicht verfügbar: kein Remote-Update-Checker ist konfiguriert; \
             version.json bleibt unverändert."
                .to_owned(),
        );
    }
    Ok(())
}

/// Dienstname der Unit für `harw serve` (MCP-Listener + Job-Worker).
pub(crate) const SERVE_SERVICE_NAME: &str = "harw-serve";

/// Dienstname der Unit für `harw gateway` (Kanäle, Telemetrie).
pub(crate) const GATEWAY_SERVICE_NAME: &str = "harw-gateway";

/// Wartezeit in Sekunden, bevor der Dienstmanager einen abgestürzten Dienst
/// neu startet.
const SERVICE_RESTART_SEC: u32 = 5;

/// `harw service <install|status|uninstall>`: verwaltet die Hintergrunddienste.
///
/// # Description
/// Verwaltet **zwei** Dienste: [`SERVE_SERVICE_NAME`] (`harw serve`, der als
/// einziger MCP-Listener und Job-Worker startet) und [`GATEWAY_SERVICE_NAME`]
/// (`harw gateway`). Vorher installierte `service install` nur den Gateway,
/// womit ein Dienstbetrieb nie Jobs ausführte (G-065).
///
/// Unter systemd schreibt `install` die User-Unit-Dateien, lädt den Manager neu
/// und aktiviert/startet beide Units. Andere Backends (launchd, schtasks)
/// installieren wie bisher über [`ServiceManager::install`]. `status`/`uninstall`
/// wirken auf beide Dienste.
///
/// # Errors
/// Home-/Executable-Auflösung, Unit-Verzeichnis, Schreib- und Backend-Fehler.
pub fn service(home_override: Option<PathBuf>, action: ServiceAction) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let platform = Platform::detect();
    let manager = detect_service_manager(&platform);
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let specs = service_specs(&exe, &home);

    match action {
        ServiceAction::Install { dry_run } => {
            if dry_run {
                for spec in &specs {
                    println!("# {}", spec.name);
                    println!("{}", manager.render_unit(spec));
                }
            } else if manager.kind() == ServiceKind::Systemd {
                let unit_dir = systemd_user_unit_dir(
                    std::env::var_os("XDG_CONFIG_HOME"),
                    std::env::var_os("HOME"),
                )?;
                let written = write_systemd_units(&unit_dir, manager.as_ref(), &specs)?;
                for path in &written {
                    println!("Unit geschrieben: {}", path.display());
                }
                activate_systemd_units(&specs)?;
                println!(
                    "Dienste aktiviert und gestartet: {}",
                    specs
                        .iter()
                        .map(|spec| format!("{}.service", spec.name))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            } else {
                for spec in &specs {
                    manager.install(spec).map_err(|error| error.to_string())?;
                }
                println!("Dienste installiert ({:?}).", manager.kind());
            }
        }
        ServiceAction::Status => {
            for spec in &specs {
                let status = manager
                    .status(&spec.name)
                    .map_err(|error| error.to_string())?;
                println!("Dienststatus {}: {status:?}", spec.name);
            }
        }
        ServiceAction::Uninstall => {
            for spec in &specs {
                manager
                    .uninstall(&spec.name)
                    .map_err(|error| error.to_string())?;
            }
            println!("Dienste entfernt.");
        }
    }
    Ok(())
}

/// Umgebungsvariable, die das Gateway-Dienst-Backend explizit festlegt
/// (`systemd`, `launchd` oder `detached`); ohne sie wird automatisch erkannt.
pub(crate) const GATEWAY_BACKEND_ENV: &str = "HARW_GATEWAY_BACKEND";

/// Wartezeit nach dem Start eines Hintergrundprozesses, bevor geprüft wird, ob
/// er sich sofort wieder beendet hat (z. B. wegen Konfigurationsfehlern).
const DETACHED_STARTUP_PROBE: std::time::Duration = std::time::Duration::from_millis(500);

/// Frist für ein geordnetes Beenden (SIGTERM/`taskkill`), bevor hart beendet wird.
const DETACHED_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Frist nach dem harten Beenden, bis der Prozess verschwunden sein muss.
const DETACHED_KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Abfrageintervall beim Warten auf das Prozessende.
const DETACHED_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Dienstverwaltung, über die `harw gateway <aktion>` den Gateway steuert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayBackend {
    /// systemd-User-Unit (`systemctl --user`, Linux).
    SystemdUser,
    /// launchd-LaunchAgent (`launchctl`, macOS).
    Launchd,
    /// Portabler Rückfall: losgelöster Hintergrundprozess mit PID-Datei unter
    /// dem HARW-Home, gesteuert über die PID.
    Detached,
}

impl std::fmt::Display for GatewayBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SystemdUser => "systemd (User-Unit)",
            Self::Launchd => "launchd (LaunchAgent)",
            Self::Detached => "Hintergrundprozess (PID-Datei)",
        })
    }
}

/// Backend-unabhängige Gateway-Operation.
///
/// Entkoppelt die Befehlsbauer von der CLI-Grammatik ([`GatewayAction`]). Der
/// Status ist keine Operation, sondern ein reiner Bericht ([`gateway_status`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayOp {
    Install,
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
}

impl GatewayOp {
    /// Bildet die CLI-Aktion auf die Operation ab.
    ///
    /// # Returns
    /// `None` für [`GatewayAction::Status`], das nichts verändert, sondern nur
    /// den Zustand berichtet.
    fn from_action(action: &GatewayAction) -> Option<Self> {
        match action {
            GatewayAction::Install => Some(Self::Install),
            GatewayAction::Start => Some(Self::Start),
            GatewayAction::Stop => Some(Self::Stop),
            GatewayAction::Restart => Some(Self::Restart),
            GatewayAction::Enable => Some(Self::Enable),
            GatewayAction::Disable => Some(Self::Disable),
            GatewayAction::Status => None,
        }
    }
}

/// Beobachteter Laufzustand des Gateway-Dienstes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayRunState {
    /// Läuft; die PID ist bekannt, sofern das Backend sie liefert.
    Running(Option<u32>),
    /// Eingerichtet bzw. bekannt, aber nicht aktiv.
    Stopped,
    /// Keine Unit/kein LaunchAgent vorhanden.
    NotInstalled,
}

impl std::fmt::Display for GatewayRunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running(Some(pid)) => write!(f, "läuft (PID {pid})"),
            Self::Running(None) => f.write_str("läuft"),
            Self::Stopped => f.write_str("gestoppt"),
            Self::NotInstalled => f.write_str("nicht installiert"),
        }
    }
}

/// Beobachteter Autostart (Start beim Login) des Gateway-Dienstes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayAutostart {
    /// Der Dienstmanager startet den Gateway beim Login.
    Enabled,
    /// Eingerichtet, aber ohne Start beim Login.
    Disabled,
    /// Das Backend kennt keinen Autostart (Hintergrundprozess).
    Unavailable,
    /// Abfrage nicht möglich oder Ausgabe nicht deutbar.
    Unknown,
}

impl std::fmt::Display for GatewayAutostart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Enabled => "aktiviert",
            Self::Disabled => "deaktiviert",
            Self::Unavailable => "nicht verfügbar (kein Dienstmanager)",
            Self::Unknown => "unbekannt",
        })
    }
}

/// Prozessfamilie für die PID-Befehle des Rückfall-Backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcFamily {
    /// `kill` (Linux, macOS, BSD).
    Unix,
    /// `tasklist`/`taskkill`.
    Windows,
}

impl ProcFamily {
    /// Familie des laufenden Systems.
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Steuert den Gateway-Dienst `harw-gateway` des aktiven Profils.
///
/// # Description
/// Die Kurzform unter `harw gateway` ist für den täglichen Betrieb gedacht;
/// `harw service` bleibt die Verwaltung beider Hintergrunddienste. Das Backend
/// wählt [`select_gateway_backend`]: systemd-User-Units (`systemctl --user`),
/// launchd-LaunchAgents (`launchctl`) oder als portabler Rückfall ein
/// losgelöster Hintergrundprozess mit PID-Datei unter `<home>/run`. Nach jeder
/// erfolgreichen Aktion wird der beobachtete Zustand ausgegeben.
///
/// `status` verändert nichts und berichtet Backend, Laufzustand, Autostart und
/// die maßgeblichen Dateien ([`gateway_status`]); ein gestoppter oder nicht
/// installierter Dienst ist dabei kein Fehler.
///
/// # Errors
/// Home-/Executable-Auflösung, ungültiges [`GATEWAY_BACKEND_ENV`], fehlschlagende
/// Dienstmanager-Befehle (mit deren Diagnose), eine nicht durchführbare
/// Zustandsabfrage bei `status` sowie `enable`/`disable` im Rückfall-Backend,
/// das keinen Autostart kennt.
pub fn gateway_service(
    home_override: Option<PathBuf>,
    action: GatewayAction,
) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let backend = detect_gateway_backend()?;
    let [_serve, gateway] = service_specs(
        &std::env::current_exe().map_err(|error| error.to_string())?,
        &home,
    );
    let Some(op) = GatewayOp::from_action(&action) else {
        tracing::info!(backend = ?backend, "gateway.service.status");
        return gateway_status(backend, &gateway, &home);
    };
    tracing::info!(backend = ?backend, op = ?op, "gateway.service.action");
    match backend {
        GatewayBackend::SystemdUser => systemd_gateway(op, &gateway)?,
        GatewayBackend::Launchd => launchd_gateway(op, &gateway)?,
        GatewayBackend::Detached => detached_gateway(op, &gateway, &home)?,
    }
    match gateway_state(backend, &gateway, &home) {
        Ok(state) => println!("Gateway-Dienst [{backend}]: {state}"),
        Err(error) => tracing::warn!(error = %error, "gateway.service.status_unavailable"),
    }
    Ok(())
}

/// Erkennt das Gateway-Backend für das laufende System.
fn detect_gateway_backend() -> Result<GatewayBackend, String> {
    let platform = Platform::detect();
    let kind = detect_service_manager(&platform).kind();
    // Entspricht `sd_booted()`: nur mit laufendem systemd existiert dieses Verzeichnis.
    let systemd_booted = Path::new("/run/systemd/system").is_dir();
    let override_value = std::env::var(GATEWAY_BACKEND_ENV).ok();
    select_gateway_backend(kind, systemd_booted, override_value.as_deref())
}

/// Wählt das Gateway-Backend rein aus den beobachteten Fakten.
///
/// # Description
/// Ein nicht leerer `override_value` ([`GATEWAY_BACKEND_ENV`]) gewinnt. Sonst:
/// systemd nur, wenn der Plattform-Manager systemd ist **und** systemd
/// tatsächlich läuft (Container/WSL ohne systemd fallen zurück); launchd auf
/// macOS; alles andere (Windows, unbekannte Systeme) nutzt den portablen
/// Hintergrundprozess.
///
/// # Errors
/// Bei einem unbekannten Override-Wert.
fn select_gateway_backend(
    kind: ServiceKind,
    systemd_booted: bool,
    override_value: Option<&str>,
) -> Result<GatewayBackend, String> {
    if let Some(raw) = override_value.map(str::trim).filter(|raw| !raw.is_empty()) {
        return match raw.to_ascii_lowercase().as_str() {
            "systemd" => Ok(GatewayBackend::SystemdUser),
            "launchd" => Ok(GatewayBackend::Launchd),
            "detached" | "pid" => Ok(GatewayBackend::Detached),
            other => Err(format!(
                "{GATEWAY_BACKEND_ENV}={other} unbekannt (erlaubt: systemd, launchd, detached)"
            )),
        };
    }
    Ok(match kind {
        ServiceKind::Systemd if systemd_booted => GatewayBackend::SystemdUser,
        ServiceKind::Launchd => GatewayBackend::Launchd,
        ServiceKind::Systemd | ServiceKind::Schtasks | ServiceKind::Unsupported => {
            GatewayBackend::Detached
        }
    })
}

/// Fragt den Laufzustand über das gewählte Backend ab.
fn gateway_state(
    backend: GatewayBackend,
    spec: &ServiceSpec,
    home: &Path,
) -> Result<GatewayRunState, String> {
    match backend {
        GatewayBackend::SystemdUser => {
            let unit_dir = systemd_user_unit_dir(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            )?;
            if !unit_dir.join(format!("{}.service", spec.name)).is_file() {
                return Ok(GatewayRunState::NotInstalled);
            }
            // `is-active` endet bei inaktiven Units mit Status != 0; maßgeblich
            // ist die ausgegebene Zustandszeile.
            let output = run_command(&systemd_status_command(&spec.name))?;
            Ok(systemd_active_state(&String::from_utf8_lossy(
                &output.stdout,
            )))
        }
        GatewayBackend::Launchd => {
            let plist = launch_agent_plist_path(std::env::var_os("HOME"), &spec.name)?;
            let output = run_command(&launchd_status_command(&spec.name))?;
            if output.status.success() {
                Ok(
                    match launchctl_list_pid(&String::from_utf8_lossy(&output.stdout)) {
                        Some(pid) => GatewayRunState::Running(Some(pid)),
                        None => GatewayRunState::Stopped,
                    },
                )
            } else if plist.is_file() {
                Ok(GatewayRunState::Stopped)
            } else {
                Ok(GatewayRunState::NotInstalled)
            }
        }
        GatewayBackend::Detached => Ok(match detached_running_pid(&gateway_pid_path(home))? {
            Some(pid) => GatewayRunState::Running(Some(pid)),
            None => GatewayRunState::Stopped,
        }),
    }
}

/// `harw gateway status`: gibt den Zustand des Gateway-Dienstes aus, ohne
/// etwas zu verändern.
///
/// # Description
/// Nutzt dieselben Abfragen wie die Zustandszeile nach einer Aktion
/// ([`gateway_state`]) und ergänzt den Autostart ([`gateway_autostart`]) sowie
/// die Dateien des Backends (Unit, Plist bzw. PID-Datei und Log).
///
/// # Errors
/// Wenn der Laufzustand nicht abgefragt werden kann (z. B. `systemctl` oder
/// `launchctl` nicht ausführbar, PID-Datei unlesbar). Eine fehlschlagende
/// Autostart-Abfrage wird als „unbekannt" berichtet.
fn gateway_status(backend: GatewayBackend, spec: &ServiceSpec, home: &Path) -> Result<(), String> {
    let state = gateway_state(backend, spec, home)?;
    let autostart = match (backend, state) {
        (GatewayBackend::Detached, _) => Some(GatewayAutostart::Unavailable),
        (_, GatewayRunState::NotInstalled) => None,
        _ => Some(gateway_autostart(backend, &spec.name)),
    };
    let files: Vec<(&str, PathBuf)> = match backend {
        GatewayBackend::SystemdUser => systemd_user_unit_dir(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
        .map(|dir| vec![("Unit", dir.join(format!("{}.service", spec.name)))])
        .unwrap_or_default(),
        GatewayBackend::Launchd => launch_agent_plist_path(std::env::var_os("HOME"), &spec.name)
            .map(|plist| vec![("Plist", plist)])
            .unwrap_or_default(),
        GatewayBackend::Detached => vec![
            ("PID-Datei", gateway_pid_path(home)),
            ("Log", gateway_log_path(home)),
        ],
    };
    for line in render_gateway_status(backend, state, autostart, &files) {
        println!("{line}");
    }
    Ok(())
}

/// Formatiert den Statusbericht (rein).
///
/// # Arguments
/// - `autostart`: `None`, wenn er nicht sinnvoll ist (Dienst nicht installiert).
/// - `files`: beschriftete Pfade des Backends, in Ausgabereihenfolge.
///
/// # Returns
/// Die auszugebenden Zeilen; die erste nennt Backend und Laufzustand.
fn render_gateway_status(
    backend: GatewayBackend,
    state: GatewayRunState,
    autostart: Option<GatewayAutostart>,
    files: &[(&str, PathBuf)],
) -> Vec<String> {
    let mut lines = vec![format!("Gateway-Dienst [{backend}]: {state}")];
    if let Some(autostart) = autostart {
        lines.push(format!("  Autostart: {autostart}"));
    }
    lines.extend(
        files
            .iter()
            .map(|(label, path)| format!("  {label}: {}", path.display())),
    );
    if state == GatewayRunState::NotInstalled {
        lines.push("  Einrichten mit: harw gateway install".to_owned());
    }
    lines
}

/// Fragt den Autostart über den Dienstmanager ab; Fehler werden als
/// [`GatewayAutostart::Unknown`] berichtet (nur Diagnose, kein Abbruch).
fn gateway_autostart(backend: GatewayBackend, name: &str) -> GatewayAutostart {
    let observed = match backend {
        GatewayBackend::SystemdUser => run_command(&systemd_enabled_command(name))
            .map(|output| systemd_enabled_state(&String::from_utf8_lossy(&output.stdout))),
        GatewayBackend::Launchd => launchd_autostart(name),
        GatewayBackend::Detached => Ok(GatewayAutostart::Unavailable),
    };
    observed.unwrap_or_else(|error| {
        tracing::warn!(error = %error, "gateway.service.autostart_unavailable");
        GatewayAutostart::Unknown
    })
}

// ---------------------------------------------------------------------------
// systemd
// ---------------------------------------------------------------------------

/// Führt eine Gateway-Operation über `systemctl --user` aus (bisheriges Verhalten).
fn systemd_gateway(op: GatewayOp, spec: &ServiceSpec) -> Result<(), String> {
    if op == GatewayOp::Install {
        let manager = harw_install::service_systemd::SystemdServiceManager::new();
        let unit_dir = systemd_user_unit_dir(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )?;
        write_systemd_units(&unit_dir, &manager, std::slice::from_ref(spec))?;
    }
    for argv in systemd_gateway_commands(op, &spec.name) {
        run_checked(&argv)?;
    }
    Ok(())
}

/// Baut die `systemctl --user`-Aufrufe einer Operation (rein, ohne I/O).
///
/// # Returns
/// Die Argumentvektoren in Ausführungsreihenfolge (`argv[0]` = Programm).
/// `Install` setzt voraus, dass die Unit-Datei bereits geschrieben ist.
fn systemd_gateway_commands(op: GatewayOp, name: &str) -> Vec<Vec<String>> {
    let unit = format!("{name}.service");
    let systemctl = |args: &[&str]| -> Vec<String> {
        ["systemctl", "--user"]
            .iter()
            .chain(args)
            .map(|arg| (*arg).to_owned())
            .collect()
    };
    match op {
        GatewayOp::Install => vec![
            systemctl(&["daemon-reload"]),
            systemctl(&["enable", "--now", &unit]),
        ],
        GatewayOp::Start => vec![systemctl(&["start", &unit])],
        GatewayOp::Stop => vec![systemctl(&["stop", &unit])],
        GatewayOp::Restart => vec![systemctl(&["restart", &unit])],
        GatewayOp::Enable => vec![systemctl(&["enable", &unit])],
        GatewayOp::Disable => vec![systemctl(&["disable", &unit])],
    }
}

/// Argumentvektor der Statusabfrage `systemctl --user is-active <name>.service` (rein).
fn systemd_status_command(name: &str) -> Vec<String> {
    vec![
        "systemctl".to_owned(),
        "--user".to_owned(),
        "is-active".to_owned(),
        format!("{name}.service"),
    ]
}

/// Argumentvektor der Autostart-Abfrage `systemctl --user is-enabled <name>.service` (rein).
fn systemd_enabled_command(name: &str) -> Vec<String> {
    vec![
        "systemctl".to_owned(),
        "--user".to_owned(),
        "is-enabled".to_owned(),
        format!("{name}.service"),
    ]
}

/// Deutet die Ausgabe von `systemctl --user is-enabled` (rein).
///
/// `is-enabled` endet bei deaktivierten Units mit Status != 0; maßgeblich ist
/// die ausgegebene Zustandszeile.
fn systemd_enabled_state(stdout: &str) -> GatewayAutostart {
    match stdout.lines().next().map(str::trim) {
        Some("enabled" | "enabled-runtime") => GatewayAutostart::Enabled,
        Some(
            "disabled" | "masked" | "masked-runtime" | "linked" | "linked-runtime" | "static"
            | "indirect",
        ) => GatewayAutostart::Disabled,
        _ => GatewayAutostart::Unknown,
    }
}

/// Deutet die Ausgabe von `systemctl --user is-active` (rein).
fn systemd_active_state(stdout: &str) -> GatewayRunState {
    match stdout.lines().next().map(str::trim) {
        Some("active" | "activating" | "reloading" | "refreshing") => {
            GatewayRunState::Running(None)
        }
        _ => GatewayRunState::Stopped,
    }
}

// ---------------------------------------------------------------------------
// launchd
// ---------------------------------------------------------------------------

/// Führt eine Gateway-Operation über `launchctl` im GUI-Domain des Nutzers aus.
///
/// `install` schreibt `~/Library/LaunchAgents/<name>.plist` (gerendert von
/// [`harw_install::service_launchd::LaunchdServiceManager`], `RunAtLoad` +
/// `KeepAlive`) und lädt den Agent (bei bereits geladenem Agent neu).
fn launchd_gateway(op: GatewayOp, spec: &ServiceSpec) -> Result<(), String> {
    let plist = launch_agent_plist_path(std::env::var_os("HOME"), &spec.name)?;
    if op == GatewayOp::Install {
        write_launch_agent(&plist, spec)?;
    }
    let uid_output = run_checked(&["id".to_owned(), "-u".to_owned()])?;
    let domain = launchd_domain(parse_uid(&String::from_utf8_lossy(&uid_output.stdout))?);
    let loaded = run_command(&launchd_status_command(&spec.name))?
        .status
        .success();
    for argv in launchd_gateway_commands(op, &domain, &spec.name, &plist, loaded) {
        run_checked(&argv)?;
    }
    Ok(())
}

/// Schreibt das LaunchAgent-Plist des Gateways (Verzeichnis wird angelegt).
fn write_launch_agent(plist: &Path, spec: &ServiceSpec) -> Result<(), String> {
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "LaunchAgents-Verzeichnis {} nicht anlegbar: {error}",
                parent.display()
            )
        })?;
    }
    let manager = harw_install::service_launchd::LaunchdServiceManager::new();
    std::fs::write(plist, manager.render_unit(spec))
        .map_err(|error| format!("Plist {} nicht schreibbar: {error}", plist.display()))?;
    tracing::info!(plist = %plist.display(), "service.launch_agent.written");
    Ok(())
}

/// Pfad des LaunchAgent-Plist `$HOME/Library/LaunchAgents/<label>.plist`
/// (Umgebung wird übergeben, damit Tests kein `set_var` brauchen).
///
/// # Errors
/// Wenn `HOME` fehlt, leer oder relativ ist.
fn launch_agent_plist_path(home: Option<OsString>, label: &str) -> Result<PathBuf, String> {
    match home.map(PathBuf::from) {
        Some(home) if home.is_absolute() => Ok(home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{label}.plist"))),
        _ => Err("LaunchAgent-Pfad nicht bestimmbar: HOME fehlt oder ist relativ".to_owned()),
    }
}

/// launchd-Domain des angemeldeten Nutzers (`gui/<uid>`).
fn launchd_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

/// Liest die numerische UID aus der Ausgabe von `id -u` (rein).
fn parse_uid(stdout: &str) -> Result<u32, String> {
    stdout.trim().parse().map_err(|error| {
        format!(
            "UID aus `id -u` nicht lesbar ({:?}): {error}",
            stdout.trim()
        )
    })
}

/// Baut die `launchctl`-Aufrufe einer Operation (rein, ohne I/O).
///
/// # Description
/// Nutzt die Domain-Befehle (`bootstrap`/`bootout`/`kickstart`/`enable`/
/// `disable`) mit Ziel `<domain>/<label>`. Weil das Plist `KeepAlive` setzt,
/// stoppt nur `bootout` den Dienst dauerhaft (ein `kill` würde launchd neu
/// starten lassen); `start` lädt den Agent per `bootstrap` bzw. stößt einen
/// geladenen per `kickstart` an. `enable`/`disable` ändern wie bei systemd nur
/// den Autostart, nicht den Laufzustand.
///
/// # Arguments
/// - `loaded`: ob der Agent aktuell in launchd geladen ist.
fn launchd_gateway_commands(
    op: GatewayOp,
    domain: &str,
    label: &str,
    plist: &Path,
    loaded: bool,
) -> Vec<Vec<String>> {
    let target = format!("{domain}/{label}");
    let plist = plist.display().to_string();
    let launchctl = |args: &[&str]| -> Vec<String> {
        std::iter::once("launchctl")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect()
    };
    let bootstrap = launchctl(&["bootstrap", domain, &plist]);
    match op {
        GatewayOp::Install if loaded => vec![
            launchctl(&["enable", &target]),
            launchctl(&["bootout", &target]),
            bootstrap,
        ],
        GatewayOp::Install => vec![launchctl(&["enable", &target]), bootstrap],
        GatewayOp::Start if loaded => vec![launchctl(&["kickstart", &target])],
        GatewayOp::Start => vec![bootstrap],
        GatewayOp::Stop if loaded => vec![launchctl(&["bootout", &target])],
        GatewayOp::Stop => Vec::new(),
        GatewayOp::Restart if loaded => vec![launchctl(&["kickstart", "-k", &target])],
        GatewayOp::Restart => vec![bootstrap],
        GatewayOp::Enable => vec![launchctl(&["enable", &target])],
        GatewayOp::Disable => vec![launchctl(&["disable", &target])],
    }
}

/// Argumentvektor der Status-/Ladeabfrage `launchctl list <label>` (rein);
/// Exit-Status 0 heißt „geladen".
fn launchd_status_command(label: &str) -> Vec<String> {
    vec!["launchctl".to_owned(), "list".to_owned(), label.to_owned()]
}

/// Argumentvektor der Abfrage `launchctl print-disabled <domain>` (rein), die
/// die per `launchctl disable` abgeschalteten Labels der Domain auflistet.
fn launchd_print_disabled_command(domain: &str) -> Vec<String> {
    vec![
        "launchctl".to_owned(),
        "print-disabled".to_owned(),
        domain.to_owned(),
    ]
}

/// Liest aus `launchctl print-disabled`, ob `label` abgeschaltet ist (rein).
///
/// Erwartet Zeilen der Form `"<label>" => disabled|enabled` (ältere
/// macOS-Versionen: `=> true|false`).
///
/// # Returns
/// `Some(true)` für abgeschaltet, `Some(false)` für ausdrücklich zugelassen,
/// `None`, wenn das Label nicht aufgeführt ist.
fn launchctl_label_disabled(stdout: &str, label: &str) -> Option<bool> {
    let quoted = format!("\"{label}\"");
    stdout.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(quoted.as_str())?.trim_start();
        let value = rest.strip_prefix("=>")?.trim().trim_end_matches(';').trim();
        match value {
            "disabled" | "true" => Some(true),
            "enabled" | "false" => Some(false),
            _ => None,
        }
    })
}

/// Autostart des LaunchAgent: Das Plist setzt `RunAtLoad`, daher startet der
/// Agent beim Login, solange das Plist existiert und das Label nicht per
/// `launchctl disable` abgeschaltet ist.
fn launchd_autostart(label: &str) -> Result<GatewayAutostart, String> {
    let plist = launch_agent_plist_path(std::env::var_os("HOME"), label)?;
    if !plist.is_file() {
        return Ok(GatewayAutostart::Disabled);
    }
    let uid_output = run_checked(&["id".to_owned(), "-u".to_owned()])?;
    let domain = launchd_domain(parse_uid(&String::from_utf8_lossy(&uid_output.stdout))?);
    let output = run_checked(&launchd_print_disabled_command(&domain))?;
    Ok(
        match launchctl_label_disabled(&String::from_utf8_lossy(&output.stdout), label) {
            Some(true) => GatewayAutostart::Disabled,
            Some(false) | None => GatewayAutostart::Enabled,
        },
    )
}

/// Liest die PID aus der Ausgabe von `launchctl list <label>` (rein).
///
/// Erwartet eine Zeile der Form `"PID" = 123;`; fehlt sie, ist der Agent
/// geladen, aber nicht laufend.
fn launchctl_list_pid(stdout: &str) -> Option<u32> {
    stdout.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("\"PID\"")?.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        rest.trim_end_matches(';').trim().parse().ok()
    })
}

// ---------------------------------------------------------------------------
// Portabler Rückfall: Hintergrundprozess mit PID-Datei
// ---------------------------------------------------------------------------

/// PID-Datei des losgelösten Gateways: `<home>/run/harw-gateway.pid`.
fn gateway_pid_path(home: &Path) -> PathBuf {
    home.join("run").join(format!("{GATEWAY_SERVICE_NAME}.pid"))
}

/// Log des losgelösten Gateways (stdout+stderr): `<home>/logs/harw-gateway.log`.
fn gateway_log_path(home: &Path) -> PathBuf {
    harw_home::paths::logs_dir(home).join(format!("{GATEWAY_SERVICE_NAME}.log"))
}

/// Führt eine Gateway-Operation über den PID-basierten Rückfall aus.
fn detached_gateway(op: GatewayOp, spec: &ServiceSpec, home: &Path) -> Result<(), String> {
    let pid_path = gateway_pid_path(home);
    let log_path = gateway_log_path(home);
    match op {
        GatewayOp::Install => {
            detached_start(spec, &pid_path, &log_path)?;
            println!(
                "Hinweis: kein nutzbarer Dienstmanager (systemd/launchd) erkannt — der Gateway \
                 läuft als Hintergrundprozess ohne Autostart beim Login und ohne automatischen \
                 Neustart nach Absturz."
            );
            Ok(())
        }
        GatewayOp::Start => detached_start(spec, &pid_path, &log_path),
        GatewayOp::Stop => detached_stop(&pid_path),
        GatewayOp::Restart => {
            detached_stop(&pid_path)?;
            detached_start(spec, &pid_path, &log_path)
        }
        GatewayOp::Enable | GatewayOp::Disable => Err(format!(
            "Autostart nicht verfügbar: kein Dienstmanager (systemd/launchd) erkannt; der \
             Gateway läuft nur als Hintergrundprozess ({GATEWAY_BACKEND_ENV}=systemd|launchd \
             erzwingt ein Backend)."
        )),
    }
}

/// Startet den Gateway losgelöst und schreibt die PID-Datei.
///
/// Läuft bereits ein Gateway laut PID-Datei, passiert nichts. Beendet sich der
/// neue Prozess innerhalb von [`DETACHED_STARTUP_PROBE`], wird das als Fehler
/// mit Verweis auf das Log gemeldet und die PID-Datei entfernt.
#[allow(
    clippy::zombie_processes,
    reason = "der Gateway soll den CLI-Prozess überleben; nach dem Ende von `harw` übernimmt init das Einsammeln"
)]
fn detached_start(spec: &ServiceSpec, pid_path: &Path, log_path: &Path) -> Result<(), String> {
    if let Some(pid) = detached_running_pid(pid_path)? {
        println!("Gateway läuft bereits (PID {pid}).");
        return Ok(());
    }
    let mut child = spawn_detached(spec, log_path)?;
    let pid = child.id();
    if let Err(error) = write_pid_file(pid_path, pid) {
        if let Err(kill_error) = child.kill() {
            tracing::warn!(error = %kill_error, pid, "gateway.detached.kill_after_pidfile_failure");
        }
        return Err(error);
    }
    std::thread::sleep(DETACHED_STARTUP_PROBE);
    match child.try_wait() {
        Ok(None) => {
            tracing::info!(pid, pid_file = %pid_path.display(), "gateway.detached.started");
            println!("Gateway gestartet (PID {pid}); Log: {}", log_path.display());
            Ok(())
        }
        Ok(Some(status)) => {
            remove_pid_file(pid_path)?;
            Err(format!(
                "Gateway beendete sich direkt nach dem Start ({status}); Log: {}",
                log_path.display()
            ))
        }
        Err(error) => Err(format!(
            "Zustand des Gateway-Prozesses {pid} unbekannt: {error}"
        )),
    }
}

/// Startet `spec.exec` losgelöst: eigene Prozessgruppe (Unix) bzw. ohne
/// Konsole (Windows), stdin leer, stdout/stderr an `log_path` angehängt.
fn spawn_detached(spec: &ServiceSpec, log_path: &Path) -> Result<std::process::Child, String> {
    let (program, args) = spec
        .exec
        .split_first()
        .ok_or_else(|| format!("Dienst {} ohne Programm", spec.name))?;
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| {
            format!("Log-Verzeichnis {} nicht anlegbar: {error}", dir.display())
        })?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|error| format!("Log {} nicht öffenbar: {error}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|error| format!("Log {} nicht duplizierbar: {error}", log_path.display()))?;
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .current_dir(&spec.working_dir)
        .envs(spec.env.iter().map(|(key, value)| (key, value)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err));
    detach_command(&mut command);
    command
        .spawn()
        .map_err(|error| format!("Gateway-Prozess {program} nicht startbar: {error}"))
}

/// Löst den Kindprozess von Terminal und Prozessgruppe des Aufrufers, damit
/// Ctrl-C bzw. das Schließen der Shell ihn nicht mitbeendet.
#[cfg(unix)]
fn detach_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

/// Löst den Kindprozess von Terminal und Prozessgruppe des Aufrufers, damit
/// Ctrl-C bzw. das Schließen der Konsole ihn nicht mitbeendet.
#[cfg(windows)]
fn detach_command(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Ohne Unix/Windows gibt es keine Loslösung; der Prozess erbt die Gruppe.
#[cfg(not(any(unix, windows)))]
fn detach_command(_command: &mut std::process::Command) {}

/// Stoppt den losgelösten Gateway: geordnet, nach [`DETACHED_STOP_GRACE`] hart.
fn detached_stop(pid_path: &Path) -> Result<(), String> {
    let Some(pid) = detached_running_pid(pid_path)? else {
        remove_pid_file(pid_path)?;
        println!("Gateway läuft nicht.");
        return Ok(());
    };
    let family = ProcFamily::current();
    // Ein gescheitertes geordnetes Signal ist kein Abbruchgrund: danach folgt
    // ohnehin das harte Beenden (unter Windows lehnt `taskkill` ohne `/F`
    // konsolenlose Prozesse regelmäßig ab).
    match run_command(&pid_terminate_command(family, pid, false)) {
        Ok(output) if !output.status.success() => tracing::warn!(
            pid,
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "gateway.detached.graceful_stop_refused"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(pid, error = %error, "gateway.detached.graceful_stop_failed"),
    }
    if wait_for_exit(family, pid, DETACHED_STOP_GRACE)? {
        remove_pid_file(pid_path)?;
        println!("Gateway gestoppt (PID {pid}).");
        return Ok(());
    }
    run_checked(&pid_terminate_command(family, pid, true))?;
    if wait_for_exit(family, pid, DETACHED_KILL_GRACE)? {
        remove_pid_file(pid_path)?;
        println!("Gateway hart beendet (PID {pid}).");
        Ok(())
    } else {
        Err(format!("Gateway-Prozess {pid} lässt sich nicht beenden"))
    }
}

/// Wartet höchstens `grace`, bis `pid` nicht mehr lebt.
///
/// # Returns
/// `true`, wenn der Prozess innerhalb der Frist verschwunden ist.
fn wait_for_exit(family: ProcFamily, pid: u32, grace: std::time::Duration) -> Result<bool, String> {
    let deadline = std::time::Instant::now() + grace;
    loop {
        if !pid_alive(family, pid)? {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(DETACHED_POLL_INTERVAL);
    }
}

/// Liefert die PID des laufenden Gateways laut PID-Datei.
///
/// # Returns
/// `None`, wenn die Datei fehlt, unlesbaren Inhalt hat, der Prozess nicht mehr
/// lebt oder die PID inzwischen einem anderen Programm gehört (Prüfung über
/// `/proc/<pid>/cmdline`, wo vorhanden). Eine solche veraltete Datei bleibt
/// liegen und wird beim nächsten Start/Stopp überschrieben bzw. entfernt.
fn detached_running_pid(pid_path: &Path) -> Result<Option<u32>, String> {
    let raw = match std::fs::read_to_string(pid_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "PID-Datei {} nicht lesbar: {error}",
                pid_path.display()
            ));
        }
    };
    let Some(pid) = parse_pid_file(&raw) else {
        tracing::warn!(pid_file = %pid_path.display(), "gateway.detached.pidfile_invalid");
        return Ok(None);
    };
    if !pid_alive(ProcFamily::current(), pid)? {
        return Ok(None);
    }
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) if !cmdline_is_gateway(&cmdline) => {
            tracing::warn!(pid, "gateway.detached.pid_reused");
            Ok(None)
        }
        _ => Ok(Some(pid)),
    }
}

/// Schreibt die PID-Datei (Verzeichnis wird angelegt).
fn write_pid_file(pid_path: &Path, pid: u32) -> Result<(), String> {
    if let Some(dir) = pid_path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| {
            format!("PID-Verzeichnis {} nicht anlegbar: {error}", dir.display())
        })?;
    }
    std::fs::write(pid_path, format!("{pid}\n"))
        .map_err(|error| format!("PID-Datei {} nicht schreibbar: {error}", pid_path.display()))
}

/// Entfernt die PID-Datei; eine fehlende Datei ist kein Fehler.
fn remove_pid_file(pid_path: &Path) -> Result<(), String> {
    match std::fs::remove_file(pid_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "PID-Datei {} nicht entfernbar: {error}",
            pid_path.display()
        )),
    }
}

/// Liest eine PID aus dem Inhalt der PID-Datei (rein); `0` ist ungültig.
fn parse_pid_file(raw: &str) -> Option<u32> {
    raw.trim().parse().ok().filter(|pid| *pid > 0)
}

/// Prüft, ob eine `/proc/<pid>/cmdline` (NUL-getrennt) den Subcommand
/// `gateway` enthält (rein) — Schutz gegen wiederverwendete PIDs.
fn cmdline_is_gateway(raw: &[u8]) -> bool {
    raw.split(|byte| *byte == 0)
        .skip(1)
        .any(|arg| arg == b"gateway")
}

/// Prüft über den externen Probe-Befehl, ob `pid` lebt.
fn pid_alive(family: ProcFamily, pid: u32) -> Result<bool, String> {
    let output = run_command(&pid_probe_command(family, pid))?;
    Ok(match family {
        ProcFamily::Unix => output.status.success(),
        ProcFamily::Windows => tasklist_lists_pid(&String::from_utf8_lossy(&output.stdout), pid),
    })
}

/// Argumentvektor der Lebendprüfung (rein): `kill -0 <pid>` bzw.
/// `tasklist /FI "PID eq <pid>" /NH /FO CSV`.
fn pid_probe_command(family: ProcFamily, pid: u32) -> Vec<String> {
    match family {
        ProcFamily::Unix => vec!["kill".to_owned(), "-0".to_owned(), pid.to_string()],
        ProcFamily::Windows => vec![
            "tasklist".to_owned(),
            "/FI".to_owned(),
            format!("PID eq {pid}"),
            "/NH".to_owned(),
            "/FO".to_owned(),
            "CSV".to_owned(),
        ],
    }
}

/// Argumentvektor des Beendens (rein): `kill -TERM|-KILL <pid>` bzw.
/// `taskkill [/F] /PID <pid>`.
fn pid_terminate_command(family: ProcFamily, pid: u32, force: bool) -> Vec<String> {
    match (family, force) {
        (ProcFamily::Unix, false) => vec!["kill".to_owned(), "-TERM".to_owned(), pid.to_string()],
        (ProcFamily::Unix, true) => vec!["kill".to_owned(), "-KILL".to_owned(), pid.to_string()],
        (ProcFamily::Windows, false) => {
            vec!["taskkill".to_owned(), "/PID".to_owned(), pid.to_string()]
        }
        (ProcFamily::Windows, true) => vec![
            "taskkill".to_owned(),
            "/F".to_owned(),
            "/PID".to_owned(),
            pid.to_string(),
        ],
    }
}

/// Prüft, ob die CSV-Ausgabe von `tasklist` eine Zeile für `pid` enthält (rein).
fn tasklist_lists_pid(stdout: &str, pid: u32) -> bool {
    let needle = format!(",\"{pid}\",");
    stdout.lines().any(|line| line.contains(&needle))
}

// ---------------------------------------------------------------------------
// Befehlsausführung
// ---------------------------------------------------------------------------

/// Führt einen Argumentvektor aus und liefert die vollständige Ausgabe.
///
/// # Errors
/// Leerer Vektor oder nicht startbares Programm.
fn run_command(argv: &[String]) -> Result<std::process::Output, String> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| "leerer Befehl".to_owned())?;
    std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("{} nicht ausführbar: {error}", argv.join(" ")))
}

/// Wie [`run_command`], wertet aber einen Exit-Status != 0 als Fehler und
/// gibt dessen Diagnose (stderr) vollständig weiter.
fn run_checked(argv: &[String]) -> Result<std::process::Output, String> {
    let output = run_command(argv)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{} fehlgeschlagen: {}",
            argv.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Builds the service descriptions `harw service` manages: `serve` first, then
/// `gateway` (G-065).
///
/// # Arguments
/// - `exe` (`&Path`): absolute path of the running `harw` binary (`ExecStart`).
/// - `home` (`&Path`): resolved HARW home; used as `WorkingDirectory` and
///   exported as `HARW_HOME`.
///
/// # Returns
/// `[serve, gateway]` specs with `restart_sec` = [`SERVICE_RESTART_SEC`].
fn service_specs(exe: &Path, home: &Path) -> [ServiceSpec; 2] {
    let make = |name: &str, subcommand: &str| ServiceSpec {
        name: name.to_owned(),
        exec: vec![exe.display().to_string(), subcommand.to_owned()],
        working_dir: home.to_path_buf(),
        env: vec![("HARW_HOME".to_owned(), home.display().to_string())],
        restart_sec: SERVICE_RESTART_SEC,
    };
    [
        make(SERVE_SERVICE_NAME, "serve"),
        make(GATEWAY_SERVICE_NAME, "gateway"),
    ]
}

/// Resolves the systemd user-unit directory from the environment values
/// (passed in, so tests need no `set_var`).
///
/// # Description
/// systemd reads user units from `$XDG_CONFIG_HOME/systemd/user`, falling back
/// to `$HOME/.config/systemd/user` when `XDG_CONFIG_HOME` is unset. A relative
/// or empty `XDG_CONFIG_HOME` is invalid per the XDG spec and ignored.
///
/// # Errors
/// When neither a usable `XDG_CONFIG_HOME` nor a non-empty `HOME` is available.
fn systemd_user_unit_dir(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, String> {
    let config_root = match xdg_config_home.map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => xdg,
        _ => match home {
            Some(home) if !home.is_empty() => PathBuf::from(home).join(".config"),
            _ => {
                return Err(
                    "systemd-User-Unit-Verzeichnis nicht bestimmbar: weder XDG_CONFIG_HOME \
                     noch HOME gesetzt"
                        .to_owned(),
                );
            }
        },
    };
    Ok(config_root.join("systemd").join("user"))
}

/// Writes one `<name>.service` file per spec into `unit_dir` (created if
/// missing). Does not call `systemctl`.
///
/// # Arguments
/// - `unit_dir` (`&Path`): target directory, usually from
///   [`systemd_user_unit_dir`].
/// - `manager` (`&dyn ServiceManager`): renders the unit text
///   ([`ServiceManager::render_unit`], pure).
/// - `specs` (`&[ServiceSpec]`): services to write.
///
/// # Returns
/// The written file paths, in `specs` order.
///
/// # Errors
/// Directory creation or file write failures, naming the path.
fn write_systemd_units(
    unit_dir: &Path,
    manager: &dyn ServiceManager,
    specs: &[ServiceSpec],
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(unit_dir).map_err(|error| {
        format!(
            "Unit-Verzeichnis {} nicht anlegbar: {error}",
            unit_dir.display()
        )
    })?;
    specs
        .iter()
        .map(|spec| {
            let path = unit_dir.join(format!("{}.service", spec.name));
            std::fs::write(&path, manager.render_unit(spec))
                .map_err(|error| format!("Unit {} nicht schreibbar: {error}", path.display()))?;
            tracing::info!(unit = %path.display(), "service.unit.written");
            Ok(path)
        })
        .collect()
}

/// Lädt die soeben geschriebenen User-Units neu und aktiviert sie dauerhaft.
///
/// Fehler werden nicht verschluckt: Ohne laufenden User-Manager (oder ohne
/// DBus-Session) wäre eine geschriebene Unit kein funktionierender Dienst.
fn activate_systemd_units(specs: &[ServiceSpec]) -> Result<(), String> {
    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .map_err(|error| format!("systemctl --user daemon-reload nicht ausführbar: {error}"))?;
    if !reload.status.success() {
        return Err(format!(
            "systemctl --user daemon-reload fehlgeschlagen: {}",
            String::from_utf8_lossy(&reload.stderr).trim()
        ));
    }
    let units = specs
        .iter()
        .map(|spec| format!("{}.service", spec.name))
        .collect::<Vec<_>>();
    let start = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now"])
        .args(&units)
        .output()
        .map_err(|error| format!("systemctl --user enable --now nicht ausführbar: {error}"))?;
    if !start.status.success() {
        return Err(format!(
            "systemctl --user enable --now fehlgeschlagen: {}",
            String::from_utf8_lossy(&start.stderr).trim()
        ));
    }
    Ok(())
}

/// Which process signal requested the shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownReason {
    /// `SIGTERM` (e.g. `systemctl stop`).
    Terminate,
    /// `SIGINT` (Ctrl-C).
    Interrupt,
}

impl ShutdownReason {
    /// Returns the conventional signal name for logs.
    pub(crate) fn signal_name(self) -> &'static str {
        match self {
            Self::Terminate => "SIGTERM",
            Self::Interrupt => "SIGINT",
        }
    }
}

/// Registered SIGTERM/SIGINT listeners for a daemon's orderly shutdown (G-022).
///
/// # Concurrency
/// Created inside a Tokio runtime with the IO driver enabled; once installed,
/// the signals no longer terminate the process by default — the owner must
/// [`wait`](Self::wait) and shut down.
pub(crate) struct ShutdownSignals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
}

impl ShutdownSignals {
    /// Registers the SIGTERM and SIGINT handlers.
    ///
    /// # Errors
    /// When the signal driver cannot register a handler (e.g. called outside a
    /// runtime with IO enabled).
    pub(crate) fn install() -> Result<Self, String> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let terminate = signal(SignalKind::terminate())
                .map_err(|error| format!("SIGTERM-Handler nicht registrierbar: {error}"))?;
            let interrupt = signal(SignalKind::interrupt())
                .map_err(|error| format!("SIGINT-Handler nicht registrierbar: {error}"))?;
            Ok(Self {
                terminate,
                interrupt,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    /// Resolves when the first shutdown signal arrives.
    pub(crate) async fn wait(self) -> ShutdownReason {
        #[cfg(unix)]
        {
            let Self {
                mut terminate,
                mut interrupt,
            } = self;
            first_signal(terminate.recv(), interrupt.recv()).await
        }
        #[cfg(not(unix))]
        {
            first_signal(std::future::pending(), async {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => Some(()),
                    Err(error) => {
                        tracing::warn!(error = %error, "ctrl-c handler unavailable");
                        None
                    }
                }
            })
            .await
        }
    }
}

/// Maps the first delivered signal to a [`ShutdownReason`].
///
/// # Description
/// A stream yielding `None` is closed and can never deliver again; its branch
/// is disabled instead of being mistaken for a signal. If both are closed the
/// future stays pending (the daemon keeps serving, as before signal handling).
async fn first_signal<T, I>(terminate: T, interrupt: I) -> ShutdownReason
where
    T: Future<Output = Option<()>>,
    I: Future<Output = Option<()>>,
{
    tokio::select! {
        Some(()) = terminate => ShutdownReason::Terminate,
        Some(()) = interrupt => ShutdownReason::Interrupt,
        else => {
            tracing::warn!("shutdown signal streams closed; no further signals can arrive");
            std::future::pending::<ShutdownReason>().await
        }
    }
}

/// `harw catalog [--refresh]`: listet den Provider-Katalog.
pub fn catalog(home_override: Option<PathBuf>, refresh: bool) -> Result<(), String> {
    let mut catalog = harw_model_catalog::embedded_catalog();
    let home = resolve_home(home_override)?;
    if refresh {
        harw_model_catalog::models_dev::refresh_models(&home.join("cache"), &mut catalog)
            .map_err(|error| error.to_string())?;
    }
    println!("{:<12} {:<28} BASE-URL", "ID", "NAME");
    for provider in &catalog {
        println!(
            "{:<12} {:<28} {}",
            provider.id, provider.name, provider.base_url
        );
    }
    Ok(())
}

/// `harw uninstall --scope … [--dry-run] [--yes]`: entfernt Artefakte.
pub fn uninstall(
    home_override: Option<PathBuf>,
    scope: &[String],
    dry_run: bool,
    yes: bool,
) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let scopes: Vec<UninstallScope> = scope
        .iter()
        .filter_map(|raw| match raw.as_str() {
            "service" => Some(UninstallScope::Service),
            "state" => Some(UninstallScope::State),
            "workspace" => Some(UninstallScope::Workspace),
            "binary" => Some(UninstallScope::Binary),
            _ => None,
        })
        .collect();
    if scopes.is_empty() {
        return Err("kein gültiger --scope (service|state|workspace|binary)".to_owned());
    }

    let plan = harw_install::uninstall::plan(&home, &scopes);
    println!("Zu entfernen:");
    for path in &plan.removals {
        println!("  {}", path.display());
    }
    if !plan.preserved.is_empty() {
        println!("Bleibt erhalten:");
        for path in &plan.preserved {
            println!("  {}", path.display());
        }
    }

    if dry_run {
        println!("(dry-run — nichts entfernt)");
        return Ok(());
    }
    if !yes {
        return Err("Bestätigung nötig: `--yes` zum tatsächlichen Entfernen".to_owned());
    }
    harw_install::uninstall::execute(&plan, false).map_err(|error| error.to_string())?;
    println!("Entfernt.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn health_evidence_observes_default_cli_composition() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;
        // A bare home (no `config.toml`) never carries `[policy]
        // .require_approval_for`, so `ApprovalChain::snapshot` never emits a
        // `ConfigPolicy` entry (`harw-runtime/src/approval.rs::for_root`) —
        // `has_approval_boundary` would be `false` regardless of approval
        // mode. This is the genuine positive case: a configured policy, with
        // the run's default (non-`FullAccess`) approval mode.
        std::fs::write(
            home.path().join("config.toml"),
            "[policy]\nrequire_approval_for = [\"shell.exec\"]\n",
        )
        .map_err(ctx("write policy config into temporary HARW home"))?;

        let evidence =
            runtime_composition_evidence(home.path()).map_err(ctx("assemble CLI composition"))?;

        // Die Liste ist bewusst vollständig ausgeschrieben und nicht auf eine
        // Mindestmenge geprüft: eine Änderung der Werkzeugfläche der Standard-
        // Zusammenstellung soll auffallen, nicht durchrutschen. `fs.glob` und
        // `fs.grep` kamen mit den Recherche-Werkzeugen hinzu (AP W2-01/02).
        // `status`/`ps`/`diff`/`stop` sind vier der `harw-ops`-Operationen, die
        // `model_tool(...)` deklarieren (`harw-ops/src/{status,ps,diff,stop}.rs`)
        // und über `harw_ops::register_all` (`harw-runtime/src/assembly.rs`)
        // unbedingt in jede Zusammenstellung eingehängt werden — unabhängig vom
        // `RegistryProfile` und ohne das `[tools.plan]`-Gate (das nur
        // `plan`/`goal` hinzufügt, hier nicht aktiv). `cancel` reiht sich seit
        // der Genehmigungs-Befehlsgruppe (`/approve /deny /review /cancel
        // /retry`, Interaktionsvertrag §2.3/§4) in dieselbe Grundausstattung
        // ein: `harw-ops/src/cancel.rs` deklariert als einzige der fünf neuen
        // Befehle ein `model_tool(approval = "always")` (siehe dortige
        // Moduldoku, Abschnitt „Verhältnis zu /stop" — dieselbe
        // `JobStore::cancel`-Transition wie `/stop`, nur über die
        // Genehmigungs-Befehlsgruppe erreichbar), `approve`/`deny`/`review`/
        // `retry` bleiben bewusst reine Commands ohne `ModelTool` und tauchen
        // hier deshalb nicht auf. Sie gehören deshalb zur Grundausstattung,
        // nicht nur zu einer optionalen Erweiterung wie `browser.*`.
        // `rights_snapshot().tools` liefert sortiert und dublettenfrei
        // (harw-runtime/src/assembly.rs `rights_snapshot`), deshalb wird hier
        // als Menge statt per `starts_with`-Präfix verglichen.
        let base: HashSet<&str> = [
            "fs.read",
            "fs.write",
            "fs.edit",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
            "shell.exec",
            "status",
            "ps",
            "diff",
            "stop",
            "cancel",
            // Explorer-Werkzeuge (`explore.*`) gehören seit der Explorer-
            // Verdrahtung zum Full-Profil, `doc.read_pdf` seit dem PDF-Fetch
            // (harw-registry-defaults/src/profile.rs `DOC_TOOLS`).
            "explore.find",
            "explore.tree",
            "explore.relations",
            "explore.projects",
            "doc.read_pdf",
            // `process.list`/`process.kill` im Full-Profil; `process.kill`
            // bleibt über ALWAYS_ASK_TOOLS immer freigabepflichtig.
            "process.list",
            "process.kill",
            // Weitere `harw-ops`-Operationen mit `model_tool(...)`:
            // `/provider-concurrency` (harw-ops/src/provider.rs,
            // approval = "always") und `/sandbox-lease`
            // (harw-ops/src/sandbox_lease.rs).
            "provider-concurrency",
            "sandbox-lease",
            // Workbench-Werkzeuge (harw-registry-defaults/src/workbench_tools.rs):
            // Notizen und Hypothesen der Sitzung, Recht `ReadWorkspace`.
            "workbench.note",
            "workbench.hypothesis",
            // Lesende Wissenswerkzeuge der Wurzel (Plan Teil D, assembly.rs
            // Schritt 12a–12a'''): Workbench, Palace, Diary — alle
            // `ReadWorkspace`, auto-freigegeben. `kanban.*` bekommt nur die
            // TUI-Wurzel bzw. ein Root-Orchestrator (Schritt 12a''), nicht
            // `Doctor`.
            "workbench.show",
            "palace.search",
            "palace.recall",
            "diary.read",
            // Plan R9, Teil A: der lesende Skill-Katalog der Wurzel
            // (`SkillCatalogToolProvider`, ohne Rechteklasse,
            // auto-freigegeben) — jeder Einstieg mit Werkzeugen außer
            // `NoTools`/`WorkspaceEdit`.
            "skills.search",
            "skills.load",
            // Runde 5, Teil F: Plan-Modus-Werkzeuge der Wurzel (Schritt 12e,
            // `AllWithModelTools`); ohne TUI-Kanal schlagen sie fail-closed
            // mit klarer Meldung fehl.
            "plan.write",
            "plan.exit",
            "plan.enter",
            "ask_user",
        ]
        .into_iter()
        .collect();
        let browser: HashSet<&str> = [
            "browser.open",
            "browser.observe",
            "browser.find",
            "browser.act",
            "browser.wait",
            "browser.events",
            "browser.close",
        ]
        .into_iter()
        .collect();
        // Workspace --all-features unifies the registry's optional browser
        // feature into this binary, even though the CLI has no such feature.
        let with_browser: HashSet<&str> = base.union(&browser).copied().collect();

        let advertised = evidence
            .advertised_tools
            .ok_or(TestError::Missing("advertised tool evidence"))?;
        let advertised: HashSet<&str> = advertised.iter().map(String::as_str).collect();
        assert!(
            advertised == base || advertised == with_browser,
            "unexpected tool surface: {advertised:?}"
        );
        assert_eq!(evidence.has_trusted_spawn_context, Some(true));
        assert_eq!(evidence.has_approval_boundary, Some(true));
        Ok(())
    }

    #[test]
    fn update_check_reports_unavailable_without_creating_version_state() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;

        let result = update(Some(home.path().to_path_buf()), true);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "update check must fail without a remote checker".into(),
            ));
        };

        assert!(error.contains("kein Remote-Update-Checker ist konfiguriert"));
        assert!(!home.path().join("version.json").exists());
        Ok(())
    }

    #[test]
    fn update_check_leaves_existing_version_state_unchanged() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;
        let checker = UpdateChecker::new(home.path());
        checker
            .write(&harw_install::VersionInfo {
                latest_version: "1.2.3".to_owned(),
                last_checked_at: jiff::Timestamp::now(),
                dismissed_version: Some("1.2.2".to_owned()),
            })
            .map_err(ctx("write existing version state"))?;
        let version_path = home.path().join("version.json");
        let before = std::fs::read(&version_path).map_err(ctx("read existing version state"))?;

        let result = update(Some(home.path().to_path_buf()), true);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "update check must fail without a remote checker".into(),
            ));
        };

        assert!(error.contains("kein Remote-Update-Checker ist konfiguriert"));
        assert_eq!(
            std::fs::read(version_path).map_err(ctx("read version state after failed check"))?,
            before
        );
        Ok(())
    }

    /// A chain carrying both the config policy and the default policy, in
    /// the order [`harw_runtime::approval::ApprovalChain::snapshot`]
    /// documents (config first).
    fn chain_with_config_policy() -> Vec<(&'static str, ApprovalHandlerKind)> {
        vec![
            ("config-policy", ApprovalHandlerKind::ConfigPolicy),
            ("default-policy", ApprovalHandlerKind::DefaultPolicy),
        ]
    }

    /// A chain carrying only the always-present default policy — the shape
    /// every bare, unconfigured run produces.
    fn chain_without_config_policy() -> Vec<(&'static str, ApprovalHandlerKind)> {
        vec![("default-policy", ApprovalHandlerKind::DefaultPolicy)]
    }

    #[test]
    fn test_evaluate_evidence_both_true_when_configured_and_bound() {
        let (has_trusted_spawn_context, has_approval_boundary) = evaluate_evidence(
            &chain_with_config_policy(),
            ApprovalMode::Delegated,
            true,
            true,
        );

        assert!(has_trusted_spawn_context);
        assert!(has_approval_boundary);
    }

    #[test]
    fn test_evaluate_evidence_no_boundary_without_config_policy_entry() {
        let (_, has_approval_boundary) = evaluate_evidence(
            &chain_without_config_policy(),
            ApprovalMode::Delegated,
            true,
            true,
        );

        assert!(
            !has_approval_boundary,
            "the default policy alone (always present) must not count as a boundary"
        );
    }

    #[test]
    fn test_evaluate_evidence_no_boundary_under_full_access_mode() {
        let (_, has_approval_boundary) = evaluate_evidence(
            &chain_with_config_policy(),
            ApprovalMode::FullAccess,
            true,
            true,
        );

        assert!(
            !has_approval_boundary,
            "full-access mode waves every call through regardless of the chain"
        );
    }

    #[test]
    fn test_evaluate_evidence_no_boundary_under_always_ask_mode_is_still_config_gated() {
        // AlwaysAsk is not FullAccess, so the mode half of the condition is
        // satisfied; without a configured policy the boundary is still absent.
        let (_, has_approval_boundary) = evaluate_evidence(
            &chain_without_config_policy(),
            ApprovalMode::AlwaysAsk,
            true,
            true,
        );

        assert!(!has_approval_boundary);
    }

    #[test]
    fn test_evaluate_evidence_no_trusted_context_without_approval_actor() {
        let (has_trusted_spawn_context, _) = evaluate_evidence(
            &chain_with_config_policy(),
            ApprovalMode::Delegated,
            false,
            true,
        );

        assert!(
            !has_trusted_spawn_context,
            "a bound sandbox without an approval actor must not read as trusted"
        );
    }

    #[test]
    fn test_evaluate_evidence_no_trusted_context_when_sandbox_unbound() {
        let (has_trusted_spawn_context, _) = evaluate_evidence(
            &chain_with_config_policy(),
            ApprovalMode::Delegated,
            true,
            false,
        );

        assert!(
            !has_trusted_spawn_context,
            "an approval actor without a project-bound sandbox must not read as trusted"
        );
    }

    #[test]
    fn test_service_specs_cover_serve_and_gateway() {
        let specs = service_specs(Path::new("/usr/bin/harw"), Path::new("/home/tester/.harw"));

        let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, [SERVE_SERVICE_NAME, GATEWAY_SERVICE_NAME]);
        assert_eq!(specs[0].exec, ["/usr/bin/harw", "serve"]);
        assert_eq!(specs[1].exec, ["/usr/bin/harw", "gateway"]);
        for spec in &specs {
            assert_eq!(spec.working_dir, PathBuf::from("/home/tester/.harw"));
            assert_eq!(
                spec.env,
                [("HARW_HOME".to_owned(), "/home/tester/.harw".to_owned())]
            );
        }
    }

    #[test]
    fn test_service_specs_render_systemd_unit_text() {
        let manager = harw_install::service_systemd::SystemdServiceManager::new();
        let [serve, gateway] =
            service_specs(Path::new("/usr/bin/harw"), Path::new("/home/tester/.harw"));

        let serve_unit = manager.render_unit(&serve);
        assert!(
            serve_unit.contains("ExecStart=/usr/bin/harw serve\n"),
            "{serve_unit}"
        );
        assert!(serve_unit.contains("Restart=always\n"), "{serve_unit}");
        assert!(serve_unit.contains("RestartSec=5\n"), "{serve_unit}");
        assert!(
            serve_unit.contains("WorkingDirectory=/home/tester/.harw\n"),
            "{serve_unit}"
        );
        assert!(serve_unit.contains("Environment=HARW_HOME=/home/tester/.harw\n"));

        let gateway_unit = manager.render_unit(&gateway);
        assert!(
            gateway_unit.contains("ExecStart=/usr/bin/harw gateway\n"),
            "{gateway_unit}"
        );
        assert!(gateway_unit.contains("Restart=always\n"), "{gateway_unit}");
        assert!(gateway_unit.contains("WorkingDirectory=/home/tester/.harw\n"));
    }

    #[test]
    fn test_write_systemd_units_writes_one_file_per_service() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temporary unit root"))?;
        let unit_dir = dir.path().join("systemd").join("user");
        let manager = harw_install::service_systemd::SystemdServiceManager::new();
        let specs = service_specs(Path::new("/usr/bin/harw"), Path::new("/srv/harw"));

        let written =
            write_systemd_units(&unit_dir, &manager, &specs).map_err(ctx("write units"))?;

        assert_eq!(
            written,
            [
                unit_dir.join("harw-serve.service"),
                unit_dir.join("harw-gateway.service")
            ]
        );
        let serve = std::fs::read_to_string(&written[0]).map_err(ctx("read serve unit"))?;
        assert_eq!(serve, manager.render_unit(&specs[0]));
        let gateway = std::fs::read_to_string(&written[1]).map_err(ctx("read gateway unit"))?;
        assert!(gateway.contains("ExecStart=/usr/bin/harw gateway\n"));
        Ok(())
    }

    #[test]
    fn test_systemd_user_unit_dir_prefers_absolute_xdg_config_home() -> TestResult {
        let dir = systemd_user_unit_dir(
            Some(OsString::from("/xdg/config")),
            Some(OsString::from("/home/tester")),
        )
        .map_err(ctx("unit dir from XDG_CONFIG_HOME"))?;
        assert_eq!(dir, PathBuf::from("/xdg/config/systemd/user"));
        Ok(())
    }

    #[test]
    fn test_systemd_user_unit_dir_ignores_relative_xdg_and_falls_back_to_home() -> TestResult {
        let dir = systemd_user_unit_dir(
            Some(OsString::from("relative")),
            Some(OsString::from("/home/tester")),
        )
        .map_err(ctx("unit dir from HOME"))?;
        assert_eq!(dir, PathBuf::from("/home/tester/.config/systemd/user"));
        Ok(())
    }

    #[test]
    fn test_systemd_user_unit_dir_without_home_is_error() -> TestResult {
        let result = systemd_user_unit_dir(None, Some(OsString::new()));
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "no usable environment must fail".into(),
            ));
        };
        assert!(error.contains("HOME"), "{error}");
        Ok(())
    }

    #[test]
    fn test_shutdown_reason_signal_name_matches_signal() {
        assert_eq!(ShutdownReason::Terminate.signal_name(), "SIGTERM");
        assert_eq!(ShutdownReason::Interrupt.signal_name(), "SIGINT");
    }

    #[tokio::test]
    async fn test_first_signal_terminate_wins() {
        let reason = first_signal(
            std::future::ready(Some(())),
            std::future::pending::<Option<()>>(),
        )
        .await;
        assert_eq!(reason, ShutdownReason::Terminate);
    }

    #[tokio::test]
    async fn test_first_signal_interrupt_after_closed_terminate_stream() {
        let reason = first_signal(std::future::ready(None), std::future::ready(Some(()))).await;
        assert_eq!(reason, ShutdownReason::Interrupt);
    }

    #[tokio::test]
    async fn test_first_signal_both_streams_closed_stays_pending() {
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(30),
            first_signal(std::future::ready(None), std::future::ready(None)),
        )
        .await;
        assert!(outcome.is_err(), "closed streams must not trigger shutdown");
    }

    #[test]
    fn test_evaluate_evidence_all_false_when_nothing_observed() {
        assert_eq!(
            evaluate_evidence(&[], ApprovalMode::FullAccess, false, false),
            (false, false)
        );
    }

    /// Wandelt erwartete `&str`-Argumentvektoren für Vergleiche um.
    fn argvs(expected: &[&[&str]]) -> Vec<Vec<String>> {
        expected
            .iter()
            .map(|argv| argv.iter().map(|arg| (*arg).to_owned()).collect())
            .collect()
    }

    #[test]
    fn test_select_gateway_backend_prefers_booted_systemd() -> TestResult {
        assert_eq!(
            select_gateway_backend(ServiceKind::Systemd, true, None)
                .map_err(ctx("systemd selection"))?,
            GatewayBackend::SystemdUser
        );
        Ok(())
    }

    #[test]
    fn test_select_gateway_backend_falls_back_without_running_systemd() -> TestResult {
        assert_eq!(
            select_gateway_backend(ServiceKind::Systemd, false, None)
                .map_err(ctx("systemd without boot"))?,
            GatewayBackend::Detached
        );
        for kind in [ServiceKind::Schtasks, ServiceKind::Unsupported] {
            assert_eq!(
                select_gateway_backend(kind, true, Some("  "))
                    .map_err(ctx("fallback selection"))?,
                GatewayBackend::Detached
            );
        }
        assert_eq!(
            select_gateway_backend(ServiceKind::Launchd, false, None)
                .map_err(ctx("launchd selection"))?,
            GatewayBackend::Launchd
        );
        Ok(())
    }

    #[test]
    fn test_select_gateway_backend_honours_override() -> TestResult {
        assert_eq!(
            select_gateway_backend(ServiceKind::Systemd, true, Some("Detached"))
                .map_err(ctx("override detached"))?,
            GatewayBackend::Detached
        );
        assert_eq!(
            select_gateway_backend(ServiceKind::Unsupported, false, Some("systemd"))
                .map_err(ctx("override systemd"))?,
            GatewayBackend::SystemdUser
        );
        let Err(error) = select_gateway_backend(ServiceKind::Systemd, true, Some("runit")) else {
            return Err(TestError::Unexpected("unknown override must fail".into()));
        };
        assert!(error.contains(GATEWAY_BACKEND_ENV), "{error}");
        Ok(())
    }

    #[test]
    fn test_systemd_gateway_commands_keep_previous_invocations() {
        assert_eq!(
            systemd_gateway_commands(GatewayOp::Install, GATEWAY_SERVICE_NAME),
            argvs(&[
                &["systemctl", "--user", "daemon-reload"],
                &[
                    "systemctl",
                    "--user",
                    "enable",
                    "--now",
                    "harw-gateway.service"
                ],
            ])
        );
        for (op, verb) in [
            (GatewayOp::Start, "start"),
            (GatewayOp::Stop, "stop"),
            (GatewayOp::Restart, "restart"),
            (GatewayOp::Enable, "enable"),
            (GatewayOp::Disable, "disable"),
        ] {
            assert_eq!(
                systemd_gateway_commands(op, GATEWAY_SERVICE_NAME),
                argvs(&[&["systemctl", "--user", verb, "harw-gateway.service"]])
            );
        }
        assert_eq!(
            systemd_status_command(GATEWAY_SERVICE_NAME),
            ["systemctl", "--user", "is-active", "harw-gateway.service"]
        );
    }

    #[test]
    fn test_gateway_op_from_action_maps_status_to_report() {
        assert_eq!(GatewayOp::from_action(&GatewayAction::Status), None);
        assert_eq!(
            GatewayOp::from_action(&GatewayAction::Install),
            Some(GatewayOp::Install)
        );
        assert_eq!(
            GatewayOp::from_action(&GatewayAction::Stop),
            Some(GatewayOp::Stop)
        );
    }

    #[test]
    fn test_systemd_enabled_command_and_state() {
        assert_eq!(
            systemd_enabled_command(GATEWAY_SERVICE_NAME),
            ["systemctl", "--user", "is-enabled", "harw-gateway.service"]
        );
        assert_eq!(
            systemd_enabled_state("enabled\n"),
            GatewayAutostart::Enabled
        );
        assert_eq!(
            systemd_enabled_state("enabled-runtime\n"),
            GatewayAutostart::Enabled
        );
        assert_eq!(
            systemd_enabled_state("disabled\n"),
            GatewayAutostart::Disabled
        );
        assert_eq!(
            systemd_enabled_state("masked\n"),
            GatewayAutostart::Disabled
        );
        assert_eq!(systemd_enabled_state(""), GatewayAutostart::Unknown);
    }

    #[test]
    fn test_launchctl_label_disabled_parses_print_disabled() {
        assert_eq!(
            launchd_print_disabled_command("gui/501"),
            ["launchctl", "print-disabled", "gui/501"]
        );
        let out = "disabled services = {\n\t\"com.apple.x\" => enabled\n\t\
                   \"harw-gateway\" => disabled\n}\n";
        assert_eq!(launchctl_label_disabled(out, "harw-gateway"), Some(true));
        assert_eq!(launchctl_label_disabled(out, "com.apple.x"), Some(false));
        assert_eq!(launchctl_label_disabled(out, "harw-serve"), None);
        let legacy = "\t\"harw-gateway\" => false\n";
        assert_eq!(
            launchctl_label_disabled(legacy, "harw-gateway"),
            Some(false)
        );
        // Ein Label, das nur mit dem gesuchten beginnt, zählt nicht.
        let prefixed = "\t\"harw-gateway-old\" => true\n";
        assert_eq!(launchctl_label_disabled(prefixed, "harw-gateway"), None);
    }

    #[test]
    fn test_render_gateway_status_reports_state_autostart_and_files() {
        let files = [
            ("PID-Datei", PathBuf::from("/srv/harw/run/harw-gateway.pid")),
            ("Log", PathBuf::from("/srv/harw/logs/harw-gateway.log")),
        ];
        assert_eq!(
            render_gateway_status(
                GatewayBackend::Detached,
                GatewayRunState::Stopped,
                Some(GatewayAutostart::Unavailable),
                &files,
            ),
            [
                "Gateway-Dienst [Hintergrundprozess (PID-Datei)]: gestoppt",
                "  Autostart: nicht verfügbar (kein Dienstmanager)",
                "  PID-Datei: /srv/harw/run/harw-gateway.pid",
                "  Log: /srv/harw/logs/harw-gateway.log",
            ]
        );
        let unit = [(
            "Unit",
            PathBuf::from("/home/u/.config/systemd/user/harw-gateway.service"),
        )];
        assert_eq!(
            render_gateway_status(
                GatewayBackend::SystemdUser,
                GatewayRunState::Running(None),
                Some(GatewayAutostart::Enabled),
                &unit,
            ),
            [
                "Gateway-Dienst [systemd (User-Unit)]: läuft",
                "  Autostart: aktiviert",
                "  Unit: /home/u/.config/systemd/user/harw-gateway.service",
            ]
        );
        let lines = render_gateway_status(
            GatewayBackend::SystemdUser,
            GatewayRunState::NotInstalled,
            None,
            &unit,
        );
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Gateway-Dienst [systemd (User-Unit)]: nicht installiert")
        );
        assert!(!lines.iter().any(|line| line.contains("Autostart")));
        assert_eq!(
            lines.last().map(String::as_str),
            Some("  Einrichten mit: harw gateway install")
        );
    }

    #[test]
    fn test_systemd_active_state_parses_is_active_output() {
        assert_eq!(
            systemd_active_state("active\n"),
            GatewayRunState::Running(None)
        );
        assert_eq!(
            systemd_active_state("activating\n"),
            GatewayRunState::Running(None)
        );
        assert_eq!(systemd_active_state("inactive\n"), GatewayRunState::Stopped);
        assert_eq!(systemd_active_state("failed\n"), GatewayRunState::Stopped);
        assert_eq!(systemd_active_state(""), GatewayRunState::Stopped);
    }

    #[test]
    fn test_launchd_gateway_commands_when_not_loaded() {
        let plist = Path::new("/Users/u/Library/LaunchAgents/harw-gateway.plist");
        let cmds = |op| launchd_gateway_commands(op, "gui/501", "harw-gateway", plist, false);
        let bootstrap: &[&str] = &[
            "launchctl",
            "bootstrap",
            "gui/501",
            "/Users/u/Library/LaunchAgents/harw-gateway.plist",
        ];
        assert_eq!(
            cmds(GatewayOp::Install),
            argvs(&[&["launchctl", "enable", "gui/501/harw-gateway"], bootstrap])
        );
        assert_eq!(cmds(GatewayOp::Start), argvs(&[bootstrap]));
        assert_eq!(cmds(GatewayOp::Restart), argvs(&[bootstrap]));
        assert!(cmds(GatewayOp::Stop).is_empty());
        assert_eq!(
            cmds(GatewayOp::Enable),
            argvs(&[&["launchctl", "enable", "gui/501/harw-gateway"]])
        );
        assert_eq!(
            cmds(GatewayOp::Disable),
            argvs(&[&["launchctl", "disable", "gui/501/harw-gateway"]])
        );
    }

    #[test]
    fn test_launchd_gateway_commands_when_loaded() {
        let plist = Path::new("/Users/u/Library/LaunchAgents/harw-gateway.plist");
        let cmds = |op| launchd_gateway_commands(op, "gui/501", "harw-gateway", plist, true);
        assert_eq!(
            cmds(GatewayOp::Install),
            argvs(&[
                &["launchctl", "enable", "gui/501/harw-gateway"],
                &["launchctl", "bootout", "gui/501/harw-gateway"],
                &[
                    "launchctl",
                    "bootstrap",
                    "gui/501",
                    "/Users/u/Library/LaunchAgents/harw-gateway.plist"
                ],
            ])
        );
        assert_eq!(
            cmds(GatewayOp::Start),
            argvs(&[&["launchctl", "kickstart", "gui/501/harw-gateway"]])
        );
        assert_eq!(
            cmds(GatewayOp::Stop),
            argvs(&[&["launchctl", "bootout", "gui/501/harw-gateway"]])
        );
        assert_eq!(
            cmds(GatewayOp::Restart),
            argvs(&[&["launchctl", "kickstart", "-k", "gui/501/harw-gateway"]])
        );
        assert_eq!(
            launchd_status_command("harw-gateway"),
            ["launchctl", "list", "harw-gateway"]
        );
    }

    #[test]
    fn test_launch_agent_plist_path_and_domain() -> TestResult {
        assert_eq!(
            launch_agent_plist_path(Some(OsString::from("/Users/u")), "harw-gateway")
                .map_err(ctx("plist path"))?,
            PathBuf::from("/Users/u/Library/LaunchAgents/harw-gateway.plist")
        );
        assert!(launch_agent_plist_path(Some(OsString::from("rel")), "x").is_err());
        assert!(launch_agent_plist_path(None, "x").is_err());
        assert_eq!(
            launchd_domain(parse_uid("501\n").map_err(ctx("uid"))?),
            "gui/501"
        );
        assert!(parse_uid("root").is_err());
        Ok(())
    }

    #[test]
    fn test_launchctl_list_pid_parses_running_and_loaded_only() {
        let running = "{\n\t\"LimitLoadToSessionType\" = \"Aqua\";\n\t\"Label\" = \
                       \"harw-gateway\";\n\t\"PID\" = 4242;\n\t\"LastExitStatus\" = 0;\n};\n";
        assert_eq!(launchctl_list_pid(running), Some(4242));
        let loaded = "{\n\t\"Label\" = \"harw-gateway\";\n\t\"LastExitStatus\" = 256;\n};\n";
        assert_eq!(launchctl_list_pid(loaded), None);
    }

    #[test]
    fn test_pid_commands_per_family() {
        assert_eq!(
            pid_probe_command(ProcFamily::Unix, 42),
            ["kill", "-0", "42"]
        );
        assert_eq!(
            pid_terminate_command(ProcFamily::Unix, 42, false),
            ["kill", "-TERM", "42"]
        );
        assert_eq!(
            pid_terminate_command(ProcFamily::Unix, 42, true),
            ["kill", "-KILL", "42"]
        );
        assert_eq!(
            pid_probe_command(ProcFamily::Windows, 42),
            ["tasklist", "/FI", "PID eq 42", "/NH", "/FO", "CSV"]
        );
        assert_eq!(
            pid_terminate_command(ProcFamily::Windows, 42, false),
            ["taskkill", "/PID", "42"]
        );
        assert_eq!(
            pid_terminate_command(ProcFamily::Windows, 42, true),
            ["taskkill", "/F", "/PID", "42"]
        );
    }

    #[test]
    fn test_tasklist_lists_pid_matches_exact_pid_column() {
        let out = "\"harw.exe\",\"4242\",\"Console\",\"1\",\"12.345 K\"\r\n";
        assert!(tasklist_lists_pid(out, 4242));
        assert!(!tasklist_lists_pid(out, 424));
        assert!(!tasklist_lists_pid(
            "INFO: No tasks are running which match the specified criteria.\r\n",
            4242
        ));
    }

    #[test]
    fn test_pid_file_parsing_and_cmdline_check() {
        assert_eq!(parse_pid_file("1234\n"), Some(1234));
        assert_eq!(parse_pid_file("0"), None);
        assert_eq!(parse_pid_file("abc"), None);
        assert!(cmdline_is_gateway(b"/usr/bin/harw\0gateway\0"));
        assert!(!cmdline_is_gateway(b"/usr/bin/gateway\0serve\0"));
        assert!(!cmdline_is_gateway(b"/usr/bin/vim\0notes.txt\0"));
    }

    #[test]
    fn test_gateway_pid_and_log_paths_live_under_home() {
        let home = Path::new("/srv/harw");
        assert_eq!(
            gateway_pid_path(home),
            PathBuf::from("/srv/harw/run/harw-gateway.pid")
        );
        assert_eq!(
            gateway_log_path(home),
            PathBuf::from("/srv/harw/logs/harw-gateway.log")
        );
    }

    #[test]
    fn test_detached_pid_file_roundtrip_and_missing_file() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;
        let pid_path = gateway_pid_path(home.path());
        assert_eq!(
            detached_running_pid(&pid_path).map_err(ctx("missing pid file"))?,
            None
        );
        write_pid_file(&pid_path, 4242).map_err(ctx("write pid file"))?;
        let raw = std::fs::read_to_string(&pid_path).map_err(ctx("read pid file"))?;
        assert_eq!(parse_pid_file(&raw), Some(4242));
        remove_pid_file(&pid_path).map_err(ctx("remove pid file"))?;
        remove_pid_file(&pid_path).map_err(ctx("remove missing pid file"))?;
        assert!(!pid_path.exists());
        Ok(())
    }

    #[test]
    fn test_detached_gateway_rejects_autostart_actions() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;
        let [_serve, gateway] = service_specs(Path::new("/usr/bin/harw"), home.path());
        for op in [GatewayOp::Enable, GatewayOp::Disable] {
            let Err(error) = detached_gateway(op, &gateway, home.path()) else {
                return Err(TestError::Unexpected(
                    "autostart must be unavailable without a service manager".into(),
                ));
            };
            assert!(error.contains("Autostart"), "{error}");
        }
        Ok(())
    }
}
