//! Die Modell-Werkzeuge `job.start`, `job.status`, `job.logs`, `job.stop`,
//! `job.list`, `job.wait` ([`JobToolProvider`]).
//!
//! # Sicherheitskontrakt
//! - `job.start`: [`Permission::ExecuteProcess`] (hier **und** im
//!   [`crate::JobLauncher`]), danach exakt der Freigabeweg von `shell.exec`
//!   ([`crate::ShellJobLauncher`]). Plan-Modus/lesende Profile haben kein
//!   `ExecuteProcess` und werden abgewiesen, bevor irgendetwas startet.
//! - `job.stop`: kein eigenes Recht — es startet nichts und wirkt nur auf
//!   Jobs, die der Aufrufer oder einer seiner Nachfahren gestartet hat
//!   (Orchestratoren ohne Shell dürfen die Jobs ihrer Worker stoppen). Die
//!   Freigabe regelt die Politik der Montage (nie auto-freigegeben).
//! - Alle Werkzeuge außer `job.list` nehmen eine `job_id`; unbekannte und
//!   fremde Jobs bekommen **dieselbe** Meldung. Steuern dürfen nur der
//!   Erzeuger und seine Vorfahren ([`crate::JobOwner::may_control`]); die
//!   aufrufende Sitzung kommt aus dem Ausführungskontext, nie aus Argumenten.
//! - `cwd` muss im Workspace liegen (kanonisiert); Umgebungsvariablen werden
//!   nur mit gültigem Namen und gequotetem Wert übernommen, ihre Werte nie
//!   in `meta.json` gespeichert.
//! - Auf dem Host läuft ein Job mit gefilterter Umgebung (`env_clear` plus
//!   Allowlist: PATH, HOME, Locale, Build-Variablen wie `CARGO_*`/
//!   `RUSTFLAGS`/`CC`, `SSH_AUTH_SOCK`, `XDG_*`; Namen mit TOKEN, SECRET,
//!   PASSWORD, PASSWD, CREDENTIAL oder API_KEY fallen weg). Alles andere
//!   muss der Aufrufer über `env` ausdrücklich übergeben; diese Variablen
//!   stehen als `export …` im Befehlstext und überstehen den Filter.
//! - `job.list` zeigt ohne `kind` nur eine Zusammenfassung je Kategorie;
//!   Arbeitseinträge (`work`) sieht das Werkzeug nicht, nur `harw jobs list
//!   --kind work` des Operators.
//!
//! # Nebenläufigkeit
//! `job.status`, `job.logs`, `job.list` und `job.wait` sind `parallel_safe`
//! (lesend), `job.start` und `job.stop` nicht.

use crate::launcher::JobLauncher;
use crate::logs::{LogQuery, LogSlice, read_log};
use crate::manager::{Caller, JobError, JobManager, StartRequest, WaitOutcome};
use crate::model::{JobEndReason, JobId, JobOwner, JobStatus, STDERR_LOG, STDOUT_LOG};
use crate::procfs::JobSignal;
use harw_authority::Permission;
use harw_extension_api::contributors::ToolProvider;
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use harw_types::SessionId;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Name des Startwerkzeugs.
pub const JOB_START_TOOL: &str = "job.start";
/// Name des Statuswerkzeugs.
pub const JOB_STATUS_TOOL: &str = "job.status";
/// Name des Logwerkzeugs.
pub const JOB_LOGS_TOOL: &str = "job.logs";
/// Name des Stopwerkzeugs.
pub const JOB_STOP_TOOL: &str = "job.stop";
/// Name des Listenwerkzeugs.
pub const JOB_LIST_TOOL: &str = "job.list";
/// Name des Wartewerkzeugs.
pub const JOB_WAIT_TOOL: &str = "job.wait";

/// Alle Job-Werkzeuge in Registrierungsreihenfolge.
pub const JOB_TOOL_NAMES: [&str; 6] = [
    JOB_START_TOOL,
    JOB_STATUS_TOOL,
    JOB_LOGS_TOOL,
    JOB_STOP_TOOL,
    JOB_LIST_TOOL,
    JOB_WAIT_TOOL,
];

/// Die rein lesenden Job-Werkzeuge (Kandidaten für `AUTO_APPROVED_TOOLS`).
pub const JOB_READ_TOOLS: [&str; 4] =
    [JOB_STATUS_TOOL, JOB_LOGS_TOOL, JOB_LIST_TOOL, JOB_WAIT_TOOL];

/// Die Werkzeuge, die Orchestratoren ohne Shell tragen: lesen, warten und
/// stoppen — kein `job.start`.
pub const JOB_CONTROL_TOOLS: [&str; 5] = [
    JOB_STATUS_TOOL,
    JOB_LOGS_TOOL,
    JOB_STOP_TOOL,
    JOB_LIST_TOOL,
    JOB_WAIT_TOOL,
];

/// Der Befehlstext eines `job.start`-Aufrufs für Freigabe und Auto-Modus:
/// `command` bzw. `argv`, mit [`shell_quote`] zu einem Befehl verbunden.
///
/// # Beschreibung
/// Dieselbe Textform, die `job.start` anzeigt und ausführt (ohne das
/// `export …`-Präfix der Umgebungsvariablen und ohne `cd`). `None`, wenn
/// weder ein nicht-leeres `command` noch ein nicht-leeres `argv` vorliegt
/// oder beide gesetzt sind (der Aufruf scheitert dann ohnehin).
///
/// # Beispiele
/// ```rust
/// use harw_tool_job::job_start_command_text;
/// use serde_json::json;
///
/// assert_eq!(
///     job_start_command_text(&json!({"argv": ["cargo", "build", "a b"]})).as_deref(),
///     Some("cargo build 'a b'")
/// );
/// assert_eq!(
///     job_start_command_text(&json!({"command": "make -j8"})).as_deref(),
///     Some("make -j8")
/// );
/// assert_eq!(job_start_command_text(&json!({})), None);
/// ```
#[must_use]
pub fn job_start_command_text(arguments: &Value) -> Option<String> {
    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.trim().is_empty());
    let argv: Option<Vec<&str>> = arguments
        .get("argv")
        .and_then(Value::as_array)
        .map(|words| words.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .filter(|words| !words.is_empty());
    match (command, argv) {
        (Some(command), None) => Some(command.to_owned()),
        (None, Some(words)) => Some(
            words
                .into_iter()
                .map(shell_quote)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// Höchste Wartezeit von `job.wait` in Sekunden.
pub const MAX_WAIT_SECS: u64 = 600;
/// Vorgabe für `job.logs` ohne `tail`/`since_line`.
const DEFAULT_LOG_TAIL: usize = 100;
/// Höchstzahl Zeilen je Stream in `job.logs`.
const MAX_LOG_LINES: usize = 500;
/// Byte-Budget von `job.logs` (beide Streams zusammen).
const MAX_LOG_BYTES: usize = 48 * 1024;
/// Höchstlänge des Anzeigenamens.
const MAX_NAME_CHARS: usize = 80;
/// Höchstzahl Umgebungsvariablen.
const MAX_ENV_VARS: usize = 64;
/// Höchstzahl Jobs in `job.list`.
const MAX_LIST_ENTRIES: usize = 100;
/// Hinweis von `job.list` zur Kategorie `work`, die das Werkzeug nicht sieht.
const WORK_NOT_VISIBLE: &str = "work items are not visible to job.list; the operator lists them with `harw jobs list --kind work`";

/// Liefert die Elternkette einer Agenten-Sitzung (nächstliegend zuerst).
///
/// # Description
/// Die Montage implementiert das über den Agentenbaum (Spawner-Register);
/// ohne Montage ([`NoLineage`]) darf nur der Erzeuger selbst steuern.
pub trait JobLineage: Send + Sync {
    /// Vorfahren von `session`: Elternteil, Großelternteil, …, Wurzel.
    fn ancestors(&self, session: &SessionId) -> Vec<String>;
}

/// Keine bekannte Elternkette.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLineage;

impl JobLineage for NoLineage {
    fn ancestors(&self, _session: &SessionId) -> Vec<String> {
        Vec::new()
    }
}

/// Elternkette aus einer Funktion (z. B. einem Abschluss über das
/// Spawner-Register der Montage).
pub struct FnLineage<F>(pub F);

impl<F> JobLineage for FnLineage<F>
where
    F: Fn(&SessionId) -> Vec<String> + Send + Sync,
{
    fn ancestors(&self, session: &SessionId) -> Vec<String> {
        (self.0)(session)
    }
}

struct Shared {
    manager: Arc<JobManager>,
    launcher: Arc<dyn JobLauncher>,
    lineage: Arc<dyn JobLineage>,
}

/// Registriert die sechs Job-Werkzeuge.
///
/// # Examples
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_extension_api::contributors::ToolProvider;
/// use harw_tool_job::{JobManager, JobManagerConfig, JobToolProvider, NoopNotifier, ShellJobLauncher};
/// use harw_tool_shell::ShellToolProvider;
///
/// # fn main() -> std::io::Result<()> {
/// let manager = JobManager::new(JobManagerConfig::new("/tmp/harw-state"), Arc::new(NoopNotifier))?;
/// let provider = JobToolProvider::new(manager, Arc::new(ShellJobLauncher::new(ShellToolProvider::new())));
/// assert_eq!(provider.tools().len(), 6);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct JobToolProvider {
    shared: Arc<Shared>,
}

impl JobToolProvider {
    /// Baut den Provider ohne Elternkette ([`NoLineage`]).
    #[must_use]
    pub fn new(manager: Arc<JobManager>, launcher: Arc<dyn JobLauncher>) -> Self {
        Self {
            shared: Arc::new(Shared {
                manager,
                launcher,
                lineage: Arc::new(NoLineage),
            }),
        }
    }

    /// Setzt die Quelle der Elternkette (Besitz: Erzeuger + Vorfahren).
    #[must_use]
    pub fn with_lineage(self, lineage: Arc<dyn JobLineage>) -> Self {
        Self {
            shared: Arc::new(Shared {
                manager: Arc::clone(&self.shared.manager),
                launcher: Arc::clone(&self.shared.launcher),
                lineage,
            }),
        }
    }

    /// Die zugrunde liegende Job-Verwaltung.
    #[must_use]
    pub fn manager(&self) -> &Arc<JobManager> {
        &self.shared.manager
    }
}

/// Kurzform für die Montage: Provider mit Elternkette.
#[must_use]
pub fn job_tools(
    manager: Arc<JobManager>,
    launcher: Arc<dyn JobLauncher>,
    lineage: Arc<dyn JobLineage>,
) -> JobToolProvider {
    JobToolProvider::new(manager, launcher).with_lineage(lineage)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Start,
    Status,
    Logs,
    Stop,
    List,
    Wait,
}

impl Kind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            JOB_START_TOOL => Some(Self::Start),
            JOB_STATUS_TOOL => Some(Self::Status),
            JOB_LOGS_TOOL => Some(Self::Logs),
            JOB_STOP_TOOL => Some(Self::Stop),
            JOB_LIST_TOOL => Some(Self::List),
            JOB_WAIT_TOOL => Some(Self::Wait),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Start => JOB_START_TOOL,
            Self::Status => JOB_STATUS_TOOL,
            Self::Logs => JOB_LOGS_TOOL,
            Self::Stop => JOB_STOP_TOOL,
            Self::List => JOB_LIST_TOOL,
            Self::Wait => JOB_WAIT_TOOL,
        }
    }
}

// ── Schema ────────────────────────────────────────────────────────────────────

fn prop(schema_type: JsonSchemaType, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(schema_type),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn string_array(description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Array),
        description: Some(description.to_owned()),
        items: Some(Box::new(JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            ..Default::default()
        })),
        ..Default::default()
    }
}

fn object(properties: Vec<(&str, JsonSchema)>, required: &[&str]) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(
            properties
                .into_iter()
                .map(|(name, schema)| (name.to_owned(), schema))
                .collect::<BTreeMap<_, _>>(),
        ),
        required: Some(required.iter().map(|name| (*name).to_owned()).collect()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

fn job_id_prop() -> JsonSchema {
    prop(
        JsonSchemaType::String,
        "Job id returned by job.start (e.g. job-20260924-101112-001).",
    )
}

fn spec(name: &str, description: &str, parameters: JsonSchema) -> ToolSpec {
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(name),
        description: description.to_owned(),
        parameters,
        strict: true,
    })
}

fn tool_specs() -> Vec<ToolSpec> {
    vec![
        spec(
            JOB_START_TOOL,
            "Start a long-running command as a background job and return its job_id at once. \
             Use this instead of shell.exec (and never tmux) for anything that may take longer \
             than about 2 minutes or that you want to follow: builds (cargo, cmake/ninja, make), \
             package restores (vcpkg, npm, pip), test suites, servers. Same permissions and \
             approval as shell.exec (sandbox or approved host mode; sudo only via \
             host.sudo_exec). Give either `command` (shell text for /bin/sh -c) or `argv` \
             (exact argument vector). stdout/stderr go to log files. You automatically get \
             system notes: job start, progress every notify_every_secs (default 60, only when \
             something changed; 0 = off), error lines as they appear, and the end with exit \
             code, duration and the last 20 lines. Do not poll in a loop: continue other work \
             or call job.wait. On the host the job gets a filtered environment (PATH, HOME, \
             locale, build-tool variables such as CARGO_*/RUSTFLAGS/CC, SSH_AUTH_SOCK, XDG_*; \
             names containing TOKEN/SECRET/PASSWORD/PASSWD/CREDENTIAL/API_KEY are removed); \
             pass anything else explicitly via env.",
            object(
                vec![
                    (
                        "command",
                        prop(
                            JsonSchemaType::String,
                            "Shell command for /bin/sh -c. Mutually exclusive with argv.",
                        ),
                    ),
                    (
                        "argv",
                        string_array(
                            "Exact argument vector, e.g. [\"cargo\", \"build\", \"--release\"]. \
                             Mutually exclusive with command.",
                        ),
                    ),
                    (
                        "cwd",
                        prop(
                            JsonSchemaType::String,
                            "Working directory inside the workspace (relative to the workspace \
                             root or absolute). Default: workspace root.",
                        ),
                    ),
                    (
                        "name",
                        prop(
                            JsonSchemaType::String,
                            "Short human-readable job name, e.g. \"ladybird build\".",
                        ),
                    ),
                    (
                        "env",
                        string_array("Extra environment variables as \"NAME=value\" strings."),
                    ),
                    (
                        "notify_every_secs",
                        prop(
                            JsonSchemaType::Integer,
                            "Seconds between progress notes (default 60, min 10, max 3600; 0 \
                             disables periodic notes, errors and the end are still reported).",
                        ),
                    ),
                ],
                &["name"],
            ),
        ),
        spec(
            JOB_STATUS_TOOL,
            "Status of one of your jobs: state (queued/running/succeeded/failed/stopped/\
             detached/unknown), pid, runtime, exit code, recognised progress, warning/error \
             counts, the last output lines, sandbox profile (host/bwrap), end reason \
             (exited/signal/stopped/timeout/launch-error/unknown), whether leftover processes \
             were reaped, whether a log hit its size budget, launch warnings.",
            object(vec![("job_id", job_id_prop())], &["job_id"]),
        ),
        spec(
            JOB_LOGS_TOOL,
            "Read a job's stdout/stderr log files. Default: the last 100 lines of both \
             streams. `tail` returns the last N (matching) lines, `since_line` starts at a line \
             number (1-based; use the numbers from a previous call to read only new output), \
             `grep` keeps lines containing a plain substring. Output is capped (~48 KiB).",
            object(
                vec![
                    ("job_id", job_id_prop()),
                    (
                        "stream",
                        JsonSchema {
                            enum_values: Some(vec![
                                json!("both"),
                                json!("stdout"),
                                json!("stderr"),
                            ]),
                            ..prop(
                                JsonSchemaType::String,
                                "Which log to read: both (default), stdout or stderr.",
                            )
                        },
                    ),
                    (
                        "tail",
                        prop(
                            JsonSchemaType::Integer,
                            "Return only the last N matching lines (max 500).",
                        ),
                    ),
                    (
                        "since_line",
                        prop(
                            JsonSchemaType::Integer,
                            "Start at this line number (1-based).",
                        ),
                    ),
                    (
                        "grep",
                        prop(
                            JsonSchemaType::String,
                            "Plain substring filter (not a regex).",
                        ),
                    ),
                ],
                &["job_id"],
            ),
        ),
        spec(
            JOB_STOP_TOOL,
            "Stop one of your jobs: sends `signal` (TERM default; INT, HUP or KILL) to the \
             whole process group, then SIGKILL after a grace period. Also kills processes the \
             job left behind in its process group.",
            object(
                vec![
                    ("job_id", job_id_prop()),
                    (
                        "signal",
                        JsonSchema {
                            enum_values: Some(vec![
                                json!("TERM"),
                                json!("INT"),
                                json!("HUP"),
                                json!("KILL"),
                            ]),
                            ..prop(
                                JsonSchemaType::String,
                                "Signal to send first (default TERM).",
                            )
                        },
                    ),
                ],
                &["job_id"],
            ),
        ),
        spec(
            JOB_LIST_TOOL,
            "Without kind: a summary per category (process = your background jobs from \
             job.start, work = durable work items) with counts per state. With \
             kind=\"process\": the rows (state, owner, sandbox profile, end reason, runtime, \
             progress). Use job.status for one job.",
            object(
                vec![(
                    "kind",
                    JsonSchema {
                        enum_values: Some(vec![json!("work"), json!("process")]),
                        ..prop(
                            JsonSchemaType::String,
                            "Category whose rows to list: process or work. Omit for the \
                             summary.",
                        )
                    },
                )],
                &[],
            ),
        ),
        spec(
            JOB_WAIT_TOOL,
            "Wait, bounded by timeout_secs (1-600), until the job ends or reaches the next \
             milestone (new error lines, progress crossing a 10% step, or a phase change such \
             as cargo `Finished`). Returns the outcome (finished/milestone/timeout) and the \
             job status.",
            object(
                vec![
                    ("job_id", job_id_prop()),
                    (
                        "timeout_secs",
                        prop(JsonSchemaType::Integer, "Maximum seconds to wait (1-600)."),
                    ),
                ],
                &["job_id", "timeout_secs"],
            ),
        ),
    ]
}

// ── Argumente ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArgs {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    argv: Option<Vec<String>>,
    #[serde(default)]
    cwd: Option<String>,
    name: String,
    #[serde(default)]
    env: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    notify_every_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobIdArgs {
    job_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogsArgs {
    job_id: String,
    #[serde(default)]
    stream: Option<String>,
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    tail: Option<usize>,
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    since_line: Option<u64>,
    #[serde(default)]
    grep: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopArgs {
    job_id: String,
    #[serde(default)]
    signal: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    kind: Option<String>,
}

/// Kategorie der Zeilenansicht von `job.list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListKind {
    /// Durable Arbeitseinträge (für das Werkzeug unsichtbar).
    Work,
    /// Hintergrund-Jobs aus `job.start`.
    Process,
}

/// `None`/leer ⇒ Zusammenfassung; sonst `work` oder `process`.
fn parse_list_kind(raw: Option<&str>) -> Result<Option<ListKind>, ToolsError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some("work") => Ok(Some(ListKind::Work)),
        Some("process") => Ok(Some(ListKind::Process)),
        Some(other) => Err(invalid(
            JOB_LIST_TOOL,
            format!("kind must be work or process, got `{other}`"),
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    job_id: String,
    #[serde(deserialize_with = "harw_extension_api::lenient::lenient_opt_u64")]
    timeout_secs: Option<u64>,
}

fn invalid(tool: &str, reason: impl Into<String>) -> ToolsError {
    ToolsError::InvalidArguments {
        name: tool.to_owned(),
        reason: reason.into(),
    }
}

fn parse_args<T: DeserializeOwned>(tool: &str, call: &ToolCall) -> Result<T, ToolsError> {
    // `{}` statt `null` für Werkzeuge ohne Argumente.
    let arguments = if call.arguments.is_null() {
        json!({})
    } else {
        call.arguments.clone()
    };
    serde_json::from_value(arguments).map_err(|err| invalid(tool, err.to_string()))
}

fn parse_job_id(tool: &str, raw: &str) -> Result<JobId, ToolsError> {
    JobId::parse(raw).ok_or_else(|| invalid(tool, format!("invalid job_id `{raw}`")))
}

/// Quotet ein Wort für `/bin/sh` (einfache Anführungszeichen).
///
/// # Examples
/// ```rust
/// use harw_tool_job::shell_quote;
///
/// assert_eq!(shell_quote("cargo"), "cargo");
/// assert_eq!(shell_quote("it's here"), "'it'\\''s here'");
/// ```
#[must_use]
pub fn shell_quote(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte));
    if safe {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Geprüfter Start: Anzeige- und Ausführungsform.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Composed {
    name: String,
    display: String,
    shell: String,
    cwd: Option<PathBuf>,
    env_keys: Vec<String>,
}

fn compose(args: &StartArgs, workspace_root: &Path) -> Result<Composed, String> {
    let name = args.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("name must be 1-{MAX_NAME_CHARS} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("name must not contain control characters".to_owned());
    }

    let command = args.command.as_deref().filter(|c| !c.trim().is_empty());
    let argv = args.argv.as_ref().filter(|argv| !argv.is_empty());
    let display = match (command, argv) {
        (Some(_), Some(_)) => return Err("give either `command` or `argv`, not both".to_owned()),
        (None, None) => return Err("one of `command` or `argv` is required".to_owned()),
        (Some(command), None) => {
            if command.contains('\0') {
                return Err("command must not contain NUL bytes".to_owned());
            }
            command.to_owned()
        }
        (None, Some(argv)) => {
            if argv.iter().any(|word| word.contains('\0')) {
                return Err("argv must not contain NUL bytes".to_owned());
            }
            if argv[0].trim().is_empty() {
                return Err("argv[0] must not be blank".to_owned());
            }
            argv.iter()
                .map(String::as_str)
                .map(shell_quote)
                .collect::<Vec<_>>()
                .join(" ")
        }
    };

    let mut prefix = String::new();
    let mut env_keys = Vec::new();
    if let Some(env) = &args.env {
        if env.len() > MAX_ENV_VARS {
            return Err(format!("at most {MAX_ENV_VARS} env entries"));
        }
        let mut assignments = Vec::new();
        for entry in env {
            let Some((key, value)) = entry.split_once('=') else {
                return Err(format!("env entry `{entry}` must look like NAME=value"));
            };
            if !valid_env_name(key) {
                return Err(format!("invalid environment variable name `{key}`"));
            }
            if value.contains('\0') {
                return Err(format!("env value of `{key}` must not contain NUL bytes"));
            }
            assignments.push(format!("{key}={}", shell_quote(value)));
            env_keys.push(key.to_owned());
        }
        if !assignments.is_empty() {
            prefix.push_str("export ");
            prefix.push_str(&assignments.join(" "));
            prefix.push_str(" && ");
        }
    }

    let cwd = match args
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
    {
        None => None,
        Some(raw) => {
            let requested = Path::new(raw);
            let joined = if requested.is_absolute() {
                requested.to_path_buf()
            } else {
                workspace_root.join(requested)
            };
            let canonical = joined
                .canonicalize()
                .map_err(|err| format!("cwd `{raw}` is not accessible: {err}"))?;
            if !canonical.starts_with(workspace_root) {
                return Err(format!("cwd `{raw}` is outside the workspace"));
            }
            if !canonical.is_dir() {
                return Err(format!("cwd `{raw}` is not a directory"));
            }
            let text = canonical
                .to_str()
                .ok_or_else(|| format!("cwd `{raw}` is not valid UTF-8"))?
                .to_owned();
            prefix.push_str("cd ");
            prefix.push_str(&shell_quote(&text));
            prefix.push_str(" && ");
            Some(canonical)
        }
    };

    Ok(Composed {
        name: name.to_owned(),
        shell: format!("{prefix}{display}"),
        display,
        cwd,
        env_keys,
    })
}

// ── Ausgaben ──────────────────────────────────────────────────────────────────

fn status_json(status: &JobStatus) -> Value {
    let meta = &status.meta;
    json!({
        "job_id": meta.job_id.as_str(),
        "name": meta.name,
        "state": meta.state.as_str(),
        "command": meta.command,
        "cwd": meta.cwd,
        "pid": meta.pid,
        "executed_on": if meta.executed_on_host { "host" } else { "sandbox" },
        "sandbox_profile": meta.sandbox_profile(),
        "end_reason": meta.end_reason().map(JobEndReason::as_str),
        "stragglers_reaped": meta.stragglers_reaped,
        "log_truncated": meta.log_truncated,
        "launch_warnings": meta.launch_warnings,
        "started_at": meta.started_at.map(|ts| ts.to_string()),
        "ended_at": meta.ended_at.map(|ts| ts.to_string()),
        "runtime_secs": status.runtime_secs,
        "exit_code": meta.exit_code,
        "signal": meta.signal,
        "progress": meta.progress.as_ref().map(|progress| json!({
            "summary": progress.render(),
            "percent": progress.percent,
            "done": progress.done,
            "total": progress.total,
            "phase": progress.phase,
        })),
        "warnings": meta.warnings,
        "errors": meta.errors,
        "stdout_lines": status.stdout_lines,
        "stderr_lines": status.stderr_lines,
        "last_lines": status.last_lines,
        "stop_requested": meta.stop_requested,
        "detached": meta.detached,
        "launch_error": meta.launch_error,
        "log_dir": status.log_dir.display().to_string(),
    })
}

/// Standardansicht von `job.list`: Zahlen je Kategorie und Zustand, keine
/// Zeilen. `count` oben bleibt die Zahl der sichtbaren Prozess-Jobs.
fn list_summary_json(jobs: &[JobStatus]) -> Value {
    let mut by_state: BTreeMap<&str, u64> = BTreeMap::new();
    for status in jobs {
        *by_state.entry(status.meta.state.as_str()).or_insert(0) += 1;
    }
    json!({
        "view": "summary",
        "count": jobs.len(),
        "categories": {
            "process": { "count": jobs.len(), "by_state": by_state },
            "work": { "visible": false, "note": WORK_NOT_VISIBLE },
        },
        "note": "Pass kind=\"process\" for the rows; job.status shows one job.",
    })
}

/// Zeilenansicht `kind=process`: die letzten [`MAX_LIST_ENTRIES`] Jobs.
fn list_process_rows_json(jobs: &[JobStatus]) -> Value {
    let count = jobs.len();
    let skip = count.saturating_sub(MAX_LIST_ENTRIES);
    let entries: Vec<Value> = jobs.iter().skip(skip).map(list_entry_json).collect();
    json!({
        "view": "rows",
        "kind": "process",
        "count": count,
        "truncated": skip > 0,
        "jobs": entries,
    })
}

/// Zeilenansicht `kind=work`: leer, das Werkzeug sieht keine Arbeitseinträge.
fn list_work_rows_json() -> Value {
    json!({
        "view": "rows",
        "kind": "work",
        "count": 0,
        "truncated": false,
        "jobs": [],
        "note": WORK_NOT_VISIBLE,
    })
}

fn list_entry_json(status: &JobStatus) -> Value {
    let meta = &status.meta;
    json!({
        "kind": "process",
        "job_id": meta.job_id.as_str(),
        "name": meta.name,
        "state": meta.state.as_str(),
        "owner": meta.owner.session,
        "sandbox_profile": meta.sandbox_profile(),
        "end_reason": meta.end_reason().map(JobEndReason::as_str),
        "runtime_secs": status.runtime_secs,
        "exit_code": meta.exit_code,
        "progress": meta.progress.as_ref().map(crate::ProgressSnapshot::render),
        "errors": meta.errors,
    })
}

fn slice_json(slice: &LogSlice) -> Value {
    json!({
        "total_lines": slice.total_lines,
        "matched_lines": slice.matched_lines,
        "truncated": slice.truncated,
        "lines": slice
            .lines
            .iter()
            .map(|(number, text)| format!("{number}: {text}"))
            .collect::<Vec<_>>(),
    })
}

fn job_error(tool: &str, err: &JobError) -> ToolOutput {
    ToolOutput::error(format!("{tool}: {err}"))
}

// ── Ausführung ────────────────────────────────────────────────────────────────

struct JobToolExecutor {
    kind: Kind,
    shared: Arc<Shared>,
}

impl JobToolExecutor {
    async fn start(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        let args: StartArgs = parse_args(JOB_START_TOOL, call)?;
        let root = context.sandbox().workspace().canonical_root().to_path_buf();
        let composed = compose(&args, &root).map_err(|reason| invalid(JOB_START_TOOL, reason))?;

        if let Some(denied) = harw_tools::sandbox_guard::require_permission(
            context,
            Permission::ExecuteProcess,
            JOB_START_TOOL,
        ) {
            warn!("job.start denied: ExecuteProcess permission missing");
            return Ok(denied);
        }
        let manager = &self.shared.manager;
        if let Err(err) = manager.check_capacity() {
            return Ok(job_error(JOB_START_TOOL, &err));
        }

        // Der Startweg filtert auf dem Host die Umgebung (Allowlist plus
        // Namensfilter); Hinweise wie „ohne rlimits“ gehen mit in den Job.
        let launch = match self
            .shared
            .launcher
            .prepare_launch(context, &composed.shell, manager.config().cpu_budget_secs)
            .await
        {
            Ok(launch) => launch,
            Err(denied) => return Ok(denied),
        };

        let session = context.session_id();
        let owner = JobOwner::new(session.as_str(), self.shared.lineage.ancestors(session));
        let notify_every = manager
            .config()
            .effective_notify_every(args.notify_every_secs);
        let request = StartRequest {
            name: composed.name,
            command: composed.display,
            cwd: composed.cwd,
            env_keys: composed.env_keys,
            notify_every,
            owner,
        };
        match manager.start_with_warnings(request, launch.job, launch.warnings) {
            Ok(status) => {
                info!(job_id = %status.meta.job_id, "job.start");
                let mut value = status_json(&status);
                value["notify_every_secs"] = json!(notify_every.as_secs());
                value["note"] = json!(
                    "Job runs in the background. You will receive progress/error/finish notes \
                     automatically; use job.wait to block until the next milestone, job.logs \
                     for output, job.stop to stop it."
                );
                Ok(ToolOutput::json(value))
            }
            Err(err) => Ok(job_error(JOB_START_TOOL, &err)),
        }
    }

    async fn logs(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        let args: LogsArgs = parse_args(JOB_LOGS_TOOL, call)?;
        let id = parse_job_id(JOB_LOGS_TOOL, &args.job_id)?;
        let streams: &[&str] = match args.stream.as_deref().map(str::trim) {
            None | Some("" | "both") => &[STDOUT_LOG, STDERR_LOG],
            Some("stdout") => &[STDOUT_LOG],
            Some("stderr") => &[STDERR_LOG],
            Some(other) => {
                return Err(invalid(
                    JOB_LOGS_TOOL,
                    format!("stream must be both, stdout or stderr, got `{other}`"),
                ));
            }
        };
        let caller = Caller::Agent(context.session_id().as_str());
        let dir = match self.shared.manager.log_dir(&id, caller) {
            Ok(dir) => dir,
            Err(err) => return Ok(job_error(JOB_LOGS_TOOL, &err)),
        };
        let tail = match (args.tail, args.since_line) {
            (None, None) => Some(DEFAULT_LOG_TAIL),
            (tail, _) => tail,
        };
        let query = LogQuery {
            tail,
            since_line: args.since_line,
            grep: args.grep.filter(|needle| !needle.is_empty()),
            max_lines: MAX_LOG_LINES,
            max_bytes: MAX_LOG_BYTES / streams.len(),
        };
        let mut value = json!({ "job_id": id.as_str() });
        for stream in streams {
            let path = dir.join(stream);
            let query = query.clone();
            let slice = tokio::task::spawn_blocking(move || read_log(&path, &query))
                .await
                .map_err(|err| ToolsError::ExecutionFailed(err.to_string()))?;
            let key = stream.trim_end_matches(".log");
            match slice {
                Ok(slice) => value[key] = slice_json(&slice),
                Err(err) => {
                    return Ok(ToolOutput::error(format!(
                        "{JOB_LOGS_TOOL}: reading {stream} failed: {err}"
                    )));
                }
            }
        }
        if let Ok(status) = self.shared.manager.status(&id, caller) {
            value["state"] = json!(status.meta.state.as_str());
        }
        Ok(ToolOutput::json(value))
    }

    async fn stop(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        let args: StopArgs = parse_args(JOB_STOP_TOOL, call)?;
        let id = parse_job_id(JOB_STOP_TOOL, &args.job_id)?;
        let signal = match args
            .signal
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => JobSignal::Term,
            Some(raw) => JobSignal::parse(raw).ok_or_else(|| {
                invalid(
                    JOB_STOP_TOOL,
                    format!("signal must be TERM, INT, HUP or KILL, got `{raw}`"),
                )
            })?,
        };
        // Kein `ExecuteProcess`: `job.stop` startet nichts, es beendet nur
        // einen Job des Aufrufers bzw. seiner Nachfahren (Besitzprüfung im
        // Manager). So können Orchestratoren ohne Shell Jobs ihrer Worker
        // stoppen; im Plan-/Explore-Modus ist das Werkzeug nicht aktiv.
        let caller = Caller::Agent(context.session_id().as_str());
        match self.shared.manager.stop(&id, caller, signal).await {
            Ok(status) => {
                info!(job_id = %id, signal = signal.name(), "job.stop");
                let mut value = status_json(&status);
                value["signal_sent"] = json!(signal.name());
                Ok(ToolOutput::json(value))
            }
            Err(err) => Ok(job_error(JOB_STOP_TOOL, &err)),
        }
    }

    async fn wait(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        let args: WaitArgs = parse_args(JOB_WAIT_TOOL, call)?;
        let id = parse_job_id(JOB_WAIT_TOOL, &args.job_id)?;
        let secs = match args.timeout_secs {
            Some(secs) if (1..=MAX_WAIT_SECS).contains(&secs) => secs,
            Some(secs) if secs > MAX_WAIT_SECS => MAX_WAIT_SECS,
            _ => {
                return Err(invalid(
                    JOB_WAIT_TOOL,
                    format!("timeout_secs must be between 1 and {MAX_WAIT_SECS}"),
                ));
            }
        };
        let caller = Caller::Agent(context.session_id().as_str());
        match self
            .shared
            .manager
            .wait(&id, caller, Duration::from_secs(secs), context.cancel())
            .await
        {
            Ok((WaitOutcome::Cancelled, _)) => Err(ToolsError::Cancelled),
            Ok((outcome, status)) => Ok(ToolOutput::json(json!({
                "outcome": outcome.as_str(),
                "waited_max_secs": secs,
                "status": status_json(&status),
            }))),
            Err(err) => Ok(job_error(JOB_WAIT_TOOL, &err)),
        }
    }

    fn status(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        let args: JobIdArgs = parse_args(JOB_STATUS_TOOL, call)?;
        let id = parse_job_id(JOB_STATUS_TOOL, &args.job_id)?;
        let caller = Caller::Agent(context.session_id().as_str());
        Ok(match self.shared.manager.status(&id, caller) {
            Ok(status) => ToolOutput::json(status_json(&status)),
            Err(err) => job_error(JOB_STATUS_TOOL, &err),
        })
    }

    fn list(
        &self,
        context: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        let args: ListArgs = parse_args(JOB_LIST_TOOL, call)?;
        let kind = parse_list_kind(args.kind.as_deref())?;
        let caller = Caller::Agent(context.session_id().as_str());
        Ok(ToolOutput::json(match kind {
            Some(ListKind::Work) => list_work_rows_json(),
            Some(ListKind::Process) => list_process_rows_json(&self.shared.manager.list(caller)),
            None => list_summary_json(&self.shared.manager.list(caller)),
        }))
    }
}

impl ToolExecutor for JobToolExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            match self.kind {
                Kind::Start => self.start(context, call).await,
                Kind::Status => self.status(context, call),
                Kind::Logs => self.logs(context, call).await,
                Kind::Stop => self.stop(context, call).await,
                Kind::List => self.list(context, call),
                Kind::Wait => self.wait(context, call).await,
            }
        })
    }
}

impl ToolProvider for JobToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        tool_specs()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        let kind = Kind::from_name(name.as_str())?;
        debug_assert_eq!(kind.name(), name.as_str());
        Some(Arc::new(JobToolExecutor {
            kind,
            shared: Arc::clone(&self.shared),
        }))
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        JOB_READ_TOOLS.contains(&name.as_str())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod list_view_tests {
    //! Zusammenfassung/Zeilen von `job.list` und die neuen Statusfelder,
    //! gebaut aus `meta.json`-Fixtures (ohne laufende Prozesse).

    use super::*;
    use crate::model::JobMeta;
    use crate::test_support::{TestError, TestResult, ctx};

    /// v2-`meta.json` mit Pflichtfeldern; `extra` überschreibt Felder.
    fn status_of(id: &str, state: &str, extra: Value) -> TestResult<JobStatus> {
        let mut meta = json!({
            "version": 2,
            "job_id": id,
            "name": "fixture",
            "command": "true",
            "state": state,
            "harw_instance": "harw-test",
            "owner": {"session": "agent-x"},
            "created_at": "2026-01-01T00:00:00Z",
            "notify_every_secs": 60,
        });
        if let (Some(target), Value::Object(fields)) = (meta.as_object_mut(), extra) {
            for (key, value) in fields {
                target.insert(key, value);
            }
        }
        let meta: JobMeta = serde_json::from_value(meta).map_err(ctx("parse meta fixture"))?;
        Ok(JobStatus {
            meta,
            runtime_secs: None,
            stdout_lines: 0,
            stderr_lines: 0,
            last_lines: Vec::new(),
            log_dir: PathBuf::new(),
        })
    }

    #[test]
    fn test_list_summary_counts_per_state() -> TestResult {
        let jobs = vec![
            status_of("job-a", "running", json!({}))?,
            status_of("job-b", "running", json!({}))?,
            status_of("job-c", "failed", json!({"exit_code": 1}))?,
        ];
        let value = list_summary_json(&jobs);
        assert_eq!(value["view"], json!("summary"));
        assert_eq!(value["count"], json!(3));
        assert_eq!(value["categories"]["process"]["count"], json!(3));
        assert_eq!(
            value["categories"]["process"]["by_state"],
            json!({"failed": 1, "running": 2})
        );
        assert!(value.get("jobs").is_none(), "{value}");
        assert_eq!(value["categories"]["work"]["visible"], json!(false));
        assert_eq!(
            value["categories"]["work"]["note"],
            json!(WORK_NOT_VISIBLE)
        );
        Ok(())
    }

    #[test]
    fn test_list_rows_process_carry_owner_profile_end_reason() -> TestResult {
        let jobs = vec![
            status_of(
                "job-host",
                "succeeded",
                json!({"executed_on_host": true, "exit_code": 0}),
            )?,
            status_of(
                "job-bwrap",
                "stopped",
                json!({"signal": 15, "stop_requested": true}),
            )?,
        ];
        let value = list_process_rows_json(&jobs);
        assert_eq!(value["view"], json!("rows"));
        assert_eq!(value["kind"], json!("process"));
        assert_eq!(value["count"], json!(2));
        assert_eq!(value["truncated"], json!(false));
        let rows = value["jobs"]
            .as_array()
            .ok_or(TestError::Missing("jobs array"))?;
        assert_eq!(rows.len(), 2);
        let expected = [("job-host", "host", "exited"), ("job-bwrap", "bwrap", "stopped")];
        for (row, (id, profile, reason)) in rows.iter().zip(expected) {
            assert_eq!(row["job_id"], json!(id));
            assert_eq!(row["kind"], json!("process"));
            assert_eq!(row["owner"], json!("agent-x"));
            assert_eq!(row["sandbox_profile"], json!(profile));
            assert_eq!(row["end_reason"], json!(reason));
        }
        Ok(())
    }

    #[test]
    fn test_list_kind_parsing() -> TestResult {
        assert_eq!(parse_list_kind(None).map_err(ctx("none"))?, None);
        assert_eq!(parse_list_kind(Some("")).map_err(ctx("empty"))?, None);
        assert_eq!(parse_list_kind(Some("  ")).map_err(ctx("blank"))?, None);
        assert_eq!(
            parse_list_kind(Some("process")).map_err(ctx("process"))?,
            Some(ListKind::Process)
        );
        assert_eq!(
            parse_list_kind(Some(" work ")).map_err(ctx("work"))?,
            Some(ListKind::Work)
        );
        match parse_list_kind(Some("all")) {
            Err(ToolsError::InvalidArguments { name, reason }) => {
                assert_eq!(name, JOB_LIST_TOOL);
                assert!(reason.contains("`all`"), "{reason}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected InvalidArguments, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_list_work_rows_are_empty_with_note() -> TestResult {
        let value = list_work_rows_json();
        assert_eq!(value["view"], json!("rows"));
        assert_eq!(value["kind"], json!("work"));
        assert_eq!(value["count"], json!(0));
        assert_eq!(value["truncated"], json!(false));
        assert_eq!(value["jobs"], json!([]));
        assert_eq!(value["note"], json!(WORK_NOT_VISIBLE));
        Ok(())
    }

    #[test]
    fn test_status_json_reports_profile_end_reason_reaped_truncated_warnings() -> TestResult {
        let status = status_of(
            "job-status",
            "failed",
            json!({
                "executed_on_host": true,
                "signal": 9,
                "stragglers_reaped": true,
                "log_truncated": true,
                "launch_warnings": ["x"],
            }),
        )?;
        let value = status_json(&status);
        assert_eq!(value["executed_on"], json!("host"));
        assert_eq!(value["sandbox_profile"], json!("host"));
        assert_eq!(value["end_reason"], json!("signal"));
        assert_eq!(value["stragglers_reaped"], json!(true));
        assert_eq!(value["log_truncated"], json!(true));
        assert_eq!(value["launch_warnings"], json!(["x"]));

        // Altes `meta.json` ohne die neuen Felder: Vorgaben, laufender Job
        // hat keinen Endgrund.
        let old = status_json(&status_of("job-old", "running", json!({}))?);
        assert_eq!(old["sandbox_profile"], json!("bwrap"));
        assert_eq!(old["end_reason"], Value::Null);
        assert_eq!(old["stragglers_reaped"], json!(false));
        assert_eq!(old["log_truncated"], json!(false));
        assert_eq!(old["launch_warnings"], json!([]));
        Ok(())
    }
}
