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

use super::{
    ShellExecArgs, ShellExecError, ShellToolProvider, host_shell_argv_with_shell,
    resolve_host_shell, resolve_setsid, timeouts,
};
use crate::limits::launch_command;
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
    warnings: Vec<String>,
}

impl BackgroundLaunch {
    /// `true`, wenn der Prozess direkt auf dem Host läuft (ohne `bwrap`).
    #[must_use]
    pub fn executed_on_host(&self) -> bool {
        self.executed_on_host
    }

    /// Hinweise des Startwegs (z. B. ohne rlimits); nie Werte von Umgebungsvariablen.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Gibt den konfigurierten Befehl heraus (noch nicht gestartet).
    #[must_use]
    pub fn into_command(self) -> TokioCommand {
        self.command
    }

    /// Befehl und Hinweise.
    #[must_use]
    pub fn into_parts(self) -> (TokioCommand, Vec<String>) {
        (self.command, self.warnings)
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

        // Android-Anbindung: auf dem Host-Pfad ist ein fehlendes `prlimit`
        // (Termux paketiert es typischerweise nicht) auf `ExecPlatform::NoSandbox`
        // bewusst kein Fehler ([`ShellExecutor::resolve_host_limits`]); der
        // Bubblewrap-Zweig behält unverändert die strenge Vorgabe.
        let (tmpfs_size, prlimit) = if effective_host {
            executor.resolve_host_limits()
        } else {
            executor.resolve_limits()
        }
        .map_err(|err| {
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
        let warnings = rlimit_warnings(prlimit.as_deref());
        let limits = timeouts::limits_for(executor.limits, cpu_budget_secs);

        let mut launched = if effective_host {
            let setsid = resolve_setsid();
            let shell = resolve_host_shell(executor.exec_platform);
            let (program, shell_args) = host_shell_argv_with_shell(setsid, &shell, command);
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
            warnings,
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
