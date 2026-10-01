//! Shell-execution tool provider and executor for Harwness.
//!
//! Covers real host execution and `ShellToolProvider::with_host_path`.
//!
//! **Security note**: By default this executor launches `/bin/sh -c` only through a
//! Bubblewrap plan derived from the per-call sandbox; failure to build or spawn that
//! plan rejects execution rather than falling back to the host. The one exception is
//! explicit, pre-approved host execution (Plan Teil B1): when
//! [`SandboxProfile::is_host`] is true and [`ShellExecutor::authorize_host_command`]
//! grants the request, or a [`HostPermitSessionRegistry`] already holds a session
//! lease ([`HostPermitSessionRegistry::is_session_approved`]) or a single-use approval
//! ([`HostPermitSessionRegistry::take_single_use`]) for the calling session, the
//! command runs directly on the host (`/bin/sh -c`, no `bwrap`) with the harness's own
//! inherited environment (no `env_clear`) and the sandbox's canonical workspace root as
//! `cwd`. There is no other path to host execution for model calls: every other
//! combination of profile/registry state still requires a successful Bubblewrap plan,
//! unchanged from before. Runde 6, Teil B: der Operator-Weg (`operator`,
//! [`run_operator_command`]) ist kein Modell-Werkzeug — er führt nur die von der
//! Nutzerin selbst getippten `!`-Befehle der TUI auf dem Host aus.
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
//! - Host-Ausführung ([`ShellExecutor::run_host_command`], Plan Teil B1) läuft, wenn
//!   `setsid` (util-linux) auffindbar ist, zusätzlich über `setsid --wait /bin/sh -c
//!   <command>` statt `/bin/sh -c <command>` direkt: das löst den Befehl aus harws
//!   Sitzung und Controlling-Terminal ([`host_shell_argv`], [`resolve_setsid`]) —
//!   ein Kind, das `/dev/tty` öffnet (git, Pager,
//!   Fortschrittsbalken), kann dann nicht mehr die Terminalmodi der harw-TUI
//!   (z. B. Maus-Reporting) verändern. Ohne `setsid` bleibt der bisherige Pfad
//!   unverändert (`tracing::debug!` einmalig).

use crate::capture::{BoundedCapture, DrainEnd};
use crate::host_permit_prompt::{HostPermitPrompt, HostPermitPromptSender, HostPermitVariant};
use crate::limits::{ShellLimits, ShellLimitsError, launch_command};
use harw_authority::{Permission, SandboxSpec};
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_sandbox::{
    BwrapLauncher, HostApprovalScope, HostPathBinding, HostPermitSessionRegistry,
    ProcessEnvironment, ProcessPermitLedger, ProcessPermitRequest, SandboxProfile,
};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use harw_types::cancel::CancelToken;
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt, io,
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::process::{Child, Command as TokioCommand};
use tracing::{debug, info, warn};

// Runde 5, Teil N: Host-Mode-Anfrage (`request_host`) und Sandbox-Hinweis;
// umhüllt jeden ausgegebenen `ShellExecutor`.
mod escalation;
// Runde 5, Teil N (Folgeauftrag): Zeitlimit per Argument, Build-Vorgabe,
// Obergrenze `[shell] max_timeout_secs`, klare Zeitablauf-Meldung.
mod timeouts;
pub use timeouts::{BUILD_COMMAND_DEFAULT_TIMEOUT_SECS, DEFAULT_MAX_TIMEOUT_SECS};
// Runde 6, Teil B: `!`-Befehle der Nutzerin laufen immer auf dem Host
// (ohne bwrap und ohne Freigabe, mit denselben Bausteinen wie
// `run_host_command`).
mod operator;
// Plan R9, Teil F: derselbe Rechte-/Freigabeweg für Hintergrund-Jobs
// (`job.start` in `harw-tool-job`), ohne Wanduhr-Zeitlimit.
mod background;
pub use background::BackgroundLaunch;
// Android-Anbindung: einzige `cfg!(target_os = "android")`-Abfrage des
// Crates, dazu die reine `host_policy`-Entscheidungsfunktion.
mod platform;
pub use operator::{
    OPERATOR_DEFAULT_TIMEOUT_SECS, OperatorCommand, OperatorEnd, OperatorOutcome,
    operator_escalation_message, run_operator_command,
};
pub use platform::ExecPlatform;

// ── Constants ─────────────────────────────────────────────────────────────────

const TOOL_NAME: &str = "shell.exec";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const TRUNCATION_MARKER: &str = "\n[...truncated...]";
/// Obergrenze für das Einsammeln des Exit-Status nach SIGKILL. `kill_on_drop` bleibt
/// als Rückfallebene, falls der Kernel den Prozess nicht rechtzeitig freigibt.
const KILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// Feste Suchpfade für `setsid` (util-linux), geprüft vor der `PATH`-Suche in
/// [`find_setsid`]. Anders als [`crate::limits::PRLIMIT_CANDIDATES`] fällt die
/// Suche zusätzlich auf `PATH` zurück ([`std::env::split_paths`]): `setsid`
/// dient nur der Sitzungs-Trennung des Host-Befehls von harws eigenem
/// Terminal, nicht der Durchsetzung sicherheitskritischer Grenzen wie
/// `bwrap`/`prlimit`, für die `PATH` bewusst nie ausgewertet wird.
const SETSID_FIXED_CANDIDATES: [&str; 2] = ["/usr/bin/setsid", "/bin/setsid"];
/// Einzige heute existierende Worker-Definition, die [`SandboxProfile::Host`]
/// aktiviert (siehe `harw-registry-defaults/agents/host-process-worker.toml`).
/// Sobald ein zweiter Host-fähiger Worker entsteht, muss dieser Konstante ein
/// echtes, aus der Worker-Konfiguration gespeistes Feld auf
/// [`ShellToolProvider`]/[`ShellExecutor`] folgen.
const HOST_WORKER_DEFINITION: &str = "host-process-worker@1";
/// Gültigkeitsdauer eines frisch über eine beantwortete
/// [`HostPermitVariant::SingleExecution`]-Frage ausgestellten Permits, bis
/// der genehmigte Auftrag tatsächlich läuft: eine Einzelfreigabe soll nicht
/// als lange gültiges „stilles Ja" liegen bleiben. Eine Host-Arbeitsphase
/// ([`HostPermitVariant::SessionLease`]) hat dagegen **keine** Ablaufzeit —
/// weder in der [`HostPermitSessionRegistry`] noch für ihre Ledger-Permits
/// (`ttl = None`): sie endet nur, wenn der Nutzer sie beendet (Strg+H oder
/// `/sandbox-lease revoke`, Nutzerentscheidung 2026-09-24).
const HOST_SINGLE_EXECUTION_TTL: Duration = Duration::from_secs(5 * 60);
/// Vorgabe-Wartezeit auf **eine** Nutzerentscheidung auf eine offene
/// [`HostPermitPrompt`]. Läuft sie ab, gilt das als Ablehnung
/// (fail-closed) — deckt sich mit
/// `harw_tui::host_permit_dialog::DEFAULT_HOST_PERMIT_TIMEOUT`. Öffentlich,
/// weil `harw-ops`'
/// `sandbox-lease`-Operation dieselbe Wartezeit für ihre eigene
/// [`HostPermitPrompt`] verwendet.
pub const HOST_PERMIT_PROMPT_TIMEOUT: Duration = Duration::from_secs(300);
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
/// Ablehnungsnachricht auf Plattformen ohne Sandbox (Android/Termux), wenn
/// weder eine laufende Host-Arbeitsphase noch eine Einmalfreigabe vorliegt
/// und kein Fragekanal angehängt ist: es gibt hier keinen Sandbox-Rückfall,
/// also bleibt nur die Ablehnung (fail-closed).
const NO_SANDBOX_NO_APPROVAL_MSG: &str = "shell.exec: this platform has no sandbox (bwrap/user namespaces unavailable); host \
     execution needs your approval — once or for a host work phase (end it with Ctrl+H) — but \
     no approval channel is attached here (fail-closed)";

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
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
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
    /// Host-PATH-Bindung für den `bwrap`-Sandbox-Pfad (Plan Teil C2). Nur
    /// wirksam, solange `run_command` tatsächlich einen `bwrap`-Plan baut —
    /// der Host-Pfad (Plan Teil B1) ignoriert dieses Feld, weil dort ohnehin
    /// kein `bwrap` läuft und die Umgebung bereits vollständig geerbt wird.
    host_path: Option<HostPathBinding>,
    /// Plattform, auf der dieser Build läuft (Android-Anbindung). Bestimmt
    /// zusammen mit [`Self::sandbox_profile`] und [`Self::approval_mode`]
    /// über [`platform::host_policy`], welcher der bestehenden Host-Wege in
    /// [`Self::determine_effective_host`] gilt. Vorgabe
    /// [`ExecPlatform::current`]; auf [`ExecPlatform::Sandboxed`] (Linux und
    /// jedes andere Nicht-Android-Ziel) bleibt jedes Verhalten unverändert.
    exec_platform: ExecPlatform,
    /// Laufzeit-lebendige Freigabemodus-Zelle
    /// ([`harw_extension_api::approval_mode::ApprovalModeCell`]).
    /// Nur auf [`ExecPlatform::NoSandbox`] gelesen: dort erlaubt
    /// [`harw_extension_api::ApprovalMode::FullAccess`] Host-Ausführung ohne
    /// Rückfrage. `None` verhält sich wie jeder andere Modus (Rückfrage
    /// nötig). Auf [`ExecPlatform::Sandboxed`] wirkungslos.
    approval_mode: Option<ApprovalModeCell>,
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
                None,
            );
        }

        self.prompt_for_authorization(ledger, registry, request)
            .await
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
            registry.mark_session_approved(request.session.clone());
            // Eine Host-Arbeitsphase gilt prozessweit — wie `/sandbox-lease`
            // und die Host-Mode-Anfrage (`exec/escalation.rs`). Vorher galt
            // sie nur für die fragende (Kind-)Sitzung: jedes neue
            // `uia-shell-worker`-Kind fragte erneut und die TUI meldete
            // „Host-Arbeitsphase freigegeben“ jedes Mal wieder. Strg+H
            // (`ChatApp::end_host_mode`) widerruft die globale Freigabe.
            registry.mark_global_approval();
        }
        let (scope, ttl) = match variant {
            HostPermitVariant::SingleExecution => (
                HostApprovalScope::SingleExecution,
                Some(HOST_SINGLE_EXECUTION_TTL),
            ),
            // Kein Zeitablauf: gilt bis Strg+H / `/sandbox-lease revoke`.
            HostPermitVariant::SessionLease => (HostApprovalScope::SessionLease, None),
        };
        Self::issue_and_remember(ledger, registry, request, scope, ttl)
    }

    /// Stellt einen Permit für `request` aus, merkt ihn in `registry` und
    /// autorisiert ihn sofort für den auslösenden Aufruf. `ttl = None`: der
    /// Permit gilt bis zum Widerruf (Host-Arbeitsphase).
    fn issue_and_remember(
        ledger: &Arc<ProcessPermitLedger>,
        registry: &Arc<HostPermitSessionRegistry>,
        request: ProcessPermitRequest,
        scope: HostApprovalScope,
        ttl: Option<Duration>,
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

    /// Determines whether this call runs directly on the host without `bwrap`
    /// (Plan Teil B1).
    ///
    /// # Description
    /// `true` in exactly three cases, in this order (a session lease is
    /// checked before a single-use approval so the latter is only consumed
    /// when no session lease already covers the call — the one-shot approval
    /// must not be spent for nothing):
    /// 1. [`Self::sandbox_profile`] is [`SandboxProfile::Host`]: delegates to
    ///    [`Self::authorize_host_command`] (unchanged ledger/prompt flow).
    /// 2. A [`HostPermitSessionRegistry`] is attached and
    ///    [`HostPermitSessionRegistry::is_session_approved`] is `true` for
    ///    `session_id` — an active `/sandbox-lease` session lease, checked
    ///    without touching the ledger and without a renewed prompt.
    /// 3. A registry is attached and
    ///    [`HostPermitSessionRegistry::take_single_use`] atomically consumes a
    ///    pending single-use approval for `session_id`.
    ///
    /// Every other combination — no registry attached, or neither approval
    /// present — returns `false`: the caller then falls through to the
    /// unchanged Bubblewrap path.
    ///
    /// Plattform-Android-Anbindung: auf [`ExecPlatform::Sandboxed`] (Linux und
    /// jedes andere Nicht-Android-Ziel) ist dies weiterhin die einzige
    /// beschriebene Verzweigung — [`platform::host_policy`] ordnet Host-Profil
    /// stets [`platform::HostPolicy::RequireApproval`] und jedes andere Profil
    /// stets [`platform::HostPolicy::SandboxUnlessLease`] zu, unabhängig vom
    /// Freigabemodus. Auf [`ExecPlatform::NoSandbox`] (Android/Termux, keine
    /// Bubblewrap-Sandbox) gibt es diese Fälle zusätzlich:
    /// - [`platform::HostPolicy::HostWithoutPrompt`]:
    ///   [`harw_extension_api::ApprovalMode::FullAccess`] erlaubt
    ///   Host-Ausführung ohne Rückfrage (einzige Stelle, die das je tut) —
    ///   quittiert mit einem [`tracing::info!`]-Audit-Eintrag.
    /// - [`platform::HostPolicy::RequireApproval`] (jeder andere
    ///   Freigabemodus): erst eine laufende Sitzungsphase oder Einmalfreigabe
    ///   der Registry, sonst — nur wenn Ledger, Registry **und** Fragekanal
    ///   alle angehängt sind — dieselbe Rückfrage wie im Host-Profil
    ///   ([`Self::authorize_host_command`]); ohne einen vollständigen Kanal
    ///   wird sofort abgelehnt (fail-closed, kein 300-Sekunden-Warten auf
    ///   niemanden).
    ///
    /// # Errors
    /// Returns the same `Err(String)` as [`Self::authorize_host_command`]
    /// when the profile is [`SandboxProfile::Host`] and authorization is
    /// denied (fail-closed; propagated to the caller as a `ToolOutput::error`
    /// without falling back to any other path). On [`ExecPlatform::NoSandbox`]
    /// without a complete approval channel, returns
    /// [`NO_SANDBOX_NO_APPROVAL_MSG`] instead.
    async fn determine_effective_host(
        &self,
        args: &ShellExecArgs,
        sandbox: &SandboxSpec,
        session_id: &str,
    ) -> Result<bool, String> {
        let mode = self.approval_mode.as_ref().map(ApprovalModeCell::get);
        match platform::host_policy(self.exec_platform, self.sandbox_profile.is_host(), mode) {
            platform::HostPolicy::SandboxUnlessLease => {
                Ok(self.host_permit_registry.as_ref().is_some_and(|registry| {
                    registry.is_session_approved(session_id) || registry.take_single_use(session_id)
                }))
            }
            platform::HostPolicy::HostWithoutPrompt => {
                info!(
                    session_id,
                    exec_platform = ?self.exec_platform,
                    "shell.exec: no sandbox on this platform; ApprovalMode::FullAccess grants \
                     host execution without prompt"
                );
                Ok(true)
            }
            platform::HostPolicy::RequireApproval if self.sandbox_profile.is_host() => {
                self.authorize_host_command(args, sandbox, session_id)
                    .await?;
                Ok(true)
            }
            platform::HostPolicy::RequireApproval => {
                let covered = self.host_permit_registry.as_ref().is_some_and(|registry| {
                    registry.is_session_approved(session_id) || registry.take_single_use(session_id)
                });
                if covered {
                    return Ok(true);
                }
                // Nur fragen, wenn Ledger, Registry und Fragekanal alle
                // angehängt sind — sonst hört niemand zu und ein Warten auf
                // die Antwort wäre sinnlos (fail-closed, siehe Moduldoku).
                let has_channel = self.permit_ledger.is_some()
                    && self.host_permit_registry.is_some()
                    && self.host_permit_prompts.is_some();
                if has_channel {
                    self.authorize_host_command(args, sandbox, session_id)
                        .await?;
                    Ok(true)
                } else {
                    Err(NO_SANDBOX_NO_APPROVAL_MSG.to_owned())
                }
            }
        }
    }

    /// Executes the shell command described by `args` in the given sandbox.
    ///
    /// # Description
    /// Determines the effective host state via
    /// [`Self::determine_effective_host`] (Plan Teil B1) and dispatches to
    /// exactly one of two paths:
    /// - effective host: [`Self::run_host_command`] — `/bin/sh -c` runs
    ///   directly, no `bwrap`.
    /// - otherwise (unchanged): resolves the pinned `bwrap`/`prlimit`
    ///   binaries, builds a Bubblewrap plan (with [`Self::host_path`] applied
    ///   if set — Plan Teil C2), and hands the resulting `TokioCommand` to
    ///   [`Self::spawn_and_collect`].
    ///
    /// Both paths share spawn, drain, timeout, cancel-race, and process
    /// termination through [`Self::spawn_and_collect`]; only the
    /// `TokioCommand` construction differs.
    ///
    /// If `cancel` is `Some`, both waits inside [`Self::spawn_and_collect`]
    /// (draining stdout/stderr, and waiting for the exit status after EOF)
    /// additionally race the token's [`CancelToken::cancelled`] future. A
    /// cancellation hit kills the process tree via the existing [`terminate`]
    /// function (the same SIGKILL path timeout and output-limit overflow
    /// already use) and returns [`ToolsError::Cancelled`] instead of a
    /// timeout `ToolOutput`. With `cancel = None` both waits behave exactly
    /// as before (plain `timeout_at`, no race).
    ///
    /// # Errors
    /// Returns `Ok(ToolOutput::error(...))` for denied-permission, missing sandbox binaries,
    /// timeout, or spawn failure — callers are not expected to match on `Err` for these cases.
    /// Returns `Err(ToolsError::Cancelled)` when `cancel` fires before the command
    /// completes. Returns `Err(ToolsError)` otherwise only for argument parsing failures.
    ///
    /// # Concurrency
    /// Safe to call from any async context. Timeout, cancellation, and output overflow
    /// all kill and reap the child explicitly via [`terminate`]; `kill_on_drop(true)`
    /// still covers a dropped future.
    ///
    /// # Panics
    /// None in production paths.
    async fn run_command(
        &self,
        args: &ShellExecArgs,
        sandbox: &SandboxSpec,
        session_id: &str,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolsError> {
        let effective_timeout = self.effective_timeout(args)?;

        let effective_host = match self
            .determine_effective_host(args, sandbox, session_id)
            .await
        {
            Ok(effective_host) => effective_host,
            Err(message) => {
                warn!(
                    session_id,
                    "shell.exec denied: host permit authorization failed"
                );
                return Ok(ToolOutput::error(message));
            }
        };

        if effective_host {
            return self
                .run_host_command(args, sandbox, effective_timeout, session_id, cancel)
                .await;
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
            Ok(launcher) => {
                let launcher = launcher
                    .with_tmpfs_size(tmpfs_size)
                    .with_profile(&self.sandbox_profile);
                match &self.host_path {
                    // Plan Teil C2: bindet den zsh-PATH (und ggf. RUSTUP_HOME/CARGO_HOME)
                    // in den bwrap-Plan, statt der hermetischen Minimal-PATH. Ohne
                    // `with_host_path` (kein Aufruf hier) bleibt der Plan unverändert.
                    Some(binding) => launcher.with_host_path(binding.clone()),
                    None => launcher,
                }
            }
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
        // Runde 5, Teil N: RLIMIT_CPU wächst mit dem gewährten Zeitlimit.
        let limits = timeouts::limits_for(self.limits, effective_timeout);
        let launch = launch_command(
            prlimit.as_deref(),
            &limits,
            launcher.executable(),
            plan.args(),
        );
        let mut command = TokioCommand::new(&launch.program);
        command.args(&launch.args);

        self.spawn_and_collect(
            &mut command,
            "Bubblewrap",
            effective_timeout,
            session_id,
            cancel,
            false,
        )
        .await
    }

    /// Runs `args.command` directly on the host, without `bwrap` (Plan Teil
    /// B1). Bubblewrap is skipped entirely for this call.
    ///
    /// # Description
    /// Only reached once [`Self::run_command`] has already established via
    /// [`Self::determine_effective_host`] that a valid approval exists (Host
    /// profile via [`Self::authorize_host_command`], or a session/single-use
    /// approval from the [`HostPermitSessionRegistry`] for any other
    /// profile) — this method performs no authorization check itself.
    ///
    /// Builds `/bin/sh -c <command>` — or, if `setsid` (util-linux) is
    /// resolvable via [`resolve_setsid`], `setsid --wait /bin/sh -c <command>`
    /// via [`host_shell_argv`] — with `current_dir` set to the sandbox's
    /// canonical workspace root, the harness's own environment fully
    /// inherited (no `env_clear`, unlike the `bwrap` path), and stdin
    /// `/dev/null`. Process-group/session isolation depends on whether
    /// `setsid` was found: without it, `process_group(0)` isolates the host
    /// command from harw's own process group (e.g. terminal signals), same as
    /// before; with it, `process_group(0)` is deliberately **not** set so
    /// `setsid` execs `/bin/sh` in place instead of forking — see the `//`
    /// comment at the `process_group` call site for the full reasoning and
    /// why [`terminate`] still kills the whole command tree unmodified in
    /// both cases. `prlimit` limits are applied through the same
    /// [`crate::limits::launch_command`] mechanism the `bwrap` path uses
    /// ([`Self::resolve_limits`]); if `prlimit` is unavailable and
    /// `require_rlimits == false`, the host command runs deliberately without
    /// rlimits, exactly like the `bwrap` path's fallback (only the tmpfs
    /// limit does not apply here, because there is no `bwrap` `/tmp` in the
    /// host path).
    ///
    /// Timeout, cancel-race, output truncation and [`terminate`] all run
    /// through [`Self::spawn_and_collect`] — the same helper the `bwrap` path
    /// uses. The result is marked `"executed_on": "host"` in the JSON output
    /// (and a `[host] ` prefix on a timeout error) so callers can tell host
    /// execution apart from the sandboxed path; the sandboxed path's output
    /// shape is unchanged.
    ///
    /// # Errors
    /// See [`Self::run_command`].
    async fn run_host_command(
        &self,
        args: &ShellExecArgs,
        sandbox: &SandboxSpec,
        effective_timeout: u64,
        session_id: &str,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolsError> {
        debug!(
            command_len = args.command.len(),
            cwd = %sandbox.workspace().canonical_root().display(),
            timeout_secs = effective_timeout,
            "shell.exec preparing host process"
        );

        // Nur der `prlimit`-Teil von `resolve_limits` ist hier relevant: die tmpfs-
        // Größe gilt ausschließlich für den bwrap-Plan (kein bwrap läuft hier), wird
        // aber weiterhin mitvalidiert, damit dieselbe `ShellLimits::validate`-Regel wie
        // im bwrap-Pfad gilt. Android-Anbindung: auf `ExecPlatform::NoSandbox` (Termux)
        // ist `prlimit` typischerweise gar nicht paketiert und es gibt keinen
        // Sandbox-Rückfall, den `require_rlimits` sonst schützt — dort ist ein
        // fehlendes `prlimit` deshalb bewusst kein Fehler ([`Self::resolve_host_limits`]).
        // Auf `ExecPlatform::Sandboxed` bleibt `require_rlimits` unverändert wirksam.
        let (_tmpfs_size, prlimit) = match self.resolve_host_limits() {
            Ok(resolved) => resolved,
            Err(err) => {
                let err = ShellExecError::ResourceLimits(err);
                warn!(error = %err, "shell.exec resource limits unavailable");
                return Ok(ToolOutput::error(err.to_string()));
            }
        };
        if prlimit.is_none() {
            warn!(
                "shell.exec (host) runs WITHOUT rlimits: prlimit missing and require_rlimits=false"
            );
        }

        let setsid = resolve_setsid();
        let shell = resolve_host_shell(self.exec_platform);
        let (program, shell_args) = host_shell_argv_with_shell(setsid, &shell, &args.command);
        // Runde 5, Teil N: RLIMIT_CPU wächst mit dem gewährten Zeitlimit.
        let limits = timeouts::limits_for(self.limits, effective_timeout);
        let launch = launch_command(prlimit.as_deref(), &limits, &program, &shell_args);

        let mut command = TokioCommand::new(&launch.program);
        command.args(&launch.args);
        command.current_dir(sandbox.workspace().canonical_root());
        // Prozessgruppen-/Sitzungs-Isolation des Host-Befehls von harws eigenem
        // Terminal. Zwei Fälle, je nachdem ob `setsid` gefunden wurde
        // (`host_shell_argv`):
        //
        // - MIT setsid: `process_group(0)` wird hier BEWUSST NICHT gesetzt. Das
        //   util-linux-`setsid` forkt nur dann einen Enkelprozess (und wartet
        //   dank `--wait` auf ihn), wenn es selbst bereits Prozessgruppenführer
        //   ist (`getpgrp() == getpid()`). Ohne `process_group(0)` erbt der
        //   direkte Kindprozess (der spätere `setsid`) harws Prozessgruppe
        //   (deren pgid == harws eigene PID ist, nicht die des Kindes) — er ist
        //   also KEIN Gruppenführer. `setsid` ruft daraufhin `setsid(2)` auf
        //   sich selbst auf und `exec`t `/bin/sh` an derselben PID weiter, statt
        //   zu forken: diese eine PID wird Sitzungs- UND Gruppenführer einer
        //   neuen, von harws Terminal vollständig gelösten Sitzung (kein
        //   Controlling-Terminal mehr — `open("/dev/tty")` scheitert dort mit
        //   ENXIO, das eigentliche Ziel dieses Fixes). `terminate()` killt
        //   unverändert genau diese eine PID (`child.start_kill()`), die damit
        //   weiterhin die Spitze des gesamten Kommandobaums ist — kein Änderung
        //   an `terminate()` nötig.
        //   Würde hier stattdessen `process_group(0)` gesetzt, wäre der direkte
        //   Kindprozess bereits Gruppenführer, `setsid` würde also forken und
        //   (dank `--wait`) auf den Enkel warten; `terminate()` träfe dann nur
        //   den wartenden Elternprozess, während `/bin/sh` in seiner eigenen,
        //   neuen Sitzung als Waise weiterliefe — exakt das Leck, das dieser Fix
        //   beheben soll. Deshalb bewusst vermieden.
        // - OHNE setsid (Fallback, unverändertes Verhalten): `process_group(0)`
        //   wie bisher, trennt den Host-Befehl zumindest von harws eigener
        //   Prozessgruppe (keine Sitzungs-Trennung, `/dev/tty` bleibt erreichbar).
        if setsid.is_none() {
            command.process_group(0);
        }
        // Umgebung wird bewusst NICHT gecleart (kein `env_clear`): der Host-Pfad erbt
        // den vollen zsh-Kontext des Nutzers (PATH/HOME/CARGO_HOME/…), im Unterschied
        // zum hermetischen bwrap-Pfad.

        self.spawn_and_collect(
            &mut command,
            "host shell",
            effective_timeout,
            session_id,
            cancel,
            true,
        )
        .await
    }

    /// Spawns an already-configured, not-yet-started `TokioCommand`, drains
    /// stdout/stderr together under one byte budget, applies the timeout and
    /// optional cancel-race, and returns the finished `ToolOutput`. Shared
    /// core for the sandboxed (`bwrap`) and host (`/bin/sh -c`, no `bwrap`)
    /// paths (Plan Teil B1) — only the `command` construction differs
    /// between [`Self::run_command`] and [`Self::run_host_command`].
    ///
    /// # Errors
    /// Returns `Ok(ToolOutput::error(...))` for spawn failure, missing
    /// stdio pipes, I/O errors, or timeout. Returns
    /// `Err(ToolsError::Cancelled)` when `cancel` fires before completion —
    /// the process tree is killed via [`terminate`] before this is returned.
    ///
    /// # Concurrency
    /// Timeout, cancellation, and output overflow all kill and reap the
    /// child explicitly via [`terminate`]; `kill_on_drop(true)` still covers
    /// a dropped future.
    async fn spawn_and_collect(
        &self,
        command: &mut TokioCommand,
        spawn_error_context: &str,
        effective_timeout: u64,
        session_id: &str,
        cancel: Option<&CancelToken>,
        executed_on_host: bool,
    ) -> Result<ToolOutput, ToolsError> {
        let mut child = match configure_stdio(command).spawn() {
            Ok(child) => child,
            Err(err) => {
                warn!(error = %err, "shell.exec spawn failed");
                return Ok(ToolOutput::error(format!(
                    "shell.exec: failed to spawn {spawn_error_context}: {err}"
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
        let drained = match cancel {
            Some(cancel) => {
                tokio::select! {
                    result = tokio::time::timeout_at(deadline, capture.drain(&mut stdout, &mut stderr)) => result,
                    () = cancel.cancelled() => {
                        terminate(&mut child).await;
                        info!(session_id, "shell.exec cancelled while draining output");
                        return Err(ToolsError::Cancelled);
                    }
                }
            }
            None => {
                tokio::time::timeout_at(deadline, capture.drain(&mut stdout, &mut stderr)).await
            }
        };

        let status = match drained {
            Err(_elapsed) => {
                terminate(&mut child).await;
                warn!(timeout_secs = effective_timeout, "shell.exec timed out");
                return Ok(self.timeout_output(effective_timeout, &capture, executed_on_host));
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
                return Ok(self.completed_output(status, &capture, true, executed_on_host));
            }
            Ok(Ok(DrainEnd::Eof)) => {
                let waited = match cancel {
                    Some(cancel) => {
                        tokio::select! {
                            result = tokio::time::timeout_at(deadline, child.wait()) => result,
                            () = cancel.cancelled() => {
                                terminate(&mut child).await;
                                info!(session_id, "shell.exec cancelled while waiting for exit");
                                return Err(ToolsError::Cancelled);
                            }
                        }
                    }
                    None => tokio::time::timeout_at(deadline, child.wait()).await,
                };
                match waited {
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
                        return Ok(self.timeout_output(
                            effective_timeout,
                            &capture,
                            executed_on_host,
                        ));
                    }
                }
            }
        };

        Ok(self.completed_output(Some(status), &capture, false, executed_on_host))
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

    /// Wie [`Self::resolve_limits`], aber für den Host-Pfad
    /// ([`Self::run_host_command`], [`ShellToolProvider::prepare_background_launch`]):
    /// auf [`ExecPlatform::NoSandbox`] gilt ein fehlendes `prlimit` als
    /// erwartbar (Android/Termux paketiert es typischerweise nicht) und wird
    /// nie zum Fehler, unabhängig von [`ShellLimits::require_rlimits`] — es
    /// gibt dort keinen Sandbox-Rückfall, den die strenge Vorgabe sonst
    /// schützt. Die übrigen Grenzwerte (`as_bytes`, `cpu_secs`, …) werden
    /// unverändert geprüft; eine echte Fehlkonfiguration (z. B. `nofile = 0`)
    /// bleibt also weiterhin ein Fehler. Auf [`ExecPlatform::Sandboxed`]
    /// verhält sich dies byte-identisch zu [`Self::resolve_limits`].
    fn resolve_host_limits(&self) -> Result<(NonZeroU64, Option<PathBuf>), ShellLimitsError> {
        match self.exec_platform {
            ExecPlatform::Sandboxed => self.resolve_limits(),
            ExecPlatform::NoSandbox => {
                let limits = ShellLimits {
                    require_rlimits: false,
                    ..self.limits
                };
                limits.validate()?;
                let tmpfs_size = limits.tmpfs_size()?;
                let prlimit = limits.resolve_prlimit()?;
                Ok((tmpfs_size, prlimit))
            }
        }
    }

    /// JSON-Ergebnis eines beendeten (oder wegen Ausgabeüberlauf getöteten) Prozesses.
    ///
    /// `killed_by_output_limit` ist der Kürzungshinweis für den Aufrufer: Die Ausgabe ist
    /// gekappt **und** das Kommando lief nicht zu Ende; `exit_code` ist dann `-1` (Signal).
    /// `executed_on_host` fügt (nur wenn `true`, Plan Teil B1) das Feld
    /// `"executed_on": "host"` hinzu — die bestehende Sandbox-Ausgabe (`false`) bleibt
    /// byte-identisch zu vorher, ohne dieses Feld.
    fn completed_output(
        &self,
        status: Option<ExitStatus>,
        capture: &BoundedCapture,
        killed_by_output_limit: bool,
        executed_on_host: bool,
    ) -> ToolOutput {
        let exit_code = status.and_then(|status| status.code()).unwrap_or(-1);
        let (stdout_str, stderr_str, truncated) = Self::truncate_combined_output(
            capture.stdout(),
            capture.stderr(),
            self.max_output_bytes,
        );
        let truncated = truncated || killed_by_output_limit;

        info!(
            exit_code,
            truncated, killed_by_output_limit, executed_on_host, "shell.exec completed"
        );

        let mut output = json!({
            "exit_code": exit_code,
            "stdout": stdout_str,
            "stderr": stderr_str,
            "truncated": truncated,
            "killed_by_output_limit": killed_by_output_limit,
        });
        if executed_on_host {
            output["executed_on"] = json!("host");
        }
        ToolOutput::json(output)
    }

    /// Fehlerergebnis bei Timeout, das die bis dahin gelesene Teilausgabe (im selben
    /// Byte-Budget gekürzt) mitliefert. `executed_on_host` stellt (nur wenn `true`,
    /// Plan Teil B1) ein `[host] `-Präfix voran; die bestehende Sandbox-Meldung
    /// (`false`) bleibt unverändert.
    fn timeout_output(
        &self,
        timeout_secs: u64,
        capture: &BoundedCapture,
        executed_on_host: bool,
    ) -> ToolOutput {
        let (stdout, stderr, truncated) = Self::truncate_combined_output(
            capture.stdout(),
            capture.stderr(),
            self.max_output_bytes,
        );
        let prefix = if executed_on_host { "[host] " } else { "" };
        ToolOutput::error(format!(
            "{prefix}shell.exec timed out after {timeout_secs}s; process tree killed. \
             Partial output (truncated: {truncated}):\n[stdout]\n{stdout}\n[stderr]\n{stderr}"
        ))
    }
}

/// Löst `setsid` (util-linux) einmalig pro Prozess auf und merkt das Ergebnis.
///
/// # Description
/// Prüft zuerst [`SETSID_FIXED_CANDIDATES`], danach jeden Eintrag von `PATH`
/// ([`find_setsid_in_path`]). `None` wird genau einmal mit
/// [`tracing::debug!`] begründet — dank [`OnceLock`] läuft die Suche (und
/// damit auch das Log) nur beim ersten Aufruf.
///
/// # Returns
/// `Some(path)` zum ersten gefundenen ausführbaren `setsid`, sonst `None`.
///
/// # Concurrency
/// `Send + Sync`; sicher von mehreren Tasks gleichzeitig aufrufbar, die
/// zugrundeliegende Suche läuft dank `OnceLock` nur einmal.
pub(crate) fn resolve_setsid() -> Option<&'static Path> {
    static SETSID_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    SETSID_PATH.get_or_init(find_setsid).as_deref()
}

/// Sucht `setsid` an [`SETSID_FIXED_CANDIDATES`], dann in `PATH`. Reine
/// Auflösungslogik ohne `OnceLock`-Caching, damit [`resolve_setsid`] die
/// Suche über `get_or_init` einmalig anstoßen kann.
fn find_setsid() -> Option<PathBuf> {
    let fixed = SETSID_FIXED_CANDIDATES.map(Path::new);
    if let Some(found) = find_setsid_in(&fixed) {
        return Some(found);
    }
    match find_setsid_in_path() {
        Some(found) => Some(found),
        None => {
            debug!(
                "shell.exec: setsid not found (fixed paths or PATH); host commands stay in \
                 harw's own session (no /dev/tty isolation from this run)"
            );
            None
        }
    }
}

/// Prüft `candidates` der Reihe nach und liefert den ersten, der eine
/// ausführbare reguläre Datei ist. Kein `PATH`-Zugriff — reine
/// Kandidatenliste, deshalb ohne Spawn unit-testbar (z. B. mit einem
/// garantiert nicht existierenden Pfad).
fn find_setsid_in(candidates: &[&Path]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|candidate| is_executable_file(candidate))
        .map(|candidate| candidate.to_path_buf())
}

/// Durchsucht `PATH` (in Reihenfolge) nach einer ausführbaren `setsid`-Datei.
/// Fehlt `PATH` oder ist es leer, liefert dies `None`.
fn find_setsid_in_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("setsid"))
        .find(|candidate| is_executable_file(candidate))
}

/// `true`, wenn `path` eine reguläre Datei mit mindestens einem
/// Ausführ-Bit (owner/group/other) ist. Ein fehlender Pfad oder ein
/// `stat`-Fehler zählt als `false`, nie als Panic.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Baut das argv für den Host-Shell-Start (Plan Teil B1, Sitzungs-Trennung).
///
/// # Description
/// Mit `setsid`: `<setsid> --wait /bin/sh -c <command>` — `--wait` lässt
/// `setsid` blockieren, bis das gestartete Programm beendet ist, und gibt
/// dessen Exit-Status weiter, sodass [`ShellExecutor::spawn_and_collect`]
/// Timing und Exit-Code unverändert erhält. Ohne `setsid`: `/bin/sh -c
/// <command>`, byte-identisch zum bisherigen Verhalten.
///
/// Reine Funktion ohne Prozessstart — unit-testbar ohne Spawn.
///
/// # Returns
/// `(program, args)`: absoluter Programmpfad und die vollständige
/// Argumentliste (ohne `program` selbst), in der Reihenfolge, in der sie an
/// `TokioCommand::args` übergeben werden.
fn host_shell_argv(setsid: Option<&Path>, command: &str) -> (PathBuf, Vec<OsString>) {
    host_shell_argv_with_shell(setsid, Path::new("/bin/sh"), command)
}

/// Wie [`host_shell_argv`], aber mit einem explizit aufgelösten Shell-Pfad
/// statt des fest verdrahteten `/bin/sh` (Android-Anbindung, siehe
/// [`resolve_host_shell`]). `host_shell_argv` bleibt für Aufrufer, die
/// weiterhin ausschließlich `/bin/sh` meinen (z. B. `exec::operator`, dessen
/// `!`-Befehle unverändert nur auf gewöhnlichem Linux/Host laufen), byte-
/// identisch zu vorher.
fn host_shell_argv_with_shell(
    setsid: Option<&Path>,
    shell: &Path,
    command: &str,
) -> (PathBuf, Vec<OsString>) {
    match setsid {
        Some(setsid) => (
            setsid.to_path_buf(),
            vec![
                OsString::from("--wait"),
                shell.as_os_str().to_owned(),
                OsString::from("-c"),
                OsString::from(command),
            ],
        ),
        None => (
            shell.to_path_buf(),
            vec![OsString::from("-c"), OsString::from(command)],
        ),
    }
}

/// Löst den Host-Shell-Pfad auf.
///
/// # Description
/// Auf [`ExecPlatform::Sandboxed`] (Linux und jedes andere Nicht-Android-Ziel)
/// immer `/bin/sh`, unverändert und ohne Existenzprüfung — byte-identisch zum
/// bisherigen Verhalten. Auf [`ExecPlatform::NoSandbox`] (Android/Termux, wo
/// `/bin/sh` fehlen kann) der erste vorhandene Pfad von `/bin/sh`,
/// `$PREFIX/bin/sh` (Termux-Präfix aus der Umgebungsvariable `PREFIX`) oder
/// `/system/bin/sh`; existiert keiner, bleibt `/bin/sh` als Rückfall (der
/// Start scheitert dann mit einer klaren `ENOENT`-Fehlermeldung statt eines
/// stillen Ausweichens auf ein anderes Verhalten).
///
/// Reine Funktion ohne Prozessstart — unit-testbar ohne Spawn.
fn resolve_host_shell(platform: ExecPlatform) -> PathBuf {
    match platform {
        ExecPlatform::Sandboxed => PathBuf::from("/bin/sh"),
        ExecPlatform::NoSandbox => {
            let mut candidates = vec![PathBuf::from("/bin/sh")];
            if let Some(prefix) = std::env::var_os("PREFIX") {
                candidates.push(PathBuf::from(prefix).join("bin/sh"));
            }
            candidates.push(PathBuf::from("/system/bin/sh"));
            candidates
                .into_iter()
                .find(|candidate| candidate.is_file())
                .unwrap_or_else(|| PathBuf::from("/bin/sh"))
        }
    }
}

/// Setzt die Standard-Streams für den Sandbox-Start: stdin `/dev/null` (nie das geerbte
/// Terminal), stdout/stderr als Pipes, SIGKILL beim Drop des `Child`.
pub(crate) fn configure_stdio(command: &mut TokioCommand) -> &mut TokioCommand {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
}

/// Beendet den direkten Kindprozess (`prlimit` hat sich per `exec` durch `bwrap` bzw.
/// `setsid`/`/bin/sh` ersetzt) per SIGKILL und sammelt den Exit-Status ein.
///
/// Im `bwrap`-Pfad beenden `--die-with-parent` und der PID-Namespace daraufhin den
/// gesamten Sandbox-Prozessbaum, auch per `setsid`/`nohup` abgekoppelte Nachfahren. Im
/// Host-Pfad ohne `bwrap` ([`ShellExecutor::run_host_command`]) ist die direkte
/// Kind-PID durch die bewusste Wahl an der `process_group`-Aufrufstelle dort immer die
/// Spitze des Kommandobaums — mit gefundenem `setsid` die `exec`te `/bin/sh`-PID der
/// neuen Sitzung, ohne `setsid` die `process_group(0)`-Gruppenführer-PID von `/bin/sh`
/// selbst — deshalb bleibt diese Funktion unverändert bei einem einzelnen SIGKILL statt
/// einer Prozessgruppen-weiten Signalisierung.
///
/// Nur SIGKILL: Ein vorgelagertes SIGTERM (kurze Gnadenfrist vor SIGKILL) bräuchte eine
/// Signalauswahl jenseits von [`tokio::process::Child::start_kill`] (immer SIGKILL) —
/// dafür gibt es in diesem `forbid(unsafe_code)`-Crate ohne neue Abhängigkeit (kein
/// `nix`/`libc`) keinen sicheren Weg, also bleibt es bei SIGKILL.
pub(crate) async fn terminate(child: &mut Child) -> Option<ExitStatus> {
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
    /// 5. Executes `/bin/sh -c <command>` inside that plan with the effective timeout,
    ///    racing `context.cancel()` (if attached) against both output-drain and
    ///    post-EOF exit waits.
    /// 6. Returns a JSON `ToolOutput` with `exit_code`, `stdout`, `stderr`, and `truncated`.
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-established sandbox authority;
    ///   its optional [`harw_types::cancel::CancelToken`] (via `context.cancel()`) is
    ///   passed through to [`Self::run_command`].
    /// - `call` (`&ToolCall`): untrusted invocation; `arguments` must match [`ShellExecArgs`].
    ///
    /// # Returns
    /// `Ok(ToolOutput::json(...))` on success, `Ok(ToolOutput::error(...))` on permission
    /// denial or runtime failure, `Err(ToolsError::InvalidArguments)` if arguments cannot
    /// be parsed, `Err(ToolsError::Cancelled)` if `context.cancel()` fires before the
    /// command completes.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: arguments JSON does not conform to the tool schema.
    /// - [`ToolsError::Cancelled`]: the attached cancel token fired before completion;
    ///   the process tree is killed via [`terminate`] before this is returned.
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

            // 2b. Runde 5, Teil B: Rechte-Werkzeuge (`sudo`, `doas`, `pkexec`,
            //     `su`, …) in Befehlsposition laufen nie über `shell.exec` —
            //     Root-Befehle gibt es nur über `host.sudo_exec` mit Freigabe
            //     im TUI-Fenster (fail-closed, auch im Host-Profil).
            if let Some(program) = crate::sudo::escalation_program(&args.command) {
                warn!(program, "shell.exec denied: privilege escalation program");
                return Ok(ToolOutput::error(crate::sudo::shell_escalation_message(
                    program,
                )));
            }

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
            self.run_command(
                &args,
                context.sandbox(),
                context.session_id().as_str(),
                context.cancel(),
            )
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
#[derive(Clone)]
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
    /// Host-PATH-Bindung für den `bwrap`-Sandbox-Pfad, gesetzt über
    /// [`Self::with_host_path`]. Reicht unverändert an
    /// [`harw_sandbox::BwrapLauncher::with_host_path`] durch, sobald
    /// `run_command` tatsächlich einen `bwrap`-Plan baut; wirkungslos für den
    /// Host-Pfad, der ohnehin ohne `bwrap` läuft.
    pub host_path: Option<HostPathBinding>,
    /// Plattform, auf der dieser Build läuft (Android-Anbindung); siehe
    /// [`Self::with_exec_platform`]. Vorgabe [`ExecPlatform::current`].
    pub exec_platform: ExecPlatform,
    /// Laufzeit-lebendige Freigabemodus-Zelle; siehe
    /// [`Self::with_approval_mode`]. Nur auf [`ExecPlatform::NoSandbox`]
    /// gelesen (Android/Termux); auf [`ExecPlatform::Sandboxed`] (Linux)
    /// wirkungslos.
    pub approval_mode: Option<ApprovalModeCell>,
    /// Verdrahtung für Host-Mode-Anfragen (`request_host`)
    /// aus dem Agentenbaum; nur die TUI setzt sie
    /// ([`Self::with_host_escalation`]). `None` heißt: jede Anfrage endet
    /// fail-closed mit [`crate::HOST_MODE_REQUIRES_TUI_MSG`].
    pub host_escalation: Option<crate::host_escalation::HostEscalation>,
    /// Runde 5, Teil N: Obergrenze für `timeout_secs` eines Aufrufs (Konfig
    /// `[shell] max_timeout_secs`, Vorgabe [`DEFAULT_MAX_TIMEOUT_SECS`]).
    /// [`Self::timeout_secs`] bleibt die Vorgabe ohne Angabe;
    /// Build-/Test-Befehle bekommen ohne Angabe
    /// [`BUILD_COMMAND_DEFAULT_TIMEOUT_SECS`], ebenfalls gedeckelt.
    pub max_timeout_secs: u64,
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

    /// Setzt die Host-PATH-Bindung für den `bwrap`-Sandbox-Pfad.
    ///
    /// # Description
    /// Reicht `binding` unverändert an
    /// [`harw_sandbox::BwrapLauncher::with_host_path`] durch, sobald
    /// `run_command` einen `bwrap`-Plan baut — nur dieser Pfad hat
    /// `--ro-bind-try`-Einträge für den zsh-PATH nötig. Wirkungslos für den
    /// direkten Host-Pfad aus Plan Teil B1: dort läuft der Befehl ohnehin ohne
    /// `bwrap` mit der vollständig geerbten Umgebung.
    #[must_use]
    pub fn with_host_path(mut self, binding: HostPathBinding) -> Self {
        self.host_path = Some(binding);
        self
    }

    /// Setzt die Plattform, auf der dieser Build läuft (Android-Anbindung).
    ///
    /// # Description
    /// Ohne diesen Aufruf gilt [`ExecPlatform::current`] — die tatsächliche
    /// Ziel-Plattform dieses Builds. Ein Test kann hier explizit
    /// [`ExecPlatform::NoSandbox`] erzwingen, um den Android-Pfad auf Linux zu
    /// prüfen, ohne für Android zu kompilieren.
    #[must_use]
    pub fn with_exec_platform(mut self, platform: ExecPlatform) -> Self {
        self.exec_platform = platform;
        self
    }

    /// Hängt die laufzeit-lebendige Freigabemodus-Zelle an.
    ///
    /// # Description
    /// Nur auf [`ExecPlatform::NoSandbox`] gelesen (Android/Termux ohne
    /// Bubblewrap-Sandbox): dort erlaubt
    /// [`harw_extension_api::ApprovalMode::FullAccess`] Host-Ausführung ohne
    /// Rückfrage (siehe [`ShellExecutor::determine_effective_host`]). Auf
    /// [`ExecPlatform::Sandboxed`] (Linux und jedes andere Nicht-Android-Ziel)
    /// bleibt das Verhalten unverändert, unabhängig vom Freigabemodus. Ohne
    /// diesen Aufruf bleibt [`Self::approval_mode`] `None`.
    #[must_use]
    pub fn with_approval_mode(mut self, mode: ApprovalModeCell) -> Self {
        self.approval_mode = Some(mode);
        self
    }

    /// Runde 5, Teil N: erlaubt Host-Mode-Anfragen (`shell.exec` mit
    /// `request_host`) über den angehängten Host-Permit-Fragekanal. Wirkt
    /// nur zusammen mit Ledger, Registry und Fragekanal; ändert nichts am
    /// Sandbox-Pfad eines Aufrufs ohne `request_host`.
    #[must_use]
    pub fn with_host_escalation(
        mut self,
        escalation: crate::host_escalation::HostEscalation,
    ) -> Self {
        self.host_escalation = Some(escalation);
        self
    }

    /// Runde 5, Teil N: setzt die Obergrenze für `timeout_secs` (bereits
    /// geklemmter Wert aus `[shell] max_timeout_secs`).
    #[must_use]
    pub fn with_max_timeout_secs(mut self, max_timeout_secs: u64) -> Self {
        self.max_timeout_secs = max_timeout_secs;
        self
    }

    /// Die Zeitlimit-Politik dieses Providers.
    fn timeout_policy(&self) -> timeouts::TimeoutPolicy {
        timeouts::TimeoutPolicy {
            default_secs: self.timeout_secs,
            build_default_secs: BUILD_COMMAND_DEFAULT_TIMEOUT_SECS,
            max_secs: self.max_timeout_secs.max(self.timeout_secs),
        }
    }

    /// Baut den inneren, unumhüllten [`ShellExecutor`] aus der
    /// Provider-Konfiguration. Seine Kappung ist die Obergrenze; das
    /// wirksame Zeitlimit schreibt die Hülle vorher in die Argumente.
    fn build_executor(&self) -> ShellExecutor {
        ShellExecutor {
            timeout_secs: self.max_timeout_secs.max(self.timeout_secs),
            max_output_bytes: self.max_output_bytes,
            limits: self.limits,
            sandbox_profile: self.sandbox_profile.clone(),
            permit_ledger: self.permit_ledger.clone(),
            host_permit_registry: self.host_permit_registry.clone(),
            host_permit_prompts: self.host_permit_prompts.clone(),
            preselected_permit_variant: self.preselected_permit_variant,
            host_permit_timeout: self.host_permit_timeout,
            host_path: self.host_path.clone(),
            exec_platform: self.exec_platform,
            approval_mode: self.approval_mode.clone(),
        }
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
                    "Shell command to execute in the isolated project sandbox. Runs as \
                     /bin/sh -c: POSIX sh (dash on Debian), not bash."
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
                    "Optional timeout in seconds for this call. Default 30 s; build/test \
                     commands (cargo, make, npm, pytest, go, ...) default to 600 s. Capped by \
                     the configured maximum ([shell] max_timeout_secs, default 900). Set it \
                     explicitly for long-running commands."
                        .to_owned(),
                ),
                ..Default::default()
            },
        );

        // Runde 5, Teil N: optionale Host-Mode-Anfrage.
        properties.insert(
            escalation::REQUEST_HOST_FIELD.to_owned(),
            escalation::request_host_schema(),
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
            host_path: None,
            exec_platform: ExecPlatform::current(),
            approval_mode: None,
            host_escalation: None,
            max_timeout_secs: DEFAULT_MAX_TIMEOUT_SECS,
        }
    }
}

/// Model-facing description of `shell.exec`.
///
/// R18 F3/F7/F1: the command runs under `/bin/sh` (dash on Debian), not bash —
/// the interpreter is deliberately not switched (minimal sandbox images,
/// identical behavior on host and sandbox), so the description states the
/// POSIX rules. It also steers file edits to `fs.edit`/`fs.write` and gateway
/// management to the `gateway.*` tools instead of the `harw` binary.
pub const SHELL_EXEC_DESCRIPTION: &str = "Execute a shell command inside the isolated project sandbox. \
    Runs under Bubblewrap via /bin/sh -c and captures stdout+stderr. \
    The shell is POSIX /bin/sh (dash on Debian), not bash: no `source`, use `.` \
    (e.g. `. \"$HOME/.cargo/env\"`); no bash arrays, no `[[ ]]` (use `[ ]`), no `{a,b}` \
    brace expansion, no `<<<` here-strings. \
    Do not edit or create files through shell heredocs, `sed -i`, or `python3 -`; \
    use fs.edit / fs.write instead. \
    Do not run `harw ...` (e.g. `harw gateway`, `harw channel`) to inspect or manage the \
    gateway: the harw binary is not available in the sandbox; use the gateway.* tools. \
    For long-running work (builds, test suites, servers) prefer job.start. \
    Requires ExecuteProcess permission. Default timeout 30 s, build/test \
    commands (cargo, make, npm, pytest, go, ...) 600 s; for long commands set \
    timeout_secs explicitly (capped by [shell] max_timeout_secs, default 900). \
    If the sandbox blocks the command \
    (network, path outside, missing tool, namespace), you may retry with \
    request_host {reason} to ask the user for Host-Mode.";

// `shell.exec` ist das einzige Werkzeug. Spezifikation, Name und Executor
// stammen aus den Konfigurationsfeldern des Providers; `tool_provider!` erzeugt
// daraus `impl ToolProvider`, `TOOL_NAMES`/`TOOL_PERMISSIONS` und die
// Compile-Zeit-Prüfung doppelter Namen.
//
// - Spezifikation: `strict = true`, damit das Modell keine Zusatzfelder
//   einschleusen kann.
// - Executor: Runde 5, Teil N — jeder Executor ist umhüllt; ohne
//   `request_host` verhält er sich wie bisher (plus Sandbox-Hinweis).
// - `parallel_safe: none` — Shell-Seiteneffekte (Dateisystem, Umgebung,
//   Prozesstabelle) gelten als nicht kommutativ; Aufrufer serialisieren.
harw_tools::tool_provider! {
    impl for ShellToolProvider as provider, parallel_safe: none {
        TOOL_NAME => {
            spec: ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new(TOOL_NAME),
                description: SHELL_EXEC_DESCRIPTION.to_owned(),
                parameters: ShellToolProvider::parameter_schema(),
                strict: true,
            }),
            executor: escalation::EscalatingShellExecutor::new(
                provider.build_executor(),
                provider.host_escalation.clone(),
                provider.timeout_policy(),
            ),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{ToolCall, ToolExecutionContext};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::OnceLock;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    // ── Test helpers ───────────────────────────────────────────────────────────

    fn make_temp_workspace() -> TestResult<TempDir> {
        tempfile::tempdir().map_err(ctx("tempdir creation must succeed in tests"))
    }

    fn make_sandbox(dir: &TempDir, permissions: Vec<Permission>) -> TestResult<SandboxSpec> {
        let harness_root = dir.path().to_path_buf();
        let ws_subdir = harness_root.join("project");
        fs::create_dir_all(&ws_subdir).map_err(ctx("project subdir must be created"))?;

        let registry = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: ws_subdir,
            }],
        )
        .map_err(ctx("registry build must succeed"))?;

        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("project"),
            )
            .map_err(ctx("resolve must succeed"))?;

        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions),
        ))
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

    /// R18 EX-03/EX-04: the description states POSIX sh and steers file edits
    /// to `fs.*` and gateway management to `gateway.*`.
    #[test]
    fn test_description_states_posix_sh_and_steers_edits_and_gateway() -> TestResult {
        let provider = ShellToolProvider::new();
        let tools = provider.tools();
        let ToolSpec::Function(spec) =
            tools.first().ok_or(TestError::Missing("shell.exec spec"))?;
        let text = spec.description.as_str();
        for needle in [
            "POSIX /bin/sh",
            "dash",
            "no `source`, use `.`",
            "no bash arrays",
            "[[ ]]",
            "fs.edit",
            "fs.write",
            "python3 -",
            "heredoc",
            "harw",
            "gateway.*",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in {text}");
        }
        assert_eq!(text, SHELL_EXEC_DESCRIPTION);
        Ok(())
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
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

    // ── setsid-Detach (Host-Pfad, Plan Teil B1 Ergänzung) ───────────────────────

    #[test]
    fn test_host_shell_argv_with_setsid_wraps_wait_and_bin_sh() {
        let setsid = Path::new("/usr/bin/setsid");

        let (program, args) = host_shell_argv(Some(setsid), "echo hi");

        assert_eq!(program, PathBuf::from("/usr/bin/setsid"));
        assert_eq!(
            args,
            vec![
                OsString::from("--wait"),
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("echo hi"),
            ],
            "with setsid the command must be wrapped in `setsid --wait /bin/sh -c <cmd>` \
             so exit status/timing still propagate to spawn_and_collect"
        );
    }

    #[test]
    fn test_host_shell_argv_without_setsid_is_unchanged_bin_sh_dash_c() {
        let (program, args) = host_shell_argv(None, "echo hi");

        assert_eq!(program, PathBuf::from("/bin/sh"));
        assert_eq!(
            args,
            vec![OsString::from("-c"), OsString::from("echo hi")],
            "without setsid the argv must stay byte-identical to the pre-fix host path"
        );
    }

    #[test]
    fn test_find_setsid_in_returns_none_for_nonexistent_explicit_path() {
        let missing = Path::new("/nonexistent/definitely-not-here/setsid");

        assert_eq!(
            find_setsid_in(&[missing]),
            None,
            "a candidate path that does not exist must never resolve"
        );
    }

    #[test]
    fn test_find_setsid_in_finds_an_executable_regular_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let fake_setsid = dir.path().join("setsid");
        fs::write(&fake_setsid, b"#!/bin/sh\nexec \"$@\"\n").map_err(ctx("write fake setsid"))?;
        fs::set_permissions(&fake_setsid, std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod fake setsid executable"))?;
        let missing = dir.path().join("does-not-exist");

        // Non-existent candidates before the real one must be skipped, not error.
        assert_eq!(
            find_setsid_in(&[missing.as_path(), fake_setsid.as_path()]),
            Some(fake_setsid)
        );
        Ok(())
    }

    #[test]
    fn test_find_setsid_in_skips_non_executable_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let not_executable = dir.path().join("setsid");
        fs::write(&not_executable, b"not a program").map_err(ctx("write file"))?;
        fs::set_permissions(&not_executable, std::fs::Permissions::from_mode(0o644))
            .map_err(ctx("chmod without exec bits"))?;

        assert_eq!(
            find_setsid_in(&[not_executable.as_path()]),
            None,
            "a regular file without any execute bit must not resolve"
        );
        Ok(())
    }

    // ── Pure Tests ohne Sandbox (W1-03) ────────────────────────────────────────

    #[tokio::test]
    async fn test_configure_stdio_sets_stdin_to_dev_null() -> TestResult {
        if !Path::new("/proc/self/fd/0").exists() || !Path::new("/bin/sh").exists() {
            eprintln!("übersprungen: /proc oder /bin/sh fehlt, stdin-Ziel nicht beobachtbar");
            return Ok(());
        }
        // Absichtlich ohne bwrap: prüft genau die Stream-Konfiguration, die der
        // Sandbox-Start verwendet.
        let mut command = TokioCommand::new("/bin/sh");
        command.args(["-c", "readlink /proc/self/fd/0"]);
        let mut child = configure_stdio(&mut command)
            .spawn()
            .map_err(ctx("spawn /bin/sh"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or(TestError::Missing("stdout piped"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or(TestError::Missing("stderr piped"))?;
        let mut capture = BoundedCapture::new(4096);
        let end = capture
            .drain(&mut stdout, &mut stderr)
            .await
            .map_err(ctx("read child output"))?;
        let status = child.wait().await.map_err(ctx("wait child"))?;

        assert_eq!(end, DrainEnd::Eof);
        assert!(status.success(), "readlink must succeed: {status:?}");
        assert_eq!(
            String::from_utf8_lossy(capture.stdout()).trim(),
            "/dev/null"
        );
        Ok(())
    }

    #[test]
    fn test_completed_output_marks_output_limit_kill_as_truncated() -> TestResult {
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
        };
        let capture = BoundedCapture::new(16);

        match executor.completed_output(None, &capture, true, false) {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], -1);
                assert_eq!(content["truncated"], true);
                assert_eq!(content["killed_by_output_limit"], true);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_timeout_output_keeps_partial_output_within_budget() -> TestResult {
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
        };
        let (mut writer, mut stdout) = tokio::io::duplex(1024);
        let (_stderr_writer, mut stderr) = tokio::io::duplex(1024);
        writer
            .write_all(b"partial-output-longer-than-budget")
            .await
            .map_err(ctx("write"))?;
        let mut capture = BoundedCapture::new(executor.max_output_bytes);
        let _ = tokio::time::timeout(
            Duration::from_millis(50),
            capture.drain(&mut stdout, &mut stderr),
        )
        .await;

        match executor.timeout_output(1, &capture, false) {
            ToolOutput::Error { message } => {
                assert!(message.contains("timed out after 1s"), "{message}");
                assert!(message.contains("partial-"), "{message}");
                assert!(message.contains("truncated: true"), "{message}");
                assert!(!message.contains("longer-than-budget"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Error output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_exec_invalid_limits_fail_closed_before_spawn() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
        };
        let args = ShellExecArgs {
            command: "echo must_not_run".to_owned(),
            timeout_secs: None,
        };

        match executor
            .run_command(&args, &sandbox, "test-session", None)
            .await
            .map_err(ctx("run"))?
        {
            ToolOutput::Error { message } => {
                assert!(message.contains("resource limits"), "{message}");
                assert!(message.contains("nofile"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "invalid limits must not spawn, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_exec_permission_denied() -> TestResult {
        let tmp = make_temp_workspace()?;
        // No ExecuteProcess permission
        let sandbox = make_sandbox(&tmp, vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let call = make_call("echo should_not_run");

        let provider = ShellToolProvider::new();
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor"))?;

        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(crate::test_support::ctx(
                "execute must not return Err even when denied",
            ))?;

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("ExecuteProcess permission missing"),
                    "denial message must mention the missing permission, got: {message:?}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Error output for denied permission, got: {other:?}"
                )));
            }
        }
        Ok(())
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
            async fn $name() -> TestResult {
                if !sandbox_runtime_available() {
                    eprintln!(
                        "übersprungen: {}: bwrap/prlimit/userns nicht startbar \
                         (Pflichtvariante: {} mit --ignored)",
                        stringify!($name),
                        stringify!($required)
                    );
                    return Ok(());
                }
                $body().await
            }

            #[tokio::test]
            #[ignore = "requires bwrap+userns"]
            async fn $required() -> TestResult {
                assert!(
                    sandbox_runtime_available(),
                    "bwrap+prlimit+userns müssen für diesen Test startbar sein"
                );
                $body().await
            }
        };
    }

    async fn run_with(provider: &ShellToolProvider, call: &ToolCall) -> TestResult<ToolOutput> {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ctx = make_ctx(sandbox);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor for shell.exec"))?;
        executor
            .execute(&ctx, call)
            .await
            .map_err(crate::test_support::ctx("execute must not return Err"))
    }

    async fn echo_returns_stdout() -> TestResult {
        let output = run_with(
            &ShellToolProvider::new(),
            &make_call("echo hello_from_shell"),
        )
        .await?;

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_echo_returns_stdout,
        test_exec_echo_returns_stdout_required,
        echo_returns_stdout
    );

    async fn timeout_reports_error_with_partial_output() -> TestResult {
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
            host_escalation: None,
            max_timeout_secs: DEFAULT_MAX_TIMEOUT_SECS,
        };
        let call = make_call_with_timeout("echo partial_before_timeout; sleep 5", 1);
        let started = std::time::Instant::now();

        let output = run_with(&provider, &call).await?;

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Error output for timeout, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_timeout,
        test_exec_timeout_required,
        timeout_reports_error_with_partial_output
    );

    async fn nonzero_exit_is_reported() -> TestResult {
        let output = run_with(&ShellToolProvider::new(), &make_call("false")).await?;

        match output {
            ToolOutput::Json { content } => {
                let exit_code = content["exit_code"].as_i64().unwrap_or(0);
                assert_ne!(exit_code, 0, "false must return a nonzero exit code");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output even for nonzero exit, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_nonzero_exit,
        test_exec_nonzero_exit_required,
        nonzero_exit_is_reported
    );

    async fn stderr_is_captured() -> TestResult {
        let output = run_with(
            &ShellToolProvider::new(),
            &make_call("echo error_output >&2"),
        )
        .await?;

        match output {
            ToolOutput::Json { content } => {
                let stderr = content["stderr"].as_str().unwrap_or("");
                assert!(
                    stderr.contains("error_output"),
                    "stderr must be captured, got: {stderr:?}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_stderr_captured,
        test_exec_stderr_captured_required,
        stderr_is_captured
    );

    async fn small_output_over_cap_is_truncated() -> TestResult {
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
            host_escalation: None,
            max_timeout_secs: DEFAULT_MAX_TIMEOUT_SECS,
        };
        // Generate more than the configured output cap.
        let call = make_call("echo 'this_is_a_longer_string_than_ten_bytes'");

        match run_with(&provider, &call).await? {
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
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_output_truncation,
        test_exec_output_truncation_required,
        small_output_over_cap_is_truncated
    );

    async fn endless_output_is_capped_and_killed() -> TestResult {
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
            host_escalation: None,
            max_timeout_secs: DEFAULT_MAX_TIMEOUT_SECS,
        };
        let started = std::time::Instant::now();

        let output = run_with(&provider, &make_call("yes")).await?;

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_endless_output_is_capped_and_killed,
        test_exec_endless_output_is_capped_and_killed_required,
        endless_output_is_capped_and_killed
    );

    async fn stdin_is_closed_inside_sandbox() -> TestResult {
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
            host_escalation: None,
            max_timeout_secs: DEFAULT_MAX_TIMEOUT_SECS,
        };
        // Mit geerbtem Terminal-stdin würde `cat` bis zum Timeout blockieren.
        let call = make_call("cat; echo stdin_reached_eof");

        match run_with(&provider, &call).await? {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], 0);
                let stdout = content["stdout"].as_str().unwrap_or("");
                assert_eq!(stdout.trim(), "stdin_reached_eof");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_stdin_is_closed,
        test_exec_stdin_is_closed_required,
        stdin_is_closed_inside_sandbox
    );

    async fn rlimits_and_tmpfs_size_apply_inside_sandbox() -> TestResult {
        let call = make_call("cat /proc/self/limits; df -B1 /tmp");

        match run_with(&ShellToolProvider::new(), &call).await? {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], 0, "{content}");
                let stdout = content["stdout"].as_str().unwrap_or("");
                // Runde 5, Teil N: RLIMIT_CPU wächst mit dem Zeitlimit
                // (`timeouts::limits_for`); Vorgabe-Zeitlimit hier 30 s.
                let cpu =
                    timeouts::limits_for(ShellLimits::default(), DEFAULT_TIMEOUT_SECS).cpu_secs;
                let cpu = cpu.to_string();
                let expected = [
                    ("Max cpu time", cpu.as_str()),
                    ("Max file size", "268435456"),
                    ("Max processes", "4096"),
                    ("Max open files", "256"),
                    ("Max address space", "2147483648"),
                ];
                for (label, value) in expected {
                    let line = stdout
                        .lines()
                        .find(|line| line.starts_with(label))
                        .ok_or_else(|| {
                            TestError::Unexpected(format!("missing limit line {label}: {stdout}"))
                        })?;
                    let fields: Vec<&str> = line[label.len()..].split_whitespace().collect();
                    assert_eq!(fields[..2], [value, value], "soft=hard for {label}: {line}");
                }
                let tmpfs = stdout
                    .lines()
                    .find(|line| line.trim_end().ends_with("/tmp"))
                    .ok_or_else(|| {
                        TestError::Unexpected(format!("missing df line for /tmp: {stdout}"))
                    })?;
                assert!(
                    tmpfs.split_whitespace().nth(1) == Some("268435456"),
                    "tmpfs /tmp must be limited to 256 MiB: {tmpfs}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_rlimits_and_tmpfs_size_apply,
        test_exec_rlimits_and_tmpfs_size_apply_required,
        rlimits_and_tmpfs_size_apply_inside_sandbox
    );

    // ── Cancel-Tests (run_command: cancel: Option<&CancelToken>) ──────────

    fn plain_executor(timeout_secs: u64) -> ShellExecutor {
        ShellExecutor {
            timeout_secs,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry: None,
            host_permit_prompts: None,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
        }
    }

    async fn cancel_during_run_kills_process_and_returns_cancelled() -> TestResult {
        use harw_types::cancel::CancelReason;

        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        // Long timeout so the timeout path can never win the race against cancel.
        let executor = plain_executor(30);
        let args = ShellExecArgs {
            command: "sleep 30".to_owned(),
            timeout_secs: None,
        };
        let cancel = CancelToken::new();
        let canceller_cancel = cancel.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            canceller_cancel.cancel(CancelReason::User);
        });

        let started = std::time::Instant::now();
        let result = executor
            .run_command(&args, &sandbox, "cancel-session", Some(&cancel))
            .await;
        canceller
            .await
            .map_err(ctx("canceller task must not panic"))?;

        assert!(
            started.elapsed() < KILL_REAP_TIMEOUT + Duration::from_secs(5),
            "cancel must kill the process tree (SIGKILL) well within the kill-reap \
             timeout, instead of waiting out `sleep 30`, took: {:?}",
            started.elapsed()
        );
        assert!(
            matches!(result, Err(ToolsError::Cancelled)),
            "a cancelled run_command must return Err(ToolsError::Cancelled) instead of \
             a timeout ToolOutput, got: {result:?}"
        );
        Ok(())
    }
    sandbox_test!(
        test_exec_cancel_kills_process_and_returns_cancelled,
        test_exec_cancel_kills_process_and_returns_cancelled_required,
        cancel_during_run_kills_process_and_returns_cancelled
    );

    async fn cancel_none_behaves_exactly_as_before() -> TestResult {
        // Regression guard: `cancel: None` must leave today's plain
        // `timeout_at`-only behavior untouched — no race, no `Cancelled` path.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let executor = plain_executor(DEFAULT_TIMEOUT_SECS);
        let args = ShellExecArgs {
            command: "echo cancel_none_ok".to_owned(),
            timeout_secs: None,
        };

        match executor
            .run_command(&args, &sandbox, "no-cancel-session", None)
            .await
            .map_err(ctx(
                "run_command must not return Err for a plain, uncancelled command",
            ))? {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], 0, "echo must exit with 0");
                let stdout = content["stdout"].as_str().unwrap_or("");
                assert!(
                    stdout.contains("cancel_none_ok"),
                    "stdout must contain echoed string, got: {stdout:?}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output for cancel=None, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_cancel_none_behaves_as_before,
        test_exec_cancel_none_behaves_as_before_required,
        cancel_none_behaves_exactly_as_before
    );

    // ── Host-Pfad-Tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_determine_effective_host_strict_without_registry_is_false() -> TestResult {
        // Kein Registry angehängt: fällt auf den (unveränderten) bwrap-Pfad
        // zurück, unabhängig von der Sitzung.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let executor = plain_executor(DEFAULT_TIMEOUT_SECS);
        let args = ShellExecArgs {
            command: "echo hi".to_owned(),
            timeout_secs: None,
        };

        let effective_host = executor
            .determine_effective_host(&args, &sandbox, "s1")
            .await
            .map_err(ctx("must not error without a registry"))?;
        assert!(!effective_host);
        Ok(())
    }

    #[tokio::test]
    async fn test_determine_effective_host_strict_with_registry_but_no_approval_is_false()
    -> TestResult {
        // Registry angehängt, aber weder Sitzungs- noch Einmalfreigabe für
        // diese Sitzung: bleibt beim bwrap-Pfad.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let mut executor = plain_executor(DEFAULT_TIMEOUT_SECS);
        executor.host_permit_registry = Some(Arc::clone(&registry));
        let args = ShellExecArgs {
            command: "echo hi".to_owned(),
            timeout_secs: None,
        };

        let effective_host = executor
            .determine_effective_host(&args, &sandbox, "s1")
            .await
            .map_err(ctx("must not error"))?;
        assert!(!effective_host);
        Ok(())
    }

    #[tokio::test]
    async fn test_determine_effective_host_strict_with_session_lease_is_true() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1");
        let mut executor = plain_executor(DEFAULT_TIMEOUT_SECS);
        executor.host_permit_registry = Some(Arc::clone(&registry));
        let args = ShellExecArgs {
            command: "echo hi".to_owned(),
            timeout_secs: None,
        };

        let effective_host = executor
            .determine_effective_host(&args, &sandbox, "s1")
            .await
            .map_err(ctx("must not error"))?;
        assert!(
            effective_host,
            "an active session lease must authorize the host path"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_determine_effective_host_single_use_approval_is_consumed_exactly_once()
    -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_single_use("s1".to_owned());
        let mut executor = plain_executor(DEFAULT_TIMEOUT_SECS);
        executor.host_permit_registry = Some(Arc::clone(&registry));
        let args = ShellExecArgs {
            command: "echo hi".to_owned(),
            timeout_secs: None,
        };

        let first = executor
            .determine_effective_host(&args, &sandbox, "s1")
            .await
            .map_err(ctx("must not error"))?;
        assert!(
            first,
            "the pending single-use approval must authorize the first call"
        );

        let second = executor
            .determine_effective_host(&args, &sandbox, "s1")
            .await
            .map_err(ctx("must not error"))?;
        assert!(
            !second,
            "a single-use approval must be consumed after exactly one call"
        );
        assert!(!registry.has_single_use("s1"));
        Ok(())
    }

    #[tokio::test]
    async fn test_determine_effective_host_session_lease_preserves_pending_single_use() -> TestResult
    {
        // Vertrag: eine Einmal-Freigabe wird nur verbraucht, wenn keine
        // Sitzungsfreigabe besteht — die Sitzungsfreigabe muss also Vortritt
        // haben, ohne die Einmal-Freigabe anzutasten.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1");
        registry.mark_single_use("s1".to_owned());
        let mut executor = plain_executor(DEFAULT_TIMEOUT_SECS);
        executor.host_permit_registry = Some(Arc::clone(&registry));
        let args = ShellExecArgs {
            command: "echo hi".to_owned(),
            timeout_secs: None,
        };

        let effective_host = executor
            .determine_effective_host(&args, &sandbox, "s1")
            .await
            .map_err(ctx("must not error"))?;
        assert!(effective_host);
        assert!(
            registry.has_single_use("s1"),
            "a session lease must satisfy the call without consuming a pending single-use approval"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_strict_profile_with_session_lease_runs_directly_on_the_host() -> TestResult {
        // Kein `sandbox_test!`-Guard nötig: der Host-Pfad läuft nie über
        // bwrap, muss also auch ohne installiertes bwrap erfolgreich sein —
        // das allein ist hier schon ein Beleg, dass tatsächlich der
        // Host-Pfad lief (bwrap setzt HOME immer fest auf `/tmp/home`, siehe
        // unten für den expliziten Gegen-Test).
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ctx = make_ctx(sandbox);
        let call = make_call("pwd");

        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved(ctx.session_id().as_str());

        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Strict)
            .with_host_permit_registry(Arc::clone(&registry));
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor"))?;

        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(crate::test_support::ctx("execute must not return Err"))?;

        match output {
            ToolOutput::Json { content } => {
                assert_eq!(
                    content["exit_code"], 0,
                    "pwd must succeed on the host, got: {content}"
                );
                assert_eq!(
                    content["executed_on"], "host",
                    "the JSON output must mark this call as host-executed: {content}"
                );
                let stdout = content["stdout"].as_str().unwrap_or("").trim();
                let expected = sandbox_root(&tmp)?;
                assert_eq!(
                    Path::new(stdout),
                    expected.as_path(),
                    "pwd must report the sandbox's canonical workspace root as cwd"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output for host execution via session lease, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_strict_profile_single_use_approval_runs_on_host_exactly_once() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ctx = make_ctx(sandbox);
        let call = make_call("echo single_use_ok");

        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_single_use(ctx.session_id().as_str().to_owned());

        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Strict)
            .with_host_permit_registry(Arc::clone(&registry));
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor"))?;

        let first = executor
            .execute(&ctx, &call)
            .await
            .map_err(crate::test_support::ctx("execute must not return Err"))?;
        match first {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], 0, "{content}");
                assert_eq!(content["executed_on"], "host", "{content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output for the single-use host call, got: {other:?}"
                )));
            }
        }
        assert!(
            !registry.has_single_use(ctx.session_id().as_str()),
            "a single-use approval must be consumed after exactly one call"
        );

        // Der zweite Aufruf hat keine Freigabe mehr: er darf kein
        // `"executed_on": "host"` mehr tragen, egal ob er als Json oder
        // Error zurückkommt (bwrap ist in der Testumgebung ggf. nicht
        // installiert).
        let second = executor
            .execute(&ctx, &call)
            .await
            .map_err(crate::test_support::ctx("execute must not return Err"))?;
        if let ToolOutput::Json { content } = second {
            assert!(
                content.get("executed_on").is_none(),
                "the single-use approval must not still authorize a second host call: {content}"
            );
        }
        Ok(())
    }

    async fn strict_without_any_approval_runs_via_bwrap() -> TestResult {
        // Ohne jede Registry-Freigabe bleibt Strict beim unveränderten
        // bwrap-Pfad: bwrap setzt `HOME` in jedem Fall fest auf `/tmp/home`
        // (siehe `harw_sandbox::bwrap`), was der echte Host-`$HOME` so gut
        // wie nie ist — ein sichtbarer, konkreter Beleg, dass hier tatsächlich
        // sandboxed statt auf dem Host gelaufen wurde.
        let output = run_with(
            &ShellToolProvider::new().with_sandbox_profile(SandboxProfile::Strict),
            &make_call("echo $HOME"),
        )
        .await?;

        match output {
            ToolOutput::Json { content } => {
                assert!(
                    content.get("executed_on").is_none(),
                    "without any registry approval this must run under bwrap, not the host \
                     path: {content}"
                );
                let stdout = content["stdout"].as_str().unwrap_or("").trim();
                assert_eq!(
                    stdout, "/tmp/home",
                    "bwrap always sets HOME to /tmp/home, proving the command ran sandboxed, \
                     got: {stdout:?}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Json output for strict profile without approval, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
    sandbox_test!(
        test_exec_strict_without_any_approval_runs_via_bwrap,
        test_exec_strict_without_any_approval_runs_via_bwrap_required,
        strict_without_any_approval_runs_via_bwrap
    );

    // ── Permit-/Profil-Tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_host_profile_without_ledger_is_denied() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ctx = make_ctx(sandbox);
        let call = make_call("echo should_not_run");

        let provider = ShellToolProvider::default().with_sandbox_profile(SandboxProfile::Host);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor"))?;

        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(crate::test_support::ctx("execute must not return Err"))?;

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("host execution requires a process permit"),
                    "expected permit denial, got: {message:?}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Error output for host without ledger, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_host_profile_with_ledger_but_without_session_approval_is_denied() -> TestResult {
        // Ledger und Registry sind konfiguriert, aber die Sitzung hat der
        // lokalen UI noch nicht zugestimmt: fail-closed.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
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
            .ok_or(TestError::Missing("executor"))?;

        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(crate::test_support::ctx("execute must not return Err"))?;

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("requires local UI approval"),
                    "expected local-approval denial, got: {message:?}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Error output without session approval, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_host_profile_with_session_approval_authorizes_via_real_ledger() -> TestResult {
        // Nach einer (simulierten) lokalen UI-Zustimmung für die Sitzung muss
        // `authorize_host_command` tatsächlich über den echten Ledger einen
        // neuen Permit ausstellen und autorisieren, statt nur dessen
        // Existenz zu prüfen.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ctx = make_ctx(sandbox);
        let call = make_call("echo host_ok");

        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved(ctx.session_id().as_str());

        let provider = ShellToolProvider::default()
            .with_sandbox_profile(SandboxProfile::Host)
            .with_permit_ledger(Arc::clone(&ledger))
            .with_host_permit_registry(Arc::clone(&registry));
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor"))?;

        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(crate::test_support::ctx("execute must not return Err"))?;

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
            workspace: sandbox_root(&tmp)?,
            environment: ProcessEnvironment::LocalHost,
        };
        let remembered_id = registry
            .lookup_permit(&request)
            .ok_or(TestError::Missing("permit remembered after issuance"))?;
        assert!(ledger.authorize(remembered_id, &request).is_ok());
        Ok(())
    }

    // Baut denselben kanonischen Workspace-Pfad wie `make_sandbox`, damit der
    // in einem Test unabhängig zusammengesetzte `ProcessPermitRequest` genau
    // dem entspricht, den `authorize_host_command` tatsächlich verwendet.
    fn sandbox_root(dir: &TempDir) -> TestResult<PathBuf> {
        dir.path()
            .join("project")
            .canonicalize()
            .map_err(ctx("project subdir must be canonicalizable"))
    }

    #[tokio::test]
    async fn test_strict_profile_without_ledger_still_works() -> TestResult {
        // Strict-Profil ohne Ledger: Sandbox ist die Grenze, nicht der Permit.
        // Dies darf nicht fehlschlagen, weil der Ledger fehlt.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ctx = make_ctx(sandbox);
        let call = make_call("echo strict_mode_ok");

        let provider = ShellToolProvider::default().with_sandbox_profile(SandboxProfile::Strict);
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .ok_or(TestError::Missing("executor"))?;

        // Wir prüfen nur, dass nicht mit einem Permit-Fehler abgelehnt wird;
        // ein Sandbox-Setup-Fehler (kein bwrap) ist hier nicht der Punkt.
        let output = executor
            .execute(&ctx, &call)
            .await
            .map_err(crate::test_support::ctx("execute must not return Err"))?;

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
        Ok(())
    }

    #[test]
    fn test_provider_with_sandbox_profile_builder() {
        let provider = ShellToolProvider::default().with_sandbox_profile(SandboxProfile::Strict);
        assert!(provider.sandbox_profile.is_strict());
        assert!(provider.permit_ledger.is_none());
    }

    #[test]
    fn test_provider_with_permit_ledger_builder() {
        let ledger = Arc::new(ProcessPermitLedger::default());
        let provider = ShellToolProvider::default().with_permit_ledger(ledger);
        assert!(provider.permit_ledger.is_some());
    }

    // ── Android-Anbindung: `determine_effective_host` auf `ExecPlatform::NoSandbox` ──
    // Diese Tests laufen auf Linux-CI (siehe Brief): `ExecPlatform::NoSandbox`
    // wird hier bewusst erzwungen, ohne für Android zu kompilieren.

    use harw_extension_api::ApprovalMode;

    /// Baut einen `ShellExecutor` mit expliziter Plattform/Freigabemodus für
    /// die Android-Anbindung; alle übrigen Felder wie [`plain_executor`],
    /// aber ohne Sandbox-Profil-Kopplung (immer `Strict`).
    fn executor_for_platform(
        exec_platform: ExecPlatform,
        approval_mode: Option<ApprovalModeCell>,
        host_permit_registry: Option<Arc<HostPermitSessionRegistry>>,
        host_permit_prompts: Option<HostPermitPromptSender>,
    ) -> ShellExecutor {
        ShellExecutor {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits::default(),
            sandbox_profile: SandboxProfile::Strict,
            permit_ledger: None,
            host_permit_registry,
            host_permit_prompts,
            preselected_permit_variant: HostPermitVariant::SingleExecution,
            host_permit_timeout: HOST_PERMIT_PROMPT_TIMEOUT,
            host_path: None,
            exec_platform,
            approval_mode,
        }
    }

    #[tokio::test]
    async fn test_no_sandbox_without_lease_or_channel_fails_closed_fast() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let executor = executor_for_platform(ExecPlatform::NoSandbox, None, None, None);
        let args = args_for("echo must_not_run");

        let started = std::time::Instant::now();
        let result = executor
            .determine_effective_host(&args, &sandbox, "no-channel-session")
            .await;
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "no listener must fail immediately, never wait for the 300s prompt timeout"
        );
        match result {
            Err(message) => assert_eq!(message, NO_SANDBOX_NO_APPROVAL_MSG),
            Ok(host) => {
                return Err(TestError::Unexpected(format!(
                    "expected fail-closed Err, got Ok({host})"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_no_sandbox_with_session_lease_runs_on_host() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("leased-session");
        let executor = executor_for_platform(ExecPlatform::NoSandbox, None, Some(registry), None);
        let args = args_for("echo leased_ok");

        let host = executor
            .determine_effective_host(&args, &sandbox, "leased-session")
            .await
            .map_err(|message| TestError::Unexpected(format!("must not deny: {message}")))?;
        assert!(host, "an active session lease must run on the host");
        Ok(())
    }

    #[tokio::test]
    async fn test_no_sandbox_full_access_runs_on_host_without_prompt() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let mode = ApprovalModeCell::new(ApprovalMode::FullAccess);
        // Kein Ledger, keine Registry, kein Fragekanal — `FullAccess` allein
        // reicht auf `NoSandbox`, ohne jede Rückfrage.
        let executor = executor_for_platform(ExecPlatform::NoSandbox, Some(mode), None, None);
        let args = args_for("echo full_access_ok");

        let host = executor
            .determine_effective_host(&args, &sandbox, "full-access-session")
            .await
            .map_err(|message| TestError::Unexpected(format!("must not deny: {message}")))?;
        assert!(
            host,
            "ApprovalMode::FullAccess must run on the host without a prompt"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_sandboxed_full_access_non_host_profile_stays_sandboxed() -> TestResult {
        // Linux-Verhalten bleibt unverändert: `ApprovalMode::FullAccess` wird
        // auf `ExecPlatform::Sandboxed` nicht ausgewertet.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let mode = ApprovalModeCell::new(ApprovalMode::FullAccess);
        let executor = executor_for_platform(ExecPlatform::Sandboxed, Some(mode), None, None);
        let args = args_for("echo sandboxed_ok");

        let host = executor
            .determine_effective_host(&args, &sandbox, "sandboxed-session")
            .await
            .map_err(|message| TestError::Unexpected(format!("must not deny: {message}")))?;
        assert!(
            !host,
            "FullAccess on a sandboxed platform must not skip the sandbox for a non-host profile"
        );
        Ok(())
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
            host_path: None,
            exec_platform: ExecPlatform::Sandboxed,
            approval_mode: None,
        }
    }

    fn args_for(command: &str) -> ShellExecArgs {
        ShellExecArgs {
            command: command.to_owned(),
            timeout_secs: None,
        }
    }

    #[tokio::test]
    async fn test_authorize_host_command_session_lease_does_not_expire_until_user_revokes()
    -> TestResult {
        // Nutzerentscheidung 2026-09-24: weder die Sitzungszustimmung noch
        // der dafür ausgestellte `SessionLease`-Permit verfallen mit der Zeit;
        // erst der Widerruf durch den Nutzer (Strg+H: `forget_session` +
        // `revoke_session`) beendet die Host-Arbeitsphase.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1");

        let executor = host_executor(&ledger, &registry);
        let args = args_for("echo repeat_me");

        assert!(
            executor
                .authorize_host_command(&args, &sandbox, "s1")
                .await
                .is_ok(),
            "first call must succeed via a fresh local-approval issuance"
        );

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            registry.is_session_approved("s1"),
            "the session-level approval must not expire over time"
        );
        assert!(
            executor
                .authorize_host_command(&args, &sandbox, "s1")
                .await
                .is_ok(),
            "an identical repeated request must still succeed via the remembered permit"
        );

        // Nutzer beendet die Phase (dasselbe wie `ChatApp::end_host_mode`).
        registry.forget_session("s1");
        ledger.revoke_session("s1").map_err(ctx("revoke_session"))?;
        assert!(
            executor
                .authorize_host_command(&args, &sandbox, "s1")
                .await
                .is_err(),
            "after the user revoked the lease, host execution must need a fresh approval"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_authorize_host_command_different_commands_same_session_both_succeed() -> TestResult
    {
        // Eine einmalige Sitzungszustimmung deckt beliebig viele
        // *unterschiedliche* Befehlstexte derselben Sitzung ab; jeder bekommt
        // seinen eigenen, getrennt gemerkten Permit.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1");

        let executor = host_executor(&ledger, &registry);
        let first = args_for("echo first_command");
        let second = args_for("echo second_command");

        assert!(
            executor
                .authorize_host_command(&first, &sandbox, "s1")
                .await
                .is_ok()
        );
        assert!(
            executor
                .authorize_host_command(&second, &sandbox, "s1")
                .await
                .is_ok()
        );

        let first_request = ProcessPermitRequest {
            session: "s1".to_owned(),
            worker_definition: HOST_WORKER_DEFINITION.to_owned(),
            command: first.command.clone(),
            workspace: sandbox_root(&tmp)?,
            environment: ProcessEnvironment::LocalHost,
        };
        let second_request = ProcessPermitRequest {
            session: "s1".to_owned(),
            worker_definition: HOST_WORKER_DEFINITION.to_owned(),
            command: second.command.clone(),
            workspace: sandbox_root(&tmp)?,
            environment: ProcessEnvironment::LocalHost,
        };
        let first_id = registry
            .lookup_permit(&first_request)
            .ok_or(TestError::Missing("first command permit"))?;
        let second_id = registry
            .lookup_permit(&second_request)
            .ok_or(TestError::Missing("second command permit"))?;
        assert_ne!(
            first_id, second_id,
            "distinct command texts must remember distinct permit ids"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_authorize_host_command_different_session_does_not_reuse_remembered_permit()
    -> TestResult {
        // Ein für Sitzung `s1` gemerkter Permit darf nicht für eine andere
        // Sitzung `s2` gefunden werden, selbst bei identischem Befehlstext.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1");

        let executor = host_executor(&ledger, &registry);
        let args = args_for("echo shared_command_text");

        assert!(
            executor
                .authorize_host_command(&args, &sandbox, "s1")
                .await
                .is_ok()
        );

        let result = executor.authorize_host_command(&args, &sandbox, "s2").await;
        assert!(
            result.is_err(),
            "session s2 has no approval and must not benefit from session s1's remembered permit"
        );
        Ok(())
    }

    // ── authorize_host_command: neue Frage über den Fragekanal ─────────────

    #[tokio::test]
    async fn test_authorize_host_command_prompts_and_grants_single_execution() -> TestResult {
        // Weder gemerkter Permit noch Sitzungsphase, aber ein Kanal ist
        // angehängt: die Anfrage muss fragen und bei Zustimmung durchgehen.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let args = args_for("echo prompted_ok");

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.ok_or(TestError::Missing("prompt"))?;
            assert_eq!(prompt.session(), "s1");
            assert_eq!(prompt.command(), "echo prompted_ok");
            assert_eq!(
                prompt.preselected_variant(),
                HostPermitVariant::SingleExecution
            );
            assert!(prompt.approve(HostPermitVariant::SingleExecution));
            Ok::<(), TestError>(())
        });

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        responder
            .await
            .map_err(ctx("responder task must not panic"))??;

        assert!(
            result.is_ok(),
            "an approved prompt must authorize the command: {result:?}"
        );
        assert!(
            !registry.is_session_approved("s1"),
            "a single-execution approval must never open a session-wide phase"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_authorize_host_command_prompts_and_session_lease_covers_next_call() -> TestResult
    {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let first = args_for("echo first");
        let second = args_for("echo second");

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.ok_or(TestError::Missing("prompt"))?;
            assert!(prompt.approve(HostPermitVariant::SessionLease));
            Ok::<(), TestError>(())
        });

        assert!(
            executor
                .authorize_host_command(&first, &sandbox, "s1")
                .await
                .is_ok()
        );
        responder
            .await
            .map_err(ctx("responder task must not panic"))??;
        assert!(registry.is_session_approved("s1"));

        // Der zweite, abweichende Befehl derselben Sitzung darf ohne erneute
        // Frage durchgehen — die Phase wurde bereits eingetragen.
        assert!(
            executor
                .authorize_host_command(&second, &sandbox, "s1")
                .await
                .is_ok()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_active_host_lease_covers_consecutive_shell_worker_children_without_prompt()
    -> TestResult {
        // Holy-Export: trotz aktiver Host-Arbeitsphase fragte jedes neue
        // `uia-shell-worker`-Kind (eigene Session-ID) erneut. Erstes Kind
        // beantwortet die Frage mit „Host-Arbeitsphase“; zwei weitere Kinder
        // mit frischen Session-IDs dürfen danach keine Frage mehr auslösen.
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();
        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);

        let responder = tokio::spawn(async move {
            let prompt = receiver.recv().await.ok_or(TestError::Missing("prompt"))?;
            assert!(prompt.approve(HostPermitVariant::SessionLease));
            Ok::<_, TestError>(receiver)
        });
        assert!(
            executor
                .authorize_host_command(&args_for("mkdir -p a"), &sandbox, "child-1")
                .await
                .is_ok()
        );
        let mut receiver = responder
            .await
            .map_err(ctx("responder task must not panic"))??;

        for child in ["child-2", "child-3"] {
            assert!(
                executor
                    .authorize_host_command(&args_for("ls -la"), &sandbox, child)
                    .await
                    .is_ok(),
                "{child}: the active lease must cover a new child session"
            );
        }
        assert!(
            receiver.try_recv().is_err(),
            "no further host-permit prompt (and thus no repeated host-mode notice) may be sent"
        );

        // Ebenso mit einer vorab über `/sandbox-lease` (global) erteilten Phase.
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_global_approval();
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();
        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        for child in ["child-a", "child-b"] {
            assert!(
                executor
                    .authorize_host_command(&args_for("true"), &sandbox, child)
                    .await
                    .is_ok(),
                "{child}"
            );
        }
        assert!(
            receiver.try_recv().is_err(),
            "no prompt under an active lease"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_authorize_host_command_prompt_denial_fails_closed() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let args = args_for("rm -rf /");

        tokio::spawn(async move {
            if let Some(prompt) = receiver.recv().await {
                assert!(prompt.deny());
            }
        });

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(result.is_err(), "an explicit denial must fail closed");
        assert!(!registry.is_session_approved("s1"));
        Ok(())
    }

    #[tokio::test]
    async fn test_authorize_host_command_without_any_channel_fails_closed() -> TestResult {
        // Kein Ledger/Registry-Zustand und kein Fragekanal: fail-closed ohne
        // dass je etwas gesendet wird (kein Empfänger existiert überhaupt).
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let executor = host_executor(&ledger, &registry);
        let args = args_for("echo should_not_run");

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(
            result.is_err_and(|message| message.contains("requires local UI approval")),
            "no channel attached must fail closed with the UI-approval message"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_authorize_host_command_dropped_prompt_fails_closed() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        let args = args_for("echo should_not_run");

        tokio::spawn(async move {
            if let Some(_prompt) = receiver.recv().await {
                // Bewusst ohne Antwort fallengelassen.
            }
        });

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(result.is_err(), "a dropped prompt must fail closed");
        Ok(())
    }

    #[tokio::test]
    async fn test_authorize_host_command_timeout_fails_closed() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
        let ledger = Arc::new(ProcessPermitLedger::default());
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (sender, mut receiver) = crate::host_permit_prompt::host_permit_prompt_channel();

        let mut executor = host_executor(&ledger, &registry);
        executor.host_permit_prompts = Some(sender);
        executor.host_permit_timeout = Duration::from_millis(20);
        let args = args_for("echo should_not_run");

        let _keep_open = tokio::spawn(async move {
            if let Some(prompt) = receiver.recv().await {
                tokio::time::sleep(Duration::from_secs(5)).await;
                drop(prompt);
            }
        });

        let result = executor.authorize_host_command(&args, &sandbox, "s1").await;
        assert!(result.is_err(), "an elapsed timeout must fail closed");
        Ok(())
    }

    #[tokio::test]
    async fn test_authorize_host_command_closed_channel_fails_closed() -> TestResult {
        let tmp = make_temp_workspace()?;
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess])?;
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
        Ok(())
    }
}
