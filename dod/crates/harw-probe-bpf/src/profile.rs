//! The active DoD profile of this probe (PL-97 E3c): loading, sensor
//! selection and the attach-time BPF scope.
//!
//! # Source and fail-closed rule
//! The profile comes exclusively from `harw-dod-config`: `--config <PATH>`
//! is read through `harw_dod_config::load_config`, without the flag the fixed
//! `harw_dod_config::SYSTEM_CONFIG_PATH` through
//! `harw_dod_config::load_system_config` (both walk the path descriptor by
//! descriptor, without following symlinks, and require root-owned,
//! non-group/world-writable components). [`load_active_profile`] then calls
//! `Config::resolve_active`.
//!
//! Every error refuses to start ([`ProbeError::Config`]): an untrusted owner
//! or mode, a symlink, a parse or schema error, a missing or unknown
//! `active_profile` — exactly the cases in which `harw-sentinel` refuses to
//! start. Unlike the unprivileged sentinel, this probe also refuses when the
//! system file does not exist: it holds `CAP_BPF`, and starting without a
//! selected profile would mean observing with a scope nobody selected. The
//! host scope is never a fallback.
//!
//! # What the profile selects
//! - **Sensors** ([`SensorSelection`]): `exec` loads the process sensor
//!   (`exec.bpf.o` + `exit.bpf.o`), `tcp-connect` the connection sensor
//!   (`tcp_v4_connect.bpf.o` + `tcp_v6_connect.bpf.o`). A sensor the profile
//!   does not list is neither resolved nor loaded nor reported as degraded:
//!   it is switched off on purpose.
//! - **Egress CIDRs**: `egress_allow_cidrs` becomes the `NetworkScope` of the
//!   connection sensor (`main::network_scope`); an empty list reports every
//!   observed connection.
//! - **Scope** ([`resolve_scope`], before Landlock is applied):
//!   - `scope = "host"` → `BpfScope::Host`, no rescan, `--cgroup-root` is
//!     neither validated nor read nor added to Landlock (unchanged
//!     behaviour).
//!   - `scope = "cgroups"`, `include_descendants = false` → every configured
//!     path resolved below `--cgroup-root`
//!     (`harw_dod_bpf::cgroup_walk::resolve_cgroup`) into a fixed
//!     `BpfScope::Cgroups` without descendants; no rescan.
//!   - `scope = "cgroups"`, `include_descendants = true` → the resolved roots
//!     become a `harw_dod_bpf::DescendantScope`; the descendants that exist
//!     now are walked (`walk_descendants`), planned and committed, so the
//!     attach-time map already holds them, and
//!     [`ProbeScope::into_rescan`] hands the scope to the 2 s rescan
//!     (`crate::scope_rescan`).
//!
//! # Missing roots
//! A configured cgroup path that does not resolve at start is logged with
//! its error, kept in [`ProbeScope::unresolved_roots`] and counted in
//! [`ProbeScope::startup_health`] (`roots_unavailable`): the start is
//! degraded, `main::run` sends `SensorDegraded` for every loaded sensor, and
//! the rescan keeps counting it in every round. It is not re-resolved later:
//! a cgroup created under that path afterwards would be a different cgroup
//! than the one the operator saw (see `harw_dod_config`), so admitting it
//! needs a restart. When **no** configured path resolves, [`resolve_scope`]
//! refuses with [`ProbeError::CgroupScopeUnresolved`]: a cgroup profile
//! never degrades into host observation.
//!
//! # `--cgroup-root`
//! For a cgroup profile `--cgroup-root` must be absolute, must not be `/` and
//! must not contain `..` ([`ProbeError::CgroupRootInvalid`]): with a
//! descendant scope it becomes a list-only Landlock root (see
//! `crate::landlock`, "Die cgroup2-Wurzel"), and that widening must never
//! cover the whole file system.
//!
//! # Concurrency
//! Called once from the main thread before Landlock; [`resolve_scope`]
//! performs blocking file-system I/O on that thread.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use harw_dod_bpf::cgroup_walk::{self, CgroupFs, WalkLimits};
use harw_dod_bpf::{BpfScope, DescendantScope, MAX_SCOPE_CGROUP_IDS, ResolvedCgroup};
use harw_dod_config::{
    CgroupScope, Config, ConfigError, ObservationScope, ResolvedObservationProfile,
    SYSTEM_CONFIG_PATH, Sensor,
};

use crate::error::ProbeError;
use crate::scope_rescan::{ScopeHealth, ScopeRescan};

/// Loads and resolves the active DoD profile.
///
/// # Description
/// `explicit = Some(path)` reads `--config` through
/// `harw_dod_config::load_config`, `None` the fixed system path through
/// `harw_dod_config::load_system_config`; see [`resolve_active_profile`].
/// Logs profile, digest, sensors and the number of egress CIDRs, so the
/// digest can be matched against the sentinel's start line.
///
/// # Errors
/// [`ProbeError::Config`] for every load, trust, parse or resolve error,
/// including a missing file.
pub fn load_active_profile(
    explicit: Option<&Path>,
) -> Result<ResolvedObservationProfile, ProbeError> {
    let (source, loaded) = match explicit {
        Some(path) => (path.to_path_buf(), harw_dod_config::load_config(path)),
        None => (
            PathBuf::from(SYSTEM_CONFIG_PATH),
            harw_dod_config::load_system_config(),
        ),
    };
    let profile = resolve_active_profile(&source, loaded)?;
    tracing::info!(
        source = %source.display(),
        profile = profile.profile_id().as_str(),
        config_digest = %profile.config_digest(),
        sensors = ?profile.sensors(),
        egress_allow_cidrs = profile.egress_allow_cidrs().len(),
        "DoD configuration loaded"
    );
    Ok(profile)
}

/// Resolves the active profile of a load result.
///
/// # Description
/// Separate from [`load_active_profile`] so the fail-closed decision is
/// testable without a root-owned file. There is no fallback of any kind:
/// a missing system file is an error like every other.
///
/// # Arguments
/// - `source` (`&Path`): the path that was read (for the error message).
/// - `loaded` (`Result<Config, ConfigError>`): the load result.
///
/// # Errors
/// [`ProbeError::Config`] for every error of `loaded` and of
/// `Config::resolve_active`.
pub fn resolve_active_profile(
    source: &Path,
    loaded: Result<Config, ConfigError>,
) -> Result<ResolvedObservationProfile, ProbeError> {
    loaded
        .and_then(|config| config.resolve_active())
        .map_err(|error| ProbeError::Config {
            source: source.display().to_string(),
            error,
        })
}

/// Which of the two sensors of this probe the profile enables.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SensorSelection {
    /// `exec`: process start/exit (`exec.bpf.o`, `exit.bpf.o`).
    pub exec: bool,
    /// `tcp-connect`: outgoing TCP connections (`tcp_v4_connect.bpf.o`,
    /// `tcp_v6_connect.bpf.o`).
    pub tcp_connect: bool,
}

impl SensorSelection {
    /// The selection of a profile's `sensors` list.
    #[must_use]
    pub fn from_profile(sensors: &[Sensor]) -> Self {
        let mut selection = Self::default();
        for sensor in sensors {
            match sensor {
                Sensor::Exec => selection.exec = true,
                Sensor::TcpConnect => selection.tcp_connect = true,
            }
        }
        selection
    }
}

/// A configured cgroup path that did not resolve at start.
#[derive(Debug)]
pub struct UnresolvedRoot {
    /// The mount-relative path from the profile.
    pub path: String,
    /// Why it did not resolve (e.g. `NotFound`, or `InvalidInput` for a
    /// non-directory or a symlink).
    pub error: std::io::Error,
}

/// The attach-time scope of the active profile.
#[derive(Debug)]
pub struct ProbeScope {
    kind: ScopeKind,
    unresolved: Vec<UnresolvedRoot>,
    startup: ScopeHealth,
}

#[derive(Debug)]
enum ScopeKind {
    /// `scope = "host"`.
    Host,
    /// `scope = "cgroups"` without descendants: a final map.
    Fixed(BpfScope),
    /// `scope = "cgroups"` with descendants: expanded now, rescanned later.
    Descendants(DescendantScope),
}

impl ProbeScope {
    /// The host scope: nothing to resolve, nothing to rescan.
    #[must_use]
    pub const fn host() -> Self {
        Self {
            kind: ScopeKind::Host,
            unresolved: Vec::new(),
            startup: ScopeHealth {
                overflowed: 0,
                depth_limited: 0,
                roots_unavailable: 0,
                incomplete: false,
                apply_failed: false,
            },
        }
    }

    /// The scope every BPF object contract is loaded with.
    #[must_use]
    pub fn bpf_scope(&self) -> BpfScope {
        match &self.kind {
            ScopeKind::Host => BpfScope::Host,
            ScopeKind::Fixed(scope) => scope.clone(),
            ScopeKind::Descendants(scope) => scope.bpf_scope(),
        }
    }

    /// `true` when a descendant rescan runs after attach. Exactly then the
    /// probe needs to list directories below `--cgroup-root` after
    /// `restrict_self` (see `main::fs_scope_roots`).
    #[must_use]
    pub const fn rescans(&self) -> bool {
        matches!(self.kind, ScopeKind::Descendants(_))
    }

    /// A short label for logs: `host`, `cgroups` or `cgroups+descendants`.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self.kind {
            ScopeKind::Host => "host",
            ScopeKind::Fixed(_) => "cgroups",
            ScopeKind::Descendants(_) => "cgroups+descendants",
        }
    }

    /// Configured roots that did not resolve at start.
    #[must_use]
    pub fn unresolved_roots(&self) -> &[UnresolvedRoot] {
        &self.unresolved
    }

    /// What kept the attach-time scope from covering the whole profile.
    #[must_use]
    pub const fn startup_health(&self) -> ScopeHealth {
        self.startup
    }

    /// The descendant scope, if the profile has one.
    #[must_use]
    pub const fn descendant_scope(&self) -> Option<&DescendantScope> {
        match &self.kind {
            ScopeKind::Descendants(scope) => Some(scope),
            ScopeKind::Host | ScopeKind::Fixed(_) => None,
        }
    }

    /// Hands a descendant scope to the rescan loop; `None` for the host
    /// scope and a fixed cgroup scope (nothing to rescan).
    ///
    /// # Arguments
    /// - `cgroup_root` (`PathBuf`): `--cgroup-root`.
    /// - `interval` (`Duration`): the rescan interval.
    /// - `limits` (`WalkLimits`): the walk bounds (as at attach time).
    /// - `now` (`Instant`): the first round is due one `interval` later.
    #[must_use]
    pub fn into_rescan(
        self,
        cgroup_root: PathBuf,
        interval: Duration,
        limits: WalkLimits,
        now: Instant,
    ) -> Option<ScopeRescan> {
        let unresolved = self.unresolved.len();
        let startup = self.startup;
        match self.kind {
            ScopeKind::Descendants(scope) => Some(
                ScopeRescan::new(scope, cgroup_root, interval, limits, now)
                    .with_startup(unresolved, startup),
            ),
            ScopeKind::Host | ScopeKind::Fixed(_) => None,
        }
    }
}

/// Resolves the attach-time scope of a profile, before Landlock.
///
/// # Description
/// See the module docs, "What the profile selects" and "Missing roots".
/// The host scope touches no file. A cgroup scope validates `cgroup_root`,
/// resolves every configured path below it, and, with
/// `include_descendants = true`, expands the roots by one bounded walk
/// (`limits`) into a committed [`DescendantScope`].
///
/// # Arguments
/// - `fs` (`&dyn CgroupFs`): `HostCgroupFs` in production.
/// - `cgroup_root` (`&Path`): `--cgroup-root`, the cgroup2 mount.
/// - `scope` (`&ObservationScope`): the profile scope.
/// - `limits` (`WalkLimits`): bounds of the attach-time walk.
///
/// # Errors
/// - [`ProbeError::CgroupRootInvalid`]: `cgroup_root` is relative, `/` or
///   contains `..` (cgroup profiles only).
/// - [`ProbeError::CgroupScopeUnresolved`]: no configured path resolved.
/// - [`ProbeError::CgroupScopeRejected`]: the resolved ids do not form a
///   valid BPF scope.
pub fn resolve_scope(
    fs: &dyn CgroupFs,
    cgroup_root: &Path,
    scope: &ObservationScope,
    limits: WalkLimits,
) -> Result<ProbeScope, ProbeError> {
    let cgroups = match scope {
        ObservationScope::Host => return Ok(ProbeScope::host()),
        ObservationScope::Cgroups(cgroups) => cgroups,
    };
    validate_cgroup_root(cgroup_root)?;
    let (roots, unresolved) = resolve_roots(fs, cgroup_root, cgroups);
    if roots.is_empty() {
        tracing::error!(
            cgroup_root = %cgroup_root.display(),
            configured = cgroups.paths().len(),
            "no configured cgroup root of the active DoD profile exists; refusing to attach"
        );
        return Err(ProbeError::CgroupScopeUnresolved {
            cgroup_root: cgroup_root.display().to_string(),
            configured: cgroups.paths().len(),
        });
    }

    if !cgroups.include_descendants() {
        let scope = BpfScope::Cgroups {
            groups: roots,
            include_descendants: false,
            descendants: Vec::new(),
        };
        scope.validate().map_err(ProbeError::CgroupScopeRejected)?;
        let startup = ScopeHealth {
            roots_unavailable: unresolved.len(),
            ..ScopeHealth::default()
        };
        return Ok(ProbeScope {
            kind: ScopeKind::Fixed(scope),
            unresolved,
            startup,
        });
    }

    let mut scope =
        DescendantScope::new(roots, MAX_SCOPE_CGROUP_IDS).map_err(ProbeError::CgroupScopeRejected)?;
    let report = cgroup_walk::walk_descendants(fs, cgroup_root, scope.roots(), limits);
    let plan = scope.plan(&report.observation);
    let overflowed = plan.overflowed;
    scope.commit(plan).map_err(ProbeError::CgroupScopeRejected)?;
    let startup = ScopeHealth {
        overflowed,
        depth_limited: report.depth_limited,
        roots_unavailable: report.roots_unavailable.saturating_add(unresolved.len()),
        incomplete: !report.observation.complete,
        apply_failed: false,
    };
    if startup.is_degraded() {
        tracing::warn!(
            overflowed,
            depth_limited = report.depth_limited,
            roots_unavailable = startup.roots_unavailable,
            walk_incomplete = startup.incomplete,
            walk_errors = report.errors,
            walk_truncated = report.truncated,
            installed = scope.installed_ids().len(),
            capacity = scope.capacity(),
            "bpf cgroup scope degraded at start: part of the profile scope is not observed"
        );
    }
    Ok(ProbeScope {
        kind: ScopeKind::Descendants(scope),
        unresolved,
        startup,
    })
}

/// Resolves every configured path; unresolvable ones are logged and kept.
fn resolve_roots(
    fs: &dyn CgroupFs,
    cgroup_root: &Path,
    cgroups: &CgroupScope,
) -> (Vec<ResolvedCgroup>, Vec<UnresolvedRoot>) {
    let mut roots = Vec::with_capacity(cgroups.paths().len());
    let mut unresolved = Vec::new();
    for configured in cgroups.paths() {
        let path = configured.as_path().display().to_string();
        let resolved = match configured.as_path().to_str() {
            Some(text) => cgroup_walk::resolve_cgroup(fs, cgroup_root, text),
            // Nicht erreichbar: `harw_dod_config` parst Pfade aus UTF-8.
            None => Err(std::io::ErrorKind::InvalidData.into()),
        };
        match resolved {
            Ok(root) => roots.push(root),
            Err(error) => {
                tracing::warn!(
                    path = %path,
                    cgroup_root = %cgroup_root.display(),
                    error = %error,
                    "configured cgroup root of the active DoD profile is unavailable at start; \
                     it is not observed until the probe is restarted (degraded)"
                );
                unresolved.push(UnresolvedRoot { path, error });
            }
        }
    }
    (roots, unresolved)
}

/// Rejects a `--cgroup-root` that would turn the list-only Landlock root
/// into a view of the whole tree or depend on the working directory.
fn validate_cgroup_root(cgroup_root: &Path) -> Result<(), ProbeError> {
    let mut components = cgroup_root.components();
    let valid = components.next() == Some(Component::RootDir)
        && components.clone().next().is_some()
        && components.all(|component| matches!(component, Component::Normal(_)));
    if valid {
        Ok(())
    } else {
        Err(ProbeError::CgroupRootInvalid {
            path: cgroup_root.display().to_string(),
        })
    }
}

/// A fake cgroup v2 tree below a tempdir, as the probe would see it through
/// `--cgroup-root`. Shared by the tests of this module and of `main`.
#[cfg(test)]
pub(crate) mod test_tree {
    use std::path::Path;

    use harw_dod_bpf::cgroup_walk::{HostCgroupFs, host_path, resolve_cgroup};

    use crate::test_support::{TestResult, ctx};

    pub(crate) struct FakeTree {
        dir: tempfile::TempDir,
    }

    impl FakeTree {
        pub(crate) fn new() -> TestResult<Self> {
            Ok(Self {
                dir: tempfile::tempdir().map_err(ctx("tempdir"))?,
            })
        }

        pub(crate) fn root(&self) -> &Path {
            self.dir.path()
        }

        /// Creates a cgroup directory (with one interface file) and returns
        /// its id.
        pub(crate) fn mkcg(&self, path: &str) -> TestResult<u64> {
            let dir = host_path(self.root(), path);
            std::fs::create_dir_all(&dir).map_err(ctx("create fake cgroup"))?;
            std::fs::write(dir.join("cgroup.procs"), b"").map_err(ctx("interface file"))?;
            self.id(path)
        }

        pub(crate) fn id(&self, path: &str) -> TestResult<u64> {
            Ok(resolve_cgroup(&HostCgroupFs, self.root(), path)
                .map_err(ctx("resolve fake cgroup"))?
                .id)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeSet;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::time::Instant;

    use harw_dod_bpf::cgroup_walk::{HostCgroupFs, WalkLimits};
    use harw_dod_bpf::{BpfHandle, BpfProgramSource, BpfScope, FixtureBpfLoader};
    use harw_dod_config::{Config, ConfigError, ObservationScope, ResolvedObservationProfile};
    use harw_types::SensorId;

    use super::test_tree::FakeTree;
    use super::{
        ProbeScope, SensorSelection, load_active_profile, resolve_active_profile, resolve_scope,
    };
    use crate::error::ProbeError;
    use crate::scope_rescan::SCOPE_RESCAN_INTERVAL;
    use crate::test_support::{TestError, TestResult, ctx};

    const SOURCE: &str = "/etc/harw-dod/config.toml";

    fn profile(toml: &str) -> TestResult<ResolvedObservationProfile> {
        resolve_active_profile(Path::new(SOURCE), Config::from_toml(toml))
            .map_err(ctx("valid test profile"))
    }

    fn cgroup_profile(paths: &[&str], include_descendants: bool) -> TestResult<ObservationScope> {
        let paths = paths
            .iter()
            .map(|path| format!("\"{path}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let toml = format!(
            r#"
schema_version = 1
mode = "observe"
active_profile = "jobs"

[profiles.jobs]
scope = "cgroups"
cgroup_paths = [{paths}]
include_descendants = {include_descendants}
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = ["10.0.0.0/8"]
"#
        );
        Ok(profile(&toml)?.scope().clone())
    }

    const HOST_PROFILE: &str = r#"
schema_version = 1
mode = "observe"
active_profile = "host"

[profiles.host]
scope = "host"
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []
"#;

    fn ids(groups: &[harw_dod_bpf::ResolvedCgroup]) -> BTreeSet<u64> {
        groups.iter().map(|group| group.id).collect()
    }

    fn resolve(tree: &FakeTree, scope: &ObservationScope) -> Result<ProbeScope, ProbeError> {
        resolve_scope(&HostCgroupFs, tree.root(), scope, WalkLimits::default())
    }

    #[test]
    fn cgroup_profile_with_descendants_yields_roots_and_existing_descendants() -> TestResult {
        let tree = FakeTree::new()?;
        let root = tree.mkcg("/harw.slice")?;
        let job = tree.mkcg("/harw.slice/harw-job-1")?;
        let step = tree.mkcg("/harw.slice/harw-job-1/step")?;
        let outside = tree.mkcg("/system.slice")?;

        let scope = resolve(&tree, &cgroup_profile(&["/harw.slice"], true)?)
            .map_err(ctx("resolve descendant scope"))?;
        assert!(scope.rescans());
        assert_eq!(scope.label(), "cgroups+descendants");
        assert!(scope.unresolved_roots().is_empty());
        assert!(!scope.startup_health().is_degraded());
        let BpfScope::Cgroups {
            groups,
            include_descendants,
            descendants,
        } = scope.bpf_scope()
        else {
            return Err(TestError::Unexpected("expected a cgroup scope".into()));
        };
        assert!(include_descendants);
        assert_eq!(ids(&groups), BTreeSet::from([root]));
        assert_eq!(ids(&descendants), BTreeSet::from([job, step]));
        assert!(!ids(&descendants).contains(&outside));
        Ok(())
    }

    #[test]
    fn host_profile_yields_host_scope_without_rescan_and_ignores_cgroup_root() -> TestResult {
        let profile = profile(HOST_PROFILE)?;
        // Für den Host-Scope wird `--cgroup-root` weder geprüft noch gelesen.
        let scope = resolve_scope(
            &HostCgroupFs,
            Path::new("relative/never/read"),
            profile.scope(),
            WalkLimits::default(),
        )
        .map_err(ctx("host scope"))?;
        assert_eq!(scope.bpf_scope(), BpfScope::Host);
        assert!(!scope.rescans());
        assert_eq!(scope.label(), "host");
        assert!(scope.descendant_scope().is_none());
        assert!(!scope.startup_health().is_degraded());
        assert!(
            scope
                .into_rescan(
                    "/sys/fs/cgroup".into(),
                    SCOPE_RESCAN_INTERVAL,
                    WalkLimits::default(),
                    Instant::now(),
                )
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn fixed_cgroup_profile_has_no_descendants_and_no_rescan() -> TestResult {
        let tree = FakeTree::new()?;
        let root = tree.mkcg("/harw.slice")?;
        tree.mkcg("/harw.slice/harw-job-1")?;
        let scope = resolve(&tree, &cgroup_profile(&["/harw.slice"], false)?)
            .map_err(ctx("resolve fixed scope"))?;
        assert!(!scope.rescans());
        assert_eq!(
            scope.bpf_scope(),
            BpfScope::Cgroups {
                groups: vec![harw_dod_bpf::ResolvedCgroup {
                    path: "/harw.slice".to_owned(),
                    id: root,
                }],
                include_descendants: false,
                descendants: Vec::new(),
            }
        );
        assert!(
            scope
                .into_rescan(
                    "/sys/fs/cgroup".into(),
                    SCOPE_RESCAN_INTERVAL,
                    WalkLimits::default(),
                    Instant::now(),
                )
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn invalid_or_unselected_config_refuses_to_start() -> TestResult {
        let source = Path::new(SOURCE);
        let cases: Vec<(&str, Result<Config, ConfigError>)> = vec![
            ("parse error", Config::from_toml("schema_version = 1\nmode = ")),
            (
                "unknown field",
                Config::from_toml(&HOST_PROFILE.replace("egress_allow_cidrs", "egress_allow")),
            ),
            (
                "no active profile",
                Config::from_toml(&HOST_PROFILE.replace("active_profile = \"host\"", "")),
            ),
            (
                "unknown active profile",
                Config::from_toml(&HOST_PROFILE.replace("active_profile = \"host\"", "active_profile = \"jobs\"")),
            ),
            // Keine eingebauten Vorgaben, kein Host-Fallback: auch eine
            // fehlende Systemdatei verhindert den Start.
            (
                "missing system file",
                Err(ConfigError::Io(ErrorKind::NotFound.into())),
            ),
            (
                "untrusted file",
                Err(ConfigError::UntrustedSystemPath { path: SOURCE }),
            ),
        ];
        for (case, loaded) in cases {
            match resolve_active_profile(source, loaded) {
                Err(ProbeError::Config { source, .. }) => assert_eq!(source, SOURCE, "{case}"),
                other => {
                    return Err(TestError::Unexpected(format!("{case}: {other:?}")));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn explicit_relative_or_missing_config_path_refuses_to_start() -> TestResult {
        for path in ["relative/config.toml", "/nonexistent-harw-dod/config.toml"] {
            match load_active_profile(Some(Path::new(path))) {
                Err(ProbeError::Config { source, .. }) => assert_eq!(source, path),
                other => return Err(TestError::Unexpected(format!("{path}: {other:?}"))),
            }
        }
        Ok(())
    }

    #[test]
    fn no_resolvable_root_refuses_the_cgroup_attach() -> TestResult {
        let tree = FakeTree::new()?;
        tree.mkcg("/system.slice")?;
        for include_descendants in [true, false] {
            let scope = cgroup_profile(&["/harw.slice", "/other.slice"], include_descendants)?;
            match resolve(&tree, &scope) {
                Err(ProbeError::CgroupScopeUnresolved { configured, .. }) => {
                    assert_eq!(configured, 2);
                }
                other => return Err(TestError::Unexpected(format!("{other:?}"))),
            }
        }
        Ok(())
    }

    #[test]
    fn a_missing_root_is_reported_as_degraded_and_not_dropped() -> TestResult {
        let tree = FakeTree::new()?;
        let root = tree.mkcg("/harw.slice")?;
        for include_descendants in [true, false] {
            let scope = resolve(
                &tree,
                &cgroup_profile(&["/harw.slice", "/missing.slice"], include_descendants)?,
            )
            .map_err(ctx("partially resolvable scope"))?;
            let unresolved = scope.unresolved_roots();
            assert_eq!(unresolved.len(), 1);
            let missing = unresolved.first().ok_or(TestError::Missing("unresolved root"))?;
            assert_eq!(missing.path, "/missing.slice");
            assert_eq!(missing.error.kind(), ErrorKind::NotFound);
            assert_eq!(scope.startup_health().roots_unavailable, 1);
            assert!(scope.startup_health().is_degraded());
            let BpfScope::Cgroups { groups, .. } = scope.bpf_scope() else {
                return Err(TestError::Unexpected("expected a cgroup scope".into()));
            };
            assert_eq!(ids(&groups), BTreeSet::from([root]));
        }
        Ok(())
    }

    #[test]
    fn a_symlinked_root_is_not_resolved() -> TestResult {
        let tree = FakeTree::new()?;
        tree.mkcg("/real.slice")?;
        std::os::unix::fs::symlink(
            tree.root().join("real.slice"),
            tree.root().join("harw.slice"),
        )
        .map_err(ctx("symlink"))?;
        match resolve(&tree, &cgroup_profile(&["/harw.slice"], true)?) {
            Err(ProbeError::CgroupScopeUnresolved { configured: 1, .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn cgroup_root_must_be_absolute_not_slash_and_without_parent_components() -> TestResult {
        let scope = cgroup_profile(&["/harw.slice"], true)?;
        for root in ["/", "relative/cgroup", "/sys/fs/../fs/cgroup", ""] {
            match resolve_scope(&HostCgroupFs, Path::new(root), &scope, WalkLimits::default()) {
                Err(ProbeError::CgroupRootInvalid { path }) => assert_eq!(path, root),
                other => return Err(TestError::Unexpected(format!("{root}: {other:?}"))),
            }
        }
        Ok(())
    }

    #[test]
    fn sensor_selection_follows_the_profile_list() -> TestResult {
        let both = profile(HOST_PROFILE)?;
        assert_eq!(
            SensorSelection::from_profile(both.sensors()),
            SensorSelection {
                exec: true,
                tcp_connect: true,
            }
        );
        let exec_only = profile(&HOST_PROFILE.replace(
            "sensors = [\"exec\", \"tcp-connect\"]",
            "sensors = [\"exec\"]",
        ))?;
        assert_eq!(
            SensorSelection::from_profile(exec_only.sensors()),
            SensorSelection {
                exec: true,
                tcp_connect: false,
            }
        );
        Ok(())
    }

    /// Ende-zu-Ende über die E3b-Bausteine: Profil → Scope vor dem Anheften →
    /// beide procmon-Objekte über die Fixture geladen → Rescan nach dem
    /// Start nimmt eine neue cgroup auf.
    #[test]
    fn rescan_after_start_picks_up_a_new_directory() -> TestResult {
        let tree = FakeTree::new()?;
        let root = tree.mkcg("/harw.slice")?;
        let job1 = tree.mkcg("/harw.slice/harw-job-1")?;
        let scope = resolve(&tree, &cgroup_profile(&["/harw.slice"], true)?)
            .map_err(ctx("resolve descendant scope"))?;

        let loader = FixtureBpfLoader::new(Vec::new());
        let source = || BpfProgramSource::Embedded(Cow::Borrowed(b"ELF".as_slice()));
        let handles = harw_dod_procmon::procmon_contracts(
            SensorId::from_str("probe-bpf-procmon-0"),
            source(),
            source(),
            scope.bpf_scope(),
        )
        .iter()
        .map(|contract| loader.load_contract(contract))
        .collect::<Result<Vec<BpfHandle>, _>>()
        .map_err(ctx("fixture loads the contracts"))?;
        let map = |handle: &BpfHandle| loader.scope_ids(handle).ok_or(TestError::Missing("map"));
        let first = handles.first().ok_or(TestError::Missing("handle"))?;
        assert_eq!(map(first)?, BTreeSet::from([root, job1]));

        let start = Instant::now();
        let mut rescan = scope
            .into_rescan(
                tree.root().to_path_buf(),
                SCOPE_RESCAN_INTERVAL,
                WalkLimits::default(),
                start,
            )
            .ok_or(TestError::Missing("descendant rescan"))?;
        let job2 = tree.mkcg("/harw.slice/harw-job-2")?;
        let refs: Vec<&BpfHandle> = handles.iter().collect();
        let round = rescan
            .rescan_if_due(&loader, &refs, start + SCOPE_RESCAN_INTERVAL)
            .ok_or(TestError::Missing("due rescan"))?;
        assert_eq!(round.delta.insert, vec![job2]);
        assert!(!round.health.is_degraded());
        for handle in &handles {
            assert_eq!(map(handle)?, BTreeSet::from([root, job1, job2]));
        }
        Ok(())
    }
}
