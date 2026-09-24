//! Hintergrund-Start für lang laufende Prozesse (`job.start` in
//! `harw-tool-job`, Plan R9 Teil F).
//!
//! # Verantwortungsbereich
//! [`ShellToolProvider::prepare_background_launch`] durchläuft **denselben**
//! Rechte- und Freigabeweg wie `shell.exec` und liefert statt eines
//! Ergebnisses einen fertig konfigurierten, **noch nicht gestarteten**
//! [`TokioCommand`] ([`BackgroundLaunch`]):
//!
//! 1. leerer Befehl → Ablehnung,
//! 2. Rechte-Werkzeug (`sudo`, `doas`, …) in Befehlsposition → dieselbe
//!    Ablehnung wie `shell.exec` (Root-Befehle nur über `host.sudo_exec`),
//! 3. [`Permission::ExecuteProcess`] fehlt (Plan-Modus, lesende Profile) →
//!    Ablehnung,
//! 4. Host oder Sandbox: exakt [`ShellExecutor::determine_effective_host`]
//!    (Host-Profil mit Permit-Ledger und Freigabefrage, Host-Arbeitsphase,
//!    Einzelfreigabe) — sonst der unveränderte Bubblewrap-Plan samt
//!    `prlimit`, Profil und Host-PATH-Bindung.
//!
//! Der Aufrufer (Job-Verwaltung) setzt nur noch stdout/stderr (Dateien) und
//! startet. Anders als `shell.exec` gilt **kein** Wanduhr-Zeitlimit; die
//! CPU-Grenze wächst mit `cpu_budget_secs` wie bei `shell.exec` mit dem
//! Zeitlimit ([`super::timeouts::limits_for`]).
//!
//! # Prozessgruppe
//! Der gestartete Prozess ist immer Führer einer eigenen Prozessgruppe, damit
//! die Job-Verwaltung die **ganze** Gruppe signalisieren kann
//! (`kill(-pgid, …)`):
//! - Host mit `setsid`: `setsid --wait /bin/sh -c …` ohne `process_group(0)`
//!   — `setsid` ist dann kein Gruppenführer, ruft `setsid(2)` und `exec`t
//!   `/bin/sh` an derselben PID (PID = SID = PGID), siehe
//!   [`ShellExecutor::run_host_command`].
//! - Host ohne `setsid` und Bubblewrap-Pfad: `process_group(0)`.
//!
//! `kill_on_drop` ist bewusst `false`: ein vom Nutzer „abgelöster" Job
//! (`detach`) läuft nach dem Ende von harw weiter. Im Bubblewrap-Pfad endet
//! der Job trotzdem mit harw (`--die-with-parent`).

use super::{
    ShellExecArgs, ShellExecError, ShellToolProvider, host_shell_argv, resolve_setsid, timeouts,
};
use crate::limits::launch_command;
use harw_authority::Permission;
use harw_sandbox::BwrapLauncher;
use harw_tools::{ToolExecutionContext, ToolOutput};
use std::ffi::OsString;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;
use tracing::{debug, warn};

/// Ein geprüfter, noch nicht gestarteter Hintergrund-Prozess.
///
/// # Description
/// Ergebnis von [`ShellToolProvider::prepare_background_launch`]. stdin ist
/// `/dev/null`; stdout/stderr setzt der Aufrufer (typisch: Logdateien).
#[derive(Debug)]
pub struct BackgroundLaunch {
    command: TokioCommand,
    executed_on_host: bool,
}

impl BackgroundLaunch {
    /// `true`, wenn der Prozess direkt auf dem Host läuft (ohne `bwrap`).
    #[must_use]
    pub fn executed_on_host(&self) -> bool {
        self.executed_on_host
    }

    /// Gibt den konfigurierten Befehl heraus (noch nicht gestartet).
    #[must_use]
    pub fn into_command(self) -> TokioCommand {
        self.command
    }
}

impl ShellToolProvider {
    /// Prüft `command` auf demselben Weg wie `shell.exec` und baut den
    /// Start-Befehl für einen Hintergrund-Prozess.
    ///
    /// # Arguments
    /// - `context`: vertrauenswürdige Sandbox/Sitzung des aufrufenden Agenten.
    /// - `command`: exakter Befehlstext für `/bin/sh -c` (auch Grundlage der
    ///   Host-Freigabe, falls eine nötig ist).
    /// - `tool_name`: Name des aufrufenden Werkzeugs für Meldungen.
    /// - `cpu_budget_secs`: Wanduhr-Budget, aus dem die CPU-rlimit wächst.
    ///
    /// # Errors
    /// Eine für das Modell lesbare [`ToolOutput::error`] für jede Ablehnung
    /// (leerer Befehl, sudo, fehlende Berechtigung, Host-Freigabe verweigert,
    /// Sandbox/Limits nicht verfügbar). Es wird nie auf einen anderen Pfad
    /// ausgewichen.
    pub async fn prepare_background_launch(
        &self,
        context: &ToolExecutionContext,
        command: &str,
        tool_name: &str,
        cpu_budget_secs: u64,
    ) -> Result<BackgroundLaunch, ToolOutput> {
        if command.trim().is_empty() {
            return Err(ToolOutput::error(format!(
                "{tool_name}: command must not be blank"
            )));
        }
        if let Some(program) = crate::sudo::escalation_program(command) {
            warn!(
                program,
                tool_name, "background launch denied: privilege escalation program"
            );
            return Err(ToolOutput::error(crate::sudo::shell_escalation_message(
                program,
            )));
        }
        if let Some(denied) = harw_tools::sandbox_guard::require_permission(
            context,
            Permission::ExecuteProcess,
            tool_name,
        ) {
            warn!(
                tool_name,
                "background launch denied: ExecuteProcess permission missing"
            );
            return Err(denied);
        }

        let executor = self.build_executor();
        let args = ShellExecArgs {
            command: command.to_owned(),
            timeout_secs: None,
        };
        let sandbox = context.sandbox();
        let session_id = context.session_id().as_str();
        let effective_host = executor
            .determine_effective_host(&args, sandbox, session_id)
            .await
            .map_err(|message| {
                warn!(
                    session_id,
                    tool_name, "background launch denied: host permit"
                );
                ToolOutput::error(message)
            })?;

        let (tmpfs_size, prlimit) = executor.resolve_limits().map_err(|err| {
            let err = ShellExecError::ResourceLimits(err);
            warn!(error = %err, tool_name, "background launch: resource limits unavailable");
            ToolOutput::error(err.to_string())
        })?;
        if prlimit.is_none() {
            warn!(
                tool_name,
                "background launch runs WITHOUT rlimits: prlimit missing"
            );
        }
        let limits = timeouts::limits_for(executor.limits, cpu_budget_secs);

        let mut launched = if effective_host {
            let setsid = resolve_setsid();
            let (program, shell_args) = host_shell_argv(setsid, command);
            let launch = launch_command(prlimit.as_deref(), &limits, &program, &shell_args);
            let mut launched = TokioCommand::new(&launch.program);
            launched.args(&launch.args);
            launched.current_dir(sandbox.workspace().canonical_root());
            // Siehe Moduldoku: mit `setsid` KEIN `process_group(0)`.
            if setsid.is_none() {
                launched.process_group(0);
            }
            launched
        } else {
            let launcher = BwrapLauncher::discover().map_err(|err| {
                warn!(error = %err, tool_name, "background launch: bubblewrap unavailable");
                ToolOutput::error(format!("{tool_name}: sandbox setup failed: {err}"))
            })?;
            let launcher = launcher
                .with_tmpfs_size(tmpfs_size)
                .with_profile(&executor.sandbox_profile);
            let launcher = match &executor.host_path {
                Some(binding) => launcher.with_host_path(binding.clone()),
                None => launcher,
            };
            let shell_command = [
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(command),
            ];
            let plan = launcher.plan(sandbox, &shell_command).map_err(|err| {
                warn!(error = %err, tool_name, "background launch: sandbox plan rejected");
                ToolOutput::error(format!("{tool_name}: sandbox setup failed: {err}"))
            })?;
            let launch = launch_command(
                prlimit.as_deref(),
                &limits,
                launcher.executable(),
                plan.args(),
            );
            let mut launched = TokioCommand::new(&launch.program);
            launched.args(&launch.args);
            launched.process_group(0);
            launched
        };
        launched.stdin(Stdio::null()).kill_on_drop(false);

        debug!(
            tool_name,
            command_len = command.len(),
            executed_on_host = effective_host,
            "background launch prepared"
        );
        Ok(BackgroundLaunch {
            command: launched,
            executed_on_host: effective_host,
        })
    }
}
