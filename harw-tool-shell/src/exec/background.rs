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
//!    `prlimit`, Profil und Host-PATH-Bindung,
//! 5. Umgebung: auf dem Host `env_clear` plus eine Allowlist
//!    ([`host_job_env_name_allowed`]: Build-, Locale- und Pfad-Variablen)
//!    mit Deny-Filter für Geheimnis-Namen (`TOKEN`, `SECRET`, …, auch
//!    innerhalb erlaubter Präfixe); fehlt PATH, gilt [`FALLBACK_PATH`].
//!    Vom Aufrufer übergebene Variablen (`env` von `job.start`) stehen als
//!    `export …` im Befehlstext und erreichen den Job deshalb trotz
//!    `env_clear`. Bubblewrap leert die Umgebung ohnehin. `shell.exec` ist
//!    davon nicht betroffen.
//!
//! Fehlt `prlimit` (und `require_rlimits` ist aus), läuft der Job ohne
//! rlimits; [`BackgroundLaunch::warnings`] trägt dazu einen Hinweis für den
//! Besitzer des Jobs.
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
//!
//! # Sandbox-Kette (PL-97 E2b)
//! Mit [`crate::job_spec::SandboxExecution::Chain`] (Vorgabe) baut der
//! Sandbox-Zweig keinen `prlimit … -- bwrap …`-Befehl mehr, sondern
//! `/bin/sh -c <Befehl>` im Workspace mit leerer Umgebung und dazu eine
//! [`SandboxedLaunch`] ([`BackgroundLaunch::sandboxed`]): Workspace-Sandbox
//! und Ressourcen, die die Job-Verwaltung statt eines Host-Jobs an den
//! Kommando-Port übergibt. Wann der bisherige Weg bleibt oder ein Aufruf
//! mit Schreibbereich abgelehnt wird, beschreibt [`crate::job_spec`].

use super::{
    CHAIN_SHELL, ShellExecArgs, ShellExecError, ShellToolProvider, host_shell_argv,
    resolve_host_shell, timeouts,
};
use crate::job_spec::SandboxedLaunch;
use crate::limits::{ShellLimitsError, launch_command};
use harw_authority::Permission;
use harw_sandbox::BwrapLauncher;
use harw_tools::{ToolExecutionContext, ToolOutput};
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;
use tracing::{debug, warn};

/// Fester PATH, falls harw selbst keinen hat.
const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Exakt erlaubte Variablennamen eines Host-Jobs.
const HOST_ENV_NAMES: [&str; 18] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "TERM",
    "TMPDIR",
    "TZ",
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CC",
    "CXX",
    "AR",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "MAKEFLAGS",
    "SSH_AUTH_SOCK",
];

/// Erlaubte Namenspräfixe eines Host-Jobs (Groß-/Kleinschreibung zählt).
const HOST_ENV_PREFIXES: [&str; 6] = ["LC_", "CARGO_", "RUSTUP_", "RUSTC", "PKG_CONFIG", "XDG_"];

/// Namensbestandteile, die trotz Allowlist nie durchgereicht werden.
const HOST_ENV_DENY: [&str; 6] = [
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "API_KEY",
];

/// Ein geprüfter, noch nicht gestarteter Hintergrund-Prozess.
///
/// # Description
/// Ergebnis von [`ShellToolProvider::prepare_background_launch`]. stdin ist
/// `/dev/null`; stdout/stderr setzt der Aufrufer (typisch: Logdateien).
#[derive(Debug)]
pub struct BackgroundLaunch {
    command: TokioCommand,
    executed_on_host: bool,
    /// The environment is exactly what is set on `command` (nothing inherited).
    env_cleared: bool,
    warnings: Vec<String>,
    /// The chain submission (PL-97 E2b); `None`: `command` runs as a host job.
    sandboxed: Option<SandboxedLaunch>,
}

impl BackgroundLaunch {
    /// `true`, wenn der Prozess direkt auf dem Host läuft (ohne `bwrap`).
    #[must_use]
    pub fn executed_on_host(&self) -> bool {
        self.executed_on_host
    }

    /// `true`, wenn die Umgebung genau die am Befehl gesetzten Variablen ist
    /// (nichts wird geerbt).
    #[must_use]
    pub fn env_cleared(&self) -> bool {
        self.env_cleared
    }

    /// Hinweise des Startwegs (z. B. ohne rlimits); nie Werte von Umgebungsvariablen.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Gibt den konfigurierten Befehl heraus (noch nicht gestartet).
    ///
    /// PL-97 E2b: Mit einer Übergabe an die Sandbox-Kette ([`Self::sandboxed`])
    /// darf dieser Befehl nie als Host-Job laufen; dann
    /// [`Self::into_launch_parts`] verwenden.
    #[must_use]
    pub fn into_command(self) -> TokioCommand {
        self.command
    }

    /// Befehl und Hinweise (ohne die Übergabe an die Sandbox-Kette, siehe
    /// [`Self::into_command`]).
    #[must_use]
    pub fn into_parts(self) -> (TokioCommand, Vec<String>) {
        (self.command, self.warnings)
    }

    /// PL-97 E2b: die Übergabe an die Sandbox-Kette, falls dieser Start sie
    /// nimmt. Dann ist [`Self::into_command`] nur `/bin/sh -c <Befehl>` im
    /// Workspace (leere Umgebung); Sandbox und Ressourcen stehen hier, und
    /// der Aufrufer darf den Befehl nicht als Host-Job starten.
    #[must_use]
    pub fn sandboxed(&self) -> Option<&SandboxedLaunch> {
        self.sandboxed.as_ref()
    }

    /// Befehl, Hinweise und die Übergabe an die Sandbox-Kette
    /// ([`Self::sandboxed`]).
    #[must_use]
    pub fn into_launch_parts(self) -> (TokioCommand, Vec<String>, Option<SandboxedLaunch>) {
        (self.command, self.warnings, self.sandboxed)
    }
}

/// Hinweise für den Besitzer, wenn der Job ohne rlimits startet
/// (`prlimit` fehlt und `require_rlimits` ist aus); sonst leer.
pub(crate) fn rlimit_warnings(prlimit: Option<&Path>) -> Vec<String> {
    match prlimit {
        None => vec![
            "runs without resource limits (CPU, memory, processes): prlimit was not found and require_rlimits is off"
                .to_owned(),
        ],
        Some(_) => Vec::new(),
    }
}

/// Ob `name` in die Umgebung eines Host-Jobs darf: Allowlist, danach Deny-Filter (auch innerhalb erlaubter Präfixe).
pub(crate) fn host_job_env_name_allowed(name: &str) -> bool {
    let listed = HOST_ENV_NAMES.contains(&name)
        || HOST_ENV_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix));
    if !listed {
        return false;
    }
    let upper = name.to_ascii_uppercase();
    !HOST_ENV_DENY.iter().any(|deny| upper.contains(deny))
}

/// Gefilterte Umgebung eines Host-Jobs aus `base`; ohne PATH wird [`FALLBACK_PATH`] gesetzt. Nicht-UTF-8-Namen fallen weg.
pub(crate) fn host_job_env<I: IntoIterator<Item = (OsString, OsString)>>(
    base: I,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = base
        .into_iter()
        .filter(|(name, _)| name.to_str().is_some_and(host_job_env_name_allowed))
        .collect();
    if !env.iter().any(|(name, _)| name == "PATH") {
        env.push((OsString::from("PATH"), OsString::from(FALLBACK_PATH)));
    }
    env
}

/// `env_clear` plus [`host_job_env`]; liefert die Anzahl durchgereichter Namen.
fn apply_host_job_env<I: IntoIterator<Item = (OsString, OsString)>>(
    command: &mut TokioCommand,
    base: I,
) -> usize {
    let env = host_job_env(base);
    let passed = env.len();
    command.env_clear();
    command.envs(env);
    passed
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
        // PL-97 E2b, fail-closed: ein Start mit Schreibbereich läuft nie auf
        // dem Host, auch nicht mit Freigabe.
        if let Some(message) = executor.host_scope_refusal(effective_host, tool_name) {
            warn!(
                session_id,
                tool_name, "background launch refused: write scope on the host path"
            );
            return Err(ToolOutput::error(message));
        }

        let limits_unavailable = |err: ShellLimitsError| {
            let err = ShellExecError::ResourceLimits(err);
            warn!(error = %err, tool_name, "background launch: resource limits unavailable");
            ToolOutput::error(err.to_string())
        };
        let limits = timeouts::limits_for(executor.limits, cpu_budget_secs);

        // PL-97 E2b: Sandbox-Kette, sofern sie nicht schwächer ist als der
        // bisherige Weg; ein Start mit Schreibbereich ohne Kette läuft nie.
        // `prlimit` erst danach: das Trampolin setzt die rlimits selbst, ein
        // fehlendes `prlimit` darf diesen Weg nicht versperren.
        if !effective_host {
            let tmpfs_size = executor
                .resolve_sandbox_limits()
                .map_err(limits_unavailable)?;
            let chain = executor
                .chain_route(sandbox, session_id, tool_name)
                .map_err(|message| {
                    warn!(
                        session_id,
                        tool_name, "background launch refused: write scope without a sandbox"
                    );
                    ToolOutput::error(message)
                })?;
            if let Some(backend) = chain {
                // Reines Bubblewrap: das Job-Runtime stellt sein `prlimit` vor
                // `bwrap`; dieselbe Prüfung und dieselben Hinweise wie bisher.
                let warnings = if backend.uses_trampoline() {
                    Vec::new()
                } else {
                    let prlimit = executor
                        .limits
                        .resolve_prlimit()
                        .map_err(limits_unavailable)?;
                    if prlimit.is_none() {
                        warn!(
                            tool_name,
                            "background launch runs WITHOUT rlimits: prlimit missing"
                        );
                    }
                    rlimit_warnings(prlimit.as_deref())
                };
                let sandboxed = executor.sandboxed_launch(sandbox, &limits, tmpfs_size, backend);
                let mut launched = TokioCommand::new(CHAIN_SHELL);
                launched
                    .arg("-c")
                    .arg(command)
                    .current_dir(sandbox.workspace().canonical_root())
                    .env_clear()
                    .stdin(Stdio::null())
                    .kill_on_drop(false);
                debug!(
                    tool_name,
                    command_len = command.len(),
                    %backend,
                    "background launch prepared for the sandbox chain"
                );
                return Ok(BackgroundLaunch {
                    command: launched,
                    executed_on_host: false,
                    env_cleared: true,
                    warnings,
                    sandboxed: Some(sandboxed),
                });
            }
        }

        // Android-Anbindung: auf dem Host-Pfad ist ein fehlendes `prlimit`
        // (Termux paketiert es typischerweise nicht) auf `ExecPlatform::NoSandbox`
        // bewusst kein Fehler ([`ShellExecutor::resolve_host_limits`]); der
        // Bubblewrap-Zweig behält unverändert die strenge Vorgabe.
        let (tmpfs_size, prlimit) = if effective_host {
            executor.resolve_host_limits()
        } else {
            executor.resolve_limits()
        }
        .map_err(limits_unavailable)?;
        if prlimit.is_none() {
            warn!(
                tool_name,
                "background launch runs WITHOUT rlimits: prlimit missing"
            );
        }
        let warnings = rlimit_warnings(prlimit.as_deref());

        let env_cleared = effective_host;
        let mut launched = if effective_host {
            // The job runtime starts the command in a new session itself.
            let shell = resolve_host_shell(executor.exec_platform);
            let (program, shell_args) = host_shell_argv(&shell, command);
            let launch = launch_command(prlimit.as_deref(), &limits, &program, &shell_args);
            let mut launched = TokioCommand::new(&launch.program);
            launched.args(&launch.args);
            launched.current_dir(sandbox.workspace().canonical_root());
            // Siehe Moduldoku, Schritt 5: nur Namen zählen, nie Werte loggen.
            let passed = apply_host_job_env(&mut launched, std::env::vars_os());
            debug!(
                tool_name,
                passed, "background launch: host environment filtered"
            );
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
            env_cleared,
            warnings,
            sandboxed: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecPlatform;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
    use harw_tools::ToolExecutionContext;
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::os::unix::ffi::OsStringExt;
    use tempfile::TempDir;

    /// Wie die gleichnamigen Helfer in `exec.rs`, hier lokal für die
    /// Android-Anbindungstests dieses Moduls.
    fn make_sandbox(dir: &TempDir, permissions: Vec<Permission>) -> TestResult<SandboxSpec> {
        let ws = dir.path().join("project");
        std::fs::create_dir_all(&ws).map_err(ctx("project subdir"))?;
        let registry = WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: ws,
            }],
        )
        .map_err(ctx("registry build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("project"),
            )
            .map_err(ctx("resolve"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions),
        ))
    }

    /// Android-Anbindung: ohne Sandbox erlaubt `ApprovalMode::FullAccess`
    /// dem Hintergrund-Start denselben Host-Pfad wie `shell.exec`, ohne
    /// Ledger/Registry/Fragekanal und ohne Rückfrage.
    #[tokio::test]
    async fn test_background_launch_no_sandbox_full_access_runs_on_host() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox);
        let mode = ApprovalModeCell::new(ApprovalMode::FullAccess);
        let provider = ShellToolProvider::default()
            .with_exec_platform(ExecPlatform::NoSandbox)
            .with_approval_mode(mode);

        let launch = provider
            .prepare_background_launch(&context, "echo bg_full_access_ok", "job.start", 5)
            .await
            .map_err(|output| TestError::Unexpected(format!("must not be denied: {output:?}")))?;

        assert!(
            launch.executed_on_host(),
            "NoSandbox + ApprovalMode::FullAccess must run the background job on the host"
        );
        Ok(())
    }

    /// Variablennamen aus der Ausgabe von `env` (Werte werden verworfen).
    fn env_names(stdout: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(stdout)
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
            .collect()
    }

    fn sentinel_base() -> Vec<(OsString, OsString)> {
        let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from(FALLBACK_PATH));
        vec![
            (OsString::from("PATH"), path),
            (
                OsString::from("HARW_JOB_SENTINEL_SECRET"),
                OsString::from("sentinel"),
            ),
            (
                OsString::from("SSH_AUTH_SOCK"),
                OsString::from("/tmp/agent.sock"),
            ),
            (OsString::from("CARGO_REGISTRY_TOKEN"), OsString::from("x")),
        ]
    }

    async fn run_filtered(script: &str) -> TestResult<Vec<String>> {
        let mut command = TokioCommand::new("/bin/sh");
        command.arg("-c").arg(script);
        apply_host_job_env(&mut command, sentinel_base());
        let output = command.output().await.map_err(ctx("run /bin/sh"))?;
        if !output.status.success() {
            return Err(TestError::Unexpected(format!(
                "/bin/sh failed: {}",
                output.status
            )));
        }
        Ok(env_names(&output.stdout))
    }

    #[test]
    fn test_host_env_allows_build_and_locale_names() -> TestResult {
        for name in [
            "PATH",
            "HOME",
            "LC_ALL",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "RUSTC_WRAPPER",
            "PKG_CONFIG_PATH",
            "SSH_AUTH_SOCK",
            "XDG_RUNTIME_DIR",
            "MAKEFLAGS",
        ] {
            if !host_job_env_name_allowed(name) {
                return Err(TestError::Unexpected(format!("{name} must be allowed")));
            }
        }
        Ok(())
    }

    #[test]
    fn test_host_env_drops_secrets_and_unlisted() -> TestResult {
        for name in [
            "CARGO_REGISTRY_TOKEN",
            "CARGO_REGISTRIES_X_TOKEN",
            "XDG_SECRET_DIR",
            "RUSTC_API_KEY",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "NODE_OPTIONS",
            "GOPATH",
            "JAVA_HOME",
            "PYTHONPATH",
        ] {
            if host_job_env_name_allowed(name) {
                return Err(TestError::Unexpected(format!("{name} must be rejected")));
            }
        }
        Ok(())
    }

    #[test]
    fn test_host_env_path_fallback() -> TestResult {
        let without_path = host_job_env([(OsString::from("HOME"), OsString::from("/home/x"))]);
        let path = without_path
            .iter()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| value.clone())
            .ok_or(TestError::Missing("fallback PATH"))?;
        if path != FALLBACK_PATH {
            return Err(TestError::Unexpected(
                "PATH without base PATH must be the fallback".to_owned(),
            ));
        }

        let with_path = host_job_env([(OsString::from("PATH"), OsString::from("/opt/bin"))]);
        let paths: Vec<&OsString> = with_path
            .iter()
            .filter(|(name, _)| name == "PATH")
            .map(|(_, value)| value)
            .collect();
        if paths != [&OsString::from("/opt/bin")] {
            return Err(TestError::Unexpected(
                "given PATH must be kept exactly once".to_owned(),
            ));
        }

        // `PATH` gefolgt von einem ungültigen UTF-8-Byte.
        let non_utf8 = OsString::from_vec(vec![b'P', b'A', b'T', b'H', 0xff]);
        let filtered = host_job_env([(non_utf8.clone(), OsString::from("/x"))]);
        if filtered.iter().any(|(name, _)| *name == non_utf8) {
            return Err(TestError::Unexpected(
                "non-UTF-8 name must be dropped".to_owned(),
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_host_job_sentinel_absent_unless_passed() -> TestResult {
        let names = run_filtered("env").await?;
        if !names.iter().any(|name| name == "SSH_AUTH_SOCK") {
            return Err(TestError::Unexpected(
                "SSH_AUTH_SOCK must pass the filter".to_owned(),
            ));
        }
        for denied in ["HARW_JOB_SENTINEL_SECRET", "CARGO_REGISTRY_TOKEN"] {
            if names.iter().any(|name| name == denied) {
                return Err(TestError::Unexpected(format!(
                    "{denied} must not reach the job"
                )));
            }
        }

        let names = run_filtered("export HARW_JOB_SENTINEL_SECRET=passed && env").await?;
        if !names.iter().any(|name| name == "HARW_JOB_SENTINEL_SECRET") {
            return Err(TestError::Unexpected(
                "caller-passed variable must reach the job".to_owned(),
            ));
        }
        Ok(())
    }

    /// PL-97 E2b: ein Port, der nur sein Backend meldet.
    #[derive(Debug)]
    struct BackendOnly(Option<harw_command::BackendKind>);

    impl harw_command::CommandPort for BackendOnly {
        fn run(
            &self,
            _request: harw_command::CommandRequest,
            _cancel: harw_types::cancel::CancelToken,
        ) -> harw_command::RunFuture<'_> {
            Box::pin(std::future::pending())
        }

        fn start(&self, _request: harw_command::CommandRequest) -> harw_command::StartFuture<'_> {
            Box::pin(async { Err("unused".to_owned()) })
        }

        fn workspace_backend(
            &self,
            _profile: harw_command::SandboxProfileName,
        ) -> Option<harw_command::BackendKind> {
            self.0
        }
    }

    fn chain_provider(backend: Option<harw_command::BackendKind>) -> ShellToolProvider {
        let mut provider = ShellToolProvider::default()
            .with_exec_platform(ExecPlatform::Sandboxed)
            .with_command_port(std::sync::Arc::new(BackendOnly(backend)))
            .with_execution(crate::job_spec::SandboxExecution::Chain);
        provider.limits.require_rlimits = false;
        provider
    }

    /// PL-97 E2b: `job.start` übergibt `/bin/sh -c <Befehl>` mit
    /// Workspace-Sandbox und Ressourcen an die Kette, nicht als Host-Job.
    #[tokio::test]
    async fn test_background_launch_takes_the_sandbox_chain() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let sandbox = make_sandbox(
            &tmp,
            vec![Permission::ExecuteProcess, Permission::WriteWorkspace],
        )?;
        let root = sandbox.workspace().canonical_root().to_path_buf();
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox);
        let provider = chain_provider(Some(harw_command::BackendKind::Bwrap));
        let launch = provider
            .prepare_background_launch(&context, "make test", "job.start", 5)
            .await
            .map_err(|output| TestError::Unexpected(format!("{output:?}")))?;
        assert!(!launch.executed_on_host());
        assert!(launch.env_cleared());
        let sandboxed = launch
            .sandboxed()
            .ok_or(TestError::Missing("chain submission"))?
            .clone();
        let harw_command::CommandSandbox::Workspace(workspace) = sandboxed.sandbox() else {
            return Err(TestError::Unexpected(format!("{:?}", sandboxed.sandbox())));
        };
        assert_eq!(workspace.root, root);
        assert_eq!(
            workspace.profile,
            harw_command::SandboxProfileName::NoNetwork
        );
        assert_eq!(
            sandboxed.limits(),
            &timeouts::limits_for(provider.limits, 5).job_resources()
        );
        let (command, _warnings, chained) = launch.into_launch_parts();
        assert!(chained.is_some());
        let std = command.as_std();
        assert_eq!(std.get_program(), "/bin/sh");
        let args: Vec<_> = std.get_args().collect();
        assert_eq!(args, ["-c", "make test"]);
        assert_eq!(std.get_current_dir(), Some(root.as_path()));
        assert_eq!(std.get_envs().count(), 0);
        Ok(())
    }

    /// PL-97 E2b (PR #154 Review): der Startweg steht vor der Suche nach
    /// `prlimit` fest. Ohne `prlimit` an den festen Pfaden und mit
    /// `require_rlimits = true` wird ein `bwrap-trampoline`-Start nicht
    /// abgelehnt (das Trampolin setzt die rlimits selbst), und die Übergabe
    /// trägt die rlimits; reines Bubblewrap braucht `prlimit` weiterhin.
    #[tokio::test]
    async fn test_background_trampoline_route_runs_without_a_fixed_path_prlimit() -> TestResult {
        let _hidden = crate::limits::test_seam::HiddenPrlimit::install();
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let sandbox = make_sandbox(
            &tmp,
            vec![Permission::ExecuteProcess, Permission::WriteWorkspace],
        )?;
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox);
        let mut provider = chain_provider(Some(harw_command::BackendKind::BwrapTrampoline));
        provider.limits.require_rlimits = true;
        assert!(provider.limits.resolve_prlimit().is_err());
        let launch = provider
            .prepare_background_launch(&context, "make test", "job.start", 5)
            .await
            .map_err(|output| TestError::Unexpected(format!("refused: {output:?}")))?;
        assert!(!launch.executed_on_host());
        let sandboxed = launch
            .sandboxed()
            .ok_or(TestError::Missing("chain submission"))?;
        let limits = timeouts::limits_for(provider.limits, 5);
        let resources = sandboxed.limits();
        assert_eq!(resources, &limits.job_resources());
        assert!(resources.require_rlimits);
        assert_eq!(resources.cpu_time_max, Some(limits.cpu_secs));
        assert_eq!(resources.open_files_max, u32::try_from(limits.nofile).ok());
        // RLIMIT_NPROC wie auf dem bisherigen prlimit-Weg (PL-97 E2f).
        assert_eq!(resources.process_count_max, Some(4096));

        let mut plain = chain_provider(Some(harw_command::BackendKind::Bwrap));
        plain.limits.require_rlimits = true;
        match plain
            .prepare_background_launch(&context, "make test", "job.start", 5)
            .await
        {
            Err(ToolOutput::Error { message }) => {
                assert!(message.contains("prlimit"), "{message}");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    /// PL-97 E2b: ein Start mit Schreibbereich ohne Bubblewrap-Backend wird
    /// abgelehnt; der bisherige Weg (`legacy`) übergibt nichts an die Kette.
    #[tokio::test]
    async fn test_background_launch_scope_and_legacy() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let sandbox = make_sandbox(
            &tmp,
            vec![Permission::ExecuteProcess, Permission::WriteWorkspace],
        )?;
        let scope =
            harw_command::JobWriteScope::new([sandbox.workspace().canonical_root().join("src")])
                .map_err(ctx("scope"))?;
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox);
        let refused = chain_provider(Some(harw_command::BackendKind::LandlockTrampoline))
            .with_write_scope(scope)
            .prepare_background_launch(&context, "make", "job.start", 5)
            .await;
        match refused {
            Err(ToolOutput::Error { message }) => {
                assert!(message.contains("write scope"), "{message}");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        if BwrapLauncher::discover().is_ok() {
            let legacy = chain_provider(Some(harw_command::BackendKind::Bwrap))
                .with_execution(crate::job_spec::SandboxExecution::Legacy)
                .prepare_background_launch(&context, "make", "job.start", 5)
                .await
                .map_err(|output| TestError::Unexpected(format!("{output:?}")))?;
            assert!(legacy.sandboxed().is_none());
            assert!(!legacy.executed_on_host());
        }
        Ok(())
    }

    /// PL-97 E2b, fail-closed: ein Start mit Schreibbereich läuft nie auf
    /// dem Host (Sitzungsfreigabe; `NoSandbox` mit `FullAccess`).
    #[tokio::test]
    async fn test_background_launch_with_scope_never_runs_on_the_host() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let sandbox = make_sandbox(
            &tmp,
            vec![Permission::ExecuteProcess, Permission::WriteWorkspace],
        )?;
        let scope =
            harw_command::JobWriteScope::new([sandbox.workspace().canonical_root().join("src")])
                .map_err(ctx("scope"))?;
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox);
        let registry = std::sync::Arc::new(harw_sandbox::HostPermitSessionRegistry::default());
        registry.mark_session_approved(context.session_id().as_str());
        let leased = chain_provider(Some(harw_command::BackendKind::BwrapTrampoline))
            .with_host_permit_registry(registry)
            .with_write_scope(scope.clone());
        let full_access = chain_provider(Some(harw_command::BackendKind::BwrapTrampoline))
            .with_exec_platform(ExecPlatform::NoSandbox)
            .with_approval_mode(ApprovalModeCell::new(ApprovalMode::FullAccess))
            .with_write_scope(scope);
        for provider in [leased, full_access] {
            match provider
                .prepare_background_launch(&context, "make", "job.start", 5)
                .await
            {
                Err(ToolOutput::Error { message }) => {
                    assert!(message.contains("write scope"), "{message}");
                    assert!(message.contains("host execution"), "{message}");
                }
                other => return Err(TestError::Unexpected(format!("{other:?}"))),
            }
        }
        Ok(())
    }

    #[test]
    fn test_missing_prlimit_yields_launch_warning() -> TestResult {
        let warnings = rlimit_warnings(None);
        let [warning] = warnings.as_slice() else {
            return Err(TestError::Unexpected(format!(
                "expected one warning, got {}",
                warnings.len()
            )));
        };
        if !warning.contains("resource limits") {
            return Err(TestError::Unexpected(
                "warning must mention resource limits".to_owned(),
            ));
        }
        if !rlimit_warnings(Some(Path::new("/usr/bin/prlimit"))).is_empty() {
            return Err(TestError::Unexpected(
                "prlimit present must yield no warning".to_owned(),
            ));
        }
        Ok(())
    }
}
