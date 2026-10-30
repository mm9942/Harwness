//! `harw dod …` (PL-97 D3): the Harw-DoD suite from the command line.
//!
//! # Read-only actions
//! `status`, `doctor`, `findings`, `sensors` and `config check|suggest`
//! read through `harw-dod-client` and render with the helpers of
//! `harw_ops::dod`, so the terminal shows what `/dod` and the `dod.*` model
//! tools show. `--json` prints the structured form through the [`Printer`].
//!
//! # Delegated cgroup root (PL-97 E3)
//! `config suggest` prints the Harw-DoD profile for harw's job cgroups
//! under the delegated root `R` that `harw_job_runtime::host::resolve_delegated_root_with`
//! resolves from `[jobs] cgroup_root` → `HARW_CGROUP_ROOT` only. The own
//! cgroup (resolver step 3) is skipped: it is the cgroup of this CLI
//! process (a terminal's scope, or the sudo session), never harw's job
//! root. Without a root it prints the template plus the known service roots
//! as candidates. It also prints the matching `[dod] allowed_cgroup_prefixes = ["<R>"]`;
//! `--drop-in` prints the sentinel drop-in (`--cgroup-root <R>`) and the
//! `harw-probe-bpf` ordering drop-in (the probe refuses to start while no
//! configured cgroup root resolves, so `R` must exist first). Values are
//! escaped for TOML and for systemd's `ExecStart=`. `doctor`
//! adds three warnings-only checks: the root resolves, the active profile
//! covers it, every allowed prefix lies within it.
//!
//! # Execution chain (PL-97 E5)
//! `doctor` also prints the [`ExecutionChainReport`] table (and
//! `execution_chain` in `--json`): Installed (`harw`, `killer`,
//! `harw-agent-runner`, `harw-job-exec`), Configured (config, cgroup root),
//! Running (`harw-serve`, `harw-dod.target`), Ready (bwrap, trampoline,
//! Landlock ABI, profile coverage, Warden, serve worker root). The facts
//! are collected by [`chain_inputs`], which `harw doctor` and the verify
//! runner share. The table is informational: it never changes the exit
//! code of `doctor`.
//!
//! # Service control (root only)
//! The mutating actions mirror `dod/Makefile` and `dod/scripts/service.sh`
//! step by step; they only call `systemctl` and write the one Warden flag
//! file, and never run `cargo` or `make`:
//!
//! | action | steps |
//! |---|---|
//! | `enable` | config check (present, valid, `active_profile`), `systemctl daemon-reload`, `systemctl enable --now harw-dod.target` |
//! | `disable` | `systemctl disable --now harw-dod.target harw-sentinel.service harw-probe-bpf.service harw-probe-fs.service`, then `systemctl disable --now harw-warden.socket harw-warden.service` (failure ignored) |
//! | `restart` | config check, `systemctl daemon-reload`, `systemctl restart harw-dod.target` |
//! | `warden enable` | `install -d -m 0750 /etc/harw-dod` (only when missing), create `/etc/harw-dod/warden.enable`, `systemctl daemon-reload`, `systemctl enable --now harw-warden.socket` |
//! | `warden disable` | `systemctl disable --now harw-warden.socket harw-warden.service`, remove the flag file |
//!
//! `warden disable` has no Makefile target; it undoes `enable-warden`.
//!
//! # Exit codes (PL-95 §3.4)
//! 0 ok, 1 failure, 2 usage (clap), 3 refused: not root, or no valid
//! `active_profile` for `enable`/`restart`.
//!
//! # Trust boundary
//! The read-only actions run as any user. The service control refuses
//! unless the effective uid is 0 (checked before anything else, like
//! `service.sh require_root`). No action reaches the escalation ladder;
//! `warden enable` only arms the socket, every kill still needs a
//! human-approved proof (`kill_requires_human`).

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use harw_config::DodSection;
use harw_dod_client::{
    ChainBinary, ChainDod, ChainHost, ChainInputs, ChainRoot, ConfigReport, DodClientContext,
    DodClientError, DodStatus, ExecutionChainReport, ExportState, ProfileCoverage, ServeWorkspace,
    ServiceState, WardenState, profile_coverage, same_or_below,
};
use harw_install::RenderPaths;
use harw_job_runtime::host::{
    CGROUP2_MOUNT, OwnCgroup, RootConfig, RootEnv, RootResolution, cgroup_path_below_mount,
    resolve_delegated_root_with,
};
use harw_ops::dod as ops;
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::cli::{
    DodAction, DodConfigAction, DodFindingsArgs, DodSeverity, DodWardenAction, GlobalArgs,
};
use crate::output::Printer;

/// Exit code: success.
pub(crate) const EXIT_OK: i32 = 0;
/// Exit code: the action failed.
pub(crate) const EXIT_FAILURE: i32 = 1;
/// Exit code: usage error.
pub(crate) const EXIT_USAGE: i32 = 2;
/// Exit code: refused (not root, no valid `active_profile`).
pub(crate) const EXIT_REFUSED: i32 = 3;

/// Units stopped by `disable` (`service.sh disable`, first call).
const OBSERVATION_UNITS: [&str; 4] = [
    "harw-dod.target",
    "harw-sentinel.service",
    "harw-probe-bpf.service",
    "harw-probe-fs.service",
];
/// The Warden units (`service.sh disable`, second call).
const WARDEN_UNITS: [&str; 2] = ["harw-warden.socket", "harw-warden.service"];
/// Longest kept `systemctl` error text, in characters.
const MAX_ERROR_CHARS: usize = 400;

/// Printed after `warden enable`.
const KILL_REMINDER: &str = "Reminder: kill_requires_human stays true — every freeze/kill still \
     needs a human-approved proof; harw itself never calls the Warden.";

/// Result of one action: exit code plus text and JSON form.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Outcome {
    /// Process exit code.
    pub(crate) code: i32,
    /// Text form.
    pub(crate) text: String,
    /// JSON form.
    pub(crate) json: Value,
}

impl Outcome {
    fn new(code: i32, text: impl Into<String>, json: Value) -> Self {
        Self {
            code,
            text: text.into(),
            json,
        }
    }
}

/// The side effects of the service control; tests inject a fake.
pub(crate) trait System {
    /// Effective uid of this process.
    fn euid(&self) -> u32;
    /// Runs `systemctl <args>`; `Err` with a bounded reason when it cannot
    /// be started or exits non-zero.
    fn systemctl(&mut self, args: &[&str]) -> Result<(), String>;
}

/// The real host.
struct HostSystem;

impl System for HostSystem {
    fn euid(&self) -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn systemctl(&mut self, args: &[&str]) -> Result<(), String> {
        // Ausgabe einfangen: systemctl schreibt Hinweise nach stderr, und
        // `--json` darf auf stdout nur das Ergebnis tragen.
        let output = Command::new("systemctl")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("systemctl could not be started: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "`systemctl {}` failed ({}): {}",
            args.join(" "),
            output.status,
            ops::bounded(stderr.trim())
        ))
    }
}

/// How the system configuration is read (tests inject a fixed report).
pub(crate) type ConfigReader = fn(Option<&Path>) -> Result<ConfigReport, DodClientError>;

/// Paths and readers of one invocation.
pub(crate) struct DodEnv {
    /// `harw-dod-client` context (export, config, libexec, flag, systemctl).
    pub(crate) client: DodClientContext,
    /// Where `doctor` looks.
    pub(crate) doctor: ops::DoctorEnv,
    /// Reads the system configuration for `config check`, `enable`, `restart`.
    pub(crate) read_config: ConfigReader,
    /// Notes about the harw home/config (shown with the status).
    pub(crate) notes: Vec<String>,
    /// PL-97 E3: the delegated cgroup root of harw's jobs, or why it was
    /// refused.
    pub(crate) cgroup_root: Result<RootResolution, String>,
    /// `[dod] allowed_cgroup_prefixes` of the harw config.
    pub(crate) allowed_cgroup_prefixes: Vec<String>,
    /// The cgroups of the harw service units ([`service_root_candidates`])
    /// and whether each exists; shown when no root resolves.
    pub(crate) root_candidates: Vec<(PathBuf, bool)>,
    /// PL-97 E5: the harw service unit cgroup that stands in when
    /// `cgroup_root` resolves to nothing ([`fallback_service_root`]), or why
    /// none counts. `doctor` folds it into the one root it reports.
    pub(crate) service_root: Result<Option<PathBuf>, String>,
    /// PL-97 E5: collects the execution chain facts around the root
    /// `doctor` reports; `None` leaves the chain table out.
    pub(crate) chain: Option<ChainCollector>,
    /// PL-97 E4: the job records of the project in the working directory
    /// (`<project>/.harw/state/jobs`) for `findings --job`; `None` outside a
    /// project.
    pub(crate) jobs_dir: Option<PathBuf>,
}

/// Collects the execution chain facts for a given cgroup root (production:
/// [`chain_inputs`]; tests inject fixed facts).
pub(crate) type ChainCollector = Box<dyn Fn(Result<ChainRoot, String>) -> ChainInputs>;

/// Runs `harw dod …`: prints the outcome and exits with its code.
///
/// # Errors
/// Only when the output cannot be written; every other failure is reported
/// on stdout with its exit code (1 failure, 2 usage, 3 refused).
pub fn run(global: &GlobalArgs, action: DodAction) -> Result<(), String> {
    let printer = Printer::new(global.output());
    let env = host_env(global.home.clone());
    let outcome = execute(action, &env, &mut HostSystem);
    printer.value(&outcome.text, outcome.json)?;
    if outcome.code != EXIT_OK {
        std::process::exit(outcome.code);
    }
    Ok(())
}

/// The environment of the running host: production paths, the harw home's
/// `[dod]` section and this process's `PATH`.
fn host_env(home_override: Option<PathBuf>) -> DodEnv {
    let mut notes = Vec::new();
    let home = crate::home::resolve_home(home_override).unwrap_or_else(|error| {
        notes.push(ops::bounded(&format!(
            "harw home not resolvable ({error}); [dod] settings and the home export fallback are skipped"
        )));
        PathBuf::from("/nonexistent/harw-home")
    });
    // Nur lesen: kein `ensure_home`, keine Migration — `sudo harw dod …`
    // darf im Home von root nichts anlegen.
    let (section, jobs) = harw_home::config_layers(&home)
        .map_err(|error| error.to_string())
        .and_then(|layers| harw_config::discover_config(&layers).map_err(|error| error.to_string()))
        .map(|config| (config.dod, config.harness.jobs))
        .unwrap_or_else(|error| {
            notes.push(ops::bounded(&format!(
                "harw config unreadable, default [dod] and [jobs] settings used: {error}"
            )));
            (DodSection::default(), Default::default())
        });
    // Schritt 3 (eigene cgroup) entfällt: das ist die cgroup dieses
    // CLI-Prozesses (Terminal-Scope oder sudo-Sitzung), nie harws Job-Wurzel.
    let cgroup_root = resolve_delegated_root_with(
        &RootConfig {
            cgroup_root: jobs.cgroup_root,
        },
        &RootEnv::from_process(),
        OwnCgroup::default,
    )
    .map_err(|error| error.to_string());
    let service_root = fallback_service_root(&cgroup_root);
    let root_candidates = service_root_candidates(invoking_uid())
        .into_iter()
        .map(|path| {
            let exists = path.is_dir();
            (path, exists)
        })
        .collect();
    DodEnv {
        client: ops::client_context(&home, &section, OffsetDateTime::now_utc()),
        doctor: ops::DoctorEnv::host(),
        read_config: harw_dod_client::read_system_config,
        notes,
        cgroup_root,
        allowed_cgroup_prefixes: section.allowed_cgroup_prefixes,
        root_candidates,
        service_root,
        chain: Some(Box::new(move |root| {
            chain_inputs(&home, ChainScope::Full, Some(root))
        })),
        jobs_dir: project_jobs_dir(),
    }
}

/// The job records of the project around the working directory (read-only,
/// nothing is created), as the TUI's job manager keeps them.
fn project_jobs_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let project = harw_home::project::discover_project(&cwd, &[]).ok()?;
    let state = harw_home::project::ProjectHome::at(&project).state_dir();
    Some(ops::jobs_dir(&state))
}

/// The uid whose user manager runs `harw-serve`: `SUDO_UID` under sudo,
/// else the real uid.
fn invoking_uid() -> u32 {
    let real = rustix::process::getuid().as_raw();
    if real == 0
        && let Some(uid) = std::env::var("SUDO_UID")
            .ok()
            .and_then(|uid| uid.trim().parse::<u32>().ok())
    {
        return uid;
    }
    real
}

/// The cgroups the harw service units run their jobs below (each a
/// delegated root while its unit runs): `harw-control.service` and the
/// `harw-serve` user unit of `uid`.
fn service_root_candidates(uid: u32) -> Vec<PathBuf> {
    let mount = Path::new(CGROUP2_MOUNT);
    vec![
        mount.join("system.slice/harw-control.service"),
        mount.join(format!(
            "user.slice/user-{uid}.slice/user@{uid}.service/app.slice/harw-serve.service"
        )),
    ]
}

/// Executes one action against `env` and `system`.
pub(crate) fn execute(action: DodAction, env: &DodEnv, system: &mut dyn System) -> Outcome {
    match action {
        DodAction::Status => {
            let snapshot = snapshot(env);
            Outcome::new(
                EXIT_OK,
                ops::status_text(&snapshot),
                ops::status_view(&snapshot),
            )
        }
        DodAction::Doctor => {
            let snapshot = snapshot(env);
            let mut report = ops::doctor(&env.doctor, &snapshot);
            // Eine Wurzel für die E3-Prüfungen und die Kettentabelle.
            let root = doctor_root(&env.cgroup_root, &env.service_root);
            add_checks(
                &mut report,
                cgroup_root_checks(&root, &env.allowed_cgroup_prefixes, &snapshot.status.config),
            );
            let code = if report.passed() {
                EXIT_OK
            } else {
                EXIT_FAILURE
            };
            let mut text = ops::doctor_text(&report);
            let mut view = ops::doctor_view(&report);
            if let Some(collect) = &env.chain {
                let mut inputs = collect(root);
                inputs.dod = Some(ChainDod::from_status(&snapshot.status));
                let chain = ExecutionChainReport::build(&inputs);
                text.push_str("\n\n");
                text.push_str(&chain.table());
                if let Some(object) = view.as_object_mut() {
                    object.insert(
                        "execution_chain".to_owned(),
                        serde_json::to_value(&chain).unwrap_or(Value::Null),
                    );
                }
            }
            Outcome::new(code, text, view)
        }
        DodAction::Findings(args) => findings(env, &args),
        DodAction::Sensors => {
            let snapshot = snapshot(env);
            let code = if matches!(
                snapshot.status.health_export,
                ExportState::Unreadable { .. }
            ) {
                EXIT_FAILURE
            } else {
                EXIT_OK
            };
            Outcome::new(
                code,
                ops::sensors_text(&snapshot),
                ops::sensors_view(&snapshot),
            )
        }
        DodAction::Config {
            action: DodConfigAction::Check { config },
        } => config_check(env, config.as_deref()),
        DodAction::Config {
            action: DodConfigAction::Suggest { drop_in },
        } => config_suggest(&env.cgroup_root, &env.root_candidates, drop_in),
        DodAction::Enable => enable(env, system),
        DodAction::Disable => disable(system),
        DodAction::Restart => restart(env, system),
        DodAction::Warden { action } => match action {
            DodWardenAction::Status => warden_status(env),
            DodWardenAction::Enable => warden_enable(env, system),
            DodWardenAction::Disable => warden_disable(env, system),
        },
        DodAction::Install { print_systemd } => install(print_systemd),
    }
}

fn snapshot(env: &DodEnv) -> ops::DodSnapshot {
    let mut snapshot = ops::read_snapshot(&env.client);
    snapshot.notes.extend(env.notes.iter().cloned());
    snapshot
}

fn severity(value: DodSeverity) -> harw_types::ImpactSeverity {
    match value {
        DodSeverity::Info => harw_types::ImpactSeverity::Info,
        DodSeverity::Low => harw_types::ImpactSeverity::Low,
        DodSeverity::Medium => harw_types::ImpactSeverity::Medium,
        DodSeverity::High => harw_types::ImpactSeverity::High,
        DodSeverity::Critical => harw_types::ImpactSeverity::Critical,
    }
}

fn findings(env: &DodEnv, args: &DodFindingsArgs) -> Outcome {
    let limit = match ops::findings_limit(args.limit) {
        Ok(limit) => limit,
        Err(error) => {
            return Outcome::new(
                EXIT_USAGE,
                error.to_string(),
                json!({ "error": error.to_string() }),
            );
        }
    };
    let mut query = ops::FindingsQuery {
        limit,
        min_severity: args.severity.map(severity),
        rule: args
            .rule
            .as_deref()
            .map(str::trim)
            .filter(|rule| !rule.is_empty())
            .map(str::to_owned),
        since: args.since,
        job: None,
    };
    if let Some(raw) = args.job.as_deref() {
        let Some(jobs_dir) = &env.jobs_dir else {
            let error = "no harw project here: `--job` needs the project's job records";
            return Outcome::new(EXIT_FAILURE, error.to_owned(), json!({ "error": error }));
        };
        match ops::job_filter_from_dir(jobs_dir, raw) {
            Ok(job) => query.job = Some(job),
            Err(error) => {
                return Outcome::new(
                    EXIT_USAGE,
                    error.to_string(),
                    json!({ "error": error.to_string() }),
                );
            }
        }
    }
    let selection = ops::select_findings(&snapshot(env), &query);
    let code = if matches!(selection.export, ExportState::Unreadable { .. }) {
        EXIT_FAILURE
    } else {
        EXIT_OK
    };
    Outcome::new(
        code,
        ops::findings_text(&selection),
        ops::findings_view(&selection),
    )
}

/// The delegated root and its mount-relative form, or the reason there is
/// none.
fn resolved_root(resolution: &Result<RootResolution, String>) -> Result<(PathBuf, String), String> {
    match resolution {
        Ok(RootResolution {
            root: Some(root), ..
        }) => cgroup_path_below_mount(root)
            .map(|relative| (root.clone(), relative))
            .ok_or_else(|| format!("{} is not below {CGROUP2_MOUNT}", root.display())),
        Ok(RootResolution { hint, .. }) => Err(hint
            .clone()
            .unwrap_or_else(|| "no delegated cgroup v2 root".to_owned())),
        Err(error) => Err(error.clone()),
    }
}

/// `value` as a TOML string (basic or literal, escaped as needed): cgroup
/// names under the user manager carry systemd escapes such as `\x2d`,
/// which are invalid escapes inside a hand-written basic string.
fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

/// `value` as one argument of a systemd `ExecStart=` line: `\` doubled,
/// `%` doubled (no specifier expansion), quoted when it contains
/// whitespace or quotes.
fn systemd_exec_arg(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('%', "%%");
    if escaped
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        format!("\"{}\"", escaped.replace('"', "\\\""))
    } else {
        escaped
    }
}

/// `value` for a `#` comment line: control characters (a newline would
/// end the comment) replaced by `?`.
fn comment_text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The line every suggestion carries: the probe resolves cgroup roots once
/// at start and refuses to start while none resolves.
const PROBE_ORDER_NOTE: &str = "# The root must exist before harw-probe-bpf starts: the probe refuses to\n\
     # start while no configured cgroup root resolves (`harw dod config suggest --drop-in`).\n";

/// The Harw-DoD profile for harw's job cgroups below `root` (`relative`:
/// its path below the cgroup2 mount, the profile's `cgroup_paths` form)
/// and the matching harw `[dod]` entry (canonical absolute form).
fn suggest_profile(root: &Path, relative: &str, source: &str) -> String {
    let root_text = root.to_string_lossy();
    let comment_root = comment_text(&root_text);
    let relative = toml_string(relative);
    let prefix = toml_string(&root_text);
    format!(
        "# Harw-DoD observation profile for harw's job cgroups (PL-97 E3).\n\
         # Delegated cgroup root: {comment_root} (source: {source}).\n\
         # Merge into /etc/harw-dod/config.toml and run `sudo harw dod enable`.\n\
         {PROBE_ORDER_NOTE}\
         schema_version = 1\n\
         mode = \"observe\"\n\
         active_profile = \"harw-jobs\"\n\
         \n\
         [profiles.harw-jobs]\n\
         scope = \"cgroups\"\n\
         cgroup_paths = [{relative}]\n\
         include_descendants = true\n\
         sensors = [\"exec\", \"tcp-connect\"]\n\
         egress_allow_cidrs = []\n\
         \n\
         # Matching entry in harw's own config (`~/.harw/config.toml`), canonical\n\
         # form (absolute, below {CGROUP2_MOUNT}):\n\
         # [dod]\n\
         # allowed_cgroup_prefixes = [{prefix}]\n"
    )
}

/// The template `config suggest` prints when no root resolves: the
/// placeholders in the two forms `[dod]` validation and the profile expect,
/// the ordering requirement, and the service roots that exist on this host
/// as candidates.
fn suggest_template(candidates: &[(PathBuf, bool)]) -> String {
    let mut text = String::from(
        "# Harw-DoD observation profile for harw's job cgroups (template).\n\
         # No delegated cgroup root resolved: set `[jobs] cgroup_root` (home or profile\n\
         # config) or run `HARW_CGROUP_ROOT=<root> harw dod config suggest` to get the\n\
         # placeholders filled in. <R> is that root without the /sys/fs/cgroup prefix.\n",
    );
    if !candidates.is_empty() {
        text.push_str(
            "# Roots of the harw service units (each exists only while its unit runs):\n",
        );
        for (path, exists) in candidates {
            let state = if *exists { "exists" } else { "not present" };
            text.push_str(&format!(
                "#   {} ({state})\n",
                comment_text(&path.to_string_lossy())
            ));
        }
    }
    text.push_str(PROBE_ORDER_NOTE);
    text.push_str(
        "schema_version = 1\n\
         mode = \"observe\"\n\
         active_profile = \"harw-jobs\"\n\
         \n\
         [profiles.harw-jobs]\n\
         scope = \"cgroups\"\n\
         cgroup_paths = [\"<R>\"]\n\
         include_descendants = true\n\
         sensors = [\"exec\", \"tcp-connect\"]\n\
         egress_allow_cidrs = []\n\
         \n\
         # Matching entry in harw's own config (`~/.harw/config.toml`), canonical\n\
         # form (absolute, below /sys/fs/cgroup):\n\
         # [dod]\n\
         # allowed_cgroup_prefixes = [\"/sys/fs/cgroup<R>\"]\n",
    );
    text
}

/// The system unit whose cgroup `relative` lies in (`/system.slice/<x>.service/…`),
/// if any: `harw-probe-bpf` can be ordered after it.
fn owning_system_unit(relative: &str) -> Option<&str> {
    relative
        .strip_prefix("/system.slice/")?
        .split('/')
        .next()
        .filter(|unit| unit.ends_with(".service") && unit.len() > ".service".len())
}

/// The drop-ins of `harw dod config suggest --drop-in`: the sentinel's
/// `--cgroup-root <root>` (its cgroup sensor then reports harw's
/// `harw-job-<attempt>` cgroups) and the ordering of `harw-probe-bpf`
/// behind the unit that creates the root.
fn suggest_drop_in(root: &Path, relative: &str) -> Result<String, String> {
    let unit = harw_install::print_systemd(Some("harw-sentinel.service"), &RenderPaths::default())
        .map_err(|error| error.to_string())?;
    let exec_start = unit
        .lines()
        .find(|line| line.starts_with("ExecStart="))
        .ok_or_else(|| "harw-sentinel.service has no ExecStart=".to_owned())?;
    let arg = systemd_exec_arg(&root.to_string_lossy());
    let root = comment_text(&root.to_string_lossy());
    let mut text = format!(
        "# /etc/systemd/system/harw-sentinel.service.d/harw-jobs.conf (PL-97 E3)\n\
         # The cgroup sensor reports the direct children of {root}: harw's job cgroups.\n\
         [Service]\n\
         ExecStart=\n\
         {exec_start} --cgroup-root {arg}\n\
         \n\
         # /etc/systemd/system/harw-probe-bpf.service.d/harw-jobs.conf\n\
         # harw-probe-bpf refuses to start while no configured cgroup root resolves,\n\
         # so {root} must exist before it starts.\n"
    );
    match owning_system_unit(relative) {
        Some(unit) => text.push_str(&format!(
            "# The probe resolves cgroup ids once at start, and every start of {unit}\n\
             # creates its cgroup anew: BindsTo= stops the probe when {unit} stops or\n\
             # fails, PartOf= restarts it with {unit}. After a crash and automatic\n\
             # restart of {unit}, run `sudo harw dod restart`.\n\
             [Unit]\nBindsTo={unit}\nPartOf={unit}\nAfter={unit}\n"
        )),
        None => text.push_str(
            "# The root belongs to no system unit (user manager or a transient scope), so\n\
             # systemd cannot order the probe after it: start harw first (for example\n\
             # `systemctl --user start harw-serve`), then run `sudo harw dod restart`.\n\
             # The probe resolves cgroup ids once at start: every harw-serve restart\n\
             # (Restart=always, `harw update`) gets a new cgroup, so run\n\
             # `sudo harw dod restart` after each one, or the probe watches a stale cgroup.\n",
        ),
    }
    text.push_str("# Then: sudo systemctl daemon-reload && sudo harw dod restart\n");
    Ok(text)
}

/// `harw dod config suggest [--drop-in]`; `candidates` are listed in the
/// template when no root resolves.
fn config_suggest(
    resolution: &Result<RootResolution, String>,
    candidates: &[(PathBuf, bool)],
    drop_in: bool,
) -> Outcome {
    let source = resolution
        .as_ref()
        .map_or("refused", |resolution| resolution.source.as_str());
    let (root, relative) = match resolved_root(resolution) {
        Ok(found) => found,
        Err(reason) => {
            let reason = ops::bounded(&reason);
            if drop_in {
                return Outcome::new(
                    EXIT_FAILURE,
                    format!("no drop-in without a delegated cgroup root: {reason}"),
                    json!({ "complete": false, "source": source, "reason": reason }),
                );
            }
            let template = suggest_template(candidates);
            let candidates: Vec<Value> = candidates
                .iter()
                .map(|(path, exists)| json!({ "path": path.to_string_lossy(), "exists": exists }))
                .collect();
            return Outcome::new(
                EXIT_OK,
                format!("# {reason}\n{}", template.trim_end()),
                json!({
                    "template": template,
                    "complete": false,
                    "source": source,
                    "reason": reason,
                    "candidates": candidates,
                }),
            );
        }
    };
    if drop_in {
        return match suggest_drop_in(&root, &relative) {
            Ok(text) => Outcome::new(
                EXIT_OK,
                text.trim_end(),
                json!({
                    "drop_in": text,
                    "cgroup_root": root.to_string_lossy(),
                    "source": source,
                    "probe_after": owning_system_unit(&relative),
                }),
            ),
            Err(error) => Outcome::new(
                EXIT_FAILURE,
                ops::bounded(&error),
                json!({ "error": ops::bounded(&error) }),
            ),
        };
    }
    let text = suggest_profile(&root, &relative, source);
    Outcome::new(
        EXIT_OK,
        text.trim_end(),
        json!({
            "template": text,
            "complete": true,
            "source": source,
            "cgroup_root": root.to_string_lossy(),
            "profile": {
                "scope": "cgroups",
                "cgroup_paths": [relative],
                "include_descendants": true,
                "sensors": ["exec", "tcp-connect"],
            },
            "allowed_cgroup_prefixes": [root.to_string_lossy()],
        }),
    )
}

/// Appends `extra` to `report` and recounts it.
fn add_checks(report: &mut ops::DoctorReport, extra: Vec<ops::DoctorCheck>) {
    report.checks.extend(extra);
    let count = |level: ops::CheckLevel| {
        report
            .checks
            .iter()
            .filter(|check| check.level == level)
            .count()
    };
    report.ok = count(ops::CheckLevel::Ok);
    report.warnings = count(ops::CheckLevel::Warn);
    report.missing = count(ops::CheckLevel::Miss);
}

fn doctor_check(id: &str, level: ops::CheckLevel, detail: &str) -> ops::DoctorCheck {
    ops::DoctorCheck {
        id: id.to_owned(),
        level,
        detail: ops::bounded(detail),
    }
}

/// The PL-97 E3 doctor checks (warnings only, never a missing
/// prerequisite): `cgroup-root` (the root resolves), `profile-root` (the
/// active Harw-DoD profile observes the jobs below it) and
/// `allowed-prefixes` (every `[dod] allowed_cgroup_prefixes` entry lies
/// within it).
fn cgroup_root_checks(
    root: &Result<ChainRoot, String>,
    prefixes: &[String],
    config: &ConfigReport,
) -> Vec<ops::DoctorCheck> {
    use ops::CheckLevel::{Ok as Pass, Warn};
    let (root, relative, source) = match root {
        Ok(ChainRoot {
            root: Some(root),
            relative: Some(relative),
            source,
            ..
        }) => (root, relative.clone(), source.as_str()),
        Ok(ChainRoot {
            root: Some(root), ..
        }) => {
            return vec![doctor_check(
                "cgroup-root",
                Warn,
                &format!("{} is not below {CGROUP2_MOUNT}", root.display()),
            )];
        }
        Ok(ChainRoot { hint, .. }) => {
            let reason = hint
                .clone()
                .unwrap_or_else(|| "no delegated cgroup v2 root".to_owned());
            return vec![doctor_check("cgroup-root", Warn, &reason)];
        }
        Err(reason) => {
            return vec![doctor_check("cgroup-root", Warn, reason)];
        }
    };
    let mut checks = vec![doctor_check(
        "cgroup-root",
        Pass,
        &format!("{} (source: {source})", root.display()),
    )];
    checks.push(match profile_coverage(config, &relative) {
        ProfileCoverage::Host(id) => doctor_check(
            "profile-root",
            Pass,
            &format!("profile {id} observes the whole host"),
        ),
        ProfileCoverage::Covers(id) => doctor_check(
            "profile-root",
            Pass,
            &format!("profile {id} covers {relative}"),
        ),
        ProfileCoverage::NotCovered(id) => doctor_check(
            "profile-root",
            Warn,
            &format!(
                "profile {id} does not observe harw's jobs below {relative} (needs a \
                 cgroup_paths entry at or above it with include_descendants = true; \
                 see `harw dod config suggest`)"
            ),
        ),
        ProfileCoverage::NoProfile => doctor_check(
            "profile-root",
            Warn,
            "no readable active profile; cannot tell whether it covers the cgroup root",
        ),
    });
    let root_text = root.to_string_lossy();
    let outside: Vec<&str> = prefixes
        .iter()
        .map(String::as_str)
        .filter(|prefix| !same_or_below(prefix, &root_text))
        .collect();
    checks.push(if outside.is_empty() {
        doctor_check(
            "allowed-prefixes",
            Pass,
            &format!("{} prefix(es) within {root_text}", prefixes.len()),
        )
    } else {
        doctor_check(
            "allowed-prefixes",
            Warn,
            &format!(
                "[dod] allowed_cgroup_prefixes outside {root_text}: {}",
                outside.join(", ")
            ),
        )
    });
    checks
}

// ── PL-97 E5: execution chain ───────────────────────────────────────────────

/// Which parts of the chain a consumer checks itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainScope {
    /// Everything except the DoD part (the caller adds its [`DodStatus`]):
    /// `harw doctor`, `harw dod doctor`.
    Full,
    /// Host facts only: no service state, no DoD (the verify runner, which
    /// sets the cgroup root it actually uses).
    HostOnly,
}

/// The `[jobs]` section and the project root markers of the trusted config
/// layers below `home`, or why the config does not load.
fn chain_config(
    home: &Path,
) -> Result<(harw_config::harness_config::JobsToml, Vec<String>), String> {
    harw_home::config_layers(home)
        .map_err(|error| error.to_string())
        .and_then(|layers| harw_config::discover_config(&layers).map_err(|error| error.to_string()))
        .map(|config| {
            let markers = config.harness.project_root_markers.unwrap_or_default();
            (config.harness.jobs, markers)
        })
}

/// An absolute executable regular file at `path`, or why not.
fn executable_at(name: &str, path: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt as _;
    if !path.is_absolute() {
        return Err(format!("{name} path '{}' is not absolute", path.display()));
    }
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => {
            Ok(path.to_path_buf())
        }
        Ok(_) => Err(format!(
            "{name} '{}' is not an executable file",
            path.display()
        )),
        Err(_) => Err(format!("no {name} at {}", path.display())),
    }
}

/// `name` next to the running executable `exe_dir`.
fn next_to_harw(name: &str, exe_dir: Option<&Path>) -> Result<PathBuf, String> {
    match exe_dir {
        Some(dir) => executable_at(name, &dir.join(name)),
        None => Err(format!(
            "no {name}: the directory of the running harw is unknown"
        )),
    }
}

/// The project workspace of this invocation: harw's own project discovery
/// ([`harw_home::discover_project`] with the configured markers, which
/// stops at `$HOME` and the temp dir, so the HARW home `~/.harw` never
/// makes `~` a project).
fn project_workspace(cwd: &Path, markers: &[String]) -> Option<PathBuf> {
    found_workspace(harw_home::discover_project(cwd, markers))
}

/// A discovered project root, or `None` when no marker was found
/// ([`harw_home::ProjectKind::Directory`]: not inside a project workspace)
/// or discovery failed.
fn found_workspace(project: harw_home::HomeResult<harw_home::ProjectRoot>) -> Option<PathBuf> {
    project
        .ok()
        .filter(|project| project.kind != harw_home::ProjectKind::Directory)
        .map(|project| project.root)
}

/// The chain's view of a cgroup root resolution: refused, none, or a root
/// plus whether the host probe confirms it as delegated.
pub(crate) fn chain_root(resolution: &Result<RootResolution, String>) -> Result<ChainRoot, String> {
    let resolution = resolution.as_ref().map_err(Clone::clone)?;
    let delegated = resolution.root.as_deref().is_some_and(|root| {
        harw_job_runtime::HostReport::probe_with_cgroup_root(Some(root)).cgroup_v2_delegated
    });
    Ok(ChainRoot {
        relative: resolution.root.as_deref().and_then(cgroup_path_below_mount),
        root: resolution.root.clone(),
        source: resolution.source.as_str().to_owned(),
        hint: resolution.hint.clone(),
        delegated,
    })
}

/// The source name of a root taken from a harw service unit's cgroup.
const SERVICE_UNIT_SOURCE: &str = "service-unit";

/// The one cgroup root a doctor run reports (PL-97 E3 + E5): the resolver's
/// answer ([`chain_root`]); when it resolves to nothing, the delegated
/// service unit cgroup `service` (or the reason none counts, appended to
/// the hint). A refused configured root stays refused.
fn doctor_root(
    resolution: &Result<RootResolution, String>,
    service: &Result<Option<PathBuf>, String>,
) -> Result<ChainRoot, String> {
    let mut root = chain_root(resolution)?;
    if root.root.is_none() {
        match service {
            Ok(Some(path)) => {
                root = ChainRoot {
                    relative: cgroup_path_below_mount(path),
                    root: Some(path.clone()),
                    source: SERVICE_UNIT_SOURCE.to_owned(),
                    hint: None,
                    delegated: true,
                };
            }
            Ok(None) => {}
            Err(note) => {
                root.hint = Some(match root.hint.take() {
                    Some(hint) => format!("{hint}; {note}"),
                    None => note.clone(),
                });
            }
        }
    }
    Ok(root)
}

/// The service unit fallback for `resolution`: only when it resolved to
/// nothing (a refused root stays refused), see [`delegated_service_root`].
fn fallback_service_root(
    resolution: &Result<RootResolution, String>,
) -> Result<Option<PathBuf>, String> {
    if !matches!(resolution, Ok(RootResolution { root: None, .. })) {
        return Ok(None);
    }
    delegated_service_root(
        &service_root_candidates(invoking_uid()),
        rustix::process::geteuid().as_raw(),
    )
}

/// The first of `candidates` that is really delegated to `euid`: owned by
/// `euid` (systemd hands a `Delegate=yes` cgroup to the unit's user) and
/// confirmed by the host probe (cgroup2, writable). As root (`euid` 0)
/// every cgroup is writable, so none counts; existing candidates then
/// yield the reason instead.
fn delegated_service_root(candidates: &[PathBuf], euid: u32) -> Result<Option<PathBuf>, String> {
    use std::os::unix::fs::MetadataExt as _;
    if euid == 0 {
        let existing: Vec<String> = candidates
            .iter()
            .filter(|path| path.is_dir())
            .map(|path| path.display().to_string())
            .collect();
        return if existing.is_empty() {
            Ok(None)
        } else {
            Err(format!(
                "running as root, so the service unit cgroup(s) {} are only candidates (root \
                 can write every cgroup); run the doctor as the harw user to confirm delegation",
                existing.join(", ")
            ))
        };
    }
    Ok(candidates
        .iter()
        .find(|path| {
            fs::metadata(path).is_ok_and(|meta| meta.is_dir() && meta.uid() == euid)
                && harw_job_runtime::HostReport::probe_with_cgroup_root(Some(path))
                    .cgroup_v2_delegated
        })
        .cloned())
}

/// The cgroup root of the chain for the harw home's `[jobs] cgroup_root`:
/// `[jobs] cgroup_root` → `HARW_CGROUP_ROOT` (resolver steps 1-2; the own
/// cgroup of this CLI process is never harw's job root), else the
/// delegated service unit cgroup ([`doctor_root`], as `harw dod doctor`).
fn host_chain_root(config_root: Option<PathBuf>) -> Result<ChainRoot, String> {
    let resolution = resolve_delegated_root_with(
        &RootConfig {
            cgroup_root: config_root,
        },
        &RootEnv::from_process(),
        OwnCgroup::default,
    )
    .map_err(|error| error.to_string());
    doctor_root(&resolution, &fallback_service_root(&resolution))
}

/// A trusted `bwrap` with user namespaces, decided by the shared backend
/// resolver (`BackendRequest::Only(Bwrap)`), or its reasons.
#[cfg(target_os = "linux")]
fn chain_bwrap(host: &harw_job_runtime::HostReport) -> Result<PathBuf, String> {
    use harw_job_runtime::{BackendKind, BackendPreferences, BackendRequest};
    let selection = harw_job_runtime::select_backend(
        host,
        &BackendPreferences {
            request: BackendRequest::Only(BackendKind::Bwrap),
            ..BackendPreferences::default()
        },
    );
    if selection.chosen == BackendKind::Bwrap {
        // Die festen, root-eigenen Pfade von `harw-sandbox` (`BWRAP_CANDIDATES`).
        Ok(["/usr/bin/bwrap", "/bin/bwrap"]
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("bwrap")))
    } else {
        Err(ops::bounded(&selection.reasons.join("; ")))
    }
}

#[cfg(not(target_os = "linux"))]
fn chain_bwrap(_host: &harw_job_runtime::HostReport) -> Result<PathBuf, String> {
    Err("Linux only".to_owned())
}

/// `harw service status` for [`harw_dod_client::chain::SERVE_SERVICE`].
fn serve_state() -> ServiceState {
    use harw_install::service::ServiceStatus;
    let platform = harw_install::platform::Platform::detect();
    let manager = harw_install::service::detect_service_manager(&platform);
    match manager.status(harw_dod_client::chain::SERVE_SERVICE) {
        Ok(ServiceStatus::Running) => ServiceState::Running,
        Ok(ServiceStatus::Stopped) => ServiceState::Stopped,
        Ok(ServiceStatus::NotInstalled) => ServiceState::NotInstalled,
        Err(error) => ServiceState::Unknown {
            reason: ops::bounded(&error.to_string()),
        },
    }
}

/// Collects the chain facts of this host for the harw home `home` (read-only:
/// file metadata, the host probe, the backend resolver and, for
/// [`ChainScope::Full`], `harw service status`). `root` is the cgroup root
/// the caller already resolved; `None` resolves it from the home's config
/// ([`host_chain_root`]). The DoD part stays `None`; the caller adds it.
pub(crate) fn chain_inputs(
    home: &Path,
    scope: ChainScope,
    root: Option<Result<ChainRoot, String>>,
) -> ChainInputs {
    let linux = cfg!(target_os = "linux");
    let config = chain_config(home);
    let (cgroup_root, trampoline_path, markers) = config
        .as_ref()
        .map(|(jobs, markers)| {
            (
                jobs.cgroup_root.clone(),
                jobs.trampoline.clone(),
                markers.clone(),
            )
        })
        .unwrap_or_default();
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_deref().and_then(Path::parent);
    let cwd = std::env::current_dir().ok();
    let runner = harw_agent_compiler::env::CompilerEnv::detect(
        Some(home.to_path_buf()),
        cwd.clone().unwrap_or_else(|| home.to_path_buf()),
    )
    .map_err(|error| error.to_string())
    .and_then(|env| {
        harw_agent_compiler::backend::runner::locate_runner(&env, None, &env.host_target)
            .map(|candidate| candidate.path)
            .map_err(|_| "no harw-agent-runner next to harw or in the toolchain".to_owned())
    });
    let trampoline = match &trampoline_path {
        Some(path) => executable_at(harw_job_runtime::TRAMPOLINE_NAME, path)
            .map_err(|why| format!("{why} ([jobs] trampoline)")),
        None => next_to_harw(harw_job_runtime::TRAMPOLINE_NAME, exe_dir),
    };
    let binaries = vec![
        ChainBinary {
            name: "harw".to_owned(),
            found: exe
                .clone()
                .ok_or_else(|| "the running harw is unknown".to_owned()),
            required: true,
        },
        ChainBinary {
            name: "killer".to_owned(),
            found: next_to_harw("killer", exe_dir),
            required: linux,
        },
        ChainBinary {
            name: "harw-agent-runner".to_owned(),
            found: runner,
            required: true,
        },
        ChainBinary {
            name: harw_job_runtime::TRAMPOLINE_NAME.to_owned(),
            found: trampoline.clone(),
            required: linux,
        },
    ];
    let host = harw_job_runtime::HostReport::probe();
    let serve = match scope {
        ChainScope::Full => Some(serve_state()),
        ChainScope::HostOnly => None,
    };
    ChainInputs {
        binaries,
        config: config.map(|_| ()),
        root: root.unwrap_or_else(|| host_chain_root(cgroup_root)),
        host: ChainHost {
            target_os: host.target_os.clone(),
            bwrap: chain_bwrap(&host),
            trampoline,
            landlock_abi: host.landlock.usable_abi(),
        },
        serve,
        dod: None,
        serve_workspace: ServeWorkspace {
            // `harw service install` setzt `WorkingDirectory=` auf das Home.
            serve_root: Some(fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf())),
            workspace: cwd
                .as_deref()
                .and_then(|cwd| project_workspace(cwd, &markers)),
        },
    }
}

/// The full execution chain of this host for `harw doctor`: [`chain_inputs`]
/// plus the Harw-DoD status read with the home's `[dod]` section.
pub(crate) fn host_execution_chain(home: &Path) -> ExecutionChainReport {
    let section = harw_home::config_layers(home)
        .ok()
        .and_then(|layers| harw_config::discover_config(&layers).ok())
        .map(|config| config.dod)
        .unwrap_or_default();
    let status: DodStatus = harw_dod_client::status(&ops::client_context(
        home,
        &section,
        OffsetDateTime::now_utc(),
    ));
    let mut inputs = chain_inputs(home, ChainScope::Full, None);
    inputs.dod = Some(ChainDod::from_status(&status));
    ExecutionChainReport::build(&inputs)
}

fn config_check(env: &DodEnv, path: Option<&Path>) -> Outcome {
    // `harw_dod_config::load_config` nimmt nur absolute Pfade.
    let absolute = match path.map(std::path::absolute).transpose() {
        Ok(path) => path,
        Err(error) => {
            return Outcome::new(
                EXIT_FAILURE,
                format!("config path not usable: {error}"),
                json!({ "error": ops::bounded(&error.to_string()) }),
            );
        }
    };
    match (env.read_config)(absolute.as_deref()) {
        Ok(report) => {
            let code = if report.is_activatable() {
                EXIT_OK
            } else {
                EXIT_FAILURE
            };
            Outcome::new(code, ops::config_text(&report), ops::config_view(&report))
        }
        Err(error) => Outcome::new(
            EXIT_FAILURE,
            format!(
                "config could not be read: {}",
                ops::bounded(&error.to_string())
            ),
            json!({ "state": "error", "reason": ops::bounded(&error.to_string()) }),
        ),
    }
}

/// `Some(refusal)` unless the effective uid is 0.
fn require_root(action: &str, system: &dyn System) -> Option<Outcome> {
    (system.euid() != 0).then(|| {
        Outcome::new(
            EXIT_REFUSED,
            format!("`harw dod {action}` requires root (run it explicitly with sudo); nothing was changed"),
            json!({ "action": action, "ok": false, "refused": "not_root" }),
        )
    })
}

/// `Some(refusal)` unless the system configuration is valid and selects an
/// `active_profile` (`check-config.sh` before `enable`/`restart`).
fn require_active_profile(action: &str, env: &DodEnv) -> Option<Outcome> {
    let report = match (env.read_config)(env.client.config_path.as_deref()) {
        Ok(report) if report.is_activatable() => return None,
        Ok(report) => ops::config_text(&report),
        Err(error) => ops::bounded(&error.to_string()),
    };
    Some(Outcome::new(
        EXIT_REFUSED,
        format!(
            "`harw dod {action}` refused: no valid active_profile ({report}); nothing was changed. \
             `harw dod config suggest` prints a profile template."
        ),
        json!({ "action": action, "ok": false, "refused": "no_active_profile", "config": report }),
    ))
}

/// Runs `steps` in order; stops at the first failure.
fn run_steps(action: &str, system: &mut dyn System, steps: &[&[&str]], done: &str) -> Outcome {
    let mut ran: Vec<Value> = Vec::new();
    for step in steps {
        if let Err(error) = system.systemctl(step) {
            let error = truncate(&error);
            return Outcome::new(
                EXIT_FAILURE,
                format!("`harw dod {action}` failed: {error}"),
                json!({ "action": action, "ok": false, "ran": ran, "error": error }),
            );
        }
        ran.push(json!(systemctl_line(step)));
    }
    Outcome::new(
        EXIT_OK,
        done,
        json!({ "action": action, "ok": true, "ran": ran, "message": done }),
    )
}

fn systemctl_line(args: &[&str]) -> String {
    format!("systemctl {}", args.join(" "))
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_ERROR_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_ERROR_CHARS - 1).collect();
    cut.push('…');
    cut
}

fn enable(env: &DodEnv, system: &mut dyn System) -> Outcome {
    if let Some(refused) =
        require_root("enable", system).or_else(|| require_active_profile("enable", env))
    {
        return refused;
    }
    run_steps(
        "enable",
        system,
        &[&["daemon-reload"], &["enable", "--now", "harw-dod.target"]],
        "Observation target enabled and started; Warden remains disabled.",
    )
}

fn restart(env: &DodEnv, system: &mut dyn System) -> Outcome {
    if let Some(refused) =
        require_root("restart", system).or_else(|| require_active_profile("restart", env))
    {
        return refused;
    }
    run_steps(
        "restart",
        system,
        &[&["daemon-reload"], &["restart", "harw-dod.target"]],
        "Observation target restarted.",
    )
}

fn disable(system: &mut dyn System) -> Outcome {
    if let Some(refused) = require_root("disable", system) {
        return refused;
    }
    let mut stop: Vec<&str> = vec!["disable", "--now"];
    stop.extend(OBSERVATION_UNITS);
    let outcome = run_steps(
        "disable",
        system,
        &[stop.as_slice()],
        "Observation stopped and disabled; any Warden activation removed.",
    );
    if outcome.code != EXIT_OK {
        return outcome;
    }
    // Wie `service.sh disable`: eine versehentliche Warden-Aktivierung
    // entfernen, ohne ihn zu starten; ein Fehler hier ist kein Abbruch.
    let mut warden: Vec<&str> = vec!["disable", "--now"];
    warden.extend(WARDEN_UNITS);
    let warden_result = system.systemctl(&warden);
    let mut json = outcome.json;
    if let Some(object) = json.as_object_mut() {
        object.insert(
            "warden".to_owned(),
            match warden_result {
                Ok(()) => json!({ "ran": systemctl_line(&warden) }),
                Err(error) => {
                    json!({ "ran": systemctl_line(&warden), "ignored_error": truncate(&error) })
                }
            },
        );
    }
    Outcome::new(EXIT_OK, outcome.text, json)
}

fn flag_present(flag: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(flag) {
        Ok(meta) if meta.file_type().is_file() => Ok(true),
        Ok(_) => Err(format!(
            "{} exists but is not a regular file",
            flag.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{}: {error}", flag.display())),
    }
}

fn warden_status(env: &DodEnv) -> Outcome {
    let snapshot = snapshot(env);
    let flag = &env.client.warden_flag;
    let flag_state = match flag_present(flag) {
        Ok(true) => "present".to_owned(),
        Ok(false) => "absent".to_owned(),
        Err(error) => format!("not checkable ({})", ops::bounded(&error)),
    };
    let state = match snapshot.status.warden {
        WardenState::Enabled => "enabled",
        WardenState::Disabled => "disabled",
        WardenState::Unknown => "unknown",
    };
    let socket = snapshot
        .status
        .units
        .iter()
        .find(|unit| unit.unit == WARDEN_UNITS[0]);
    let socket_text = socket.map_or_else(
        || "not reported".to_owned(),
        |unit| match &unit.status {
            harw_dod_client::UnitStatus::Reported {
                active_state,
                sub_state,
                unit_file_state,
            } => format!(
                "{active_state}/{sub_state} ({})",
                unit_file_state.as_deref().unwrap_or("no unit file")
            ),
            harw_dod_client::UnitStatus::NotAvailable => "systemd not available".to_owned(),
            harw_dod_client::UnitStatus::Unknown { reason } => {
                format!("unknown ({})", ops::bounded(reason))
            }
        },
    );
    let text = format!(
        "warden     {state}\nflag       {} {flag_state}\nsocket     {} {socket_text}",
        flag.display(),
        WARDEN_UNITS[0]
    );
    Outcome::new(
        EXIT_OK,
        text,
        json!({
            "warden": state,
            "flag": { "path": ops::bounded(&flag.to_string_lossy()), "state": flag_state },
            "socket": socket_text,
        }),
    )
}

/// Create the missing directory with at most 0750, respecting the caller's
/// umask, and create the flag without replacing an existing file.
fn create_flag(flag: &Path) -> Result<(), String> {
    if let Some(dir) = flag.parent() {
        match fs::symlink_metadata(dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(format!("{} is not a directory", dir.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o750)
                    .create(dir)
                    .map_err(|error| format!("{}: {error}", dir.display()))?;
            }
            Err(error) => return Err(format!("{}: {error}", dir.display())),
        }
    }
    if flag_present(flag)? {
        return Ok(());
    }
    // `create_new` (O_CREAT|O_EXCL) folgt keinem Symlink und überschreibt nichts.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(flag)
        .map(drop)
        .map_err(|error| format!("{}: {error}", flag.display()))
}

fn warden_enable(env: &DodEnv, system: &mut dyn System) -> Outcome {
    if let Some(refused) = require_root("warden enable", system) {
        return refused;
    }
    let flag = &env.client.warden_flag;
    if let Err(error) = create_flag(flag) {
        return Outcome::new(
            EXIT_FAILURE,
            format!("`harw dod warden enable` failed: {}", ops::bounded(&error)),
            json!({ "action": "warden enable", "ok": false, "error": ops::bounded(&error) }),
        );
    }
    let mut outcome = run_steps(
        "warden enable",
        system,
        &[&["daemon-reload"], &["enable", "--now", WARDEN_UNITS[0]]],
        "Warden guard file created and socket enabled/started.",
    );
    if outcome.code == EXIT_OK {
        outcome.text = format!("{}\n{KILL_REMINDER}", outcome.text);
        if let Some(object) = outcome.json.as_object_mut() {
            object.insert(
                "flag".to_owned(),
                json!(ops::bounded(&flag.to_string_lossy())),
            );
            object.insert("reminder".to_owned(), json!(KILL_REMINDER));
        }
    }
    outcome
}

fn warden_disable(env: &DodEnv, system: &mut dyn System) -> Outcome {
    if let Some(refused) = require_root("warden disable", system) {
        return refused;
    }
    let mut steps: Vec<&str> = vec!["disable", "--now"];
    steps.extend(WARDEN_UNITS);
    let mut outcome = run_steps(
        "warden disable",
        system,
        &[steps.as_slice()],
        "Warden socket and service disabled; guard file removed.",
    );
    if outcome.code != EXIT_OK {
        return outcome;
    }
    // Erst nach dem Stoppen: `ConditionPathExists=` wirkt nur beim Start.
    let flag = &env.client.warden_flag;
    match fs::remove_file(flag) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let error = format!("{}: {error}", flag.display());
            return Outcome::new(
                EXIT_FAILURE,
                format!(
                    "`harw dod warden disable`: units disabled, but {}",
                    ops::bounded(&error)
                ),
                json!({ "action": "warden disable", "ok": false, "error": ops::bounded(&error) }),
            );
        }
    }
    if let Some(object) = outcome.json.as_object_mut() {
        object.insert(
            "flag".to_owned(),
            json!(ops::bounded(&flag.to_string_lossy())),
        );
    }
    outcome
}

fn install(print_systemd: Option<Option<String>>) -> Outcome {
    let Some(unit) = print_systemd else {
        let text = "`harw dod install`: only `--print-systemd [UNIT]` exists; the Harw-DoD units \
                    are installed by `sudo make -C dod install`.";
        return Outcome::new(EXIT_USAGE, text, json!({ "error": text }));
    };
    match harw_install::print_systemd(unit.as_deref(), &RenderPaths::default()) {
        Ok(text) => {
            let json = json!({ "unit": unit, "systemd": text });
            Outcome::new(EXIT_OK, text.trim_end(), json)
        }
        Err(error) => Outcome::new(
            EXIT_FAILURE,
            error.to_string(),
            json!({ "error": error.to_string() }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use harw_dod_client::{ActiveProfile, ExportSource, ProfileScope};

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Fake host: a fixed euid, recorded `systemctl` calls, optional failures.
    struct FakeSystem {
        euid: u32,
        calls: Vec<Vec<String>>,
        fail_when: Option<&'static str>,
    }

    impl FakeSystem {
        fn new(euid: u32) -> Self {
            Self {
                euid,
                calls: Vec::new(),
                fail_when: None,
            }
        }
    }

    impl System for FakeSystem {
        fn euid(&self) -> u32 {
            self.euid
        }

        fn systemctl(&mut self, args: &[&str]) -> Result<(), String> {
            self.calls
                .push(args.iter().map(|arg| (*arg).to_owned()).collect());
            match self.fail_when {
                Some(unit) if args.contains(&unit) => Err(format!("{unit}: boom")),
                _ => Ok(()),
            }
        }
    }

    fn calls(lines: &[&[&str]]) -> Vec<Vec<String>> {
        lines
            .iter()
            .map(|line| line.iter().map(|arg| (*arg).to_owned()).collect())
            .collect()
    }

    fn valid_config(path: Option<&Path>) -> Result<ConfigReport, DodClientError> {
        Ok(ConfigReport::Valid {
            path: path.map_or_else(
                || PathBuf::from("/etc/harw-dod/config.toml"),
                Path::to_path_buf,
            ),
            config_digest: "00".to_owned(),
            active_profile: Some(ActiveProfile {
                id: "host".to_owned(),
                scope: ProfileScope::Host,
                sensors: vec!["exec".to_owned()],
                egress_allow_cidrs: 0,
            }),
        })
    }

    fn no_profile_config(path: Option<&Path>) -> Result<ConfigReport, DodClientError> {
        Ok(ConfigReport::Valid {
            path: path.map_or_else(
                || PathBuf::from("/etc/harw-dod/config.toml"),
                Path::to_path_buf,
            ),
            config_digest: "00".to_owned(),
            active_profile: None,
        })
    }

    /// A host without Harw-DoD below `dir`: no daemons, no systemctl, no
    /// export, no config.
    fn env_in(dir: &Path) -> DodEnv {
        let mut client = DodClientContext::new(dir.join("home"), OffsetDateTime::now_utc());
        client.system_export_dir = dir.join("var-lib-harw-dod-export");
        client.export_dir = Some(dir.join("export"));
        client.config_path = Some(dir.join("etc-harw-dod").join("config.toml"));
        client.libexec_dir = dir.join("libexec");
        client.warden_flag = dir.join("etc-harw-dod").join("warden.enable");
        client.search_path = None;
        DodEnv {
            client,
            doctor: ops::DoctorEnv {
                root: dir.join("root"),
                search_path: None,
            },
            read_config: harw_dod_client::read_system_config,
            notes: Vec::new(),
            cgroup_root: Ok(RootResolution {
                root: None,
                source: harw_job_runtime::host::RootSource::None,
                hint: Some(harw_job_runtime::host::NO_ROOT_HINT.to_owned()),
            }),
            allowed_cgroup_prefixes: Vec::new(),
            root_candidates: Vec::new(),
            service_root: Ok(None),
            chain: None,
            jobs_dir: None,
        }
    }

    #[test]
    fn dod_service_control_refuses_without_root_with_exit_3_and_changes_nothing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut env = env_in(dir.path());
        env.read_config = valid_config;
        for action in [
            DodAction::Enable,
            DodAction::Disable,
            DodAction::Restart,
            DodAction::Warden {
                action: DodWardenAction::Enable,
            },
            DodAction::Warden {
                action: DodWardenAction::Disable,
            },
        ] {
            let mut system = FakeSystem::new(1000);
            let outcome = execute(action.clone(), &env, &mut system);
            assert_eq!(outcome.code, EXIT_REFUSED, "{action:?}: {}", outcome.text);
            assert!(outcome.text.contains("requires root"), "{}", outcome.text);
            assert_eq!(outcome.json["refused"], json!("not_root"));
            assert!(system.calls.is_empty(), "{action:?}: {:?}", system.calls);
        }
        assert!(!env.client.warden_flag.exists(), "no flag written");
        Ok(())
    }

    #[test]
    fn dod_enable_and_restart_refuse_without_a_valid_active_profile() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut env = env_in(dir.path());
        for reader in [
            harw_dod_client::read_system_config as ConfigReader,
            no_profile_config,
        ] {
            env.read_config = reader;
            for action in [DodAction::Enable, DodAction::Restart] {
                let mut system = FakeSystem::new(0);
                let outcome = execute(action.clone(), &env, &mut system);
                assert_eq!(outcome.code, EXIT_REFUSED, "{action:?}: {}", outcome.text);
                assert_eq!(outcome.json["refused"], json!("no_active_profile"));
                assert!(system.calls.is_empty(), "{:?}", system.calls);
            }
        }
        Ok(())
    }

    #[test]
    fn dod_enable_restart_and_disable_mirror_service_sh() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut env = env_in(dir.path());
        env.read_config = valid_config;

        let mut system = FakeSystem::new(0);
        let outcome = execute(DodAction::Enable, &env, &mut system);
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        assert_eq!(
            system.calls,
            calls(&[&["daemon-reload"], &["enable", "--now", "harw-dod.target"]])
        );
        assert!(outcome.text.contains("Warden remains disabled"));

        let mut system = FakeSystem::new(0);
        let outcome = execute(DodAction::Restart, &env, &mut system);
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        assert_eq!(
            system.calls,
            calls(&[&["daemon-reload"], &["restart", "harw-dod.target"]])
        );

        let mut system = FakeSystem::new(0);
        system.fail_when = Some("harw-warden.socket");
        let outcome = execute(DodAction::Disable, &env, &mut system);
        assert_eq!(
            outcome.code, EXIT_OK,
            "warden failure is ignored: {}",
            outcome.text
        );
        assert_eq!(
            system.calls,
            calls(&[
                &[
                    "disable",
                    "--now",
                    "harw-dod.target",
                    "harw-sentinel.service",
                    "harw-probe-bpf.service",
                    "harw-probe-fs.service",
                ],
                &[
                    "disable",
                    "--now",
                    "harw-warden.socket",
                    "harw-warden.service"
                ],
            ])
        );
        assert!(outcome.json["warden"]["ignored_error"].is_string());

        let mut system = FakeSystem::new(0);
        system.fail_when = Some("harw-dod.target");
        let outcome = execute(DodAction::Disable, &env, &mut system);
        assert_eq!(outcome.code, EXIT_FAILURE, "{}", outcome.text);
        assert_eq!(system.calls.len(), 1, "stops at the first failure");
        Ok(())
    }

    #[test]
    fn dod_warden_enable_writes_only_the_flag_and_enables_the_socket() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let env = env_in(dir.path());
        for _ in 0..2 {
            let mut system = FakeSystem::new(0);
            let outcome = execute(
                DodAction::Warden {
                    action: DodWardenAction::Enable,
                },
                &env,
                &mut system,
            );
            assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
            assert_eq!(
                system.calls,
                calls(&[
                    &["daemon-reload"],
                    &["enable", "--now", "harw-warden.socket"]
                ])
            );
            assert!(
                outcome.text.contains("kill_requires_human"),
                "{}",
                outcome.text
            );
        }
        let flag = &env.client.warden_flag;
        assert!(flag.is_file(), "flag created");
        let dir_mode = fs::metadata(flag.parent().ok_or(TestError::Missing("flag dir"))?)
            .map_err(ctx("flag dir"))?
            .permissions();
        // DirBuilder applies the caller's umask: 077 legitimately produces
        // 0700. Preserve that stricter policy while checking the permission ceiling.
        let mode = std::os::unix::fs::PermissionsExt::mode(&dir_mode) & 0o777;
        assert_eq!(
            mode & !0o750,
            0,
            "no permissions beyond the requested ceiling"
        );
        assert_eq!(mode & 0o700, 0o700, "the owner can manage the flag");
        // Nur Freigabedatei und ihr Verzeichnis sind neu.
        let mut created: Vec<String> = fs::read_dir(dir.path())
            .map_err(ctx("list"))?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        created.sort();
        assert_eq!(created, ["etc-harw-dod"]);

        let mut system = FakeSystem::new(0);
        let outcome = execute(
            DodAction::Warden {
                action: DodWardenAction::Disable,
            },
            &env,
            &mut system,
        );
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        assert_eq!(
            system.calls,
            calls(&[&[
                "disable",
                "--now",
                "harw-warden.socket",
                "harw-warden.service"
            ]])
        );
        assert!(!flag.exists(), "flag removed");
        Ok(())
    }

    #[test]
    fn dod_warden_enable_refuses_a_flag_path_that_is_not_a_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let env = env_in(dir.path());
        fs::create_dir_all(&env.client.warden_flag).map_err(ctx("flag as dir"))?;
        let mut system = FakeSystem::new(0);
        let outcome = execute(
            DodAction::Warden {
                action: DodWardenAction::Enable,
            },
            &env,
            &mut system,
        );
        assert_eq!(outcome.code, EXIT_FAILURE, "{}", outcome.text);
        assert!(system.calls.is_empty());
        Ok(())
    }

    #[test]
    fn dod_read_only_actions_work_on_a_host_without_the_suite() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let env = env_in(dir.path());
        let mut system = FakeSystem::new(1000);
        let status = execute(DodAction::Status, &env, &mut system);
        assert_eq!(status.code, EXIT_OK);
        assert!(status.text.contains("not installed"), "{}", status.text);
        assert_eq!(status.json["installed"], json!(false));
        assert_eq!(status.json["findings"]["export"]["state"], json!("missing"));

        let doctor = execute(DodAction::Doctor, &env, &mut system);
        assert_eq!(doctor.code, EXIT_FAILURE, "{}", doctor.text);
        assert!(doctor.text.contains("MISS daemons"), "{}", doctor.text);

        let findings = execute(
            DodAction::Findings(DodFindingsArgs {
                limit: 10,
                severity: None,
                rule: None,
                since: None,
                job: None,
            }),
            &env,
            &mut system,
        );
        assert_eq!(findings.code, EXIT_OK, "{}", findings.text);
        assert!(findings.text.contains("missing"), "{}", findings.text);

        let sensors = execute(DodAction::Sensors, &env, &mut system);
        assert_eq!(sensors.code, EXIT_OK, "{}", sensors.text);
        assert_eq!(sensors.json["state"], json!("missing"));

        let warden = execute(
            DodAction::Warden {
                action: DodWardenAction::Status,
            },
            &env,
            &mut system,
        );
        assert_eq!(warden.code, EXIT_OK);
        assert_eq!(warden.json["warden"], json!("disabled"));
        assert_eq!(warden.json["flag"]["state"], json!("absent"));

        let check = execute(
            DodAction::Config {
                action: DodConfigAction::Check {
                    config: Some(dir.path().join("etc-harw-dod").join("config.toml")),
                },
            },
            &env,
            &mut system,
        );
        assert_eq!(check.code, EXIT_FAILURE, "{}", check.text);
        assert_eq!(check.json["activatable"], json!(false));

        let suggest = execute(
            DodAction::Config {
                action: DodConfigAction::Suggest { drop_in: false },
            },
            &env,
            &mut system,
        );
        assert_eq!(suggest.code, EXIT_OK);
        assert!(suggest.text.contains("[profiles.harw-jobs]"));
        assert!(
            system.calls.is_empty(),
            "read-only actions never call systemctl"
        );
        Ok(())
    }

    #[test]
    fn dod_findings_reads_a_v2_export_with_filters_and_the_config_check_reports_valid() -> TestResult
    {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut env = env_in(dir.path());
        env.read_config = valid_config;
        let export = dir.path().join("export");
        fs::create_dir_all(&export).map_err(ctx("export dir"))?;
        let now = OffsetDateTime::now_utc();
        let at = |minutes: i64| {
            (now - time::Duration::minutes(minutes))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        };
        let lines = [
            format!(
                r#"{{"schema_version":2,"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"s","observed_at":"{}","scope":"/sys/fs/cgroup/harw.slice"}}"#,
                at(30)
            ),
            format!(
                r#"{{"finding_id":"f-2","host":"host-a","rule_id":"egress-flow","severity":"low","summary":"v1 line","observed_at":"{}"}}"#,
                at(20)
            ),
        ];
        fs::write(
            export.join("findings.jsonl"),
            format!("{}\n", lines.join("\n")),
        )
        .map_err(ctx("write export"))?;
        let mut system = FakeSystem::new(1000);
        let outcome = execute(
            DodAction::Findings(DodFindingsArgs {
                limit: 50,
                severity: Some(DodSeverity::High),
                rule: None,
                since: Some(std::time::Duration::from_secs(3600)),
                job: None,
            }),
            &env,
            &mut system,
        );
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        assert_eq!(outcome.json["matched"], json!(1));
        assert_eq!(outcome.json["findings"][0]["id"], json!("f-1"));
        assert_eq!(
            outcome.json["findings"][0]["scope"],
            json!("/sys/fs/cgroup/harw.slice")
        );

        let all = execute(
            DodAction::Findings(DodFindingsArgs {
                limit: 50,
                severity: None,
                rule: None,
                since: None,
                job: None,
            }),
            &env,
            &mut system,
        );
        assert_eq!(all.json["matched"], json!(2), "v1 and v2 lines both parse");

        let check = execute(
            DodAction::Config {
                action: DodConfigAction::Check {
                    config: Some(PathBuf::from("/etc/harw-dod/config.toml")),
                },
            },
            &env,
            &mut system,
        );
        assert_eq!(check.code, EXIT_OK, "{}", check.text);
        assert!(
            check.text.contains("active profile `host`"),
            "{}",
            check.text
        );
        Ok(())
    }

    /// PL-97 E4: `harw dod findings --job <id>` lists only the findings in
    /// that job's proven cgroup (path-component prefix); a job without one
    /// gets none, an unknown or malformed id is a usage error.
    #[test]
    fn dod_findings_job_filter_uses_the_proven_job_cgroup() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut env = env_in(dir.path());
        let export = dir.path().join("export");
        fs::create_dir_all(&export).map_err(ctx("export dir"))?;
        let at = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let line = |id: &str, scope: &str| {
            format!(
                r#"{{"schema_version":2,"finding_id":"{id}","host":"h","rule_id":"egress-flow","severity":"high","summary":"s","observed_at":"{at}","scope":"{scope}"}}"#
            )
        };
        let lines = [
            line("f-own", "/harw.slice/harw-job-j-1-e1/sub"),
            line("f-other", "/harw.slice/harw-job-j-1-e10"),
        ];
        fs::write(
            export.join("findings.jsonl"),
            format!("{}\n", lines.join("\n")),
        )
        .map_err(ctx("write export"))?;
        let jobs = dir.path().join("jobs");
        let write_job = |id: &str, identity: &str| -> TestResult {
            let job_dir = jobs.join(id);
            fs::create_dir_all(&job_dir).map_err(ctx("job dir"))?;
            fs::write(
                job_dir.join("meta.json"),
                format!(
                    r#"{{"version":2,"job_id":"{id}","name":"t","command":"true","state":"succeeded",{identity}"harw_instance":"h","owner":{{"session":"s"}},"created_at":"2026-01-01T00:00:00Z","notify_every_secs":60}}"#
                ),
            )
            .map_err(ctx("meta.json"))
        };
        write_job(
            "job-1",
            r#""identity":{"start_ticks":5,"cgroup_path":"/harw.slice/harw-job-j-1-e1","dedicated_cgroup":{"job_id":"job-1","attempt_id":"j-1-e1","delegated_root":"/harw.slice"}},"#,
        )?;
        write_job(
            "job-2",
            r#""identity":{"start_ticks":5,"cgroup_path":"/harw.slice/harw-job-j-1-e1"},"#,
        )?;
        let args = |job: Option<&str>| {
            DodAction::Findings(DodFindingsArgs {
                limit: 50,
                severity: None,
                rule: None,
                since: None,
                job: job.map(str::to_owned),
            })
        };
        let mut system = FakeSystem::new(1000);
        let outside = execute(args(Some("job-1")), &env, &mut system);
        assert_eq!(outside.code, EXIT_FAILURE, "no project: {}", outside.text);

        env.jobs_dir = Some(jobs);
        let own = execute(args(Some("job-1")), &env, &mut system);
        assert_eq!(own.code, EXIT_OK, "{}", own.text);
        assert_eq!(own.json["matched"], json!(1), "{}", own.json);
        assert_eq!(own.json["findings"][0]["id"], json!("f-own"));
        assert_eq!(own.json["job"]["attributable"], json!(true));

        let unproven = execute(args(Some("job-2")), &env, &mut system);
        assert_eq!(unproven.code, EXIT_OK, "{}", unproven.text);
        assert_eq!(unproven.json["matched"], json!(0));
        assert!(
            unproven.text.contains("no dedicated cgroup"),
            "{}",
            unproven.text
        );

        for bad in ["job-9", "../job-1"] {
            let outcome = execute(args(Some(bad)), &env, &mut system);
            assert_eq!(outcome.code, EXIT_USAGE, "{bad}: {}", outcome.text);
        }
        assert_eq!(
            execute(args(None), &env, &mut system).json["matched"],
            json!(2)
        );
        Ok(())
    }

    /// `env_in` re-pointed at `source`, with one recent finding planted in
    /// the export directory it resolves to. For
    /// [`ExportSource::SystemUnreadable`] the system export sits below a
    /// regular file (not inspectable, also as root) and the finding is
    /// planted in the home fallback, which must stay unread.
    fn env_for(dir: &Path, source: ExportSource) -> TestResult<DodEnv> {
        let mut env = env_in(dir);
        let export = match source {
            ExportSource::Override => dir.join("export"),
            ExportSource::Home => {
                env.client.export_dir = None;
                env.client.harw_home.join("dod").join("export")
            }
            ExportSource::System => {
                env.client.export_dir = None;
                env.client.system_export_dir.clone()
            }
            ExportSource::SystemUnreadable => {
                env.client.export_dir = None;
                fs::write(dir.join("blocker"), "").map_err(ctx("blocker"))?;
                env.client.system_export_dir = dir.join("blocker").join("export");
                env.client.harw_home.join("dod").join("export")
            }
        };
        fs::create_dir_all(&export).map_err(ctx("export dir"))?;
        let at = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(ctx("timestamp"))?;
        fs::write(
            export.join("findings.jsonl"),
            format!(
                r#"{{"schema_version":2,"finding_id":"f-1","host":"host-a","rule_id":"egress-flow","severity":"high","summary":"s","observed_at":"{at}"}}"#
            ) + "\n",
        )
        .map_err(ctx("write export"))?;
        Ok(env)
    }

    /// Binding PL-97 D3 requirement: `harw dod status|findings|sensors`
    /// label findings and health from an unverified source (home,
    /// `[dod] export_dir`, an unreadable system export) with the source and
    /// its hint, in text and JSON; the system export carries no mark.
    #[test]
    fn dod_cli_labels_findings_from_an_unverified_source() -> TestResult {
        for source in [
            ExportSource::Override,
            ExportSource::Home,
            ExportSource::SystemUnreadable,
            ExportSource::System,
        ] {
            let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let env = env_for(dir.path(), source)?;
            let mut system = FakeSystem::new(1000);
            let status = execute(DodAction::Status, &env, &mut system);
            let findings = execute(
                DodAction::Findings(DodFindingsArgs {
                    limit: 10,
                    severity: None,
                    rule: None,
                    since: None,
                    job: None,
                }),
                &env,
                &mut system,
            );
            let sensors = execute(DodAction::Sensors, &env, &mut system);
            let what = format!("{source:?}");
            assert_eq!(status.code, EXIT_OK, "{what}: {}", status.text);
            assert_eq!(status.json["export"]["source"], json!(source), "{what}");
            for view in [
                &status.json["export"],
                &status.json["findings"],
                &status.json["health"],
                &findings.json,
                &sensors.json,
            ] {
                assert_eq!(
                    view["verified"],
                    json!(source.is_verified()),
                    "{what}: {view}"
                );
            }
            assert_eq!(
                status.json["export"]["hint"],
                json!(source.hint()),
                "{what}"
            );
            assert_eq!(findings.json["hint"], json!(source.hint()), "{what}");
            assert_eq!(sensors.json["hint"], json!(source.hint()), "{what}");
            match source.hint() {
                Some(hint) => {
                    for text in [&status.text, &findings.text, &sensors.text] {
                        assert!(text.contains("UNVERIFIED"), "{what}: {text}");
                        assert!(text.contains(hint), "{what}: {text}");
                        assert!(text.contains(ops::source_label(source)), "{what}: {text}");
                    }
                }
                None => {
                    for text in [&status.text, &findings.text, &sensors.text] {
                        assert!(!text.contains("UNVERIFIED"), "{what}: {text}");
                    }
                }
            }
            if source == ExportSource::SystemUnreadable {
                // Nie der Home-Fallback: der Systemexport gilt als unlesbar,
                // der Fund im harw-Home bleibt ungelesen.
                assert_eq!(findings.code, EXIT_FAILURE, "{}", findings.text);
                assert_eq!(findings.json["export"]["state"], json!("unreadable"));
                assert_eq!(findings.json["matched"], json!(0));
                assert!(
                    findings.text.contains("harw-dod-readers"),
                    "{}",
                    findings.text
                );
            } else {
                assert_eq!(findings.code, EXIT_OK, "{what}: {}", findings.text);
                assert_eq!(findings.json["matched"], json!(1), "{what}");
            }
        }
        Ok(())
    }

    #[test]
    fn dod_install_prints_the_embedded_units_and_names_unknown_ones() {
        let unit = install(Some(Some("harw-warden.socket".to_owned())));
        assert_eq!(unit.code, EXIT_OK, "{}", unit.text);
        assert!(unit.text.contains("[Socket]"), "{}", unit.text);
        assert_eq!(unit.json["unit"], json!("harw-warden.socket"));

        let unknown = install(Some(Some("nope.service".to_owned())));
        assert_eq!(unknown.code, EXIT_FAILURE, "{}", unknown.text);

        let bare = install(None);
        assert_eq!(bare.code, EXIT_USAGE, "{}", bare.text);
    }

    // ── PL-97 E3: delegated cgroup root ──

    fn resolved(
        root: &str,
        source: harw_job_runtime::host::RootSource,
    ) -> Result<RootResolution, String> {
        Ok(RootResolution {
            root: Some(PathBuf::from(root)),
            source,
            hint: None,
        })
    }

    fn cgroup_profile(paths: &[&str], include_descendants: bool) -> ConfigReport {
        ConfigReport::Valid {
            path: PathBuf::from("/etc/harw-dod/config.toml"),
            config_digest: "00".to_owned(),
            active_profile: Some(ActiveProfile {
                id: "harw-jobs".to_owned(),
                scope: ProfileScope::Cgroups {
                    paths: paths.iter().map(PathBuf::from).collect(),
                    include_descendants,
                },
                sensors: vec!["exec".to_owned()],
                egress_allow_cidrs: 0,
            }),
        }
    }

    const SUGGEST_GOLDEN: &str = r#"# Harw-DoD observation profile for harw's job cgroups (PL-97 E3).
# Delegated cgroup root: /sys/fs/cgroup/system.slice/harw-control.service (source: config).
# Merge into /etc/harw-dod/config.toml and run `sudo harw dod enable`.
# The root must exist before harw-probe-bpf starts: the probe refuses to
# start while no configured cgroup root resolves (`harw dod config suggest --drop-in`).
schema_version = 1
mode = "observe"
active_profile = "harw-jobs"

[profiles.harw-jobs]
scope = "cgroups"
cgroup_paths = ["/system.slice/harw-control.service"]
include_descendants = true
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []

# Matching entry in harw's own config (`~/.harw/config.toml`), canonical
# form (absolute, below /sys/fs/cgroup):
# [dod]
# allowed_cgroup_prefixes = ["/sys/fs/cgroup/system.slice/harw-control.service"]"#;

    #[test]
    fn dod_config_suggest_prints_the_profile_for_the_resolved_root() -> TestResult {
        use harw_job_runtime::host::RootSource;
        let root = resolved(
            "/sys/fs/cgroup/system.slice/harw-control.service",
            RootSource::Config,
        );
        let outcome = config_suggest(&root, &[], false);
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        assert_eq!(outcome.text, SUGGEST_GOLDEN);
        assert_eq!(outcome.json["complete"], json!(true));
        assert_eq!(outcome.json["source"], json!("config"));
        assert_eq!(
            outcome.json["profile"]["cgroup_paths"],
            json!(["/system.slice/harw-control.service"])
        );
        assert_eq!(
            outcome.json["allowed_cgroup_prefixes"],
            json!(["/sys/fs/cgroup/system.slice/harw-control.service"])
        );
        // The printed profile is accepted by `[dod]` validation on the harw side.
        let section: DodSection = toml::from_str(
            "allowed_cgroup_prefixes = [\"/sys/fs/cgroup/system.slice/harw-control.service\"]",
        )
        .map_err(ctx("parse [dod]"))?;
        assert_eq!(section.allowed_cgroup_prefixes.len(), 1);

        // No root: the template with the hint, never a made-up path.
        let none = Ok(RootResolution {
            root: None,
            source: RootSource::None,
            hint: Some(harw_job_runtime::host::NO_ROOT_HINT.to_owned()),
        });
        let candidates = [
            (
                PathBuf::from("/sys/fs/cgroup/system.slice/harw-control.service"),
                true,
            ),
            (PathBuf::from("/sys/fs/cgroup/user.slice/x.service"), false),
        ];
        let outcome = config_suggest(&none, &candidates, false);
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        assert!(
            outcome.text.contains("cgroup_paths = [\"<R>\"]"),
            "{}",
            outcome.text
        );
        assert!(
            outcome
                .text
                .contains("allowed_cgroup_prefixes = [\"/sys/fs/cgroup<R>\"]"),
            "{}",
            outcome.text
        );
        assert!(
            outcome
                .text
                .contains("The root must exist before harw-probe-bpf starts"),
            "{}",
            outcome.text
        );
        assert!(!outcome.text.contains("fills it in"), "{}", outcome.text);
        assert!(
            outcome
                .text
                .contains("#   /sys/fs/cgroup/system.slice/harw-control.service (exists)"),
            "{}",
            outcome.text
        );
        assert!(
            outcome
                .text
                .contains("#   /sys/fs/cgroup/user.slice/x.service (not present)"),
            "{}",
            outcome.text
        );
        assert_eq!(outcome.json["candidates"][0]["exists"], json!(true));
        assert!(
            outcome.text.contains("systemd-run --user --scope"),
            "{}",
            outcome.text
        );
        assert_eq!(outcome.json["complete"], json!(false));
        let outcome = config_suggest(&none, &[], true);
        assert_eq!(outcome.code, EXIT_FAILURE, "{}", outcome.text);
        Ok(())
    }

    #[test]
    fn dod_config_suggest_drop_in_sets_the_sentinel_root_and_orders_the_probe() -> TestResult {
        use harw_job_runtime::host::RootSource;
        let unit =
            harw_install::print_systemd(Some("harw-sentinel.service"), &RenderPaths::default())
                .map_err(|error| TestError::Unexpected(error.to_string()))?;
        let exec_start = unit
            .lines()
            .find(|line| line.starts_with("ExecStart="))
            .ok_or(TestError::Missing("sentinel ExecStart"))?;
        let root = "/sys/fs/cgroup/system.slice/harw-control.service";
        let outcome = config_suggest(&resolved(root, RootSource::OwnCgroupDelegated), &[], true);
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        let expected = format!(
            "# /etc/systemd/system/harw-sentinel.service.d/harw-jobs.conf (PL-97 E3)\n\
             # The cgroup sensor reports the direct children of {root}: harw's job cgroups.\n\
             [Service]\n\
             ExecStart=\n\
             {exec_start} --cgroup-root {root}\n\
             \n\
             # /etc/systemd/system/harw-probe-bpf.service.d/harw-jobs.conf\n\
             # harw-probe-bpf refuses to start while no configured cgroup root resolves,\n\
             # so {root} must exist before it starts.\n\
             # The probe resolves cgroup ids once at start, and every start of harw-control.service\n\
             # creates its cgroup anew: BindsTo= stops the probe when harw-control.service stops or\n\
             # fails, PartOf= restarts it with harw-control.service. After a crash and automatic\n\
             # restart of harw-control.service, run `sudo harw dod restart`.\n\
             [Unit]\n\
             BindsTo=harw-control.service\n\
             PartOf=harw-control.service\n\
             After=harw-control.service\n\
             # Then: sudo systemctl daemon-reload && sudo harw dod restart"
        );
        assert_eq!(outcome.text, expected);
        assert_eq!(outcome.json["probe_after"], json!("harw-control.service"));

        // A user-manager root: no system unit to order after, a documented route.
        let user = "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service";
        let outcome = config_suggest(&resolved(user, RootSource::Env), &[], true);
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        assert!(!outcome.text.contains("BindsTo="), "{}", outcome.text);
        assert!(
            outcome
                .text
                .contains("run\n# `sudo harw dod restart` after each one"),
            "{}",
            outcome.text
        );
        assert!(
            outcome.text.contains("start harw first"),
            "{}",
            outcome.text
        );
        assert!(
            outcome.text.contains(&format!("--cgroup-root {user}")),
            "{}",
            outcome.text
        );
        Ok(())
    }

    #[test]
    fn dod_config_suggest_escapes_systemd_names_for_toml_and_exec_start() -> TestResult {
        use harw_job_runtime::host::RootSource;
        let root = "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome\\x2dterminal-50%.scope";
        let relative = "/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome\\x2dterminal-50%.scope";

        let outcome = config_suggest(&resolved(root, RootSource::Env), &[], false);
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        // The printed profile parses, and the values round-trip unchanged.
        let profile: toml::Table = toml::from_str(&outcome.text).map_err(ctx("parse profile"))?;
        let paths = profile
            .get("profiles")
            .and_then(|p| p.get("harw-jobs"))
            .and_then(|p| p.get("cgroup_paths"))
            .and_then(toml::Value::as_array)
            .ok_or(TestError::Missing("cgroup_paths"))?;
        assert_eq!(paths.first().and_then(toml::Value::as_str), Some(relative));
        let prefix_line = outcome
            .text
            .lines()
            .find_map(|line| line.strip_prefix("# allowed_cgroup_prefixes = "))
            .ok_or(TestError::Missing("prefix line"))?;
        let section: DodSection =
            toml::from_str(&format!("allowed_cgroup_prefixes = {prefix_line}"))
                .map_err(ctx("parse [dod]"))?;
        assert_eq!(section.allowed_cgroup_prefixes, vec![root.to_owned()]);

        // ExecStart: backslashes and % doubled, so systemd passes R verbatim.
        let outcome = config_suggest(&resolved(root, RootSource::Env), &[], true);
        assert_eq!(outcome.code, EXIT_OK, "{}", outcome.text);
        let escaped = "--cgroup-root /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome\\\\x2dterminal-50%%.scope";
        assert!(
            outcome
                .text
                .lines()
                .any(|line| line.starts_with("ExecStart=/") && line.ends_with(escaped)),
            "{}",
            outcome.text
        );
        assert_eq!(systemd_exec_arg("/a b\"c"), "\"/a b\\\"c\"");
        Ok(())
    }

    #[test]
    fn dod_doctor_warns_when_the_profile_or_prefixes_miss_the_cgroup_root() {
        use harw_job_runtime::host::RootSource;
        use ops::CheckLevel;
        let root = chain_root(&resolved("/sys/fs/cgroup/harw.slice", RootSource::Config));
        let level = |checks: &[ops::DoctorCheck], id: &str| {
            checks
                .iter()
                .find(|check| check.id == id)
                .map(|check| check.level)
        };

        // Covered: profile root at or above R with descendants, prefixes inside R.
        for paths in [&["/harw.slice"][..], &["/"][..]] {
            let checks = cgroup_root_checks(
                &root,
                &["/sys/fs/cgroup/harw.slice/".to_owned()],
                &cgroup_profile(paths, true),
            );
            assert_eq!(level(&checks, "cgroup-root"), Some(CheckLevel::Ok));
            assert_eq!(
                level(&checks, "profile-root"),
                Some(CheckLevel::Ok),
                "{checks:?}"
            );
            assert_eq!(level(&checks, "allowed-prefixes"), Some(CheckLevel::Ok));
        }

        // Not covered: a sibling, a prefix lookalike, no descendants.
        for (paths, descendants) in [
            (&["/other.slice"][..], true),
            (&["/harw"][..], true),
            (&["/harw.slice"][..], false),
        ] {
            let checks = cgroup_root_checks(&root, &[], &cgroup_profile(paths, descendants));
            assert_eq!(
                level(&checks, "profile-root"),
                Some(CheckLevel::Warn),
                "{paths:?} {checks:?}"
            );
        }

        // Prefixes outside R.
        let checks = cgroup_root_checks(
            &root,
            &[
                "/sys/fs/cgroup/harw.slice/harw-job-1".to_owned(),
                "/sys/fs/cgroup/evil.slice".to_owned(),
            ],
            &cgroup_profile(&["/harw.slice"], true),
        );
        let prefixes = checks.iter().find(|check| check.id == "allowed-prefixes");
        assert!(
            prefixes.is_some_and(|check| check.level == CheckLevel::Warn
                && check.detail.contains("evil.slice")
                && !check.detail.contains("harw-job-1")),
            "{checks:?}"
        );

        // No root: one warning with the hint, never a missing prerequisite.
        let none = chain_root(&Ok(RootResolution {
            root: None,
            source: RootSource::None,
            hint: Some(harw_job_runtime::host::NO_ROOT_HINT.to_owned()),
        }));
        let checks = cgroup_root_checks(&none, &[], &cgroup_profile(&["/"], true));
        assert_eq!(checks.len(), 1);
        assert_eq!(level(&checks, "cgroup-root"), Some(CheckLevel::Warn));
        let mut report = ops::DoctorReport {
            checks: Vec::new(),
            ok: 0,
            warnings: 0,
            missing: 0,
        };
        add_checks(&mut report, checks);
        assert!(report.passed());
        assert_eq!(report.warnings, 1);
    }

    // ── PL-97 E5: execution chain ──

    #[test]
    fn chain_root_and_project_workspace_follow_the_resolution_and_markers() -> TestResult {
        use harw_job_runtime::host::RootSource;
        let refused: Result<RootResolution, String> = Err("not absolute".to_owned());
        assert_eq!(chain_root(&refused), Err("not absolute".to_owned()));
        let none = Ok(RootResolution {
            root: None,
            source: RootSource::None,
            hint: Some(harw_job_runtime::host::NO_ROOT_HINT.to_owned()),
        });
        let root = chain_root(&none).map_err(TestError::Unexpected)?;
        assert!(root.root.is_none() && !root.delegated && root.source == "none");
        let missing = resolved("/sys/fs/cgroup/harw-e5-missing.slice", RootSource::Env);
        let root = chain_root(&missing).map_err(TestError::Unexpected)?;
        assert_eq!(root.relative.as_deref(), Some("/harw-e5-missing.slice"));
        assert!(!root.delegated, "a missing cgroup is never delegated");

        // A `.harw` HARW home in a parent of cwd is no project marker: a
        // cwd below the user home without `.git` is not inside a project.
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = fs::canonicalize(dir.path()).map_err(ctx("canonicalize"))?;
        fs::create_dir(home.join(".harw")).map_err(ctx(".harw"))?;
        let nested = home.join("notes").join("deep");
        fs::create_dir_all(&nested).map_err(ctx("mkdir"))?;
        let discover = |cwd: &Path, markers: &[String]| {
            found_workspace(harw_home::project::discover_project_with_home_stop(
                cwd,
                markers,
                Some(&home),
            ))
        };
        assert_eq!(discover(&nested, &[]), None);
        assert_eq!(
            discover(&home, &[]),
            None,
            "the user home is never a project"
        );
        // A `.git` below the home is the project; configured markers count.
        let app = home.join("src").join("app");
        fs::create_dir_all(app.join(".git")).map_err(ctx(".git"))?;
        fs::create_dir_all(app.join("crates").join("x")).map_err(ctx("mkdir"))?;
        assert_eq!(
            discover(&app.join("crates").join("x"), &[]),
            Some(app.clone())
        );
        fs::write(nested.join("PROJECT"), "").map_err(ctx("marker"))?;
        assert_eq!(
            discover(&nested, &["PROJECT".to_owned()]),
            Some(nested.clone())
        );
        Ok(())
    }

    #[test]
    fn doctor_root_is_one_root_with_the_service_unit_fallback() -> TestResult {
        use harw_job_runtime::host::RootSource;
        let none = Ok(RootResolution {
            root: None,
            source: RootSource::None,
            hint: Some(harw_job_runtime::host::NO_ROOT_HINT.to_owned()),
        });
        let service = PathBuf::from("/sys/fs/cgroup/system.slice/harw-control.service");
        let root = doctor_root(&none, &Ok(Some(service.clone()))).map_err(TestError::Unexpected)?;
        assert_eq!(root.root.as_ref(), Some(&service));
        assert_eq!(root.source, SERVICE_UNIT_SOURCE);
        // The E3 checks see the same root as the chain row.
        let checks = cgroup_root_checks(&Ok(root), &[], &cgroup_profile(&["/"], true));
        assert!(
            checks
                .iter()
                .any(|check| check.id == "cgroup-root" && check.detail.contains("service-unit")),
            "{checks:?}"
        );
        // A configured root wins; a refused one stays refused.
        let configured = resolved("/sys/fs/cgroup/harw.slice", RootSource::Config);
        let root =
            doctor_root(&configured, &Ok(Some(service.clone()))).map_err(TestError::Unexpected)?;
        assert_eq!(root.source, "config");
        let refused: Result<RootResolution, String> = Err("not absolute".to_owned());
        assert!(doctor_root(&refused, &Ok(Some(service))).is_err());
        // Why no service unit counts lands in the hint.
        let root = doctor_root(&none, &Err("running as root".to_owned()))
            .map_err(TestError::Unexpected)?;
        assert!(root.root.is_none());
        assert!(
            root.hint
                .as_deref()
                .is_some_and(|hint| hint.ends_with("; running as root")),
            "{root:?}"
        );
        Ok(())
    }

    #[test]
    fn delegated_service_root_needs_ownership_and_never_trusts_root() -> TestResult {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let unit = dir.path().join("harw-serve.service");
        fs::create_dir(&unit).map_err(ctx("mkdir"))?;
        let missing = dir.path().join("harw-control.service");
        let candidates = [missing.clone(), unit.clone()];
        // euid 0: every cgroup is writable, so an existing unit is only a candidate.
        let as_root = delegated_service_root(&candidates, 0);
        assert!(
            as_root.as_ref().is_err_and(
                |why| why.contains("harw-serve.service") && !why.contains("harw-control")
            ),
            "{as_root:?}"
        );
        assert_eq!(delegated_service_root(&[missing], 0), Ok(None));
        // Another owner never counts; the own directory is no cgroup2.
        let owner = fs::metadata(&unit).map_err(ctx("metadata"))?.uid();
        assert_eq!(
            delegated_service_root(&candidates, owner.wrapping_add(1).max(1)),
            Ok(None)
        );
        if owner != 0 {
            assert_eq!(delegated_service_root(&candidates, owner), Ok(None));
        }
        Ok(())
    }

    /// Fixed chain facts around `root`: everything present, harw-serve
    /// stopped.
    fn fixture_chain(root: Result<ChainRoot, String>) -> ChainInputs {
        let bin = |name: &str| ChainBinary {
            name: name.to_owned(),
            found: Ok(PathBuf::from(format!("/usr/local/bin/{name}"))),
            required: true,
        };
        ChainInputs {
            binaries: vec![
                bin("harw"),
                bin("killer"),
                bin("harw-agent-runner"),
                bin("harw-job-exec"),
            ],
            config: Ok(()),
            root,
            host: ChainHost {
                target_os: "linux".to_owned(),
                bwrap: Ok(PathBuf::from("/usr/bin/bwrap")),
                trampoline: Ok(PathBuf::from("/usr/local/bin/harw-job-exec")),
                landlock_abi: Some(6),
            },
            serve: Some(ServiceState::Stopped),
            dod: None,
            serve_workspace: ServeWorkspace::default(),
        }
    }

    #[test]
    fn dod_doctor_appends_the_chain_without_changing_the_exit_code() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut system = FakeSystem::new(1000);
        let plain = execute(DodAction::Doctor, &env_in(dir.path()), &mut system);
        assert!(plain.json.get("execution_chain").is_none());
        assert!(!plain.text.contains("execution chain"));

        let mut env = env_in(dir.path());
        env.chain = Some(Box::new(fixture_chain));
        let doctor = execute(DodAction::Doctor, &env, &mut system);
        assert_eq!(doctor.code, plain.code, "{}", doctor.text);
        assert!(doctor.text.starts_with(&plain.text), "{}", doctor.text);
        // No root resolves in `env_in` and harw-serve is stopped: miss rows.
        assert!(
            doctor.text.contains("execution chain: installed")
                && doctor
                    .text
                    .contains("configured cgroup-root              miss")
                && doctor
                    .text
                    .contains("running    harw-serve               miss"),
            "{}",
            doctor.text
        );
        let chain = &doctor.json["execution_chain"];
        assert_eq!(chain["cgroup_root"]["level"], json!("miss"), "{chain}");
        // The DoD part comes from the doctor's own status: not installed.
        assert_eq!(chain["dod_target"]["level"], json!("skip"), "{chain}");

        // With the service unit fallback both views show the same root.
        env.service_root = Ok(Some(PathBuf::from(
            "/sys/fs/cgroup/system.slice/harw-control.service",
        )));
        let doctor = execute(DodAction::Doctor, &env, &mut system);
        assert_eq!(doctor.code, plain.code, "{}", doctor.text);
        let service_rows = doctor
            .text
            .lines()
            .filter(|line| line.contains("harw-control.service (source: service-unit)"))
            .count();
        assert_eq!(service_rows, 2, "{}", doctor.text);
        Ok(())
    }
}
