//! One read-only snapshot of the whole suite: install, config, units,
//! Warden, health, findings, export paths.
//!
//! [`status`] never fails: every part that cannot be read is reported as
//! such inside the snapshot, so `harw dod status`, the `dod.status` model
//! tool and the TUI always have something honest to show.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::{ConfigReport, read_system_config};
use crate::error::DodClientError;
use crate::findings::{
    DEFAULT_FINDINGS_WINDOW, FindingsSummary, MAX_TAIL_BYTES, read_findings_tail,
};
use crate::fsio::sanitize;
use crate::health::{HealthReport, read_health};
use crate::paths::{ExportPaths, SYSTEM_EXPORT_DIR, resolve_export_paths_with};
use crate::units::{UnitState, UnitStatus, unit_states_in};

/// Where `make -C dod install` puts the daemons.
pub const LIBEXEC_DIR: &str = "/usr/local/libexec/harw-dod";

/// The Warden guard file (`ConditionPathExists=` of the Warden units).
pub const WARDEN_FLAG_PATH: &str = "/etc/harw-dod/warden.enable";

/// The four daemons of the suite.
pub const DOD_BINARIES: [&str; 4] = [
    "harw-sentinel",
    "harw-probe-fs",
    "harw-probe-bpf",
    "harw-warden",
];

/// The Warden socket unit that gates enforcement.
const WARDEN_SOCKET_UNIT: &str = "harw-warden.socket";

/// Longest kept reason text, in bytes.
const MAX_REASON_BYTES: usize = 256;

/// Inputs of [`status`]. All paths are injectable for tests and staged
/// installs; [`DodClientContext::new`] fills in the production defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DodClientContext {
    /// The harw home (`HARW_HOME`).
    pub harw_home: PathBuf,
    /// `[dod] export_dir`, if configured by a trusted layer.
    pub export_dir: Option<PathBuf>,
    /// System export directory ([`SYSTEM_EXPORT_DIR`]).
    pub system_export_dir: PathBuf,
    /// Explicit DoD config path; `None` reads the fixed system path.
    pub config_path: Option<PathBuf>,
    /// Directory holding the daemons ([`LIBEXEC_DIR`]).
    pub libexec_dir: PathBuf,
    /// Warden guard file ([`WARDEN_FLAG_PATH`]).
    pub warden_flag: PathBuf,
    /// `PATH`-style search list for `systemctl`; `None` means "not
    /// available".
    pub search_path: Option<OsString>,
    /// Findings summary window (`[dod] findings_window_hours`).
    pub findings_window: Duration,
    /// The injected "now".
    pub now: OffsetDateTime,
}

impl DodClientContext {
    /// Production defaults for `harw_home` at `now`; `systemctl` is looked up
    /// in this process's `PATH`.
    #[must_use]
    pub fn new(harw_home: impl Into<PathBuf>, now: OffsetDateTime) -> Self {
        Self {
            harw_home: harw_home.into(),
            export_dir: None,
            system_export_dir: PathBuf::from(SYSTEM_EXPORT_DIR),
            config_path: None,
            libexec_dir: PathBuf::from(LIBEXEC_DIR),
            warden_flag: PathBuf::from(WARDEN_FLAG_PATH),
            search_path: std::env::var_os("PATH"),
            findings_window: DEFAULT_FINDINGS_WINDOW,
            now,
        }
    }
}

/// Whether the daemons are installed in the checked libexec directory.
///
/// Only the configured directory is looked at (`dod/Makefile` lets
/// `PREFIX`/`LIBEXECDIR` move it); [`DodStatus::suite_installed`] also
/// counts the units systemd knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InstallState {
    /// All four daemons are present.
    Installed {
        /// The directory that was checked.
        libexec_dir: PathBuf,
    },
    /// Some daemons are missing.
    Partial {
        /// The directory that was checked.
        libexec_dir: PathBuf,
        /// Missing daemon names.
        missing: Vec<String>,
    },
    /// None of the daemons is present.
    NotInstalled {
        /// The directory that was checked.
        libexec_dir: PathBuf,
    },
}

/// Whether the Warden (enforcement) is enabled; see [`warden_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WardenState {
    /// The socket is neither listening nor set to start with its guard file.
    Disabled,
    /// The socket is listening, or enabled with its guard file in place.
    Enabled,
    /// systemd could not be asked and the guard file does not rule the
    /// Warden out.
    Unknown,
}

/// Whether an export file could be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExportState {
    /// Read successfully.
    Readable,
    /// The file does not exist.
    Missing,
    /// The file exists but was refused or could not be read.
    Unreadable {
        /// Sanitized description.
        reason: String,
    },
}

impl ExportState {
    fn from_error(error: &DodClientError) -> Self {
        if error.is_not_found() {
            Self::Missing
        } else {
            Self::Unreadable {
                reason: sanitize(&error.to_string(), MAX_REASON_BYTES, ' '),
            }
        }
    }
}

/// The read-only snapshot of the suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DodStatus {
    /// Daemon installation.
    pub install: InstallState,
    /// System configuration.
    pub config: ConfigReport,
    /// The six units.
    pub units: Vec<UnitState>,
    /// Warden enablement.
    pub warden: WardenState,
    /// Sentinel health report, if present and readable.
    pub health: Option<HealthReport>,
    /// Read state of the health report.
    pub health_export: ExportState,
    /// Findings inside the window.
    pub findings: FindingsSummary,
    /// Read state of the findings export.
    pub findings_export: ExportState,
    /// Where the export was looked up.
    pub export_paths: ExportPaths,
}

impl DodStatus {
    /// `true` when any part of the suite is installed: a daemon in the
    /// checked libexec directory, a unit file systemd knows for any DoD unit,
    /// or an active DoD unit. The units also recognize installs built with
    /// another `PREFIX`/`LIBEXECDIR`.
    #[must_use]
    pub fn suite_installed(&self) -> bool {
        !matches!(self.install, InstallState::NotInstalled { .. })
            || self
                .units
                .iter()
                .any(|unit| unit.is_installed() == Some(true) || unit.is_active())
    }

    /// `true` when the findings and health files come from a source only
    /// the sentinel or a trusted config layer writes
    /// ([`crate::paths::ExportSource::is_verified`]). A `false` snapshot
    /// may still carry findings (the home fallback); consumers that present
    /// them as Harw-DoD facts show `export_paths.source` and its
    /// [`crate::paths::ExportSource::hint`] next to them.
    #[must_use]
    pub const fn export_verified(&self) -> bool {
        self.export_paths.source.is_verified()
    }

    /// `high + critical` findings in the window, or `None` when DoD is not
    /// installed ([`Self::suite_installed`]) or the findings export is
    /// unreadable (the TUI hides its status mark then). When
    /// `findings.complete` is `false` the count is a lower bound. The count
    /// does not check [`Self::export_verified`]; the caller labels the mark.
    #[must_use]
    pub fn attention_count(&self) -> Option<u32> {
        (self.suite_installed() && self.findings_export == ExportState::Readable)
            .then(|| self.findings.high_or_critical())
    }
}

/// Assembles the snapshot described by `ctx`.
#[must_use]
pub fn status(ctx: &DodClientContext) -> DodStatus {
    let export_paths = resolve_export_paths_with(
        ctx.export_dir.as_deref(),
        &ctx.harw_home,
        &ctx.system_export_dir,
    );
    let config = read_system_config(ctx.config_path.as_deref()).unwrap_or_else(|error| {
        ConfigReport::Unreadable {
            path: ctx
                .config_path
                .clone()
                .unwrap_or_else(|| PathBuf::from(harw_dod_config::SYSTEM_CONFIG_PATH)),
            reason: sanitize(&error.to_string(), MAX_REASON_BYTES, ' '),
        }
    });
    let units = unit_states_in(ctx.search_path.as_deref()).unwrap_or_else(|error| {
        let reason = sanitize(&error.to_string(), MAX_REASON_BYTES, ' ');
        crate::units::DOD_UNITS
            .iter()
            .map(|unit| UnitState {
                unit: (*unit).to_owned(),
                status: UnitStatus::Unknown {
                    reason: reason.clone(),
                },
            })
            .collect()
    });
    let warden = warden_state(&ctx.warden_flag, &units);
    let (health, health_export) = match read_health(&export_paths.health) {
        Ok(Some(report)) => (Some(report), ExportState::Readable),
        Ok(None) => (None, ExportState::Missing),
        Err(error) => (None, ExportState::from_error(&error)),
    };
    let (findings, findings_export) =
        match read_findings_tail(&export_paths.findings, MAX_TAIL_BYTES, ctx.now) {
            Ok(tail) => (
                tail.summarize(ctx.findings_window, ctx.now),
                ExportState::Readable,
            ),
            Err(error) => {
                let export = ExportState::from_error(&error);
                let mut summary = FindingsSummary::empty(ctx.findings_window, ctx.now);
                // A missing export has nothing to count; an unreadable one
                // may hold findings that were not counted.
                summary.complete = export == ExportState::Missing;
                (summary, export)
            }
        };
    // An unreadable system export is reported as such, with the operator
    // hint, never replaced by the home fallback (see `resolve_export_paths`).
    let (health_export, findings_export) = if export_paths.source.is_verified() {
        (health_export, findings_export)
    } else {
        (
            with_source_hint(health_export, export_paths.source),
            with_source_hint(findings_export, export_paths.source),
        )
    };
    DodStatus {
        install: install_state(&ctx.libexec_dir),
        config,
        units,
        warden,
        health,
        health_export,
        findings,
        findings_export,
        export_paths,
    }
}

/// Appends the source hint to an unreadable state; other states are kept.
fn with_source_hint(state: ExportState, source: crate::paths::ExportSource) -> ExportState {
    match (state, source.hint()) {
        (ExportState::Unreadable { reason }, Some(hint)) => ExportState::Unreadable {
            reason: sanitize(&format!("{reason}; {hint}"), MAX_REASON_BYTES * 2, ' '),
        },
        (state, _) => state,
    }
}

/// Checks which of [`DOD_BINARIES`] exist in `libexec_dir`.
#[must_use]
pub fn install_state(libexec_dir: &Path) -> InstallState {
    let missing: Vec<String> = DOD_BINARIES
        .iter()
        .filter(|binary| {
            !fs::symlink_metadata(libexec_dir.join(binary)).is_ok_and(|meta| meta.is_file())
        })
        .map(|binary| (*binary).to_owned())
        .collect();
    let libexec_dir = libexec_dir.to_path_buf();
    if missing.is_empty() {
        InstallState::Installed { libexec_dir }
    } else if missing.len() == DOD_BINARIES.len() {
        InstallState::NotInstalled { libexec_dir }
    } else {
        InstallState::Partial {
            libexec_dir,
            missing,
        }
    }
}

/// Warden enablement from `harw-warden.socket` and its guard file.
///
/// The socket is what makes enforcement reachable, so systemd's answer
/// decides whenever there is one:
/// - socket `ActiveState=active` ⇒ [`WardenState::Enabled`], with or
///   without the guard file (`ConditionPathExists=` is evaluated only when
///   the unit starts, so removing the flag leaves a listening socket up);
/// - socket enabled (`UnitFileState=enabled`/`enabled-runtime`) and the
///   guard file present or not checkable (e.g. `/etc/harw-dod` is `0750`)
///   ⇒ [`WardenState::Enabled`] (`make -C dod enable-warden` state);
/// - any other reported socket state ⇒ [`WardenState::Disabled`].
///
/// Only when systemd cannot be asked does the guard file decide alone:
/// absent ⇒ [`WardenState::Disabled`], present or not checkable ⇒
/// [`WardenState::Unknown`] — nothing confirms a listening socket, and a
/// status must never claim enforcement it cannot see.
#[must_use]
pub fn warden_state(flag: &Path, units: &[UnitState]) -> WardenState {
    let flag_absent = matches!(
        fs::symlink_metadata(flag),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    );
    let socket = units
        .iter()
        .find(|unit| unit.unit == WARDEN_SOCKET_UNIT)
        .map(|unit| &unit.status);
    match socket {
        Some(UnitStatus::Reported {
            active_state,
            unit_file_state,
            ..
        }) => {
            let socket_enabled = matches!(
                unit_file_state.as_deref(),
                Some("enabled" | "enabled-runtime")
            );
            if active_state == "active" || (socket_enabled && !flag_absent) {
                WardenState::Enabled
            } else {
                WardenState::Disabled
            }
        }
        Some(UnitStatus::NotAvailable | UnitStatus::Unknown { .. }) | None => {
            if flag_absent {
                WardenState::Disabled
            } else {
                WardenState::Unknown
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ExportSource;
    use crate::units::DOD_UNITS;
    use std::os::unix::fs::PermissionsExt as _;
    use time::format_description::well_known::Rfc3339;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn reported(unit: &str, active: &str, file: Option<&str>) -> UnitState {
        UnitState {
            unit: unit.to_owned(),
            status: UnitStatus::Reported {
                active_state: active.to_owned(),
                sub_state: "x".to_owned(),
                unit_file_state: file.map(str::to_owned),
            },
        }
    }

    #[test]
    fn status_assembles_every_part_from_fixtures() -> TestResult {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let export = home.join("dod").join("export");
        fs::create_dir_all(&export)?;
        fs::write(
            export.join("findings.jsonl"),
            concat!(
                r#"{"finding_id":"f-1","host":"h","rule_id":"egress","severity":"high","summary":"s","observed_at":"2026-10-28T09:00:00Z"}"#,
                "\n",
                r#"{"schema_version":2,"finding_id":"f-2","host":"h","rule_id":"egress","severity":"critical","summary":"s","observed_at":"2026-10-28T09:30:00Z","scope":"/harw.slice/harw-job-1"}"#,
                "\n",
                r#"{"finding_id":"f-3","host":"h","rule_id":"drift","severity":"low","summary":"s","observed_at":"2026-10-20T09:30:00Z"}"#,
                "\n",
            ),
        )?;
        fs::write(
            export.join("health.json"),
            r#"{"schema_version":1,"written_at":"2026-10-28T09:59:00Z","profile":"harw-jobs","sensors":[{"id":"exec","health":"bound"}]}"#,
        )?;
        let libexec = root.path().join("libexec");
        fs::create_dir(&libexec)?;
        for binary in ["harw-sentinel", "harw-probe-bpf"] {
            fs::write(libexec.join(binary), "")?;
            fs::set_permissions(libexec.join(binary), fs::Permissions::from_mode(0o755))?;
        }
        let empty_path = root.path().join("bin");
        fs::create_dir(&empty_path)?;
        let now = OffsetDateTime::parse("2026-10-28T10:00:00Z", &Rfc3339)?;
        let missing_config = PathBuf::from(format!(
            "/harw-dod-client-status-test-{}/config.toml",
            std::process::id()
        ));
        let ctx = DodClientContext {
            harw_home: home.clone(),
            export_dir: None,
            system_export_dir: root.path().join("no-system-export"),
            config_path: Some(missing_config.clone()),
            libexec_dir: libexec.clone(),
            warden_flag: root.path().join("warden.enable"),
            search_path: Some(empty_path.into_os_string()),
            findings_window: DEFAULT_FINDINGS_WINDOW,
            now,
        };

        let snapshot = status(&ctx);
        assert_eq!(snapshot.export_paths.source, ExportSource::Home);
        assert_eq!(
            snapshot.export_paths.findings,
            export.join("findings.jsonl")
        );
        assert_eq!(
            snapshot.install,
            InstallState::Partial {
                libexec_dir: libexec,
                missing: vec!["harw-probe-fs".into(), "harw-warden".into()],
            }
        );
        assert_eq!(
            snapshot.config,
            ConfigReport::Missing {
                path: missing_config
            }
        );
        assert_eq!(snapshot.units.len(), DOD_UNITS.len());
        assert!(
            snapshot
                .units
                .iter()
                .all(|unit| unit.status == UnitStatus::NotAvailable)
        );
        assert_eq!(snapshot.warden, WardenState::Disabled);
        assert_eq!(snapshot.health_export, ExportState::Readable);
        assert_eq!(
            snapshot.health.as_ref().and_then(|h| h.profile.as_deref()),
            Some("harw-jobs")
        );
        assert_eq!(snapshot.findings_export, ExportState::Readable);
        assert_eq!(snapshot.findings.total(), 2);
        assert_eq!(snapshot.findings.high_or_critical(), 2);
        assert_eq!(snapshot.attention_count(), Some(2));

        // The snapshot serializes for `--json` and reads back unchanged.
        let json = serde_json::to_string(&snapshot)?;
        let again: DodStatus = serde_json::from_str(&json)?;
        assert_eq!(again, snapshot);

        // A guard file alone, with systemd out of reach, confirms nothing.
        fs::write(&ctx.warden_flag, "")?;
        assert_eq!(status(&ctx).warden, WardenState::Unknown);
        Ok(())
    }

    #[test]
    fn unreadable_system_export_is_not_replaced_by_planted_home_findings() -> TestResult {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let planted = home.join("dod").join("export");
        fs::create_dir_all(&planted)?;
        fs::write(
            planted.join("findings.jsonl"),
            concat!(
                r#"{"finding_id":"f-1","host":"h","rule_id":"egress","severity":"critical","summary":"planted","observed_at":"2026-10-28T09:00:00Z"}"#,
                "
",
            ),
        )?;
        let system = root.path().join("system-export");
        fs::create_dir(&system)?;
        fs::set_permissions(&system, fs::Permissions::from_mode(0o000))?;
        let listable = fs::read_dir(&system).is_ok();
        let libexec = root.path().join("libexec");
        fs::create_dir(&libexec)?;
        for binary in DOD_BINARIES {
            fs::write(libexec.join(binary), "")?;
            fs::set_permissions(libexec.join(binary), fs::Permissions::from_mode(0o755))?;
        }
        let empty_path = root.path().join("bin");
        fs::create_dir(&empty_path)?;
        let now = OffsetDateTime::parse("2026-10-28T10:00:00Z", &Rfc3339)?;
        let ctx = DodClientContext {
            harw_home: home,
            export_dir: None,
            system_export_dir: system.clone(),
            config_path: Some(root.path().join("absent-config.toml")),
            libexec_dir: libexec,
            warden_flag: root.path().join("warden.enable"),
            search_path: Some(empty_path.into_os_string()),
            findings_window: DEFAULT_FINDINGS_WINDOW,
            now,
        };
        let snapshot = status(&ctx);
        fs::set_permissions(&system, fs::Permissions::from_mode(0o750))?;
        if listable {
            // Root can list a 0o000 directory; the ordinary rule applies and
            // the planted home file is out of the picture either way.
            assert_eq!(snapshot.export_paths.source, ExportSource::System);
            assert_eq!(snapshot.findings.total(), 0);
            return Ok(());
        }
        assert_eq!(snapshot.export_paths.source, ExportSource::SystemUnreadable);
        assert!(!snapshot.export_verified());
        assert_eq!(
            snapshot.export_paths.findings,
            system.join("findings.jsonl")
        );
        // The planted critical finding below the home is never counted.
        assert_eq!(snapshot.findings.total(), 0);
        assert!(!snapshot.findings.complete);
        assert_eq!(snapshot.attention_count(), None);
        match &snapshot.findings_export {
            ExportState::Unreadable { reason } => {
                assert!(reason.contains("harw-dod-readers"), "{reason}");
            }
            other => return Err(format!("expected unreadable, got {other:?}").into()),
        }
        assert!(matches!(
            snapshot.health_export,
            ExportState::Unreadable { .. }
        ));
        Ok(())
    }

    #[test]
    fn nothing_installed_reports_missing_parts_and_hides_the_mark() -> TestResult {
        let root = tempfile::tempdir()?;
        let mut ctx = DodClientContext::new(
            root.path().join("home"),
            OffsetDateTime::parse("2026-10-28T10:00:00Z", &Rfc3339)?,
        );
        ctx.system_export_dir = root.path().join("no-system-export");
        ctx.libexec_dir = root.path().join("no-libexec");
        ctx.warden_flag = root.path().join("warden.enable");
        ctx.search_path = None;
        ctx.config_path = Some(root.path().join("absent").join("config.toml"));
        let snapshot = status(&ctx);
        assert!(matches!(
            snapshot.install,
            InstallState::NotInstalled { .. }
        ));
        assert_eq!(snapshot.findings_export, ExportState::Missing);
        assert_eq!(snapshot.health_export, ExportState::Missing);
        assert!(snapshot.health.is_none());
        assert_eq!(snapshot.findings.total(), 0);
        assert!(snapshot.findings.complete);
        assert!(!snapshot.suite_installed());
        assert_eq!(snapshot.attention_count(), None);
        Ok(())
    }

    #[test]
    fn units_reveal_an_install_outside_the_default_libexec_dir() -> TestResult {
        let root = tempfile::tempdir()?;
        let export = root.path().join("export");
        fs::create_dir(&export)?;
        fs::write(
            export.join("findings.jsonl"),
            concat!(
                r#"{"finding_id":"f-1","host":"h","rule_id":"egress","severity":"critical","summary":"s","observed_at":"2026-10-28T09:00:00Z"}"#,
                "\n"
            ),
        )?;
        let mut ctx = DodClientContext::new(
            root.path().join("home"),
            OffsetDateTime::parse("2026-10-28T10:00:00Z", &Rfc3339)?,
        );
        ctx.export_dir = Some(export);
        ctx.search_path = None;
        ctx.libexec_dir = root.path().join("no-libexec");
        ctx.warden_flag = root.path().join("warden.enable");
        ctx.config_path = Some(root.path().join("absent").join("config.toml"));
        let mut snapshot = status(&ctx);
        assert!(matches!(
            snapshot.install,
            InstallState::NotInstalled { .. }
        ));
        assert_eq!(snapshot.findings_export, ExportState::Readable);
        assert_eq!(snapshot.attention_count(), None);

        // Built with PREFIX=/usr: the daemons live elsewhere, but systemd
        // knows the units — the security mark must not be hidden.
        snapshot.units = DOD_UNITS
            .iter()
            .map(|unit| reported(unit, "inactive", None))
            .collect();
        assert!(!snapshot.suite_installed());
        snapshot.units = vec![reported(
            "harw-sentinel.service",
            "inactive",
            Some("enabled"),
        )];
        assert!(snapshot.suite_installed());
        assert_eq!(snapshot.attention_count(), Some(1));
        snapshot.units = vec![reported("harw-sentinel.service", "active", None)];
        assert_eq!(snapshot.attention_count(), Some(1));
        Ok(())
    }

    #[test]
    fn symlinked_export_is_unreadable_not_missing() -> TestResult {
        let root = tempfile::tempdir()?;
        let export = root.path().join("export");
        fs::create_dir(&export)?;
        let real = root.path().join("real.jsonl");
        fs::write(&real, "")?;
        std::os::unix::fs::symlink(&real, export.join("findings.jsonl"))?;
        let mut ctx = DodClientContext::new(
            root.path().join("home"),
            OffsetDateTime::parse("2026-10-28T10:00:00Z", &Rfc3339)?,
        );
        ctx.export_dir = Some(export);
        ctx.search_path = None;
        ctx.libexec_dir = root.path().join("no-libexec");
        let snapshot = status(&ctx);
        assert_eq!(snapshot.export_paths.source, ExportSource::Override);
        assert!(matches!(
            &snapshot.findings_export,
            ExportState::Unreadable { reason } if reason.contains("symlink")
        ));
        assert!(!snapshot.findings.complete);
        Ok(())
    }

    #[test]
    fn warden_state_combines_the_socket_unit_and_the_guard_file() -> TestResult {
        let root = tempfile::tempdir()?;
        let present = root.path().join("warden.enable");
        fs::write(&present, "")?;
        let absent = root.path().join("absent.enable");
        // A flag below a non-directory cannot be checked (ENOTDIR).
        let unknowable = present.join("warden.enable");
        let socket =
            |active: &str, file: Option<&str>| [reported("harw-warden.socket", active, file)];
        let listening = socket("active", Some("disabled"));
        let enabled = socket("inactive", Some("enabled"));
        let runtime = socket("inactive", Some("enabled-runtime"));
        let off = socket("inactive", Some("disabled"));
        let no_unit_file = socket("inactive", None);

        // A listening socket is enforcement, whatever the flag says.
        for flag in [&present, &absent, &unknowable] {
            assert_eq!(warden_state(flag, &listening), WardenState::Enabled);
        }
        // Enabled socket: starts with the flag, stays down without it.
        assert_eq!(warden_state(&present, &enabled), WardenState::Enabled);
        assert_eq!(warden_state(&present, &runtime), WardenState::Enabled);
        assert_eq!(warden_state(&unknowable, &enabled), WardenState::Enabled);
        assert_eq!(warden_state(&absent, &enabled), WardenState::Disabled);
        // Flag present, socket disabled, inactive or not installed.
        assert_eq!(warden_state(&present, &off), WardenState::Disabled);
        assert_eq!(warden_state(&present, &no_unit_file), WardenState::Disabled);
        assert_eq!(warden_state(&unknowable, &off), WardenState::Disabled);

        // systemd out of reach: only an absent flag is conclusive.
        let unavailable = [UnitState {
            unit: "harw-warden.socket".to_owned(),
            status: UnitStatus::NotAvailable,
        }];
        let failed = [UnitState {
            unit: "harw-warden.socket".to_owned(),
            status: UnitStatus::Unknown {
                reason: "timeout".to_owned(),
            },
        }];
        for units in [&unavailable[..], &failed[..], &[]] {
            assert_eq!(warden_state(&absent, units), WardenState::Disabled);
            assert_eq!(warden_state(&present, units), WardenState::Unknown);
            assert_eq!(warden_state(&unknowable, units), WardenState::Unknown);
        }
        Ok(())
    }
}
