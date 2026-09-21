//! Shell-execution tool provider and executor for Harwness.
//!
//! **Security note**: This executor launches `/bin/sh -c` only through a Bubblewrap
//! plan derived from the per-call sandbox. Failure to build or spawn that plan rejects
//! execution rather than falling back to the host.
//!
//! # Responsibility scope
//! - Owns [`ShellToolProvider`] (implements [`harw_extension_api::contributors::ToolProvider`])
//! - Owns [`ShellExecutor`] (implements [`harw_tools::ToolExecutor`])
//! - Owns [`ShellExecError`] — internal error type (never crosses the `ToolExecutor` boundary;
//!   all failures are returned as [`harw_tools::ToolOutput::error`] or [`harw_tools::ToolsError`])
//!
//! # Key types
//! - [`ShellToolProvider`] — stateful provider holding timeout and output-size configuration
//! - [`ShellExecutor`] — stateless per-call executor (state driven by provider config)
//! - [`ShellExecError`] — typed internal errors for spawn/timeout/truncation scenarios
//!
//! # Concurrency model
//! [`ShellToolProvider`] and [`ShellExecutor`] are `Send + Sync`. Shell side-effects are
//! presumed non-commutative, so [`ShellToolProvider::parallel_safe`] always returns `false`.
//!
//! # Error types
//! [`ShellExecError`] — converted to [`harw_tools::ToolOutput::error`] before crossing the
//! public `ToolExecutor` boundary. Callers only ever see `Result<ToolOutput, ToolsError>`.
//!
//! # Ressourcen-Härtung (W1-03)
//! - `bwrap` und `prlimit` werden nur an festen Pfaden gesucht, nie über `PATH` (F-021).
//! - Start: `/usr/bin/prlimit --as --cpu --fsize --nofile --nproc -- /usr/bin/bwrap …`,
//!   tmpfs `/tmp` mit `--size` ([`ShellLimits`], F-061). Fehlt `prlimit` und ist
//!   [`ShellLimits::require_rlimits`] gesetzt, wird nicht gestartet.
//! - stdin ist `/dev/null` statt des geerbten TUI-Terminals (F-119).
//! - stdout/stderr werden streamend mit gemeinsamem Budget gelesen; bei Überschreitung wird
//!   `bwrap` per SIGKILL beendet (`--die-with-parent` + PID-Namespace räumen den Baum ab),
//!   Teilausgabe bleibt bei Kappung und Timeout erhalten (F-060).

use crate::capture::{BoundedCapture, DrainEnd};
use crate::host_permit_prompt::{HostPermitPrompt, HostPermitPromptSender, HostPermitVariant};
use crate::limits::{ShellLimits, ShellLimitsError, launch_command};
use harw_extension_api::contributors::ToolProvider;
use harw_authority::{Permission, SandboxSpec};
use harw_sandbox::{BwrapLauncher, HostApprovalScope, HostPermitSessionRegistry, ProcessEnvironment, ProcessPermitLedger, ProcessPermitRequest, SandboxProfile};
use harw_tools::{
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt, io,
    num::NonZeroU64,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::process::{Child, Command as TokioCommand};
use tracing::{debug, info, warn};

// ── Constants ─────────────────────────────────────────────────────────────────

const TOOL_NAME: &str = "shell.exec";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const TRUNCATION_MARKER: &str = "\n[...truncated...]";
/// Obergrenze für das Einsammeln des Exit-Status nach SIGKILL. `kill_on_drop` bleibt
/// als Rückfallebene, falls der Kernel den Prozess nicht rechtzeitig freigibt.
const KILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// Einzige heute existierende Worker-Definition, die [`SandboxProfile::Host`]
/// aktiviert (siehe `harw-registry-defaults/agents/host-process-worker.toml`).
/// Sobald ein zweiter Host-fähiger Worker entsteht, muss dieser Konstante ein
/// echtes, aus der Worker-Konfiguration gespeistes Feld auf
/// [`ShellToolProvider`]/[`ShellExecutor`] folgen.
const HOST_WORKER_DEFINITION: &str = "host-process-worker@1";
/// Dauer einer per lokaler UI bestätigten Host-Sitzungsfreigabe, bevor sie
/// ohne explizites Sitzungsende automatisch verfällt (Verteidigungslinie
/// gegen eine vergessene, nie beendete Sitzung).
const HOST_SESSION_LEASE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// Gültigkeitsdauer eines frisch über eine beantwortete
/// [`HostPermitVariant::SingleExecution`]-Frage ausgestellten Permits, bis
/// der genehmigte Auftrag tatsächlich läuft. Deutlich kürzer als
/// [`HOST_SESSION_LEASE_TTL`]: eine Einzelfreigabe soll nicht als lange
/// gültiges „stilles Ja" liegen bleiben.
const HOST_SINGLE_EXECUTION_TTL: Duration = Duration::from_secs(5 * 60);
/// Vorgabe-Wartezeit auf **eine** Nutzerentscheidung auf eine offene
/// [`HostPermitPrompt`]. Läuft sie ab, gilt das als Ablehnung
/// (fail-closed) — deckt sich mit
/// `harw_tui::host_permit_dialog::DEFAULT_HOST_PERMIT_TIMEOUT`.
const HOST_PERMIT_PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
/// Ablehnungsnachricht, wenn weder ein Permit-Ledger noch eine
/// Sitzungs-Registry konfiguriert sind — Host-Ausführung ist dann
/// grundsätzlich nicht erreichbar, unabhängig von jedem Fragekanal.
const NO_PERMIT_LEDGER_MSG: &str =
    "shell.exec: host execution requires a process permit, but no permit ledger is configured";
/// Ablehnungsnachricht, wenn eine Frage nötig wäre, aber kein Fragekanal
/// angehängt ist oder er bereits geschlossen ist (fail-closed).
const NO_UI_APPROVAL_MSG: &str = "shell.exec: host execution requires local UI approval for \
     this session, but no approval channel is attached (fail-closed)";
/// Ablehnungsnachricht für jeden Ausgang einer geöffneten Frage, der keine
/// ausdrückliche Zustimmung ist: Ablehnung, Zeitablauf oder fallengelassene
/// Antwort.
const HOST_PERMIT_DENIED_MSG: &str = "shell.exec: host execution was not approved (denied, \
     timed out, or the answer channel was dropped)";

// ── ShellExecError ────────────────────────────────────────────────────────────

/// Internal error type for shell execution failures.
///
/// # Description
/// Represents the distinct failure modes of [`ShellExecutor`]. This type is
/// never returned across the [`ToolExecutor`] boundary — all variants are
/// converted to human-readable [`ToolOutput::error`] messages before returning.
///
/// # Concurrency
/// `ShellExecError` is `Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_shell::ShellExecError;
///
/// let err = ShellExecError::InvalidArgs("command field missing".to_owned());
/// assert!(err.to_string().contains("invalid arguments"));
/// ```
#[derive(Debug)]
pub enum ShellExecError {
    /// The tool call arguments could not be parsed or are semantically invalid.
    InvalidArgs(String),
    /// The child process could not be spawned.
    Spawn(io::Error),
    /// The child process did not complete within the allowed time.
    Timeout {
        /// Effective timeout in seconds that was exceeded.
        secs: u64,
    },
    /// The child process exited with a non-zero status.
    ExitWithError {
        /// The exit code returned by the process.
        code: i32,
    },
    /// The combined output exceeded `max_output_bytes` and was truncated.
    TruncatedOutput,
    /// Ressourcengrenzen sind ungültig oder nicht durchsetzbar (z. B. `prlimit` fehlt).
    ResourceLimits(ShellLimitsError),
}

impl fmt::Display for ShellExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgs(msg) => write!(f, "shell.exec: invalid arguments: {msg}"),
            Self::Spawn(err) => write!(f, "shell.exec: failed to spawn Bubblewrap: {err}"),
            Self::Timeout { secs } => write!(f, "shell.exec timed out after {secs}s"),
            Self::ExitWithError { code } => {
                write!(f, "shell.exec: process exited with code {code}")
            }
            Self::TruncatedOutput => write!(f, "shell.exec: output truncated"),
            Self::ResourceLimits(err) => write!(f, "shell.exec: resource limits: {err}"),
        }
    }
}

impl std::error::Error for ShellExecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(err) => Some(err),
            Self::ResourceLimits(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for ShellExecError {
    /// Converts an [`io::Error`] into [`ShellExecError::Spawn`].
    ///
    /// # Description
    /// Used by `?` propagation when a process spawn fails.
    fn from(err: io::Error) -> Self {
        Self::Spawn(err)
    }
}

// ── Argument deserialization ──────────────────────────────────────────────────

/// Deserialized arguments for the `shell.exec` tool call.
///
/// # Description
/// Parsed from `call.arguments` (a [`serde_json::Value`]). Unknown fields are
/// rejected via `deny_unknown_fields` to prevent prompt-injection via surplus keys.
///
/// # Errors
/// [`ShellExecError::InvalidArgs`] when the JSON does not match this schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellExecArgs {
    /// The shell command to execute via `/bin/sh -c`.
    command: String,
    /// Optional per-call timeout override. Clamped to the provider's configured maximum.
    #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_u64")]
    timeout_secs: Option<u64>,
}

// ── ShellExecutor ─────────────────────────────────────────────────────────────

/// Stateless executor for one `shell.exec` invocation.
///
/// # Description
/// Spawns `/bin/sh -c <command>` through Bubblewrap, captures stdout and stderr,
/// applies timeout and combined output-size limits, and returns a JSON `ToolOutput`.
///
/// The executor is stateless: all configuration is fixed at construction time and
/// comes from the owning [`ShellToolProvider`].
///
/// # Concurrency
/// `ShellExecutor` is `Send + Sync`. Multiple calls may run concurrently on different
/// Tokio tasks, but [`ShellToolProvider::parallel_safe`] returns `false` because shell
/// commands typically have side effects that are not safe to interleave.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_shell::ShellToolProvider;
/// use harw_extension_api::contributors::ToolProvider;
/// use harw_tools::spec::ToolName;
///
/// let provider = ShellToolProvider::new();
/// let name = ToolName::new("shell.exec");
/// let executor = provider.executor(&name);
/// assert!(executor.is_some());
/// ```
pub struct ShellExecutor {
    timeout_secs: u64,
    max_output_bytes: usize,
    limits: ShellLimits,
    /// Vertrauenswürdiges Sandbox-Profil, vom Runtime-Aufbau gesetzt.
    /// Tool-Aufrufe können es nicht setzen oder überschreiben.
    sandbox_profile: SandboxProfile,
    /// Optionaler Permit-Ledger; vorhanden, wenn die Runtime Permit-
    /// geschützte Ausführung aktiviert hat. Fehlt er, ist die Ausführung
    /// nur mit ExecuteProcess-Berechtigung erlaubt (keine Host-Ausführung).
    permit_ledger: Option<Arc<ProcessPermitLedger>>,
    /// Sitzungsseitige Zuordnung von UI-Zustimmungen zu bereits ausgestellten
    /// Permits (siehe [`harw_sandbox::HostPermitSessionRegistry`]). Fehlt sie,
    /// verhält sich Host-Ausführung wie ohne jede Sitzungs-Vorgeschichte:
    /// jeder Befehl braucht einen frisch über die UI ausgestellten Permit.
    host_permit_registry: Option<Arc<HostPermitSessionRegistry>>,
    /// Sendeseite des Host-Permit-Fragekanals (siehe [`crate::host_permit_prompt`]).
    /// `None` heißt: keine Oberfläche hört zu — jede Anfrage ohne bereits
    /// gemerkten Permit oder laufende Sitzungsphase wird sofort abgelehnt
    /// (fail-closed); es gibt keinen Pfad, der das stillschweigend erlaubt.
    host_permit_prompts: Option<HostPermitPromptSender>,
    /// Vorauswahl, die eine geöffnete Frage anzeigt; ändert nie das Ergebnis,
    /// nur die Anzeige. Kommt ausschließlich von der Runtime (siehe
    /// [`ShellToolProvider::with_preselected_permit_variant`]) — ein
    /// Modellaufruf kann diesen Wert über `args`/`ToolCall` nicht erreichen.
    preselected_permit_variant: HostPermitVariant,
    /// Wartezeit auf eine einzelne Nutzerentscheidung, bevor eine geöffnete
    /// Frage fail-closed als Ablehnung gilt.
    host_permit_timeout: Duration,
}

impl ShellExecutor {
    /// Validates caller-controlled arguments and computes the effective timeout.
    ///
    /// Explicit zero values are rejected instead of being passed to the timeout
    /// machinery, while positive caller values remain capped by the provider.
    fn effective_timeout(&self, args: &ShellExecArgs) -> Result<u64, ToolsError> {
        if args.command.trim().is_empty() {
            return Err(ToolsError::InvalidArguments {
                name: TOOL_NAME.to_owned(),
                reason: "command must not be blank".to_owned(),
            });
        }

        if args.timeout_secs == Some(0) {
            return Err(ToolsError::InvalidArguments {
                name: TOOL_NAME.to_owned(),
                reason: "timeout_secs must be greater than zero".to_owned(),
            });
        }

        Ok(args
            .timeout_secs
            .unwrap_or(self.timeout_secs)
            .min(self.timeout_secs))
    }

    /// Truncates `bytes` to at most `limit` bytes, appending a truncation marker if needed.
    ///
    /// # Description
    /// The limit applies to the returned string, including the marker. The retained
    /// prefix is adjusted to the last valid UTF-8 boundary before conversion, so a
    /// multibyte code point is never split. If the limit is too small for the marker,
    /// the marker is omitted and the largest valid prefix is returned within the limit.
    ///
    /// # Returns
    /// `(string, was_truncated)` — the (possibly clipped) string and whether clipping occurred.
    ///
    /// # Concurrency
    /// Pure function; no shared state.
    fn truncate_output(bytes: &[u8], limit: usize) -> (String, bool) {
        if bytes.len() <= limit {
            (String::from_utf8_lossy(bytes).into_owned(), false)
        } else {
            let marker_fits = limit >= TRUNCATION_MARKER.len();
            let prefix_limit = if marker_fits {
                limit - TRUNCATION_MARKER.len()
            } else {
                limit
            };
            let prefix_end = match std::str::from_utf8(&bytes[..prefix_limit]) {
                Ok(_) => prefix_limit,
                Err(error) => error.valid_up_to(),
            };
            let mut clipped = String::from_utf8_lossy(&bytes[..prefix_end]).into_owned();
            if marker_fits {
                clipped.push_str(TRUNCATION_MARKER);
            }
            (clipped, true)
        }
    }

    /// Applies the configured output budget across stdout and stderr together.
    ///
    /// Stdout is retained first because it is normally the primary command result;
    /// stderr receives the unused portion of the same raw-byte budget.
    fn truncate_combined_output(
        stdout: &[u8],
        stderr: &[u8],
        limit: usize,
    ) -> (String, String, bool) {
        let stdout_limit = stdout.len().min(limit);
        let stderr_limit = limit.saturating_sub(stdout_limit);
        let (stdout, stdout_truncated) = Self::truncate_output(stdout, stdout_limit);
        let (stderr, stderr_truncated) = Self::truncate_output(stderr, stderr_limit);

        (stdout, stderr, stdout_truncated || stderr_truncated)
    }

    /// Prüft für Host-Profil-Ausführung, dass ein gültiger Permit den
    /// konkreten Antrag trägt, stellt bei Bedarf einen neuen Permit für
    /// einen bereits sitzungsweit zugestimmten Auftrag aus, oder fragt —
    /// wenn beides fehlt — über den angehängten Fragekanal nach, **bevor**
    /// irgendeine Ausführung stattfindet.
    ///
    /// # Description
    /// Baut zunächst den kanonischen [`ProcessPermitRequest`] aus Sitzung,
    /// Worker-Definition, dem exakten Befehlstext und der aufgelösten
    /// Workspace-Wurzel. Vier Fälle, in dieser Reihenfolge:
    /// 1. Für genau diesen Antrag existiert bereits ein gemerkter Permit
    ///    (siehe [`HostPermitSessionRegistry::lookup_permit`]): er wird über
    ///    [`ProcessPermitLedger::authorize`] direkt verwendet.
    /// 2. Kein gemerkter Permit, aber die Sitzung hat bereits eine laufende
    ///    Host-Arbeitsphase (siehe [`HostPermitSessionRegistry::is_session_approved`]):
    ///    ein neuer Permit wird für genau diesen Antrag ausgestellt, gemerkt
    ///    und sofort autorisiert — ohne erneute Frage.
    /// 3. Weder ein gemerkter Permit noch eine Sitzungsphase, aber ein
    ///    Fragekanal ist angehängt ([`Self::host_permit_prompts`]): eine
    ///    [`HostPermitPrompt`] geht an die anzeigende Oberfläche und diese
    ///    Methode wartet `await`-basiert auf genau eine Antwort, höchstens
    ///    [`Self::host_permit_timeout`] lang. Eine Zustimmung stellt einen
    ///    Permit aus (Umfang aus der gewählten [`HostPermitVariant`]) und
    ///    trägt bei [`HostPermitVariant::SessionLease`] zusätzlich die
    ///    Sitzungsphase ein — danach fragt kein weiterer Auftrag dieser
    ///    Sitzung erneut.
    /// 4. Jeder andere Ausgang — kein Fragekanal angehängt, der Kanal ist
    ///    geschlossen, die Antwort wurde fallengelassen, die Wartezeit lief
    ///    ab, oder ausdrücklich abgelehnt — ist eine Ablehnung (fail-closed,
    ///    Sicherheitsregel „Ablehnung ist der Default").
    ///
    /// Die Vorauswahl, die eine geöffnete Frage anzeigt, ist ausschließlich
    /// [`Self::preselected_permit_variant`]: weder `args`/`ToolCall` noch
    /// irgendein Modellausgang kann sie erreichen oder die Frage selbst
    /// umgehen.
    ///
    /// # Errors
    /// Liefert eine für das Modell lesbare Ablehnungsnachricht als `Err(String)`,
    /// niemals interne Details über andere Permits oder Sitzungen.
    async fn authorize_host_command(
        &self,
        args: &ShellExecArgs,
        sandbox: &SandboxSpec,
        session_id: &str,
    ) -> Result<(), String> {
        let Some(ledger) = &self.permit_ledger else {
            return Err(NO_PERMIT_LEDGER_MSG.to_owned());
        };
        let Some(registry) = &self.host_permit_registry else {
            return Err(NO_PERMIT_LEDGER_MSG.to_owned());
        };
        let request = ProcessPermitRequest {
            session: session_id.to_owned(),
            worker_definition: HOST_WORKER_DEFINITION.to_owned(),
            command: args.command.clone(),
            workspace: sandbox.workspace().canonical_root().to_path_buf(),
            environment: ProcessEnvironment::LocalHost,
        };

        if let Some(id) = registry.lookup_permit(&request) {
            if ledger.authorize(id, &request).is_ok() {
                return Ok(());
            }
            // Gemerkter Permit ist abgelaufen oder wurde widerrufen; fällt
            // unten zur Neubeantragung durch, falls die Sitzung weiterhin
            // zugestimmt hat oder jetzt gefragt werden kann.
        }

        if registry.is_session_approved(session_id) {
            return Self::issue_and_remember(
                ledger,
                registry,
                request,
                HostApprovalScope::SessionLease,
                HOST_SESSION_LEASE_TTL,
            );
        }

        self.prompt_for_authorization(ledger, registry, request).await
    }

    /// Fragt — wenn ein Kanal angehängt ist — über [`HostPermitPrompt`] nach
    /// und verbucht eine ausdrückliche Zustimmung; jeder andere Ausgang ist
    /// eine Ablehnung (siehe [`Self::authorize_host_command`], Fälle 3/4).
    ///
    /// # Concurrency
    /// Blockiert keinen Renderer-Thread: die Frage geht über einen
    /// ungepufferten `mpsc`-Kanal, gewartet wird ausschließlich auf einem
    /// `tokio::sync::oneshot`, begrenzt durch [`Self::host_permit_timeout`].
    async fn prompt_for_authorization(
        &self,
        ledger: &Arc<ProcessPermitLedger>,
        registry: &Arc<HostPermitSessionRegistry>,
        request: ProcessPermitRequest,
    ) -> Result<(), String> {
        let Some(sender) = &self.host_permit_prompts else {
            return Err(NO_UI_APPROVAL_MSG.to_owned());
        };

        let (prompt, answer) = HostPermitPrompt::new(
            request.session.clone(),
            request.worker_definition.clone(),
            request.command.clone(),
            request.workspace.clone(),
            self.preselected_permit_variant,
        );
        if sender.send(prompt).is_err() {
            warn!(session_id = %request.session, "shell.exec: host permit prompt channel closed");
            return Err(NO_UI_APPROVAL_MSG.to_owned());
        }

        let decision = match tokio::time::timeout(self.host_permit_timeout, answer).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => {
                warn!(session_id = %request.session, "shell.exec: host permit answer dropped");
                None
            }
            Err(_elapsed) => {
                warn!(session_id = %request.session, "shell.exec: host permit prompt timed out");
                None
            }
        };

        let Some(variant) = decision else {
            return Err(HOST_PERMIT_DENIED_MSG.to_owned());
        };

        if variant == HostPermitVariant::SessionLease {
            registry.mark_session_approved(request.session.clone(), HOST_SESSION_LEASE_TTL);
        }
        let (scope, ttl) = match variant {
            HostPermitVariant::SingleExecution => {
                (HostApprovalScope::SingleExecution, HOST_SINGLE_EXECUTION_TTL)
            }
            HostPermitVariant::SessionLease => {
                (HostApprovalScope::SessionLease, HOST_SESSION_LEASE_TTL)
            }
        };
        Self::issue_and_remember(ledger, registry, request, scope, ttl)
    }

    /// Stellt einen Permit für `request` aus, merkt ihn in `registry` und
    /// autorisiert ihn sofort für den auslösenden Aufruf.
    fn issue_and_remember(
        ledger: &Arc<ProcessPermitLedger>,
        registry: &Arc<HostPermitSessionRegistry>,
        request: ProcessPermitRequest,
        scope: HostApprovalScope,
        ttl: Duration,
    ) -> Result<(), String> {
        let id = ledger
            .issue_after_local_approval(request.clone(), scope, ttl)
            .map_err(|err| format!("shell.exec: host execution not authorized: {err}"))?;
        registry.remember_permit(request.clone(), id);
        ledger
            .authorize(id, &request)
            .map(|_granted| ())
            .map_err(|err| format!("shell.exec: host execution not authorized: {err}"))
    }

    /// Executes the shell command described by `args` in the given sandbox.
    ///
    /// # Description
    /// Core async logic extracted for readability. Resolves the pinned `bwrap`/`prlimit`
    /// binaries, spawns the subprocess with stdin `/dev/null`, streams stdout/stderr under
    /// one byte budget and applies the timeout to reading and waiting together.
    ///
    /// For [`SandboxProfile::Host`] this first calls [`Self::authorize_host_command`];
    /// Strict/Cargo/Tmux profiles are unaffected because the sandbox itself is
    /// their enforcement boundary, not a permit.
    ///
    /// # Errors
    /// Returns `Ok(ToolOutput::error(...))` for denied-permission, missing sandbox binaries,
    /// timeout, or spawn failure — callers are not expected to match on `Err` for these cases.
    /// Returns `Err(ToolsError)` only for argument parsing failures.
    ///
    /// # Concurrency
    /// Safe to call from any async context. Timeout and output overflow kill and reap the
    /// child explicitly; `kill_on_drop(true)` still covers a dropped future.
    ///
    /// # Panics
    /// None in production paths.
    async fn run_command(
        &self,
        args: &ShellExecArgs,
        sandbox: &SandboxSpec,
        session_id: &str,
    ) -> Result<ToolOutput, ToolsError> {
        let effective_timeout = self.effective_timeout(args)?;

        if self.sandbox_profile.is_host() {
            if let Err(message) = self.authorize_host_command(args, sandbox, session_id).await {
                warn!(session_id, "shell.exec denied: host permit authorization failed");
                return Ok(ToolOutput::error(message));
            }
        }

        debug!(
            command_len = args.command.len(),
            cwd = %sandbox.workspace().canonical_root().display(),
            timeout_secs = effective_timeout,
            "shell.exec preparing isolated process"
        );

        let (tmpfs_size, prlimit) = match self.resolve_limits() {
            Ok(resolved) => resolved,
            Err(err) => {
                let err = ShellExecError::ResourceLimits(err);
                warn!(error = %err, "shell.exec resource limits unavailable");
                return Ok(ToolOutput::error(err.to_string()));
            }
        };
        if prlimit.is_none() {
            warn!("shell.exec runs WITHOUT rlimits: prlimit missing and require_rlimits=false");
        }

        let launcher = match BwrapLauncher::discover() {
            Ok(launcher) => launcher
                .with_tmpfs_size(tmpfs_size)
                .with_profile(&self.sandbox_profile),
            Err(err) => {
                warn!(error = %err, "shell.exec bubblewrap unavailable");
                return Ok(ToolOutput::error(format!(
                    "shell.exec: sandbox setup failed: {err}"
                )));
            }
        };
        let shell_command = [
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from(&args.command),
        ];
        let plan = match launcher.plan(sandbox, &shell_command) {
            Ok(plan) => plan,
            Err(err) => {
                warn!(error = %err, "shell.exec sandbox plan rejected");
                return Ok(ToolOutput::error(format!(
                    "shell.exec: sandbox setup failed: {err}"
                )));
            }
        };

        // `BwrapLauncher::spawn` ist synchron und kann nicht pipen. Der validierte Plan wird
        // daher hier mit Tokio gestartet — mit dem festgepinnten `launcher.executable()`,
        // nie mit einem über `PATH` gesuchten `bwrap`.
        let launch = launch_command(
            prlimit.as_deref(),
            &self.limits,
            launcher.executable(),
            &plan,
        );
        let mut command = TokioCommand::new(&launch.program);
        command.args(&launch.args);
        let mut child = match configure_stdio(&mut command).spawn() {
            Ok(child) => child,
            Err(err) => {
                warn!(
                    error = %err,
                    program = %launch.program.display(),
                    "shell.exec spawn failed"
                );
                return Ok(ToolOutput::error(format!(
                    "shell.exec: failed to spawn Bubblewrap: {err}"
                )));
            }
        };

        let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take())
        else {
            terminate(&mut child).await;
            return Ok(ToolOutput::error(
                "shell.exec: process I/O error: stdout/stderr pipes missing",
            ));
        };

        let deadline = tokio::time::Instant::now() + Duration::from_secs(effective_timeout);
        let mut capture = BoundedCapture::new(self.max_output_bytes);
        let drained =
            tokio::time::timeout_at(deadline, capture.drain(&mut stdout, &mut stderr)).await;

        let status = match drained {
            Err(_elapsed) => {
                terminate(&mut child).await;
                warn!(timeout_secs = effective_timeout, "shell.exec timed out");
                return Ok(self.timeout_output(effective_timeout, &capture));
            }
            Ok(Err(err)) => {
                terminate(&mut child).await;
                warn!(error = %err, "shell.exec output read failed");
                return Ok(ToolOutput::error(format!(
                    "shell.exec: process I/O error: {err}"
                )));
            }
            Ok(Ok(DrainEnd::LimitExceeded)) => {
                // Niemand liest mehr: Prozessbaum sofort beenden statt bis zum Timeout
                // weiterlaufen zu lassen (Pipe voll → Kind blockiert, CPU/Disk-Last bleibt).
                let status = terminate(&mut child).await;
                warn!(
                    max_output_bytes = self.max_output_bytes,
                    "shell.exec output limit exceeded; process tree killed"
                );
                return Ok(self.completed_output(status, &capture, true));
            }
            Ok(Ok(DrainEnd::Eof)) => {
                match tokio::time::timeout_at(deadline, child.wait()).await {
                    Ok(Ok(status)) => status,
                    Ok(Err(err)) => {
                        terminate(&mut child).await;
                        warn!(error = %err, "shell.exec wait failed");
                        return Ok(ToolOutput::error(format!(
                            "shell.exec: process I/O error: {err}"
                        )));
                    }
                    Err(_elapsed) => {
                        terminate(&mut child).await;
                        warn!(timeout_secs = effective_timeout, "shell.exec timed out");
                        return Ok(self.timeout_output(effective_timeout, &capture));
                    }
                }
            }
        };

        Ok(self.completed_output(Some(status), &capture, false))
    }

    /// Prüft die Limits und löst `prlimit` an den festen Pfaden auf.
    ///
    /// # Returns
    /// tmpfs-Größe für bwrap `--size` und den `prlimit`-Pfad (`None` nur, wenn
    /// `require_rlimits == false` und `prlimit` fehlt).
    fn resolve_limits(&self) -> Result<(NonZeroU64, Option<PathBuf>), ShellLimitsError> {
        self.limits.validate()?;
        let tmpfs_size = self.limits.tmpfs_size()?;
        let prlimit = self.limits.resolve_prlimit()?;
        Ok((tmpfs_size, prlimit))
    }

    /// JSON-Ergebnis eines beendeten (oder wegen Ausgabeüberlauf getöteten) Prozesses.
    ///
    /// `killed_by_output_limit` ist der Kürzungshinweis für den Aufrufer: Die Ausgabe ist
    /// gekappt **und** das Kommando lief nicht zu Ende; `exit_code` ist dann `-1` (Signal).
    fn completed_output(
        &self,
        status: Option<ExitStatus>,
        capture: &BoundedCapture,
        killed_by_output_limit: bool,
    ) -> ToolOutput {
        let exit_code = status.and_then(|status| status.code()).unwrap_or(-1);
        let (stdout_str, stderr_str, truncated) = Self::truncate_combined_output(
            capture.stdout(),
            capture.stderr(),
            self.max_output_bytes,
        );
        let truncated = truncated || killed_by_output_limit;

        info!(exit_code, truncated, killed_by_output_limit, "shell.exec completed");

        ToolOutput::json(json!({
            "exit_code": exit_code,
            "stdout": stdout_str,
            "stderr": stderr_str,
            "truncated": truncated,
            "killed_by_output_limit": killed_by_output_limit,
        }))
    }

    /// Fehlerergebnis bei Timeout, das die bis dahin gelesene Teilausgabe (im selben
    /// Byte-Budget gekürzt) mitliefert.
    fn timeout_output(&self, timeout_secs: u64, capture: &BoundedCapture) -> ToolOutput {
        let (stdout, stderr, truncated) = Self::truncate_combined_output(
            capture.stdout(),
            capture.stderr(),
            self.max_output_bytes,
        );
        ToolOutput::error(format!(
            "shell.exec timed out after {timeout_secs}s; process tree killed. \
             Partial output (truncated: {truncated}):\n[stdout]\n{stdout}\n[stderr]\n{stderr}"
        ))
    }
}

/// Setzt die Standard-Streams für den Sandbox-Start: stdin `/dev/null` (nie das geerbte
/// Terminal), stdout/stderr als Pipes, SIGKILL beim Drop des `Child`.
fn configure_stdio(command: &mut TokioCommand) -> &mut TokioCommand {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
}

/// Beendet den direkten Kindprozess (`prlimit` hat sich per `exec` durch `bwrap` ersetzt)
/// per SIGKILL und sammelt den Exit-Status ein. `bwrap --die-with-parent` und der
/// PID-Namespace beenden daraufhin den gesamten Sandbox-Prozessbaum, auch per
/// `setsid`/`nohup` abgekoppelte Nachfahren.
async fn terminate(child: &mut Child) -> Option<ExitStatus> {
    if let Err(err) = child.start_kill() {
        warn!(error = %err, "shell.exec kill failed");
    }
    match tokio::time::timeout(KILL_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(err)) => {
            warn!(error = %err, "shell.exec reap after kill failed");
            None
        }
        Err(_elapsed) => {
            warn!("shell.exec reap after kill timed out; relying on kill_on_drop");
            None
        }
    }
}

impl ToolExecutor for ShellExecutor {
    /// Executes the `shell.exec` tool call.
    ///
    /// # Description
    /// 1. Parses `call.arguments` into [`ShellExecArgs`].
    /// 2. Rejects blank commands and caller-provided zero timeouts.
    /// 3. Checks that [`Permission::ExecuteProcess`] is granted in `context.sandbox()`.
    /// 4. Builds a Bubblewrap plan from `context.sandbox()`.
    /// 5. Executes `/bin/sh -c <command>` inside that plan with the effective timeout.
    /// 6. Returns a JSON `ToolOutput` with `exit_code`, `stdout`, `stderr`, and `truncated`.
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-established sandbox authority.
    /// - `call` (`&ToolCall`): untrusted invocation; `arguments` must match [`ShellExecArgs`].
    ///
    /// # Returns
    /// `Ok(ToolOutput::json(...))` on success, `Ok(ToolOutput::error(...))` on permission
    /// denial or runtime failure, `Err(ToolsError::InvalidArguments)` if arguments cannot
    /// be parsed.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: arguments JSON does not conform to the tool schema.
    ///
    /// # Concurrency
    /// `Send + Sync`. The returned future is `Send`.
    ///
    /// # Panics
    /// None.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    /// use harw_extension_api::contributors::ToolProvider;
    /// use harw_tools::spec::ToolName;
    ///
    /// let provider = ShellToolProvider::new();
    /// let executor = provider.executor(&ToolName::new("shell.exec")).unwrap();
    /// // executor.execute(&ctx, &call).await
    /// ```
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            // 1. Parse arguments
            let args: ShellExecArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|err| {
                    ToolsError::InvalidArguments {
                        name: TOOL_NAME.to_owned(),
                        reason: err.to_string(),
                    }
                })?;

            // 2. Reject invalid caller-controlled values before sandbox planning or spawn.
            self.effective_timeout(&args)?;

            // 3. Permission check
            if let Some(err) = harw_tools::sandbox_guard::require_permission(
                context,
                Permission::ExecuteProcess,
                TOOL_NAME,
            ) {
                warn!("shell.exec denied: ExecuteProcess permission missing");
                return Ok(err);
            }

            // 4. Permit-Prüfung für Host-Profil erfolgt vollständig in
            //    `run_command` (`Self::authorize_host_command`), sobald der
            //    Befehlstext und die aufgelöste Workspace-Wurzel für den
            //    konkreten `ProcessPermitRequest` vorliegen. Strict/Cargo/Tmux
            //    ohne Ledger laufen unverändert weiter (die Sandbox ist die
            //    Grenze, nicht der Permit).

            // 5 + 6. Build an isolated launch plan, spawn, and collect.
            self.run_command(&args, context.sandbox(), context.session_id().as_str())
                .await
        })
    }
}

// ── ShellToolProvider ─────────────────────────────────────────────────────────

/// Tool provider that registers the `shell.exec` function-tool.
///
/// # Description
/// Holds the shared configuration (timeout, max output size) and manufactures
/// [`ShellExecutor`] instances on demand via [`ToolProvider::executor`].
///
/// Defaults: `timeout_secs = 30`, `max_output_bytes = 65536` (64 KiB),
/// `limits = ShellLimits::default()` (rlimits über `/usr/bin/prlimit`, tmpfs 256 MiB).
///
/// # Concurrency
/// `Send + Sync`. Multiple threads may call [`tools`][ShellToolProvider::tools] and
/// [`executor`][ShellToolProvider::executor] concurrently without synchronisation.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_shell::ShellToolProvider;
/// use harw_extension_api::contributors::ToolProvider;
/// use harw_tools::spec::ToolName;
///
/// let provider = ShellToolProvider::new();
/// assert_eq!(provider.tools().len(), 1);
/// assert!(provider.executor(&ToolName::new("shell.exec")).is_some());
/// assert!(provider.executor(&ToolName::new("other.tool")).is_none());
/// ```
pub struct ShellToolProvider {
    /// Maximum time in seconds a single shell command may run.
    pub timeout_secs: u64,
    /// Maximum number of raw stdout and stderr bytes returned together.
    pub max_output_bytes: usize,
    /// rlimits und tmpfs-Größe für jeden Aufruf (siehe [`ShellLimits`]).
    pub limits: ShellLimits,
    /// Vertrauenswürdiges Sandbox-Profil vom Runtime-Aufbau.
    pub sandbox_profile: SandboxProfile,
    /// Optionaler Permit-Ledger für Permit-geschützte Ausführung.
    pub permit_ledger: Option<Arc<ProcessPermitLedger>>,
    /// Sitzungsseitige Zuordnung von UI-Zustimmungen zu bereits ausgestellten
    /// Permits; von der Runtime beim Aufbau des Host-Profil-Workers gesetzt.
    pub host_permit_registry: Option<Arc<HostPermitSessionRegistry>>,
    /// Sendeseite des Host-Permit-Fragekanals (siehe [`crate::host_permit_prompt`]);
    /// von der Runtime gesetzt, damit `ShellExecutor` vor einer Host-Ausführung
    /// ohne bereits gemerkten Permit/Sitzungsphase tatsächlich fragen kann.
    /// `None` heißt fail-closed: keine Frage, keine Ausführung.
    pub host_permit_prompts: Option<HostPermitPromptSender>,
    /// Vorauswahl, die eine geöffnete Frage anzeigt (siehe
    /// [`Self::with_preselected_permit_variant`]); ändert nie das Ergebnis.
    pub preselected_permit_variant: HostPermitVariant,
    /// Wartezeit auf eine einzelne Nutzerentscheidung auf eine offene Frage,
    /// bevor sie fail-closed als Ablehnung gilt.
    pub host_permit_timeout: Duration,
}

impl ShellToolProvider {
    /// Creates a [`ShellToolProvider`] with default configuration.
    ///
    /// # Description
    /// Equivalent to [`Default::default`]. Provides sane defaults suitable for
    /// interactive coding-agent use: 30-second timeout, 64 KiB output cap.
    ///
    /// # Returns
    /// A new [`ShellToolProvider`] with `timeout_secs = 30` and `max_output_bytes = 65536`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    ///
    /// let provider = ShellToolProvider::new();
    /// assert_eq!(provider.timeout_secs, 30);
    /// assert_eq!(provider.max_output_bytes, 64 * 1024);
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Setzt das vertrauenswürdige Sandbox-Profil vom Runtime-Aufbau.
    /// Tool-Aufrufe können das Profil nicht setzen oder überschreiben.
    #[must_use]
    pub fn with_sandbox_profile(mut self, profile: SandboxProfile) -> Self {
        self.sandbox_profile = profile;
        self
    }

    /// Setzt den Permit-Ledger für Permit-geschützte Ausführung.
    #[must_use]
    pub fn with_permit_ledger(mut self, ledger: Arc<ProcessPermitLedger>) -> Self {
        self.permit_ledger = Some(ledger);
        self
    }

    /// Setzt die sitzungsseitige Zuordnung von UI-Zustimmungen zu bereits
    /// ausgestellten Host-Permits.
    #[must_use]
    pub fn with_host_permit_registry(mut self, registry: Arc<HostPermitSessionRegistry>) -> Self {
        self.host_permit_registry = Some(registry);
        self
    }

    /// Hängt die Sendeseite des Host-Permit-Fragekanals an (siehe
    /// [`crate::host_permit_prompt::host_permit_prompt_channel`]). Ohne
    /// diesen Aufruf bleibt jede Host-Anfrage ohne bereits gemerkten Permit
    /// oder laufende Sitzungsphase fail-closed abgelehnt — es gibt keine
    /// Rückfrage, nur eine Ablehnung.
    #[must_use]
    pub fn with_host_permit_prompts(mut self, sender: HostPermitPromptSender) -> Self {
        self.host_permit_prompts = Some(sender);
        self
    }

    /// Setzt die Vorauswahl, die eine geöffnete Host-Permit-Frage anzeigt.
    ///
    /// # Description
    /// Reine Anzeige-Vorauswahl — ändert nie, was tatsächlich genehmigt
    /// wird, nur was der Dialog vorschlägt. Der Aufrufer (`harw-runtime`)
    /// leitet sie nach Möglichkeit aus dem aktiven `InteractionMode` der
    /// Sitzung ab; ohne diesen Aufruf bleibt es bei
    /// [`HostPermitVariant::SingleExecution`] ([`HostPermitVariant::default`]).
    #[must_use]
    pub fn with_preselected_permit_variant(mut self, variant: HostPermitVariant) -> Self {
        self.preselected_permit_variant = variant;
        self
    }

    /// Setzt die Wartezeit auf eine einzelne Nutzerentscheidung, bevor eine
    /// offene Host-Permit-Frage fail-closed als Ablehnung gilt (Vorgabe:
    /// [`HOST_PERMIT_PROMPT_TIMEOUT`]).
    #[must_use]
    pub fn with_host_permit_timeout(mut self, timeout: Duration) -> Self {
        self.host_permit_timeout = timeout;
        self
    }

    /// Builds the [`JsonSchema`] for the `shell.exec` parameters.
    ///
    /// # Description
    /// Returns an `object` schema with two properties — `command` (required) and
    /// `timeout_secs` (optional integer). Constructed once per [`tools`][ShellToolProvider::tools]
    /// call; cheap enough not to cache.
    ///
    /// # Returns
    /// A [`JsonSchema`] matching the tool's parameter contract.
    ///
    /// # Concurrency
    /// Pure function; no shared state.
    fn parameter_schema() -> JsonSchema {
        let mut properties = BTreeMap::new();

        properties.insert(
            "command".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                description: Some(
                    "Shell command to execute in the isolated project sandbox. Uses /bin/sh -c."
                        .to_owned(),
                ),
                ..Default::default()
            },
        );

        properties.insert(
            "timeout_secs".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::Integer),
                description: Some(
                    "Optional per-call timeout override. Cannot exceed provider default."
                        .to_owned(),
                ),
                ..Default::default()
            },
        );

        JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["command".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        }
    }
}

impl Default for ShellToolProvider {
    /// Returns a [`ShellToolProvider`] with default timeout (30 s) and output cap (64 KiB).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    ///
    /// let provider = ShellToolProvider::default();
    /// assert_eq!(provider.timeout_secs, 30);
    /// ```
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        }
    }
}

impl ToolProvider for ShellToolProvider {
    /// Returns the single tool specification for `shell.exec`.
    ///
    /// # Description
    /// Constructs a [`ToolSpec::Function`] with the JSON schema, description, and
    /// `strict = true` so the model cannot inject extra fields.
    ///
    /// # Returns
    /// A one-element `Vec<ToolSpec>`.
    ///
    /// # Concurrency
    /// Safe to call from multiple threads.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    /// use harw_extension_api::contributors::ToolProvider;
    ///
    /// let specs = ShellToolProvider::new().tools();
    /// assert_eq!(specs.len(), 1);
    /// assert_eq!(specs[0].name(), "shell.exec");
    /// ```
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(TOOL_NAME),
            description: "Execute a shell command inside the isolated project sandbox. \
                Runs under Bubblewrap via /bin/sh -c and captures stdout+stderr. \
                Requires ExecuteProcess permission."
                .to_owned(),
            parameters: Self::parameter_schema(),
            strict: true,
        })]
    }

    /// Returns a [`ShellExecutor`] for `shell.exec`, or `None` for any other name.
    ///
    /// # Description
    /// Constructs an executor carrying the provider's timeout and output-cap configuration.
    /// Each call allocates a new `Arc<ShellExecutor>`; the executor itself is stateless.
    ///
    /// # Arguments
    /// - `name` (`&ToolName`): the requested tool name.
    ///
    /// # Returns
    /// `Some(Arc<ShellExecutor>)` when `name == "shell.exec"`, `None` otherwise.
    ///
    /// # Concurrency
    /// Safe to call from multiple threads.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    /// use harw_extension_api::contributors::ToolProvider;
    /// use harw_tools::spec::ToolName;
    ///
    /// let provider = ShellToolProvider::new();
    /// assert!(provider.executor(&ToolName::new("shell.exec")).is_some());
    /// assert!(provider.executor(&ToolName::new("other")).is_none());
    /// ```
    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        if name.as_str() == TOOL_NAME {
            Some(Arc::new(ShellExecutor {
                timeout_secs: self.timeout_secs,
                max_output_bytes: self.max_output_bytes,
                limits: self.limits,
                sandbox_profile: self.sandbox_profile.clone(),
                permit_ledger: self.permit_ledger.clone(),
                host_permit_registry: self.host_permit_registry.clone(),
                host_permit_prompts: self.host_permit_prompts.clone(),
                preselected_permit_variant: self.preselected_permit_variant,
                host_permit_timeout: self.host_permit_timeout,
            }))
        } else {
            None
        }
    }

    /// Returns `false` — shell side-effects are presumed non-commutative.
    ///
    /// # Description
    /// Shell commands modify filesystem state, environment variables, and process
    /// tables. Running them in parallel without coordination risks data races.
    /// Callers must serialize `shell.exec` invocations.
    ///
    /// # Returns
    /// Always `false`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    /// use harw_extension_api::contributors::ToolProvider;
    /// use harw_tools::spec::ToolName;
    ///
    /// let provider = ShellToolProvider::new();
    /// assert!(!provider.parallel_safe(&ToolName::new("shell.exec")));
    /// ```
    fn parallel_safe(&self, _name: &ToolName) -> bool {
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_tools::{ToolCall, ToolExecutionContext};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use std::path::Path;
    use std::sync::OnceLock;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    // ── Test helpers ───────────────────────────────────────────────────────────

    fn make_temp_workspace() -> TempDir {
        tempfile::tempdir().expect("tempdir creation must succeed in tests")
    }

    fn make_sandbox(dir: &TempDir, permissions: Vec<Permission>) -> SandboxSpec {
        let harness_root = dir.path().to_path_buf();
        let ws_subdir = harness_root.join("project");
        fs::create_dir_all(&ws_subdir).expect("project subdir must be created");

        let registry = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: ws_subdir,
            }],
        )
        .expect("registry build must succeed");

        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("project"),
            )
            .expect("resolve must succeed");

        SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions))
    }

    fn make_ctx(sandbox: SandboxSpec) -> ToolExecutionContext {
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
    }

    fn make_call(command: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(TOOL_NAME),
            arguments: serde_json::json!({ "command": command }),
        }
    }

    fn make_call_with_timeout(command: &str, timeout_secs: u64) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(TOOL_NAME),
            arguments: serde_json::json!({ "command": command, "timeout_secs": timeout_secs }),
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_provider_lists_one_tool() {
        let provider = ShellToolProvider::new();
        let tools = provider.tools();
        assert_eq!(tools.len(), 1, "provider must expose exactly one tool");
        assert_eq!(tools[0].name(), TOOL_NAME);
    }

    #[test]
    fn test_provider_not_parallel_safe() {
        let provider = ShellToolProvider::new();
        let name = ToolName::new(TOOL_NAME);
        assert!(
            !provider.parallel_safe(&name),
            "shell.exec must never be parallel-safe"
        );
    }

    #[test]
    fn test_effective_timeout_rejects_blank_command_without_planning() {
        let executor = ShellExecutor {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };

        for command in ["", " ", "\t\n"] {
            let args = ShellExecArgs {
                command: command.to_owned(),
                timeout_secs: None,
            };
            let result = executor.effective_timeout(&args);

            assert!(
                matches!(result, Err(ToolsError::InvalidArguments { .. })),
                "blank command must be rejected before planning"
            );
        }
    }

    #[test]
    fn test_effective_timeout_rejects_caller_zero_without_planning() {
        let executor = ShellExecutor {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        let args = ShellExecArgs {
            command: "echo valid".to_owned(),
            timeout_secs: Some(0),
        };

        let result = executor.effective_timeout(&args);

        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "caller-provided zero timeout must be rejected before planning"
        );
    }

    #[test]
    fn test_effective_timeout_preserves_provider_cap() {
        let executor = ShellExecutor {
            timeout_secs: 5,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        let args = ShellExecArgs {
            command: "echo valid".to_owned(),
            timeout_secs: Some(30),
        };

        assert_eq!(executor.effective_timeout(&args).unwrap_or(0), 5);
    }

    #[test]
    fn test_truncate_output_handles_multibyte_utf8_boundary() {
        let limit = 3 + TRUNCATION_MARKER.len();
        let (output, truncated) =
            ShellExecutor::truncate_output("abc€defghijklmnopqrs".as_bytes(), limit);

        assert!(
            truncated,
            "output over the byte cap must be marked truncated"
        );
        assert!(
            output.starts_with("abc"),
            "truncation must retain only complete UTF-8 characters, got: {output:?}"
        );
        assert!(
            !output.contains('�'),
            "truncation must not split a code point"
        );
        assert!(
            output.len() <= limit,
            "returned output must respect its byte cap"
        );
        assert!(
            output.ends_with(TRUNCATION_MARKER),
            "truncated output must include its marker, got: {output:?}"
        );
    }

    #[test]
    fn test_truncate_combined_output_uses_one_budget() {
        let limit = 6 + 4 + TRUNCATION_MARKER.len();
        let (stdout, stderr, truncated) = ShellExecutor::truncate_combined_output(
            b"stdout",
            b"stderr-output-that-is-long",
            limit,
        );

        assert_eq!(stdout, "stdout");
        assert_eq!(stderr, format!("stde{TRUNCATION_MARKER}"));
        assert!(
            truncated,
            "combined output over the cap must be marked truncated"
        );
        assert!(
            stdout.len() + stderr.len() <= limit,
            "stdout and stderr together must respect one byte budget"
        );
    }

    #[test]
    fn test_truncate_combined_output_prioritizes_over_budget_stdout() {
        let limit = 4 + TRUNCATION_MARKER.len();
        let (stdout, stderr, truncated) = ShellExecutor::truncate_combined_output(
            b"stdout-output-that-is-long",
            b"stderr-output",
            limit,
        );

        assert_eq!(stdout, format!("stdo{TRUNCATION_MARKER}"));
        assert_eq!(stderr, "");
        assert!(truncated, "over-budget stdout must be marked truncated");
        assert!(
            stdout.len() + stderr.len() <= limit,
            "stdout and stderr together must respect one byte budget"
        );
    }

    #[test]
    fn test_truncate_output_with_small_budget_keeps_valid_prefix() {
        let (output, truncated) = ShellExecutor::truncate_output("abc€def".as_bytes(), 4);

        assert_eq!(output, "abc");
        assert!(
            truncated,
            "output over the byte cap must be marked truncated"
        );
        assert!(
            output.len() <= 4,
            "returned output must respect its byte cap"
        );
    }

    // ── Pure Tests ohne Sandbox (W1-03) ────────────────────────────────────────

    #[tokio::test]
    async fn test_configure_stdio_sets_stdin_to_dev_null() {
        if !Path::new("/proc/self/fd/0").exists() || !Path::new("/bin/sh").exists() {
            eprintln!("übersprungen: /proc oder /bin/sh fehlt, stdin-Ziel nicht beobachtbar");
            return;
        }
        // Absichtlich ohne bwrap: prüft genau die Stream-Konfiguration, die der
        // Sandbox-Start verwendet.
        let mut command = TokioCommand::new("/bin/sh");
        command.args(["-c", "readlink /proc/self/fd/0"]);
        let mut child = configure_stdio(&mut command)
            .spawn()
            .expect("spawn /bin/sh");
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        let mut capture = BoundedCapture::new(4096);
        let end = capture
            .drain(&mut stdout, &mut stderr)
            .await
            .expect("read child output");
        let status = child.wait().await.expect("wait child");

        assert_eq!(end, DrainEnd::Eof);
        assert!(status.success(), "readlink must succeed: {status:?}");
        assert_eq!(String::from_utf8_lossy(capture.stdout()).trim(), "/dev/null");
    }

    #[test]
    fn test_completed_output_marks_output_limit_kill_as_truncated() {
        let executor = ShellExecutor {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: 16,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        let capture = BoundedCapture::new(16);

        match executor.completed_output(None, &capture, true) {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], -1);
                assert_eq!(content["truncated"], true);
                assert_eq!(content["killed_by_output_limit"], true);
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_timeout_output_keeps_partial_output_within_budget() {
        let executor = ShellExecutor {
            timeout_secs: 1,
            max_output_bytes: 8 + TRUNCATION_MARKER.len(),
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        let (mut writer, mut stdout) = tokio::io::duplex(1024);
        let (_stderr_writer, mut stderr) = tokio::io::duplex(1024);
        writer
            .write_all(b"partial-output-longer-than-budget")
            .await
            .expect("write");
        let mut capture = BoundedCapture::new(executor.max_output_bytes);
        let _ = tokio::time::timeout(
            Duration::from_millis(50),
            capture.drain(&mut stdout, &mut stderr),
        )
        .await;

        match executor.timeout_output(1, &capture) {
            ToolOutput::Error { message } => {
                assert!(message.contains("timed out after 1s"), "{message}");
                assert!(message.contains("partial-"), "{message}");
                assert!(message.contains("truncated: true"), "{message}");
                assert!(!message.contains("longer-than-budget"), "{message}");
            }
            other => panic!("expected Error output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_exec_invalid_limits_fail_closed_before_spawn() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let executor = ShellExecutor {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits {
                nofile: 0,
                ..ShellLimits::default()
            },
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        let args = ShellExecArgs {
            command: "echo must_not_run".to_owned(),
            timeout_secs: None,
        };

        match executor.run_command(&args, &sandbox, "test-session").await.expect("run") {
            ToolOutput::Error { message } => {
                assert!(message.contains("resource limits"), "{message}");
                assert!(message.contains("nofile"), "{message}");
            }
            other => panic!("invalid limits must not spawn, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_exec_permission_denied() {
        let tmp = make_temp_workspace();
        // No ExecuteProcess permission
        let sandbox = make_sandbox(&tmp, vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call("echo should_not_run");

        let provider = ShellToolProvider::new();
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err even when denied");

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("ExecuteProcess permission missing"),
                    "denial message must mention the missing permission, got: {message:?}"
                );
            }
            other => panic!("expected Error output for denied permission, got: {other:?}"),
        }
    }

    // ── Sandbox-Integrationstests (brauchen bwrap + prlimit + userns) ─────────
    //
    // Jeder Test existiert zweimal: Die normale Variante prüft zur Laufzeit, ob die
    // Sandbox startbar ist, und kehrt sonst mit Begründung früh zurück. Die
    // `#[ignore]`-Variante (`cargo test -- --ignored`) verlangt die Umgebung und
    // schlägt ohne sie fehl.

    /// Startet einmalig `prlimit <Default-Limits> -- bwrap --unshare-all … /bin/true` an den
    /// festen Pfaden. Scheitert das (kein bwrap/prlimit, userns gesperrt, NPROC zu knapp),
    /// sind die Integrationstests nicht aussagekräftig.
    fn sandbox_runtime_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            let Ok(launcher) = BwrapLauncher::discover() else {
                return false;
            };
            let Ok(Some(prlimit)) = ShellLimits::default().resolve_prlimit() else {
                return false;
            };
            let mut args = ShellLimits::default().prlimit_args();
            args.push(launcher.executable().as_os_str().to_owned());
            args.extend(
                [
                    "--die-with-parent",
                    "--unshare-all",
                    "--ro-bind",
                    "/",
                    "/",
                    "--",
                    "/bin/true",
                ]
                .map(OsString::from),
            );
            std::process::Command::new(prlimit)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    }

    macro_rules! sandbox_test {
        ($name:ident, $required:ident, $body:ident) => {
            #[tokio::test]
            async fn $name() {
                if !sandbox_runtime_available() {
                    eprintln!(
                        "übersprungen: {}: bwrap/prlimit/userns nicht startbar \
                         (Pflichtvariante: {} mit --ignored)",
                        stringify!($name),
                        stringify!($required)
                    );
                    return;
                }
                $body().await;
            }

            #[tokio::test]
            #[ignore = "requires bwrap+userns"]
            async fn $required() {
                assert!(
                    sandbox_runtime_available(),
                    "bwrap+prlimit+userns müssen für diesen Test startbar sein"
                );
                $body().await;
            }
        };
    }

    async fn run_with(provider: &ShellToolProvider, call: &ToolCall) -> ToolOutput {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned for shell.exec");
        executor
            .execute(&ctx, call)
            .await
            .expect("execute must not return Err")
    }

    async fn echo_returns_stdout() {
        let output = run_with(&ShellToolProvider::new(), &make_call("echo hello_from_shell")).await;

        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], 0, "echo must exit with 0");
                let stdout = content["stdout"].as_str().unwrap_or("");
                assert!(
                    stdout.contains("hello_from_shell"),
                    "stdout must contain echoed string, got: {stdout:?}"
                );
                assert_eq!(
                    content["truncated"], false,
                    "echo output must not be truncated"
                );
                assert_eq!(content["killed_by_output_limit"], false);
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }
    sandbox_test!(
        test_exec_echo_returns_stdout,
        test_exec_echo_returns_stdout_required,
        echo_returns_stdout
    );

    async fn timeout_reports_error_with_partial_output() {
        // Provider with 1-second timeout; per-call override also 1s
        let provider = ShellToolProvider {
            timeout_secs: 1,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        let call = make_call_with_timeout("echo partial_before_timeout; sleep 5", 1);
        let started = std::time::Instant::now();

        let output = run_with(&provider, &call).await;

        assert!(
            started.elapsed() < Duration::from_secs(4),
            "timeout must kill the process tree instead of waiting for sleep"
        );
        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("timed out"),
                    "timeout message must contain 'timed out', got: {message:?}"
                );
                assert!(
                    message.contains("partial_before_timeout"),
                    "partial output must survive the timeout, got: {message:?}"
                );
            }
            other => panic!("expected Error output for timeout, got: {other:?}"),
        }
    }
    sandbox_test!(
        test_exec_timeout,
        test_exec_timeout_required,
        timeout_reports_error_with_partial_output
    );

    async fn nonzero_exit_is_reported() {
        let output = run_with(&ShellToolProvider::new(), &make_call("false")).await;

        match output {
            ToolOutput::Json { content } => {
                let exit_code = content["exit_code"].as_i64().unwrap_or(0);
                assert_ne!(exit_code, 0, "false must return a nonzero exit code");
            }
            other => panic!("expected Json output even for nonzero exit, got: {other:?}"),
        }
    }
    sandbox_test!(
        test_exec_nonzero_exit,
        test_exec_nonzero_exit_required,
        nonzero_exit_is_reported
    );

    async fn stderr_is_captured() {
        let output = run_with(&ShellToolProvider::new(), &make_call("echo error_output >&2")).await;

        match output {
            ToolOutput::Json { content } => {
                let stderr = content["stderr"].as_str().unwrap_or("");
                assert!(
                    stderr.contains("error_output"),
                    "stderr must be captured, got: {stderr:?}"
                );
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }
    sandbox_test!(
        test_exec_stderr_captured,
        test_exec_stderr_captured_required,
        stderr_is_captured
    );

    async fn small_output_over_cap_is_truncated() {
        // The cap leaves room for a marker plus a truncated stdout prefix.
        let provider = ShellToolProvider {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: TRUNCATION_MARKER.len() + 10,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        // Generate more than the configured output cap.
        let call = make_call("echo 'this_is_a_longer_string_than_ten_bytes'");

        match run_with(&provider, &call).await {
            ToolOutput::Json { content } => {
                assert_eq!(
                    content["truncated"], true,
                    "output must be reported as truncated when cap is exceeded"
                );
                let stdout = content["stdout"].as_str().unwrap_or("");
                assert!(
                    stdout.contains("[...truncated...]"),
                    "truncation marker must appear in stdout, got: {stdout:?}"
                );
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }
    sandbox_test!(
        test_exec_output_truncation,
        test_exec_output_truncation_required,
        small_output_over_cap_is_truncated
    );

    async fn endless_output_is_capped_and_killed() {
        let provider = ShellToolProvider {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: 1024,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        let started = std::time::Instant::now();

        let output = run_with(&provider, &make_call("yes")).await;

        assert!(
            started.elapsed() < Duration::from_secs(DEFAULT_TIMEOUT_SECS / 2),
            "output overflow must kill the process instead of running into the timeout"
        );
        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["truncated"], true);
                assert_eq!(content["killed_by_output_limit"], true);
                let stdout = content["stdout"].as_str().unwrap_or("");
                let stderr = content["stderr"].as_str().unwrap_or("");
                assert!(stdout.len() + stderr.len() <= 1024, "budget must hold");
                assert!(stdout.starts_with("y\ny\n"), "got: {stdout:?}");
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }
    sandbox_test!(
        test_exec_endless_output_is_capped_and_killed,
        test_exec_endless_output_is_capped_and_killed_required,
        endless_output_is_capped_and_killed
    );

    async fn stdin_is_closed_inside_sandbox() {
        let provider = ShellToolProvider {
            timeout_secs: 10,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        };
        // Mit geerbtem Terminal-stdin würde `cat` bis zum Timeout blockieren.
        let call = make_call("cat; echo stdin_reached_eof");

        match run_with(&provider, &call).await {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], 0);
                let stdout = content["stdout"].as_str().unwrap_or("");
                assert_eq!(stdout.trim(), "stdin_reached_eof");
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }
    sandbox_test!(
        test_exec_stdin_is_closed,
        test_exec_stdin_is_closed_required,
        stdin_is_closed_inside_sandbox
    );

    async fn rlimits_and_tmpfs_size_apply_inside_sandbox() {
        let call = make_call("cat /proc/self/limits; df -B1 /tmp");

        match run_with(&ShellToolProvider::new(), &call).await {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], 0, "{content}");
                let stdout = content["stdout"].as_str().unwrap_or("");
                let expected = [
                    ("Max cpu time", "60"),
                    ("Max file size", "268435456"),
                    ("Max processes", "4096"),
                    ("Max open files", "256"),
                    ("Max address space", "2147483648"),
                ];
                for (label, value) in expected {
                    let line = stdout
                        .lines()
                        .find(|line| line.starts_with(label))
                        .unwrap_or_else(|| panic!("missing limit line {label}: {stdout}"));
                    let fields: Vec<&str> = line[label.len()..].split_whitespace().collect();
                    assert_eq!(fields[..2], [value, value], "soft=hard for {label}: {line}");
                }
                let tmpfs = stdout
                    .lines()
                    .find(|line| line.trim_end().ends_with("/tmp"))
                    .unwrap_or_else(|| panic!("missing df line for /tmp: {stdout}"));
                assert!(
                    tmpfs.split_whitespace().nth(1) == Some("268435456"),
                    "tmpfs /tmp must be limited to 256 MiB: {tmpfs}"
                );
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }
    sandbox_test!(
        test_exec_rlimits_and_tmpfs_size_apply,
        test_exec_rlimits_and_tmpfs_size_apply_required,
        rlimits_and_tmpfs_size_apply_inside_sandbox
    );

    // ── Permit-/Profil-Tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_host_profile_without_ledger_is_denied() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);
        let call = make_call("echo should_not_run");

        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Host);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err");

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("host execution requires a process permit"),
                    "expected permit denial, got: {message:?}"
                );
            }
            other => panic!("expected Error output for host without ledger, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_host_profile_with_ledger_but_without_session_approval_is_denied() {
        // Ledger und Registry sind konfiguriert, aber die Sitzung hat der
        // lokalen UI noch nicht zugestimmt: fail-closed.
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);
        let call = make_call("echo should_not_run");

        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Host)
            .with_permit_ledger(ledger)
            .with_host_permit_registry(registry);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err");

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("requires local UI approval"),
                    "expected local-approval denial, got: {message:?}"
                );
            }
            other => panic!("expected Error output without session approval, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_host_profile_with_session_approval_authorizes_via_real_ledger() {
        // Nach einer (simulierten) lokalen UI-Zustimmung für die Sitzung muss
        // `authorize_host_command` tatsächlich über den echten Ledger einen
        // neuen Permit ausstellen und autorisieren, statt nur dessen
        // Existenz zu prüfen.
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);
        let call = make_call("echo host_ok");

        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved(ctx.session_id().as_str(), Duration::from_secs(60));

        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Host)
            .with_permit_ledger(Arc::clone(&ledger))
            .with_host_permit_registry(Arc::clone(&registry));
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err");

        // Die Permit-Prüfung muss durchlaufen sein: eine verbleibende
        // Fehlermeldung darf nur noch vom fehlenden Bubblewrap-Binary in der
        // Testumgebung stammen, nicht von der Permit-Grenze.
        match &output {
            ToolOutput::Error { message } => {
                assert!(
                    !message.contains("process permit") && !message.contains("UI approval"),
                    "host command with session approval must pass the permit boundary: {message:?}"
                );
            }
            ToolOutput::Json { .. } | ToolOutput::Text { .. } => {}
        }

        // Der Permit wurde tatsächlich über den Ledger ausgestellt und
        // gemerkt; ein zweiter identischer Aufruf muss ihn wiederverwenden
        // können, statt erneut auszustellen.
        let request = ProcessPermitRequest {
            session: ctx.session_id().as_str().to_owned(),
            worker_definition: HOST_WORKER_DEFINITION.to_owned(),
            command: "echo host_ok".to_owned(),
            workspace: sandbox_root(&tmp),
            environment: ProcessEnvironment::LocalHost,
        };
        let remembered_id = registry
            .lookup_permit(&request)
            .expect("permit must have been remembered after issuance");
        assert!(ledger.authorize(remembered_id, &request).is_ok());
    }

    // Baut denselben kanonischen Workspace-Pfad wie `make_sandbox`, damit der
    // in einem Test unabhängig zusammengesetzte `ProcessPermitRequest` genau
    // dem entspricht, den `authorize_host_command` tatsächlich verwendet.
    fn sandbox_root(dir: &TempDir) -> PathBuf {
        dir.path()
            .join("project")
            .canonicalize()
            .expect("project subdir must be canonicalizable")
    }

    #[tokio::test]
    async fn test_strict_profile_without_ledger_still_works() {
        // Strict-Profil ohne Ledger: Sandbox ist die Grenze, nicht der Permit.
        // Dies darf nicht fehlschlagen, weil der Ledger fehlt.
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);
        let call = make_call("echo strict_mode_ok");

        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Strict);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        // Wir prüfen nur, dass nicht mit einem Permit-Fehler abgelehnt wird;
        // ein Sandbox-Setup-Fehler (kein bwrap) ist hier nicht der Punkt.
        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err");

        // Die Ausgabe darf eine Sandbox-Fehlermeldung sein, aber keine
        // Permit-Fehlermeldung.
        match &output {
            ToolOutput::Error { message } => {
                assert!(
                    !message.contains("host execution requires a process permit"),
                    "strict profile must not trigger permit denial: {message:?}"
                );
            }
            ToolOutput::Json { .. } | ToolOutput::Text { .. } => {}
        }
    }

    #[test]
    fn test_provider_with_sandbox_profile_builder() {
        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Strict);
        assert!(provider.sandbox_profile.is_strict());
        assert!(provider.permit_ledger.is_none());
    }

    #[test]
    fn test_provider_with_permit_ledger_builder() {
        let ledger = Arc::new(ProcessPermitLedger::default());
        let provider = ShellToolProvider::default()
            .with_permit_ledger(ledger);
        assert!(provider.permit_ledger.is_some());
    }

    // ── authorize_host_command: gemerkter Permit / mehrere Anträge ─────────

    fn host_executor(
        ledger: &Arc<ProcessPermitLedger>,
        registry: &Arc<HostPermitSessionRegistry>,
    ) -> ShellExecutor {
        ShellExecutor {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Host,
            permit_ledger: Some(Arc::clone(ledger)),
            host_permit_registry: Some(Arc::clone(registry)),
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
        }
    }

    fn args_for(command: &str) -> ShellExecArgs {
        ShellExecArgs {
            command: command.to_owned(),
            timeout_secs: None,
        }
    }

    #[tokio::test]
    async fn test_authorize_host_command_reuses_remembered_permit_after_session_approval_expires() {
        // Die Sitzungszustimmung selbst darf verfallen (kurze TTL), ohne dass
        // ein bereits ausgestellter, gemerkter Permit für exakt denselben
        // Antrag verloren geht: `authorize_host_command` prüft `lookup_permit`
        // zuerst und braucht dann keine erneute Zustimmung.
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1", Duration::from_millis(20));

        let executor = host_executor(&ledger, &registry);
        let args = args_for("echo repeat_me");

        assert!(
            executor.authorize_host_command(&args, &sandbox, "s1").await.is_ok(),
            "first call must succeed via a fresh local-approval issuance"
        );

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            !registry.is_session_approved("s1"),
            "the session-level approval must have expired by now"
        );

        assert!(
            executor.authorize_host_command(&args, &sandbox, "s1").await.is_ok(),
            "an identical repeated request must succeed via the remembered permit, \
             without requiring a fresh session approval"
        );
    }

    #[tokio::test]
    async fn test_authorize_host_command_different_commands_same_session_both_succeed() {
        // Eine einmalige Sitzungszustimmung deckt beliebig viele
        // *unterschiedliche* Befehlstexte derselben Sitzung ab; jeder bekommt
        // seinen eigenen, getrennt gemerkten Permit.
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1", Duration::from_secs(60));

        let executor = host_executor(&ledger, &registry);
        let first = args_for("echo first_command");
        let second = args_for("echo second_command");

        assert!(executor.authorize_host_command(&first, &sandbox, "s1").await.is_ok());
        assert!(executor.authorize_host_command(&second, &sandbox, "s1").await.is_ok());

        let first_request = ProcessPermitRequest {
            session: "s1".to_owned(),
            worker_definition: HOST_WORKER_DEFINITION.to_owned(),
            command: first.command.clone(),
            workspace: sandbox_root(&tmp),
            environment: ProcessEnvironment::LocalHost,
        };
        let second_request = ProcessPermitRequest {
            session: "s1".to_owned(),
            worker_definition: HOST_WORKER_DEFINITION.to_owned(),
            command: second.command.clone(),
            workspace: sandbox_root(&tmp),
            environment: ProcessEnvironment::LocalHost,
        };
        let first_id = registry
            .lookup_permit(&first_request)
            .expect("first command must have a remembered permit");
        let second_id = registry
            .lookup_permit(&second_request)
            .expect("second command must have a remembered permit");
        assert_ne!(
            first_id, second_id,
            "distinct command texts must remember distinct permit ids"
        );
    }

    #[tokio::test]
    async fn test_authorize_host_command_different_session_does_not_reuse_remembered_permit() {
        // Ein für Sitzung `s1` gemerkter Permit darf nicht für eine andere
        // Sitzung `s2` gefunden werden, selbst bei identischem Befehlstext.
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1", Duration::from_secs(60));

        let executor = host_executor(&ledger, &registry);
        let args = args_for("echo shared_command_text");

        assert!(executor.authorize_host_command(&args, &sandbox, "s1").await.is_ok());

        let result = executor.authorize_host_command(&args, &sandbox, "s2").await;
        assert!(
            result.is_err(),
            "session s2 has no approval and must not benefit from session s1's remembered permit"
        );
    }

    // ── authorize_host_command: neue Frage über den Fragekanal ─────────────

    #[tokio::test]
    async fn test_authorize_host_command_prompts_and_grants_single_execution() {
        // Weder gemerkter Permit noch Sitzungsphase, aber ein Kanal ist
        // angehängt: die Anfrage muss fragen und bei Zustimmung durchgehen.
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let args = args_for("echo prompted_ok");

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.expect("prompt must arrive");
            assert_eq!(prompt.session(), "s1");
            assert_eq!(prompt.command(), "echo prompted_ok");
            assert_eq!(prompt.preselected_variant(), HostPermitVariant::SingleExecution);
            assert!(prompt.approve(HostPermitVariant::SingleExecution));
        });

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        responder.await.expect("responder task must not panic");

        assert!(result.is_ok(), "an approved prompt must authorize the command: {result:?}");
        assert!(
            !registry.is_session_approved("s1"),
            "a single-execution approval must never open a session-wide phase"
        );
    }

    #[tokio::test]
    async fn test_authorize_host_command_prompts_and_session_lease_covers_next_call() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let first = args_for("echo first");
        let second = args_for("echo second");

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.expect("prompt must arrive");
            assert!(prompt.approve(HostPermitVariant::SessionLease));
        });

        assert!(executor.authorize_host_command(&first, &sandbox, "s1").await.is_ok());
        responder.await.expect("responder task must not panic");
        assert!(registry.is_session_approved("s1"));

        // Der zweite, abweichende Befehl derselben Sitzung darf ohne erneute
        // Frage durchgehen — die Phase wurde bereits eingetragen.
        assert!(executor.authorize_host_command(&second, &sandbox, "s1").await.is_ok());
    }

    #[tokio::test]
    async fn test_authorize_host_command_prompt_denial_fails_closed() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let args = args_for("rm -rf /");

        tokio::spawn(async move {
            let prompt = receiver.recv().await.expect("prompt must arrive");
            assert!(prompt.deny());
        });

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(result.is_err(), "an explicit denial must fail closed");
        assert!(!registry.is_session_approved("s1"));
    }

    #[tokio::test]
    async fn test_authorize_host_command_without_any_channel_fails_closed() {
        // Kein Ledger/Registry-Zustand und kein Fragekanal: fail-closed ohne
        // dass je etwas gesendet wird (kein Empfänger existiert überhaupt).
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let executor = host_executor(&ledger, &registry);
        let args = args_for("echo should_not_run");

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(
            result.is_err_and(|message| message.contains("requires local UI approval")),
            "no channel attached must fail closed with the UI-approval message"
        );
    }

    #[tokio::test]
    async fn test_authorize_host_command_dropped_prompt_fails_closed() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let args = args_for("echo should_not_run");

        tokio::spawn(async move {
            let _prompt = receiver.recv().await.expect("prompt must arrive");
            // Bewusst ohne Antwort fallengelassen.
        });

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(result.is_err(), "a dropped prompt must fail closed");
    }

    #[tokio::test]
    async fn test_authorize_host_command_timeout_fails_closed() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        executor.host_permit_timeout = Duration::from_millis(20);
        let args = args_for("echo should_not_run");

        let _keep_open = tokio::spawn(async move {
            let prompt = receiver.recv().await.expect("prompt must arrive");
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(prompt);
        });

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(result.is_err(), "an elapsed timeout must fail closed");
    }

    #[tokio::test]
    async fn test_authorize_host_command_closed_channel_fails_closed() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, receiver) = crate::host_permit_prompt::host_permit_prompt_channel();
        drop(receiver);

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let args = args_for("echo should_not_run");

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(
            result.is_err_and(|message| message.contains("requires local UI approval")),
            "a closed receiver must fail closed with the UI-approval message"
        );
    }
}
