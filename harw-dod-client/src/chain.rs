//! The execution chain report (PL-97 E5): one honest answer to "can harw
//! run a sandboxed, cgroup-limited, Harw-DoD-observed job on this host?".
//!
//! [`ExecutionChainReport::build`] is pure. It takes plain facts
//! ([`ChainInputs`]) that the caller collects from
//! `harw_job_runtime::HostReport`, the delegated cgroup root resolution
//! (`harw_job_runtime::host::RootResolution`, E3) and a [`DodStatus`]
//! ([`ChainDod::from_status`]). The input types mirror those instead of
//! naming them, so this layer-A crate stays free of the job runtime (the
//! consumers already link both).
//!
//! # Readiness stages (PR #153 W4)
//! | stage | items |
//! |---|---|
//! | [`ChainStage::Installed`] | `harw`, `killer`, `harw-agent-runner`, `harw-job-exec` |
//! | [`ChainStage::Configured`] | `config` (the harw config loads), `cgroup-root` (resolves and is delegated) |
//! | [`ChainStage::Running`] | `harw-serve` (`harw service status`), `harw-dod.target` |
//! | [`ChainStage::Ready`] | `bwrap`, `trampoline`, `landlock-abi`, `dod-profile-covers-root`, `warden`, `serve-workspace` |
//!
//! Each item is [`ChainLevel::Ok`], [`ChainLevel::Warn`] (works, weaker),
//! [`ChainLevel::Miss`] (the stage is not reached) or [`ChainLevel::Skip`]
//! (not applicable here, e.g. every DoD item on a host without the suite).
//! [`ExecutionChainReport::reached`] is the highest stage with no missing
//! item in it or below it.
//!
//! # `serve-workspace`
//! The serve worker (`harw-serve`) runs its jobs with the HARW home (its
//! `WorkingDirectory=`) as workspace root. The row is [`ChainLevel::Ok`]
//! only when that home is the project workspace of the invocation (a
//! HARW home inside the project), [`ChainLevel::Skip`] outside a project
//! workspace or while `harw-serve` is not running, and otherwise a
//! standing advisory [`ChainLevel::Warn`]: project jobs belong to the
//! interactive harw, not to the serve worker.
//!
//! # Consumers
//! `harw doctor` (one check per stage), `harw dod doctor` (the table and
//! `execution_chain` in `--json`) and the verify runner's dimension table
//! log. All three render through [`ExecutionChainReport::table`].

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{ConfigReport, ProfileScope};
use crate::status::{DodStatus, WardenState};

/// The DoD target unit whose state is the Running item of the suite.
pub const DOD_TARGET_UNIT: &str = "harw-dod.target";

/// The user service that runs harw's job worker (`harw service install`).
pub const SERVE_SERVICE: &str = "harw-serve";

/// How good one chain item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainLevel {
    /// Present and working.
    Ok,
    /// Works, but weaker than the full chain.
    Warn,
    /// Missing: the item's stage is not reached.
    Miss,
    /// Not applicable on this host (not counted).
    Skip,
}

impl ChainLevel {
    /// Stable lowercase name (`ok`, `warn`, `miss`, `skip`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Miss => "miss",
            Self::Skip => "skip",
        }
    }
}

impl fmt::Display for ChainLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The readiness stages, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainStage {
    /// The binaries are present.
    Installed,
    /// The config loads and the cgroup root resolves.
    Configured,
    /// The services run.
    Running,
    /// The host can enforce the chain.
    Ready,
}

impl ChainStage {
    /// All stages, in order.
    pub const ALL: [Self; 4] = [
        Self::Installed,
        Self::Configured,
        Self::Running,
        Self::Ready,
    ];

    /// Stable lowercase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Configured => "configured",
            Self::Running => "running",
            Self::Ready => "ready",
        }
    }
}

impl fmt::Display for ChainStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainItem {
    /// Stable item id (`bwrap`, `cgroup-root`, ...).
    pub id: String,
    /// The stage it belongs to.
    pub stage: ChainStage,
    /// How good it is.
    pub level: ChainLevel,
    /// One line for the operator.
    pub detail: String,
}

impl ChainItem {
    fn new(id: &str, stage: ChainStage, level: ChainLevel, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            stage,
            level,
            detail: detail.into(),
        }
    }
}

/// One installed binary (Installed stage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainBinary {
    /// File name (`harw`, `killer`, ...).
    pub name: String,
    /// Where it was found, or why not.
    pub found: Result<PathBuf, String>,
    /// Missing is [`ChainLevel::Miss`] (else [`ChainLevel::Warn`]).
    pub required: bool,
}

/// The delegated cgroup root (mirror of `RootResolution` plus the probe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRoot {
    /// The root, absolute below `/sys/fs/cgroup`; `None`: nothing resolved.
    pub root: Option<PathBuf>,
    /// The root relative to the cgroup2 mount, with a leading `/`
    /// (`harw_job_runtime::host::cgroup_path_below_mount`).
    pub relative: Option<String>,
    /// Where it came from (`config`, `env`, `own-cgroup-delegated`, `none`).
    pub source: String,
    /// Operator hint of the resolver.
    pub hint: Option<String>,
    /// The host probe confirms the root as delegated (cgroup2, writable).
    pub delegated: bool,
}

/// Host facts of the Ready stage (from `HostReport` and the backend
/// resolver).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainHost {
    /// `std::env::consts::OS`. Off Linux the sandbox items are skipped.
    pub target_os: String,
    /// A trusted `bwrap` with user namespaces, or why not.
    pub bwrap: Result<PathBuf, String>,
    /// The `harw-job-exec` trampoline, or why not.
    pub trampoline: Result<PathBuf, String>,
    /// The usable Landlock ABI (`>= 1`), if any.
    pub landlock_abi: Option<u8>,
}

/// State of a service as `harw service status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServiceState {
    /// Running.
    Running,
    /// Installed but not running.
    Stopped,
    /// Not installed.
    NotInstalled,
    /// Could not be asked.
    Unknown {
        /// Why.
        reason: String,
    },
}

/// The DoD part (from a [`DodStatus`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainDod {
    /// [`DodStatus::suite_installed`].
    pub installed: bool,
    /// [`DOD_TARGET_UNIT`] is active; `None`: systemd could not be asked.
    pub target_active: Option<bool>,
    /// The system configuration.
    pub config: ConfigReport,
    /// Warden enablement.
    pub warden: WardenState,
}

impl ChainDod {
    /// The DoD facts of `status`.
    #[must_use]
    pub fn from_status(status: &DodStatus) -> Self {
        let target_active = status
            .units
            .iter()
            .find(|unit| unit.unit == DOD_TARGET_UNIT)
            .and_then(|unit| unit.is_installed().map(|_| unit.is_active()));
        Self {
            installed: status.suite_installed(),
            target_active,
            config: status.config.clone(),
            warden: status.warden,
        }
    }
}

/// Whether the serve worker's workspace root matches the project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeWorkspace {
    /// The serve worker's root (its working directory, the HARW home).
    pub serve_root: Option<PathBuf>,
    /// The project workspace of this invocation, if inside one.
    pub workspace: Option<PathBuf>,
}

/// Everything [`ExecutionChainReport::build`] needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainInputs {
    /// The binaries of the Installed stage.
    pub binaries: Vec<ChainBinary>,
    /// The harw config loads, or why not.
    pub config: Result<(), String>,
    /// The cgroup root, or why the configured one was refused.
    pub root: Result<ChainRoot, String>,
    /// Host facts.
    pub host: ChainHost,
    /// [`SERVE_SERVICE`]; `None`: not checked by this consumer.
    pub serve: Option<ServiceState>,
    /// The DoD part; `None`: not checked by this consumer.
    pub dod: Option<ChainDod>,
    /// Serve worker root vs. project.
    pub serve_workspace: ServeWorkspace,
}

/// How the active DoD profile relates to the cgroup root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileCoverage {
    /// Whole-host profile `id`.
    Host(String),
    /// Profile `id` observes the root and its descendants.
    Covers(String),
    /// Profile `id` does not observe the jobs below the root.
    NotCovered(String),
    /// No readable active profile.
    NoProfile,
}

/// Whether `ancestor` is `path` or one of its ancestors (whole path
/// components, trailing `/` ignored): `/a` covers `/a/b`, not `/ab`.
#[must_use]
pub fn same_or_below(path: &str, ancestor: &str) -> bool {
    let ancestor = ancestor.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// How the active profile of `config` relates to the cgroup root
/// `relative` (mount-relative, leading `/`). A cgroup profile covers it
/// only with an entry at or above it and `include_descendants = true`.
#[must_use]
pub fn profile_coverage(config: &ConfigReport, relative: &str) -> ProfileCoverage {
    let ConfigReport::Valid {
        active_profile: Some(profile),
        ..
    } = config
    else {
        return ProfileCoverage::NoProfile;
    };
    match &profile.scope {
        ProfileScope::Host => ProfileCoverage::Host(profile.id.clone()),
        ProfileScope::Cgroups {
            paths,
            include_descendants,
        } => {
            let covers = *include_descendants
                && paths
                    .iter()
                    .any(|path| same_or_below(relative, &path.to_string_lossy()));
            if covers {
                ProfileCoverage::Covers(profile.id.clone())
            } else {
                ProfileCoverage::NotCovered(profile.id.clone())
            }
        }
    }
}

/// The execution chain of this host (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionChainReport {
    /// Installed: one item per binary.
    pub installed: Vec<ChainItem>,
    /// Configured: the harw config loads.
    pub config: ChainItem,
    /// Configured: the delegated cgroup root resolves and is delegated.
    pub cgroup_root: ChainItem,
    /// Running: `harw-serve`.
    pub serve: ChainItem,
    /// Running: `harw-dod.target`.
    pub dod_target: ChainItem,
    /// Ready: trusted Bubblewrap with user namespaces.
    pub bwrap: ChainItem,
    /// Ready: the `harw-job-exec` trampoline.
    pub trampoline: ChainItem,
    /// Ready: the Landlock ABI.
    pub landlock_abi: ChainItem,
    /// Ready: the active DoD profile observes the cgroup root.
    pub dod_profile_covers_root: ChainItem,
    /// Ready: Warden enablement.
    pub warden: ChainItem,
    /// Ready: the serve worker's root matches the project workspace.
    pub serve_workspace: ChainItem,
}

impl ExecutionChainReport {
    /// Builds the report from `inputs` (pure).
    #[must_use]
    pub fn build(inputs: &ChainInputs) -> Self {
        use ChainLevel::{Miss, Ok, Skip, Warn};
        use ChainStage::{Configured, Installed, Ready, Running};
        let linux = inputs.host.target_os == "linux";
        let installed = inputs
            .binaries
            .iter()
            .map(|binary| match &binary.found {
                Result::Ok(path) => {
                    ChainItem::new(&binary.name, Installed, Ok, path.display().to_string())
                }
                Err(why) => ChainItem::new(
                    &binary.name,
                    Installed,
                    if binary.required { Miss } else { Warn },
                    why.clone(),
                ),
            })
            .collect();
        let config = match &inputs.config {
            Result::Ok(()) => ChainItem::new("config", Configured, Ok, "harw config loads"),
            Err(why) => ChainItem::new("config", Configured, Miss, why.clone()),
        };
        let cgroup_root = cgroup_root_item(&inputs.root, linux);
        let serve = match &inputs.serve {
            None => ChainItem::new(SERVE_SERVICE, Running, Skip, "not checked here"),
            Some(ServiceState::Running) => {
                ChainItem::new(SERVE_SERVICE, Running, Ok, "running (job worker)")
            }
            Some(ServiceState::Stopped) => ChainItem::new(
                SERVE_SERVICE,
                Running,
                Miss,
                "stopped: background jobs run only inside `harw chat` (`harw service install`)",
            ),
            Some(ServiceState::NotInstalled) => ChainItem::new(
                SERVE_SERVICE,
                Running,
                Miss,
                "not installed (`harw service install`)",
            ),
            Some(ServiceState::Unknown { reason }) => ChainItem::new(
                SERVE_SERVICE,
                Running,
                Warn,
                format!("state unknown: {reason}"),
            ),
        };
        let dod = inputs.dod.as_ref();
        let dod_installed = dod.filter(|dod| dod.installed);
        let dod_skip = |id: &str, stage| match dod {
            None => ChainItem::new(id, stage, Skip, "not checked here"),
            Some(_) => ChainItem::new(id, stage, Skip, "Harw-DoD not installed"),
        };
        let dod_target = match dod_installed {
            None => dod_skip(DOD_TARGET_UNIT, Running),
            Some(dod) => match dod.target_active {
                Some(true) => ChainItem::new(DOD_TARGET_UNIT, Running, Ok, "active"),
                Some(false) => ChainItem::new(
                    DOD_TARGET_UNIT,
                    Running,
                    Miss,
                    "inactive (`sudo harw dod enable`)",
                ),
                None => ChainItem::new(
                    DOD_TARGET_UNIT,
                    Running,
                    Warn,
                    "state unknown: systemd could not be asked",
                ),
            },
        };
        let host = &inputs.host;
        let bwrap = if !linux {
            ChainItem::new("bwrap", Ready, Skip, "Linux only")
        } else {
            match &host.bwrap {
                Result::Ok(path) => ChainItem::new(
                    "bwrap",
                    Ready,
                    Ok,
                    format!("{} with user namespaces", path.display()),
                ),
                Err(why) => ChainItem::new(
                    "bwrap",
                    Ready,
                    Warn,
                    format!("{why}: no namespaces, network isolation only by Landlock"),
                ),
            }
        };
        let trampoline = if !linux {
            ChainItem::new("trampoline", Ready, Skip, "Linux only")
        } else {
            match (&host.trampoline, host.landlock_abi) {
                (Result::Ok(path), Some(_)) => {
                    ChainItem::new("trampoline", Ready, Ok, path.display().to_string())
                }
                (Result::Ok(path), None) => ChainItem::new(
                    "trampoline",
                    Ready,
                    Warn,
                    format!(
                        "{} present, but no usable Landlock ABI for its backends",
                        path.display()
                    ),
                ),
                (Err(why), _) => ChainItem::new(
                    "trampoline",
                    Ready,
                    if host.bwrap.is_ok() { Warn } else { Miss },
                    format!("{why}: jobs do not join their cgroup before exec"),
                ),
            }
        };
        let landlock_abi = if !linux {
            ChainItem::new("landlock-abi", Ready, Skip, "Linux only")
        } else {
            match host.landlock_abi {
                Some(abi) if abi >= 5 => ChainItem::new(
                    "landlock-abi",
                    Ready,
                    Ok,
                    format!("ABI {abi}: filesystem enforced, TCP partial"),
                ),
                Some(abi) => ChainItem::new(
                    "landlock-abi",
                    Ready,
                    Warn,
                    format!("ABI {abi}: filesystem partial"),
                ),
                None => ChainItem::new(
                    "landlock-abi",
                    Ready,
                    if host.bwrap.is_ok() { Warn } else { Miss },
                    "no usable Landlock ABI",
                ),
            }
        };
        let relative = inputs
            .root
            .as_ref()
            .ok()
            .and_then(|root| root.relative.clone());
        let dod_profile_covers_root = match (dod_installed, relative) {
            (None, _) => dod_skip("dod-profile-covers-root", Ready),
            (Some(_), None) => {
                ChainItem::new("dod-profile-covers-root", Ready, Skip, "no cgroup root")
            }
            (Some(dod), Some(relative)) => match profile_coverage(&dod.config, &relative) {
                ProfileCoverage::Host(id) => ChainItem::new(
                    "dod-profile-covers-root",
                    Ready,
                    Ok,
                    format!("profile {id} observes the whole host"),
                ),
                ProfileCoverage::Covers(id) => ChainItem::new(
                    "dod-profile-covers-root",
                    Ready,
                    Ok,
                    format!("profile {id} covers {relative}"),
                ),
                ProfileCoverage::NotCovered(id) => ChainItem::new(
                    "dod-profile-covers-root",
                    Ready,
                    Warn,
                    format!("profile {id} does not observe {relative} (`harw dod config suggest`)"),
                ),
                ProfileCoverage::NoProfile => ChainItem::new(
                    "dod-profile-covers-root",
                    Ready,
                    Warn,
                    "no readable active profile",
                ),
            },
        };
        let warden = match dod_installed {
            None => dod_skip("warden", Ready),
            Some(dod) => match dod.warden {
                WardenState::Enabled => ChainItem::new(
                    "warden",
                    Ready,
                    Ok,
                    "enabled (every kill still needs a human-approved proof)",
                ),
                WardenState::Disabled => {
                    ChainItem::new("warden", Ready, Ok, "disabled: observe only")
                }
                WardenState::Unknown => ChainItem::new("warden", Ready, Warn, "state unknown"),
            },
        };
        let serve_workspace = serve_workspace_item(&inputs.serve, &inputs.serve_workspace);
        Self {
            installed,
            config,
            cgroup_root,
            serve,
            dod_target,
            bwrap,
            trampoline,
            landlock_abi,
            dod_profile_covers_root,
            warden,
            serve_workspace,
        }
    }

    /// All items in table order (by stage).
    #[must_use]
    pub fn items(&self) -> Vec<&ChainItem> {
        let mut items: Vec<&ChainItem> = self.installed.iter().collect();
        items.extend([
            &self.config,
            &self.cgroup_root,
            &self.serve,
            &self.dod_target,
            &self.bwrap,
            &self.trampoline,
            &self.landlock_abi,
            &self.dod_profile_covers_root,
            &self.warden,
            &self.serve_workspace,
        ]);
        items
    }

    /// The items of `stage`.
    #[must_use]
    pub fn stage_items(&self, stage: ChainStage) -> Vec<&ChainItem> {
        self.items()
            .into_iter()
            .filter(|item| item.stage == stage)
            .collect()
    }

    /// The worst level of `stage` (`Skip` when every item is skipped).
    #[must_use]
    pub fn stage_level(&self, stage: ChainStage) -> ChainLevel {
        let levels: Vec<ChainLevel> = self
            .stage_items(stage)
            .into_iter()
            .map(|item| item.level)
            .collect();
        [ChainLevel::Miss, ChainLevel::Warn, ChainLevel::Ok]
            .into_iter()
            .find(|level| levels.contains(level))
            .unwrap_or(ChainLevel::Skip)
    }

    /// The highest stage reached: no missing item in it or any stage
    /// below it. `None` when the binaries are already incomplete.
    #[must_use]
    pub fn reached(&self) -> Option<ChainStage> {
        ChainStage::ALL
            .into_iter()
            .take_while(|stage| self.stage_level(*stage) != ChainLevel::Miss)
            .last()
    }

    /// The operator table (`harw dod doctor`, `harw doctor`, verify log).
    #[must_use]
    pub fn table(&self) -> String {
        let reached = self.reached().map_or("none", ChainStage::as_str);
        let mut out = format!("execution chain: {reached}");
        for item in self.items() {
            out.push_str(&format!(
                "\n  {:<10} {:<24} {:<4} {}",
                item.stage.as_str(),
                item.id,
                item.level.as_str(),
                item.detail
            ));
        }
        out
    }

    /// One line for `stage`: the non-ok items, or how many are ok.
    #[must_use]
    pub fn stage_summary(&self, stage: ChainStage) -> String {
        let items = self.stage_items(stage);
        let issues: Vec<String> = items
            .iter()
            .filter(|item| matches!(item.level, ChainLevel::Warn | ChainLevel::Miss))
            .map(|item| format!("{} {}: {}", item.id, item.level, item.detail))
            .collect();
        if issues.is_empty() {
            let ok = items
                .iter()
                .filter(|item| item.level == ChainLevel::Ok)
                .count();
            format!("{ok} ok ({} skipped)", items.len() - ok)
        } else {
            issues.join("; ")
        }
    }
}

fn cgroup_root_item(root: &Result<ChainRoot, String>, linux: bool) -> ChainItem {
    use ChainLevel::{Miss, Ok, Skip};
    const ID: &str = "cgroup-root";
    let stage = ChainStage::Configured;
    if !linux {
        return ChainItem::new(ID, stage, Skip, "Linux only");
    }
    match root {
        Err(why) => ChainItem::new(ID, stage, Miss, format!("refused: {why}")),
        Result::Ok(ChainRoot {
            root: None, hint, ..
        }) => ChainItem::new(
            ID,
            stage,
            Miss,
            hint.clone()
                .unwrap_or_else(|| "no delegated cgroup v2 root".to_owned()),
        ),
        Result::Ok(ChainRoot {
            root: Some(path),
            source,
            delegated,
            ..
        }) => {
            if *delegated {
                ChainItem::new(
                    ID,
                    stage,
                    Ok,
                    format!("{} (source: {source}), delegated", path.display()),
                )
            } else {
                ChainItem::new(
                    ID,
                    stage,
                    Miss,
                    format!(
                        "{} (source: {source}) is not a delegated cgroup v2 root (cgroup2, \
                         writable): resource limits stay partial",
                        path.display()
                    ),
                )
            }
        }
    }
}

fn serve_workspace_item(serve: &Option<ServiceState>, facts: &ServeWorkspace) -> ChainItem {
    use ChainLevel::{Ok, Skip, Warn};
    const ID: &str = "serve-workspace";
    let stage = ChainStage::Ready;
    if !matches!(serve, Some(ServiceState::Running)) {
        return ChainItem::new(ID, stage, Skip, "harw-serve not running");
    }
    let (Some(serve_root), Some(workspace)) = (&facts.serve_root, &facts.workspace) else {
        return ChainItem::new(ID, stage, Skip, "not inside a project workspace");
    };
    if same_path(serve_root, workspace) {
        ChainItem::new(
            ID,
            stage,
            Ok,
            format!("serve worker root is {}", workspace.display()),
        )
    } else {
        ChainItem::new(
            ID,
            stage,
            Warn,
            format!(
                "serve worker jobs use {} (its working directory) as workspace root, not the \
                 project {}",
                serve_root.display(),
                workspace.display()
            ),
        )
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.components().eq(b.components())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ActiveProfile;

    fn bin(name: &str, found: bool, required: bool) -> ChainBinary {
        ChainBinary {
            name: name.to_owned(),
            found: if found {
                Ok(PathBuf::from(format!("/usr/local/bin/{name}")))
            } else {
                Err(format!("no {name} next to harw"))
            },
            required,
        }
    }

    fn dod(installed: bool) -> ChainDod {
        ChainDod {
            installed,
            target_active: Some(installed),
            config: ConfigReport::Valid {
                path: PathBuf::from("/etc/harw-dod/config.toml"),
                config_digest: "00".to_owned(),
                active_profile: Some(ActiveProfile {
                    id: "harw-jobs".to_owned(),
                    scope: ProfileScope::Cgroups {
                        paths: vec![PathBuf::from("/user.slice")],
                        include_descendants: true,
                    },
                    sensors: vec!["exec".to_owned()],
                    egress_allow_cidrs: 0,
                }),
            },
            warden: WardenState::Disabled,
        }
    }

    const ROOT: &str =
        "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service";

    fn full_chain() -> ChainInputs {
        ChainInputs {
            binaries: vec![
                bin("harw", true, true),
                bin("killer", true, true),
                bin("harw-agent-runner", true, true),
                bin("harw-job-exec", true, true),
            ],
            config: Ok(()),
            root: Ok(ChainRoot {
                root: Some(PathBuf::from(ROOT)),
                relative: ROOT.strip_prefix("/sys/fs/cgroup").map(str::to_owned),
                source: "own-cgroup-delegated".to_owned(),
                hint: None,
                delegated: true,
            }),
            host: ChainHost {
                target_os: "linux".to_owned(),
                bwrap: Ok(PathBuf::from("/usr/bin/bwrap")),
                trampoline: Ok(PathBuf::from("/usr/local/bin/harw-job-exec")),
                landlock_abi: Some(6),
            },
            serve: Some(ServiceState::Running),
            dod: Some(dod(true)),
            // Outside a project workspace (e.g. cwd in the user home).
            serve_workspace: ServeWorkspace {
                serve_root: Some(PathBuf::from("/home/u/.harw")),
                workspace: None,
            },
        }
    }

    const FULL_CHAIN: &str = "execution chain: ready
  installed  harw                     ok   /usr/local/bin/harw
  installed  killer                   ok   /usr/local/bin/killer
  installed  harw-agent-runner        ok   /usr/local/bin/harw-agent-runner
  installed  harw-job-exec            ok   /usr/local/bin/harw-job-exec
  configured config                   ok   harw config loads
  configured cgroup-root              ok   /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service (source: own-cgroup-delegated), delegated
  running    harw-serve               ok   running (job worker)
  running    harw-dod.target          ok   active
  ready      bwrap                    ok   /usr/bin/bwrap with user namespaces
  ready      trampoline               ok   /usr/local/bin/harw-job-exec
  ready      landlock-abi             ok   ABI 6: filesystem enforced, TCP partial
  ready      dod-profile-covers-root  ok   profile harw-jobs covers /user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service
  ready      warden                   ok   disabled: observe only
  ready      serve-workspace          skip not inside a project workspace";

    const NO_BWRAP: &str = "execution chain: ready
  installed  harw                     ok   /usr/local/bin/harw
  installed  killer                   ok   /usr/local/bin/killer
  installed  harw-agent-runner        ok   /usr/local/bin/harw-agent-runner
  installed  harw-job-exec            ok   /usr/local/bin/harw-job-exec
  configured config                   ok   harw config loads
  configured cgroup-root              ok   /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service (source: own-cgroup-delegated), delegated
  running    harw-serve               ok   running (job worker)
  running    harw-dod.target          ok   active
  ready      bwrap                    warn no trusted bwrap at /usr/bin/bwrap, /bin/bwrap: no namespaces, network isolation only by Landlock
  ready      trampoline               ok   /usr/local/bin/harw-job-exec
  ready      landlock-abi             ok   ABI 6: filesystem enforced, TCP partial
  ready      dod-profile-covers-root  ok   profile harw-jobs covers /user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service
  ready      warden                   ok   disabled: observe only
  ready      serve-workspace          skip not inside a project workspace";

    const NO_DELEGATION: &str = "execution chain: installed
  installed  harw                     ok   /usr/local/bin/harw
  installed  killer                   ok   /usr/local/bin/killer
  installed  harw-agent-runner        ok   /usr/local/bin/harw-agent-runner
  installed  harw-job-exec            ok   /usr/local/bin/harw-job-exec
  configured config                   ok   harw config loads
  configured cgroup-root              miss no delegated cgroup v2 root: set `[jobs] cgroup_root`
  running    harw-serve               ok   running (job worker)
  running    harw-dod.target          ok   active
  ready      bwrap                    ok   /usr/bin/bwrap with user namespaces
  ready      trampoline               ok   /usr/local/bin/harw-job-exec
  ready      landlock-abi             ok   ABI 6: filesystem enforced, TCP partial
  ready      dod-profile-covers-root  skip no cgroup root
  ready      warden                   ok   disabled: observe only
  ready      serve-workspace          warn serve worker jobs use /home/u/.harw (its working directory) as workspace root, not the project /home/u/src/app";

    const NO_DOD: &str = "execution chain: ready
  installed  harw                     ok   /usr/local/bin/harw
  installed  killer                   ok   /usr/local/bin/killer
  installed  harw-agent-runner        ok   /usr/local/bin/harw-agent-runner
  installed  harw-job-exec            ok   /usr/local/bin/harw-job-exec
  configured config                   ok   harw config loads
  configured cgroup-root              ok   /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service (source: own-cgroup-delegated), delegated
  running    harw-serve               ok   running (job worker)
  running    harw-dod.target          skip Harw-DoD not installed
  ready      bwrap                    ok   /usr/bin/bwrap with user namespaces
  ready      trampoline               ok   /usr/local/bin/harw-job-exec
  ready      landlock-abi             ok   ABI 6: filesystem enforced, TCP partial
  ready      dod-profile-covers-root  skip Harw-DoD not installed
  ready      warden                   skip Harw-DoD not installed
  ready      serve-workspace          skip not inside a project workspace";

    #[test]
    fn chain_table_golden_for_the_four_host_situations() {
        let full = ExecutionChainReport::build(&full_chain());
        assert_eq!(full.table(), FULL_CHAIN);
        assert_eq!(full.reached(), Some(ChainStage::Ready));

        let mut inputs = full_chain();
        inputs.host.bwrap = Err("no trusted bwrap at /usr/bin/bwrap, /bin/bwrap".to_owned());
        let no_bwrap = ExecutionChainReport::build(&inputs);
        assert_eq!(no_bwrap.table(), NO_BWRAP);
        assert_eq!(no_bwrap.stage_level(ChainStage::Ready), ChainLevel::Warn);

        let mut inputs = full_chain();
        inputs.root = Ok(ChainRoot {
            root: None,
            relative: None,
            source: "none".to_owned(),
            hint: Some("no delegated cgroup v2 root: set `[jobs] cgroup_root`".to_owned()),
            delegated: false,
        });
        inputs.serve_workspace.workspace = Some(PathBuf::from("/home/u/src/app"));
        let no_delegation = ExecutionChainReport::build(&inputs);
        assert_eq!(no_delegation.table(), NO_DELEGATION);
        assert_eq!(no_delegation.reached(), Some(ChainStage::Installed));

        let mut inputs = full_chain();
        inputs.dod = Some(dod(false));
        let no_dod = ExecutionChainReport::build(&inputs);
        assert_eq!(no_dod.table(), NO_DOD);
        assert_eq!(no_dod.reached(), Some(ChainStage::Ready));
    }

    #[test]
    fn chain_marks_missing_pieces_and_skips_what_is_not_checked() {
        let mut inputs = full_chain();
        inputs.binaries[1] = bin("killer", false, true);
        inputs.binaries[2] = bin("harw-agent-runner", false, true);
        inputs.host.bwrap = Err("no bwrap".to_owned());
        inputs.host.trampoline = Err("no harw-job-exec".to_owned());
        inputs.host.landlock_abi = None;
        inputs.serve = None;
        inputs.dod = None;
        let report = ExecutionChainReport::build(&inputs);
        assert_eq!(report.reached(), None);
        assert_eq!(report.installed[1].level, ChainLevel::Miss);
        assert_eq!(report.installed[2].level, ChainLevel::Miss);
        assert_eq!(report.trampoline.level, ChainLevel::Miss);
        assert_eq!(report.landlock_abi.level, ChainLevel::Miss);
        assert_eq!(report.serve.level, ChainLevel::Skip);
        assert_eq!(report.warden.detail, "not checked here");
        assert_eq!(report.serve_workspace.level, ChainLevel::Skip);
        assert!(
            report
                .stage_summary(ChainStage::Installed)
                .contains("killer miss")
        );
        assert_eq!(
            report.stage_summary(ChainStage::Configured),
            "2 ok (0 skipped)"
        );

        // Ok only when the serve worker's HARW home is the project.
        let mut inputs = full_chain();
        inputs.serve_workspace.workspace = Some(PathBuf::from("/home/u/.harw"));
        let report = ExecutionChainReport::build(&inputs);
        assert_eq!(report.serve_workspace.level, ChainLevel::Ok);
        let mut inputs = full_chain();
        inputs.serve_workspace.workspace = Some(PathBuf::from("/home/u/src/app"));
        let report = ExecutionChainReport::build(&inputs);
        assert_eq!(report.serve_workspace.level, ChainLevel::Warn);

        // A root that resolves but is not delegated stays missing.
        let mut inputs = full_chain();
        if let Ok(root) = &mut inputs.root {
            root.delegated = false;
        }
        let report = ExecutionChainReport::build(&inputs);
        assert_eq!(report.cgroup_root.level, ChainLevel::Miss);
        assert_eq!(report.reached(), Some(ChainStage::Installed));

        // Off Linux the sandbox items are not applicable.
        let mut inputs = full_chain();
        inputs.host.target_os = "macos".to_owned();
        let report = ExecutionChainReport::build(&inputs);
        for item in [
            &report.bwrap,
            &report.trampoline,
            &report.landlock_abi,
            &report.cgroup_root,
        ] {
            assert_eq!(item.level, ChainLevel::Skip, "{item:?}");
        }
    }

    #[test]
    fn profile_coverage_needs_an_ancestor_with_descendants() {
        let profile = |paths: &[&str], include_descendants: bool| ConfigReport::Valid {
            path: PathBuf::from("/etc/harw-dod/config.toml"),
            config_digest: "00".to_owned(),
            active_profile: Some(ActiveProfile {
                id: "p".to_owned(),
                scope: ProfileScope::Cgroups {
                    paths: paths.iter().map(PathBuf::from).collect(),
                    include_descendants,
                },
                sensors: Vec::new(),
                egress_allow_cidrs: 0,
            }),
        };
        let covers = |config| profile_coverage(&config, "/harw.slice");
        assert_eq!(
            covers(profile(&["/"], true)),
            ProfileCoverage::Covers("p".to_owned())
        );
        assert_eq!(
            covers(profile(&["/harw.slice/"], true)),
            ProfileCoverage::Covers("p".to_owned())
        );
        assert_eq!(
            covers(profile(&["/harw"], true)),
            ProfileCoverage::NotCovered("p".to_owned())
        );
        assert_eq!(
            covers(profile(&["/harw.slice"], false)),
            ProfileCoverage::NotCovered("p".to_owned())
        );
        let missing = ConfigReport::Missing {
            path: PathBuf::from("/etc/harw-dod/config.toml"),
        };
        assert_eq!(covers(missing), ProfileCoverage::NoProfile);
    }
}
