//! Startweg eines Jobs: [`JobLauncher`] prüft Rechte/Freigabe und baut den
//! noch nicht gestarteten Prozess ([`PreparedJob`]).
//!
//! # Produktiv: [`ShellJobLauncher`]
//! Delegiert an [`harw_tool_shell::ShellToolProvider::prepare_background_launch`]
//! — exakt der Weg von `shell.exec`: sudo-Verbot, `ExecuteProcess`,
//! Host-Profil mit Permit-Ledger/Freigabefrage, Host-Arbeitsphase
//! (`/sandbox-lease`), Einzelfreigabe, sonst Bubblewrap mit `prlimit`. Die
//! Montage übergibt denselben (geklonten) `ShellToolProvider`, den sie für
//! `shell.exec` desselben Agenten baut; damit verhalten sich Plan-Modus,
//! Sandbox, Host-Lease und Voll-Zugriff identisch.
//!
//! Hinweise des Startwegs (z. B. Start ohne rlimits) wandern über
//! [`PreparedLaunch`] in `meta.json` und als `JobEvent::Warning` zum Besitzer.
//!
//! # Nebenläufigkeit
//! `Send + Sync`; [`JobLauncher::prepare`] und
//! [`JobLauncher::prepare_launch`] können auf eine Nutzerfreigabe warten
//! (Host-Permit-Frage).

use harw_tool_shell::ShellToolProvider;
use harw_tools::{ToolExecutionContext, ToolOutput};
use std::future::Future;
use std::pin::Pin;
use tokio::process::Command as TokioCommand;

/// Ein geprüfter, noch nicht gestarteter Job-Prozess.
#[derive(Debug)]
pub struct PreparedJob {
    /// Konfigurierter Befehl: stdin `/dev/null`, eigene Prozessgruppe,
    /// `kill_on_drop(false)`; stdout/stderr setzt die Job-Verwaltung.
    pub command: TokioCommand,
    /// Direkt auf dem Host (ohne `bwrap`).
    pub executed_on_host: bool,
}

/// Future von [`JobLauncher::prepare`].
pub type LaunchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PreparedJob, ToolOutput>> + Send + 'a>>;

/// Ergebnis von [`JobLauncher::prepare_launch`]: geprüfter Prozess und Hinweise des Startwegs.
#[derive(Debug)]
pub struct PreparedLaunch {
    /// Der geprüfte, noch nicht gestartete Prozess.
    pub job: PreparedJob,
    /// Hinweise für den Besitzer (z. B. Start ohne rlimits); nie Werte von Umgebungsvariablen.
    pub warnings: Vec<String>,
}

/// Future von [`JobLauncher::prepare_launch`].
pub type PreparedLaunchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PreparedLaunch, ToolOutput>> + Send + 'a>>;

/// Prüft Rechte/Freigabe eines Job-Starts und baut den Prozess.
pub trait JobLauncher: Send + Sync {
    /// # Arguments
    /// - `context`: vertrauenswürdige Sandbox/Sitzung des Aufrufers.
    /// - `command`: exakter Shell-Befehlstext (`/bin/sh -c`).
    /// - `cpu_budget_secs`: Wanduhr-Budget, aus dem die CPU-rlimit wächst.
    ///
    /// # Errors
    /// Eine für das Modell lesbare [`ToolOutput::error`] bei jeder Ablehnung.
    fn prepare<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        command: &'a str,
        cpu_budget_secs: u64,
    ) -> LaunchFuture<'a>;

    /// Dieselben Prüfungen wie [`JobLauncher::prepare`], zusätzlich mit den
    /// Hinweisen des Startwegs; `job.start` nutzt diese Methode. Die
    /// Vorgabe liefert keine Hinweise.
    ///
    /// # Errors
    /// Wie [`JobLauncher::prepare`].
    fn prepare_launch<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        command: &'a str,
        cpu_budget_secs: u64,
    ) -> PreparedLaunchFuture<'a> {
        Box::pin(async move {
            let job = self.prepare(context, command, cpu_budget_secs).await?;
            Ok(PreparedLaunch {
                job,
                warnings: Vec::new(),
            })
        })
    }
}

/// Produktiver Startweg über `harw-tool-shell` (siehe Moduldoku).
#[derive(Clone)]
pub struct ShellJobLauncher {
    shell: ShellToolProvider,
}

impl ShellJobLauncher {
    /// Übernimmt die `shell.exec`-Konfiguration des Agenten (Profil,
    /// Permit-Ledger, Registry, Fragekanal, Host-PATH, Limits).
    #[must_use]
    pub fn new(shell: ShellToolProvider) -> Self {
        Self { shell }
    }
}

impl JobLauncher for ShellJobLauncher {
    fn prepare<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        command: &'a str,
        cpu_budget_secs: u64,
    ) -> LaunchFuture<'a> {
        Box::pin(async move {
            self.prepare_launch(context, command, cpu_budget_secs)
                .await
                .map(|launch| launch.job)
        })
    }

    fn prepare_launch<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        command: &'a str,
        cpu_budget_secs: u64,
    ) -> PreparedLaunchFuture<'a> {
        Box::pin(async move {
            let launch = self
                .shell
                .prepare_background_launch(context, command, crate::JOB_START_TOOL, cpu_budget_secs)
                .await?;
            let executed_on_host = launch.executed_on_host();
            let (command, warnings) = launch.into_parts();
            Ok(PreparedLaunch {
                job: PreparedJob {
                    command,
                    executed_on_host,
                },
                warnings,
            })
        })
    }
}

/// Startet `/bin/sh -c <command>` direkt auf dem Host in eigener
/// Prozessgruppe — **nur für Tests**, ohne Freigabe und ohne Sandbox.
#[cfg(test)]
pub(crate) struct DirectLauncher;

#[cfg(test)]
impl JobLauncher for DirectLauncher {
    fn prepare<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        command: &'a str,
        _cpu_budget_secs: u64,
    ) -> LaunchFuture<'a> {
        Box::pin(async move {
            if let Some(denied) = harw_tools::sandbox_guard::require_permission(
                context,
                harw_authority::Permission::ExecuteProcess,
                crate::JOB_START_TOOL,
            ) {
                return Err(denied);
            }
            let mut command_line = TokioCommand::new("/bin/sh");
            command_line
                .arg("-c")
                .arg(command)
                .current_dir(context.sandbox().workspace().canonical_root())
                .stdin(std::process::Stdio::null())
                .process_group(0)
                .kill_on_drop(false);
            Ok(PreparedJob {
                command: command_line,
                executed_on_host: true,
            })
        })
    }
}

#[cfg(test)]
impl DirectLauncher {
    /// Baut `argv[0] argv[1..]` direkt (kein `/bin/sh -c` dazwischen) in
    /// eigener Prozessgruppe — **nur für Tests** von
    /// [`crate::JobManager::start_piped`], das die stdin selbst auf
    /// `Stdio::piped()` setzt (hier unangetastet gelassen). Spiegelt den
    /// produktiven Weg (`harw-agent-runner`s `CurrentExeSpawner`: exakter
    /// argv-Aufruf, keine Shell) ohne dessen Rechte-/Protokollteil.
    ///
    /// # Errors
    /// Eine für das Modell lesbare [`ToolOutput::error`], wenn die
    /// Freigabe fehlt oder `argv` leer ist.
    pub(crate) async fn prepare_piped(
        context: &ToolExecutionContext,
        argv: &[&str],
    ) -> Result<PreparedJob, ToolOutput> {
        if let Some(denied) = harw_tools::sandbox_guard::require_permission(
            context,
            harw_authority::Permission::ExecuteProcess,
            crate::JOB_START_TOOL,
        ) {
            return Err(denied);
        }
        let Some((program, args)) = argv.split_first() else {
            return Err(ToolOutput::error("prepare_piped: empty argv"));
        };
        let mut command_line = TokioCommand::new(program);
        command_line
            .args(args)
            .current_dir(context.sandbox().workspace().canonical_root())
            .process_group(0)
            .kill_on_drop(false);
        Ok(PreparedJob {
            command: command_line,
            executed_on_host: true,
        })
    }
}
