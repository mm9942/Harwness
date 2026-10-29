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
//! PL-97 E2b: Nimmt der Start die Sandbox-Kette des Job-Runtimes
//! ([`harw_tool_shell::job_spec`]), trägt [`PreparedLaunch::sandboxed`]
//! Workspace-Sandbox und Ressourcen; die Job-Verwaltung übergibt den Befehl
//! (`/bin/sh -c …`) dann mit ihnen an den Kommando-Port statt als Host-Job
//! ([`crate::JobManager::start_launch_with_origin`]).
//!
//! Kind-Agenten (PL-97 E2b): [`child_agent_launch`] — kein Host-Auftrag
//! mehr, sondern ein Job-Runtime-Auftrag über den ausdrücklich übergebenen
//! Workspace des Elternteils (siehe dort, warum der Kind-Prozess selbst
//! noch unbeschränkt bleibt).
//!
//! # Nebenläufigkeit
//! `Send + Sync`; [`JobLauncher::prepare`] und
//! [`JobLauncher::prepare_launch`] können auf eine Nutzerfreigabe warten
//! (Host-Permit-Frage).

use harw_command::{ResourceRequest, SandboxProfileName, SandboxRequirement, WorkspaceSandbox};
use harw_tool_shell::{SandboxedLaunch, ShellToolProvider};
use harw_tools::{ToolExecutionContext, ToolOutput};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::process::Command as TokioCommand;

// PL-97 E2b: Startweg-Wahl (`[sandbox] execution`) auch für Kind-Agenten,
// die diese Crate, aber nicht `harw-tool-shell` kennen.
pub use harw_tool_shell::SandboxExecution;

/// Ein geprüfter, noch nicht gestarteter Job-Prozess.
#[derive(Debug)]
pub struct PreparedJob {
    /// Konfigurierter Befehl: stdin `/dev/null`, eigene Prozessgruppe,
    /// `kill_on_drop(false)`; stdout/stderr setzt die Job-Verwaltung.
    pub command: TokioCommand,
    /// Direkt auf dem Host (ohne `bwrap`).
    pub executed_on_host: bool,
    /// Die Umgebung ist genau das, was am `command` gesetzt ist (nichts wird
    /// geerbt); sonst kommt die geerbte Umgebung hinzu.
    pub env_cleared: bool,
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
    /// PL-97 E2b: Übergabe an die Sandbox-Kette; `None`: `job.command`
    /// läuft wie bisher als Host-Job.
    pub sandboxed: Option<SandboxedLaunch>,
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
                sandboxed: None,
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
            let launch = self
                .prepare_launch(context, command, cpu_budget_secs)
                .await?;
            // PL-97 E2b, fail-closed: ohne die Übergabe an die Sandbox-Kette
            // liefe `/bin/sh -c …` als Host-Job, also unbeschränkt.
            if launch.sandboxed.is_some() {
                return Err(ToolOutput::error(format!(
                    "{}: this start runs through the job runtime's sandbox chain, which needs \
                     `JobLauncher::prepare_launch`; it never runs as a host job",
                    crate::JOB_START_TOOL
                )));
            }
            Ok(launch.job)
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
            let env_cleared = launch.env_cleared();
            let (command, warnings, sandboxed) = launch.into_launch_parts();
            Ok(PreparedLaunch {
                job: PreparedJob {
                    command,
                    executed_on_host,
                    env_cleared,
                },
                warnings,
                sandboxed,
            })
        })
    }
}

/// The start of a child agent's process through the job runtime's chain
/// path (PL-97 E2b), for [`crate::JobManager::start_piped_launch_with_origin`].
///
/// # Description
/// The child becomes a [`harw_command::CommandSandbox::Workspace`] request
/// over `workspace_root`, the parent's canonical workspace root the caller
/// passes explicitly (never harw's process working directory), with the
/// profile [`SandboxProfileName::WorkspaceBuild`], instead of a
/// [`harw_command::CommandSandbox::Host`] submission over the runtime's
/// `/` root. The host-permit path of `shell.exec` stays the only host
/// submission of a model call.
///
/// Not yet confined (an accepted deviation from plan §3.4 E2, tracked as a
/// follow-up: grants for the harw home and the runner path, and the
/// child's tool processes delegated to the parent runtime). The child
/// process itself runs with [`SandboxRequirement::None`], and
/// `executed_on_host` stays `true`:
/// - a Landlock domain (both trampoline backends) forbids `mount(2)` and
///   `pivot_root(2)`, so the Bubblewrap sandboxes the child starts for its
///   own tool calls would fail inside it;
/// - the `workspace_build` policy grants neither the harw home (the
///   child's session store and job state) nor the runner executable
///   outside the system trees (`~/.local/bin` by default);
/// - a networked profile gets no Bubblewrap backend from the host port (no
///   egress relay is configured).
///
/// The child keeps what every runtime job has: its own cgroup (when the
/// runtime has a cgroup root), the process group and the attempt record.
/// Its tool processes are confined one by one, as before. The working
/// directory of `job.command` is set to `workspace_root`.
///
/// # Errors
/// The prepared job back, with the reason, when the chain cannot take it:
/// `workspace_root` is not an absolute directory, or `manager`'s command
/// port runs no workspace requests. The caller then keeps the legacy host
/// submission (an unscoped call; a child carries no write scope).
pub fn child_agent_launch(
    manager: &crate::JobManager,
    mut job: PreparedJob,
    workspace_root: &Path,
) -> Result<PreparedLaunch, Box<(PreparedJob, String)>> {
    let profile = SandboxProfileName::WorkspaceBuild;
    let root = match workspace_root.canonicalize() {
        Ok(root) if root.is_dir() => root,
        Ok(root) => {
            let reason = format!("the workspace root {} is no directory", root.display());
            return Err(Box::new((job, reason)));
        }
        Err(error) => {
            let reason = format!(
                "the workspace root {} cannot be resolved: {error}",
                workspace_root.display()
            );
            return Err(Box::new((job, reason)));
        }
    };
    let Some(backend) = manager.workspace_backend(profile) else {
        let reason = "the job runtime runs no workspace sandbox requests".to_owned();
        return Err(Box::new((job, reason)));
    };
    job.command.current_dir(&root);
    job.executed_on_host = true;
    let workspace = WorkspaceSandbox::new(root, profile, SandboxRequirement::None);
    Ok(PreparedLaunch {
        job,
        warnings: Vec::new(),
        sandboxed: Some(SandboxedLaunch::new(
            workspace,
            ResourceRequest::default(),
            backend,
        )),
    })
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
                env_cleared: false,
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
            env_cleared: false,
        })
    }
}
