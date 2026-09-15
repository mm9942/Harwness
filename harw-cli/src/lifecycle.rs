//! Handler für die Betriebs-Lebenszyklus-Subcommands: `update`, `service`,
//! `catalog`, `uninstall`, sowie die Prozess-Signale der Daemons.
//!
//! # Verantwortung
//! - Dünne Verdrahtung auf `harw-install`/`harw-model-catalog`; die Logik lebt
//!   in den jeweiligen Descriptor-Modulen.
//! - `harw service install` legt je eine Dienst-Unit für `harw serve`
//!   (MCP-Listener + Job-Worker) und `harw gateway` an (G-065). Unter systemd
//!   werden nur die Unit-Dateien geschrieben; `systemctl` ruft der Nutzer selbst
//!   auf (Hinweis wird ausgegeben).
//! - [`ShutdownSignals`]: SIGTERM/SIGINT als Auslöser des geordneten Shutdowns
//!   von `harw serve` (G-022, `crate::serve_until`).
//!
//! # Nebenläufigkeit
//! [`ShutdownSignals::install`] muss innerhalb einer Tokio-Runtime mit
//! aktiviertem IO-Treiber laufen; die Signal-Registrierung ist prozessweit.
//! Alle übrigen Funktionen sind synchron und zustandslos.
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
                println!("Dienste aktiviert und gestartet: {}", specs
                    .iter()
                    .map(|spec| format!("{}.service", spec.name))
                    .collect::<Vec<_>>()
                    .join(" "));
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

/// Verwaltet ausschließlich die `harw-gateway.service` des aktiven Profils.
///
/// Die Kurzform unter `harw gateway` ist für den täglichen Betrieb gedacht;
/// `harw service` bleibt die Verwaltung beider Hintergrunddienste.
pub fn gateway_service(home_override: Option<PathBuf>, action: GatewayAction) -> Result<(), String> {
    let home = resolve_home(home_override)?;
    let platform = Platform::detect();
    if detect_service_manager(&platform).kind() != ServiceKind::Systemd {
        return Err("Gateway-Service-Steuerung wird auf dieser Plattform noch nicht unterstützt".to_owned());
    }
    let [_serve, gateway] = service_specs(
        &std::env::current_exe().map_err(|error| error.to_string())?,
        &home,
    );
    match action {
        GatewayAction::Install => {
            let manager = harw_install::service_systemd::SystemdServiceManager::new();
            let unit_dir = systemd_user_unit_dir(std::env::var_os("XDG_CONFIG_HOME"), std::env::var_os("HOME"))?;
            write_systemd_units(&unit_dir, &manager, std::slice::from_ref(&gateway))?;
            systemctl_gateway(&["daemon-reload"])?;
            systemctl_gateway(&["enable", "--now", "harw-gateway.service"])?;
        }
        GatewayAction::Start => systemctl_gateway(&["start", "harw-gateway.service"] )?,
        GatewayAction::Stop => systemctl_gateway(&["stop", "harw-gateway.service"] )?,
        GatewayAction::Restart => systemctl_gateway(&["restart", "harw-gateway.service"] )?,
        GatewayAction::Enable => systemctl_gateway(&["enable", "harw-gateway.service"] )?,
        GatewayAction::Disable => systemctl_gateway(&["disable", "harw-gateway.service"] )?,
    }
    Ok(())
}

/// Führt einen systemd-User-Befehl aus und gibt dessen Diagnose vollständig weiter.
fn systemctl_gateway(args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|error| format!("systemctl --user nicht ausführbar: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("systemctl --user {} fehlgeschlagen: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()))
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

    #[test]
    fn health_evidence_observes_default_cli_composition() {
        let home = tempfile::tempdir().expect("create temporary HARW home");
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
        .expect("write policy config into temporary HARW home");

        let evidence =
            runtime_composition_evidence(home.path()).expect("assemble CLI composition");

        // Die Liste ist bewusst vollständig ausgeschrieben und nicht auf eine
        // Mindestmenge geprüft: eine Änderung der Werkzeugfläche der Standard-
        // Zusammenstellung soll auffallen, nicht durchrutschen. `fs.glob` und
        // `fs.grep` kamen mit den Recherche-Werkzeugen hinzu (AP W2-01/02).
        // `rights_snapshot().tools` liefert sortiert und dublettenfrei
        // (harw-runtime/src/assembly.rs `rights_snapshot`), deshalb wird hier
        // als Menge statt per `starts_with`-Präfix verglichen.
        let base: HashSet<&str> = [
            "fs.read",
            "fs.write",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
            "shell.exec",
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

        let advertised = evidence.advertised_tools.expect("advertised tool evidence");
        let advertised: HashSet<&str> = advertised.iter().map(String::as_str).collect();
        assert!(
            advertised == base || advertised == with_browser,
            "unexpected tool surface: {advertised:?}"
        );
        assert_eq!(evidence.has_trusted_spawn_context, Some(true));
        assert_eq!(evidence.has_approval_boundary, Some(true));
    }

    #[test]
    fn update_check_reports_unavailable_without_creating_version_state() {
        let home = tempfile::tempdir().expect("create temporary HARW home");

        let error = update(Some(home.path().to_path_buf()), true)
            .expect_err("update check must fail without a remote checker");

        assert!(error.contains("kein Remote-Update-Checker ist konfiguriert"));
        assert!(!home.path().join("version.json").exists());
    }

    #[test]
    fn update_check_leaves_existing_version_state_unchanged() {
        let home = tempfile::tempdir().expect("create temporary HARW home");
        let checker = UpdateChecker::new(home.path());
        checker
            .write(&harw_install::VersionInfo {
                latest_version: "1.2.3".to_owned(),
                last_checked_at: jiff::Timestamp::now(),
                dismissed_version: Some("1.2.2".to_owned()),
            })
            .expect("write existing version state");
        let version_path = home.path().join("version.json");
        let before = std::fs::read(&version_path).expect("read existing version state");

        let error = update(Some(home.path().to_path_buf()), true)
            .expect_err("update check must fail without a remote checker");

        assert!(error.contains("kein Remote-Update-Checker ist konfiguriert"));
        assert_eq!(
            std::fs::read(version_path).expect("read version state after failed check"),
            before
        );
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
        assert!(serve_unit.contains("ExecStart=/usr/bin/harw serve\n"), "{serve_unit}");
        assert!(serve_unit.contains("Restart=always\n"), "{serve_unit}");
        assert!(serve_unit.contains("RestartSec=5\n"), "{serve_unit}");
        assert!(
            serve_unit.contains("WorkingDirectory=/home/tester/.harw\n"),
            "{serve_unit}"
        );
        assert!(serve_unit.contains("Environment=HARW_HOME=/home/tester/.harw\n"));

        let gateway_unit = manager.render_unit(&gateway);
        assert!(gateway_unit.contains("ExecStart=/usr/bin/harw gateway\n"), "{gateway_unit}");
        assert!(gateway_unit.contains("Restart=always\n"), "{gateway_unit}");
        assert!(gateway_unit.contains("WorkingDirectory=/home/tester/.harw\n"));
    }

    #[test]
    fn test_write_systemd_units_writes_one_file_per_service() {
        let dir = tempfile::tempdir().expect("create temporary unit root");
        let unit_dir = dir.path().join("systemd").join("user");
        let manager = harw_install::service_systemd::SystemdServiceManager::new();
        let specs = service_specs(Path::new("/usr/bin/harw"), Path::new("/srv/harw"));

        let written = write_systemd_units(&unit_dir, &manager, &specs).expect("write units");

        assert_eq!(
            written,
            [
                unit_dir.join("harw-serve.service"),
                unit_dir.join("harw-gateway.service")
            ]
        );
        let serve = std::fs::read_to_string(&written[0]).expect("read serve unit");
        assert_eq!(serve, manager.render_unit(&specs[0]));
        let gateway = std::fs::read_to_string(&written[1]).expect("read gateway unit");
        assert!(gateway.contains("ExecStart=/usr/bin/harw gateway\n"));
    }

    #[test]
    fn test_systemd_user_unit_dir_prefers_absolute_xdg_config_home() {
        let dir = systemd_user_unit_dir(
            Some(OsString::from("/xdg/config")),
            Some(OsString::from("/home/tester")),
        )
        .expect("unit dir from XDG_CONFIG_HOME");
        assert_eq!(dir, PathBuf::from("/xdg/config/systemd/user"));
    }

    #[test]
    fn test_systemd_user_unit_dir_ignores_relative_xdg_and_falls_back_to_home() {
        let dir = systemd_user_unit_dir(
            Some(OsString::from("relative")),
            Some(OsString::from("/home/tester")),
        )
        .expect("unit dir from HOME");
        assert_eq!(dir, PathBuf::from("/home/tester/.config/systemd/user"));
    }

    #[test]
    fn test_systemd_user_unit_dir_without_home_is_error() {
        let error = systemd_user_unit_dir(None, Some(OsString::new()))
            .expect_err("no usable environment must fail");
        assert!(error.contains("HOME"), "{error}");
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
}
