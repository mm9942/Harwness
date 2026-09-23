//! `ProcessToolProvider` — die Werkzeuge `process.list` und `process.kill`
//! über `harw_killer::api`.
//!
//! # Verantwortung
//! - `process.list` — Snapshot plus Auswahl über `harw_killer::api::preview`;
//!   liefert die getroffenen Prozesse als JSON (`pid`, `ppid`, `uid`, `name`,
//!   `command`). Sendet **nie** ein Signal.
//! - `process.kill` — dieselbe Auswahl, dann `harw_killer::api::kill_own`:
//!   SIGKILL, Warten bis `timeout_secs`, zweites SIGKILL an Überlebende über
//!   denselben pidfd, Warten bis `kill_wait_secs`. Fremde Prozesse (anderer
//!   effektiver Benutzer) werden nur berichtet, nie über sudo beendet.
//!
//! # Sicherheitskontrakt
//! - Permission: [`harw_authority::Permission::ExecuteProcess`] für **beide**
//!   Werkzeuge, vom Prolog von `#[harw_macros::tool]` geprüft, bevor
//!   Argumente deserialisiert werden. Auch `process.list` bekommt nicht
//!   `ReadWorkspace`: es liest Host-Zustand außerhalb des Workspace, und
//!   Kommandozeilen fremder Prozesse können Geheimnisse enthalten. Die
//!   Werkzeuge erscheinen damit nur in Profilen, die ohnehin Prozesse
//!   ausführen dürfen (wie `shell.exec`).
//! - Freigabe: keines der beiden Werkzeuge gehört in
//!   `AUTO_APPROVED_TOOLS`; `process.kill` ist destruktiv und gehört in
//!   `ALWAYS_ASK_TOOLS` (`harw-registry-defaults`), damit es auch unter
//!   `FullAccess` und trotz passender Allow-Regel immer rückfragt.
//! - Eine leere Auswahl (weder `names` noch `pids`) wird abgewiesen, bevor
//!   `harw-killer` überhaupt aufgerufen wird — nie „alles“ auswählen. `uid`
//!   allein ist kein Selektor, sondern nur ein Filter.
//! - PID 1, der eigene Prozess und seine Vorfahren schützt `harw-killer`
//!   selbst; dieses Crate lockert keine dieser Regeln.
//! - Jede Antwort ist auf [`MAX_OUTPUT_BYTES`] begrenzt; Kürzungen werden
//!   im Ergebnis vermerkt.
//!
//! # Plattform
//! Nur Linux. Auf anderen Zielen ist `harw-killer` keine Abhängigkeit; beide
//! Werkzeuge validieren ihre Argumente trotzdem und liefern dann einen
//! Tool-Fehler.
//!
//! # Nebenläufigkeit
//! Zustandslos. `process.list` ist `parallel_safe`, `process.kill` nicht
//! (Nebenwirkung). Die blockierenden Aufrufe laufen in
//! `tokio::task::spawn_blocking`.

use harw_tools::{ToolOutput, ToolsError, executor::ToolExecutionContext};
use serde::Deserialize;
use serde_json::{Value, json};

/// Namen beider Prozess-Werkzeuge (in Registrierungsreihenfolge).
pub const PROCESS_TOOL_NAMES: [&str; 2] = ["process.list", "process.kill"];

/// Obergrenze einer einzelnen Tool-Antwort (≈ 64 KiB).
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Standard-Wartezeit nach dem ersten SIGKILL (Sekunden).
pub const DEFAULT_TIMEOUT_SECS: u64 = 5;

/// Höchste erlaubte Wartezeit nach dem ersten SIGKILL (Sekunden).
pub const MAX_TIMEOUT_SECS: u64 = 60;

/// Standard-Wartezeit nach dem zweiten SIGKILL (Sekunden).
pub const DEFAULT_KILL_WAIT_SECS: u64 = 2;

/// Höchste erlaubte Wartezeit nach dem zweiten SIGKILL (Sekunden).
pub const MAX_KILL_WAIT_SECS: u64 = 30;

/// Höchstzahl Prozessnamen pro Aufruf.
pub const MAX_NAMES: usize = 32;

/// Höchstzahl PIDs pro Aufruf.
pub const MAX_PIDS: usize = 256;

/// Höchstlänge eines Prozessnamens in Bytes.
pub const MAX_NAME_BYTES: usize = 256;

/// Budget für Listenelemente: Gesamtbudget abzüglich Reserve für Kopfdaten.
const ITEM_BUDGET: usize = MAX_OUTPUT_BYTES - 4 * 1024;

// ---------------------------------------------------------------------------
// Argumente
// ---------------------------------------------------------------------------

/// Argumente für `process.list`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct ProcessListArgs {
    /// Process names to select (exact process names as shown by ps). At least one of 'names' or 'pids' is required.
    pub names: Option<Vec<String>>,
    /// Process IDs to select (positive integers). At least one of 'names' or 'pids' is required.
    pub pids: Option<Vec<i64>>,
    /// Optional filter: only processes owned by this numeric user ID. Not a selector on its own.
    pub uid: Option<u32>,
}

/// Argumente für `process.kill`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
pub struct ProcessKillArgs {
    /// Process names to kill (exact process names as shown by ps). At least one of 'names' or 'pids' is required.
    pub names: Option<Vec<String>>,
    /// Process IDs to kill (positive integers). At least one of 'names' or 'pids' is required.
    pub pids: Option<Vec<i64>>,
    /// Optional filter: only processes owned by this numeric user ID. Not a selector on its own.
    pub uid: Option<u32>,
    /// Seconds to wait after the first SIGKILL before retrying survivors (1-60, default 5).
    pub timeout_secs: Option<u64>,
    /// Seconds to wait after the second SIGKILL (1-30, default 2).
    pub kill_wait_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Validierung (plattformunabhängig)
// ---------------------------------------------------------------------------

/// Geprüfte, plattformunabhängige Form einer Prozessauswahl.
///
/// Garantiert: mindestens ein Name oder eine PID; Namen getrimmt, nicht leer
/// und dedupliziert; PIDs positiv und in `i32`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct SelectorSpec {
    names: Vec<String>,
    pids: Vec<i32>,
    uid: Option<u32>,
}

/// Geprüfte Wartezeiten für `process.kill` (Sekunden).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct KillTimings {
    timeout_secs: u64,
    kill_wait_secs: u64,
}

/// Prüft die Selektorfelder und baut daraus eine [`SelectorSpec`].
///
/// Eine leere Auswahl (keine Namen, keine PIDs) ist ein Fehler — `uid` allein
/// würde sonst alle Prozesse eines Benutzers treffen.
fn validate_selector(
    names: Option<Vec<String>>,
    pids: Option<Vec<i64>>,
    uid: Option<u32>,
) -> Result<SelectorSpec, String> {
    let raw_names = names.unwrap_or_default();
    let raw_pids = pids.unwrap_or_default();
    if raw_names.len() > MAX_NAMES {
        return Err(format!("too many names (max {MAX_NAMES})"));
    }
    if raw_pids.len() > MAX_PIDS {
        return Err(format!("too many pids (max {MAX_PIDS})"));
    }

    let mut names = Vec::with_capacity(raw_names.len());
    for raw in raw_names {
        let name = raw.trim();
        if name.is_empty() {
            return Err("process names must not be empty".to_owned());
        }
        if name.len() > MAX_NAME_BYTES {
            return Err(format!(
                "process name longer than {MAX_NAME_BYTES} bytes: {}…",
                name.chars().take(32).collect::<String>()
            ));
        }
        if name.contains('\0') {
            return Err("process names must not contain NUL bytes".to_owned());
        }
        if !names.iter().any(|known: &String| known == name) {
            names.push(name.to_owned());
        }
    }

    let mut pids = Vec::with_capacity(raw_pids.len());
    for raw in raw_pids {
        let pid = i32::try_from(raw)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| format!("invalid pid {raw}: must be a positive process ID"))?;
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }

    if names.is_empty() && pids.is_empty() {
        return Err(
            "at least one selector is required: give 'names' and/or 'pids' ('uid' alone only filters)"
                .to_owned(),
        );
    }
    Ok(SelectorSpec { names, pids, uid })
}

/// Prüft eine optionale Sekundenangabe gegen `1..=max` und setzt den Standard.
fn validate_secs(value: Option<u64>, default: u64, max: u64, field: &str) -> Result<u64, String> {
    match value {
        None => Ok(default),
        Some(secs) if (1..=max).contains(&secs) => Ok(secs),
        Some(secs) => Err(format!("{field} must be between 1 and {max} seconds, got {secs}")),
    }
}

/// Prüft beide Wartezeiten von `process.kill`.
fn validate_timings(
    timeout_secs: Option<u64>,
    kill_wait_secs: Option<u64>,
) -> Result<KillTimings, String> {
    Ok(KillTimings {
        timeout_secs: validate_secs(
            timeout_secs,
            DEFAULT_TIMEOUT_SECS,
            MAX_TIMEOUT_SECS,
            "timeout_secs",
        )?,
        kill_wait_secs: validate_secs(
            kill_wait_secs,
            DEFAULT_KILL_WAIT_SECS,
            MAX_KILL_WAIT_SECS,
            "kill_wait_secs",
        )?,
    })
}

// ---------------------------------------------------------------------------
// Hilfsfunktionen
// ---------------------------------------------------------------------------

/// Nimmt Elemente auf, solange ihre serialisierte Größe in das Budget passt.
///
/// Liefert die aufgenommenen Werte und die Zahl der ausgelassenen Elemente.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn bounded_items(items: impl IntoIterator<Item = Value>, budget: usize) -> (Vec<Value>, usize) {
    let mut used = 0usize;
    let mut kept = Vec::new();
    let mut omitted = 0usize;
    for item in items {
        if omitted > 0 {
            omitted += 1;
            continue;
        }
        let size = item.to_string().len() + 1;
        if used + size > budget {
            omitted += 1;
            continue;
        }
        used += size;
        kept.push(item);
    }
    (kept, omitted)
}

/// Einheitliche Fehlerausgabe mit strukturiertem Log.
fn error_output(tool: &str, message: &str) -> ToolOutput {
    tracing::warn!(tool, error = %message, "process-Werkzeug abgebrochen");
    ToolOutput::error(format!("{tool}: {message}"))
}

// ---------------------------------------------------------------------------
// Plattformanbindung
// ---------------------------------------------------------------------------

/// Linux-Anbindung an `harw_killer::api`.
#[cfg(target_os = "linux")]
mod platform {
    use std::time::Duration;

    use harw_killer::api::{self, KillOptions, KillReport, KillResult, Selection, TargetInfo};
    use serde_json::{Value, json};

    use super::{ITEM_BUDGET, KillTimings, SelectorSpec, bounded_items};

    /// Übersetzt die geprüfte Auswahl in die `harw-killer`-Form.
    fn selection(spec: SelectorSpec) -> Selection {
        Selection {
            names: spec.names,
            pids: spec.pids,
            uid: spec.uid,
        }
    }

    /// Führt eine blockierende Arbeit außerhalb des Executors aus.
    async fn blocking<T, F>(work: F) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, harw_killer::Error> + Send + 'static,
        T: Send + 'static,
    {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|error| format!("background task failed: {error}"))?
            .map_err(|error| error.to_string())
    }

    /// JSON-Liste der Ziele, auf das Ausgabebudget begrenzt.
    fn targets_json(targets: &[TargetInfo]) -> Result<Value, String> {
        let mut items = Vec::with_capacity(targets.len());
        for target in targets {
            items.push(serde_json::to_value(target).map_err(|error| error.to_string())?);
        }
        let total = items.len();
        let (kept, omitted) = bounded_items(items, ITEM_BUDGET);
        let mut value = json!({
            "targets": kept,
            "count": total,
            "truncated": omitted > 0,
        });
        if omitted > 0 {
            value["note"] = json!(format!(
                "{omitted} targets omitted (output limit ~64 KiB); narrow the selection"
            ));
        }
        Ok(value)
    }

    /// `process.list`: Vorschau ohne Signal.
    pub(super) async fn list(spec: SelectorSpec) -> Result<Value, String> {
        let selection = selection(spec);
        let targets = blocking(move || api::preview(&selection)).await?;
        tracing::info!(targets = targets.len(), "process.list");
        targets_json(&targets)
    }

    /// `process.kill`: KILL/Warten/KILL für eigene Prozesse.
    pub(super) async fn kill(spec: SelectorSpec, timings: KillTimings) -> Result<Value, String> {
        let selection = selection(spec);
        let options = KillOptions {
            timeout: Duration::from_secs(timings.timeout_secs),
            kill_wait: Duration::from_secs(timings.kill_wait_secs),
        };
        let reports = blocking(move || api::kill_own(&selection, &options)).await?;
        reports_json(&reports)
    }

    /// JSON-Form der Berichte plus Zusammenfassung je Ergebnisart.
    fn reports_json(reports: &[KillReport]) -> Result<Value, String> {
        let mut killed = 0usize;
        let mut already_exited = 0usize;
        let mut survived = 0usize;
        let mut errors = 0usize;
        for report in reports {
            match report.result {
                KillResult::Killed | KillResult::KilledAfterRetry => killed += 1,
                KillResult::AlreadyExited => already_exited += 1,
                KillResult::Survived => survived += 1,
                KillResult::Error(_) => errors += 1,
            }
        }
        tracing::info!(
            targets = reports.len(),
            killed,
            already_exited,
            survived,
            errors,
            "process.kill"
        );

        let mut items = Vec::with_capacity(reports.len());
        for report in reports {
            items.push(serde_json::to_value(report).map_err(|error| error.to_string())?);
        }
        let total = items.len();
        let (kept, omitted) = bounded_items(items, ITEM_BUDGET);
        let mut value = json!({
            "reports": kept,
            "count": total,
            "summary": {
                "killed": killed,
                "already_exited": already_exited,
                "survived": survived,
                "errors": errors,
            },
            "all_terminated": survived == 0 && errors == 0,
            "truncated": omitted > 0,
        });
        if omitted > 0 {
            value["note"] = json!(format!("{omitted} reports omitted (output limit ~64 KiB)"));
        }
        Ok(value)
    }
}

/// Ersatz auf Nicht-Linux-Zielen: jeder Aufruf ist ein klarer Tool-Fehler.
#[cfg(not(target_os = "linux"))]
mod platform {
    use serde_json::Value;

    use super::{KillTimings, SelectorSpec};

    /// Fehlermeldung für nicht unterstützte Plattformen.
    const UNSUPPORTED: &str =
        "process tools are only supported on Linux (pidfd-based process identity)";

    /// `process.list` ist außerhalb von Linux nicht verfügbar.
    pub(super) async fn list(_spec: SelectorSpec) -> Result<Value, String> {
        Err(UNSUPPORTED.to_owned())
    }

    /// `process.kill` ist außerhalb von Linux nicht verfügbar.
    pub(super) async fn kill(_spec: SelectorSpec, _timings: KillTimings) -> Result<Value, String> {
        Err(UNSUPPORTED.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Zeigt, welche Prozesse eine Auswahl treffen würde, ohne zu signalisieren.
#[harw_macros::tool(
    name = "process.list",
    description = "Lists the processes a selection would match, without sending any signal. \
                   Select by 'names' (exact process names) and/or 'pids'; at least one of the \
                   two is required. Optional 'uid' only keeps processes of that user. PID 1, \
                   the agent's own process and its ancestors are never selected. Returns JSON \
                   {targets:[{pid, ppid, uid, name, command}], count, truncated}. Use this \
                   before process.kill to confirm the targets. Linux only. Read-only.",
    permission = "execute_process",
    parallel_safe
)]
async fn process_list(
    _context: &ToolExecutionContext,
    args: ProcessListArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "process.list";

    let spec = match validate_selector(args.names, args.pids, args.uid) {
        Ok(spec) => spec,
        Err(message) => return Ok(error_output(TOOL, &message)),
    };
    match platform::list(spec).await {
        Ok(value) => Ok(ToolOutput::json(value)),
        Err(message) => Ok(error_output(TOOL, &message)),
    }
}

/// Beendet ausgewählte eigene Prozesse mit SIGKILL (zweiter Versuch nach Timeout).
#[harw_macros::tool(
    name = "process.kill",
    description = "DESTRUCTIVE: terminates the selected processes with SIGKILL (no graceful \
                   shutdown). Select by 'names' (exact process names) and/or 'pids'; at least \
                   one of the two is required; optional 'uid' only filters. Survivors get a \
                   second SIGKILL after 'timeout_secs' (1-60, default 5); the tool then waits \
                   'kill_wait_secs' (1-30, default 2). Only processes of the current user are \
                   killed; processes of other users are reported as errors and never killed \
                   (no sudo). PID 1, the agent's own process and its ancestors are protected. \
                   Run process.list first. Returns JSON {reports:[{target, result}], count, \
                   summary, all_terminated}. Linux only. Requires user approval.",
    permission = "execute_process"
)]
async fn process_kill(
    _context: &ToolExecutionContext,
    args: ProcessKillArgs,
) -> Result<ToolOutput, ToolsError> {
    const TOOL: &str = "process.kill";

    let spec = match validate_selector(args.names, args.pids, args.uid) {
        Ok(spec) => spec,
        Err(message) => return Ok(error_output(TOOL, &message)),
    };
    let timings = match validate_timings(args.timeout_secs, args.kill_wait_secs) {
        Ok(timings) => timings,
        Err(message) => return Ok(error_output(TOOL, &message)),
    };
    tracing::info!(
        names = ?spec.names,
        pids = ?spec.pids,
        uid = ?spec.uid,
        timeout_secs = timings.timeout_secs,
        kill_wait_secs = timings.kill_wait_secs,
        "process.kill angefordert"
    );
    match platform::kill(spec, timings).await {
        Ok(value) => Ok(ToolOutput::json(value)),
        Err(message) => Ok(error_output(TOOL, &message)),
    }
}

harw_tools::tool_provider! {
    /// Stellt die Prozess-Werkzeuge bereit: `process.list` (lesend) und
    /// `process.kill` (destruktiv, freigabepflichtig).
    pub struct ProcessToolProvider {
        ProcessListTool,
        ProcessKillTool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_extension_api::contributors::ToolProvider as _;
    use harw_tools::{ToolCall, ToolName};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fmt;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Test-Fehlertyp: ersetzt panic!/unwrap/expect in Tests.
    enum TestError {
        Missing(&'static str),
        Unexpected(String),
        Context {
            context: &'static str,
            source: String,
        },
    }

    type TestResult<T = ()> = Result<T, TestError>;

    impl fmt::Debug for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
                Self::Unexpected(message) => write!(f, "unerwartetes Ergebnis: {message}"),
                Self::Context { context, source } => write!(f, "{context}: {source}"),
            }
        }
    }

    /// Übersetzt `.expect("…")` in `.map_err(ctx("…"))?`.
    fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
        move |error| TestError::Context {
            context,
            source: error.to_string(),
        }
    }

    /// Ausführungskontext mit Workspace `<harness>/ws`.
    fn sandbox_context(
        harness_root: &Path,
        permissions: Vec<Permission>,
    ) -> TestResult<ToolExecutionContext> {
        fs::create_dir_all(harness_root.join("ws")).map_err(ctx("Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        Ok(ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox,
        ))
    }

    /// Führt ein Tool über den Provider aus.
    fn run(context: &ToolExecutionContext, name: &str, arguments: Value) -> TestResult<ToolOutput> {
        let provider = ProcessToolProvider::new();
        let executor = provider
            .executor(&ToolName::new(name))
            .ok_or(TestError::Missing("Executor vorhanden"))?;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("Tokio-Runtime bauen"))?;
        runtime
            .block_on(executor.execute(context, &call))
            .map_err(ctx("Tool läuft"))
    }

    fn expect_error(output: ToolOutput) -> TestResult<String> {
        match output {
            ToolOutput::Error { message } => Ok(message),
            other => Err(TestError::Unexpected(format!("Fehler erwartet: {other:?}"))),
        }
    }

    #[test]
    fn test_provider_lists_both_tools_in_order() {
        assert_eq!(ProcessToolProvider::TOOL_NAMES, PROCESS_TOOL_NAMES.as_slice());
        let provider = ProcessToolProvider::new();
        let tools = provider.tools();
        let names: Vec<&str> = tools.iter().map(harw_tools::ToolSpec::name).collect();
        assert_eq!(names, PROCESS_TOOL_NAMES.to_vec());
    }

    #[test]
    fn test_provider_permissions_and_parallel_safety() {
        let provider = ProcessToolProvider::new();
        for permission in ProcessToolProvider::TOOL_PERMISSIONS {
            assert_eq!(*permission, Some(Permission::ExecuteProcess));
        }
        assert!(provider.parallel_safe(&ToolName::new("process.list")));
        assert!(!provider.parallel_safe(&ToolName::new("process.kill")));
        assert!(!provider.parallel_safe(&ToolName::new("process.unknown")));
        assert!(provider.executor(&ToolName::new("process.unknown")).is_none());
    }

    #[test]
    fn test_validate_selector_requires_names_or_pids() {
        assert!(validate_selector(None, None, None).is_err());
        assert!(validate_selector(Some(Vec::new()), Some(Vec::new()), None).is_err());
        // `uid` allein ist nur ein Filter, kein Selektor.
        assert!(validate_selector(None, None, Some(1000)).is_err());
    }

    #[test]
    fn test_validate_selector_normalizes_and_dedups() -> TestResult {
        let spec = validate_selector(
            Some(vec![" sleep ".to_owned(), "sleep".to_owned()]),
            Some(vec![42, 42, 7]),
            Some(1000),
        )
        .map_err(ctx("gültige Auswahl"))?;
        assert_eq!(
            spec,
            SelectorSpec {
                names: vec!["sleep".to_owned()],
                pids: vec![42, 7],
                uid: Some(1000),
            }
        );
        let only_pid = validate_selector(None, Some(vec![1234]), None).map_err(ctx("nur PID"))?;
        assert!(only_pid.names.is_empty());
        Ok(())
    }

    #[test]
    fn test_validate_selector_rejects_bad_values() {
        assert!(validate_selector(Some(vec!["  ".to_owned()]), None, None).is_err());
        assert!(validate_selector(Some(vec!["a\0b".to_owned()]), None, None).is_err());
        assert!(validate_selector(Some(vec!["x".repeat(MAX_NAME_BYTES + 1)]), None, None).is_err());
        assert!(validate_selector(None, Some(vec![0]), None).is_err());
        assert!(validate_selector(None, Some(vec![-1]), None).is_err());
        assert!(validate_selector(None, Some(vec![i64::from(i32::MAX) + 1]), None).is_err());
        let too_many_names = (0..=MAX_NAMES).map(|i| format!("p{i}")).collect();
        assert!(validate_selector(Some(too_many_names), None, None).is_err());
        let too_many_pids = (1..=(MAX_PIDS as i64 + 1)).collect();
        assert!(validate_selector(None, Some(too_many_pids), None).is_err());
    }

    #[test]
    fn test_validate_timings_defaults_and_bounds() -> TestResult {
        let defaults = validate_timings(None, None).map_err(ctx("Standardwerte"))?;
        assert_eq!(
            defaults,
            KillTimings {
                timeout_secs: DEFAULT_TIMEOUT_SECS,
                kill_wait_secs: DEFAULT_KILL_WAIT_SECS,
            }
        );
        let edges = validate_timings(Some(1), Some(MAX_KILL_WAIT_SECS)).map_err(ctx("Grenzen"))?;
        assert_eq!(edges.timeout_secs, 1);
        assert_eq!(edges.kill_wait_secs, MAX_KILL_WAIT_SECS);
        assert!(validate_timings(Some(MAX_TIMEOUT_SECS), Some(1)).is_ok());

        assert!(validate_timings(Some(0), None).is_err());
        assert!(validate_timings(Some(MAX_TIMEOUT_SECS + 1), None).is_err());
        assert!(validate_timings(None, Some(0)).is_err());
        assert!(validate_timings(None, Some(MAX_KILL_WAIT_SECS + 1)).is_err());
        Ok(())
    }

    #[test]
    fn test_bounded_items_counts_omitted() {
        let items = (0..100).map(|i| json!({ "i": i, "pad": "x".repeat(50) }));
        let (kept, omitted) = bounded_items(items, 700);
        assert!(!kept.is_empty());
        assert_eq!(kept.len() + omitted, 100);
        assert!(omitted > 0);
    }

    /// Ohne Selektor bricht jedes Werkzeug ab, bevor `harw-killer` läuft.
    #[test]
    fn test_tools_reject_missing_selector() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = sandbox_context(harness.path(), vec![Permission::ExecuteProcess])?;
        for name in PROCESS_TOOL_NAMES {
            let message = expect_error(run(&context, name, json!({}))?)?;
            assert!(message.contains("at least one selector"), "{name}: {message}");
            let message = expect_error(run(&context, name, json!({ "uid": 0 }))?)?;
            assert!(message.contains("at least one selector"), "{name}: {message}");
        }
        Ok(())
    }

    /// Ungültige Wartezeiten brechen `process.kill` ab, bevor ein Signal
    /// gesendet werden könnte (die PID würde sonst tatsächlich getroffen).
    #[test]
    fn test_kill_rejects_out_of_range_timeouts() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = sandbox_context(harness.path(), vec![Permission::ExecuteProcess])?;
        let pid = i64::from(i32::MAX);
        for arguments in [
            json!({ "pids": [pid], "timeout_secs": 0 }),
            json!({ "pids": [pid], "timeout_secs": 61 }),
            json!({ "pids": [pid], "kill_wait_secs": 0 }),
            json!({ "pids": [pid], "kill_wait_secs": 31 }),
        ] {
            let message = expect_error(run(&context, "process.kill", arguments.clone())?)?;
            assert!(message.contains("seconds"), "{arguments}: {message}");
        }
        Ok(())
    }

    #[test]
    fn test_tools_deny_without_execute_process() -> TestResult {
        let harness = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let context = sandbox_context(harness.path(), vec![Permission::ReadWorkspace])?;
        for name in PROCESS_TOOL_NAMES {
            let message = expect_error(run(&context, name, json!({ "pids": [1] }))?)?;
            assert!(message.contains("ExecuteProcess"), "{name}: {message}");
        }
        Ok(())
    }
}
