//! `harw dod …` (PL-97 D3): the Harw-DoD suite from the command line.
//!
//! # Read-only actions
//! `status`, `doctor`, `findings`, `sensors` and `config check|suggest`
//! read through `harw-dod-client` and render with the helpers of
//! `harw_ops::dod`, so the terminal shows what `/dod` and the `dod.*` model
//! tools show. `--json` prints the structured form through the [`Printer`].
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
use harw_dod_client::{ConfigReport, DodClientContext, DodClientError, ExportState, WardenState};
use harw_install::RenderPaths;
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
}

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
    let section = harw_home::config_layers(&home)
        .map_err(|error| error.to_string())
        .and_then(|layers| harw_config::discover_config(&layers).map_err(|error| error.to_string()))
        .map(|config| config.dod)
        .unwrap_or_else(|error| {
            notes.push(ops::bounded(&format!(
                "harw config unreadable, default [dod] settings used: {error}"
            )));
            DodSection::default()
        });
    DodEnv {
        client: ops::client_context(&home, &section, OffsetDateTime::now_utc()),
        doctor: ops::DoctorEnv::host(),
        read_config: harw_dod_client::read_system_config,
        notes,
    }
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
            let report = ops::doctor(&env.doctor, &snapshot(env));
            let code = if report.passed() {
                EXIT_OK
            } else {
                EXIT_FAILURE
            };
            Outcome::new(code, ops::doctor_text(&report), ops::doctor_view(&report))
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
            action: DodConfigAction::Suggest,
        } => Outcome::new(
            EXIT_OK,
            ops::CONFIG_SUGGEST_TEMPLATE.trim_end(),
            json!({
                "template": ops::CONFIG_SUGGEST_TEMPLATE,
                "complete": false,
                "note": "the delegated cgroup root is filled in by PL-97 E3",
            }),
        ),
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
    let query = ops::FindingsQuery {
        limit,
        min_severity: args.severity.map(severity),
        rule: args
            .rule
            .as_deref()
            .map(str::trim)
            .filter(|rule| !rule.is_empty())
            .map(str::to_owned),
        since: args.since,
    };
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

/// `install -d -m 0750 <dir>` (only when missing) and `touch <flag>`.
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
    use harw_dod_client::{ActiveProfile, ProfileScope};

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
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&dir_mode) & 0o777,
            0o750
        );
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
                action: DodConfigAction::Suggest,
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
}
