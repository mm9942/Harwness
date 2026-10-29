//! Side-effect-free host capability probe (PL-90 runtime admission).
//!
//! [`HostReport::probe`] answers "what is the strongest sandbox this host
//! can give a job?" **before** any job runs, so admission can refuse a
//! [`harw_job_core::SandboxRequirement::Required`] job up front instead of
//! failing it later.
//!
//! # No side effects
//! The probe only reads: sysctls, the LSM list, `fstatfs`/`faccessat` and
//! the controller files of the own cgroup (never `cgroup.subtree_control`
//! writes, never `mkdir`), and `PATH` entries. The Landlock ABI is
//! determined on a throwaway thread that restricts only itself (see
//! `harw_job_linux::sandbox::landlock_support`); the calling thread and the
//! rest of the process stay unrestricted. No user namespace is created and
//! no process is spawned.
//!
//! # Never fails
//! Anything that cannot be determined is reported conservatively
//! (`false`, `None`, and in [`HostReport::best_report`] `NotEnforced` /
//! `Unsupported`), never guessed upwards.
//!
//! # Best achievable report
//! [`HostReport::best_report_from`] is pure: per sandbox dimension it takes
//! the strongest state any available backend reaches
//! (`Enforced > Partial > NotEnforced > Unsupported`).
//!
//! | Linux backend          | filesystem              | network                 | nnp      | caps     |
//! |------------------------|-------------------------|-------------------------|----------|----------|
//! | none                   | `Unsupported`           | `Unsupported`           | Enforced | Enforced |
//! | Landlock ABI 1-3       | `Partial`               | `NotEnforced`           | Enforced | Enforced |
//! | Landlock ABI 4         | `Partial`               | `Partial` (TCP only)    | Enforced | Enforced |
//! | Landlock ABI >= 5      | `Enforced`              | `Partial` (TCP only)    | Enforced | Enforced |
//! | Bubblewrap             | `Partial`               | `Enforced` (netns deny) | Enforced | Enforced |
//!
//! The Landlock rows mirror `landlock_states` in `harw-job-linux`
//! (`sandbox.rs`). `NO_NEW_PRIVS` and the capability drop are always
//! possible on Linux. Bubblewrap counts only when a `bwrap` executable is
//! on `PATH` and unprivileged user namespaces are not known to be disabled.
//! `resource_limits` is `Enforced` with a delegated cgroup v2 root (the
//! own cgroup, or the root passed to [`HostReport::probe_with_cgroup_root`]),
//! otherwise `Partial` (rlimits only).
//!
//! The report is a kernel/host upper bound: the Landlock trampoline
//! backend additionally needs the `harw-job-exec` binary, which the probe
//! does not look for.
//!
//! On macOS the report is `harw_job_darwin::DarwinSandbox::report_for` for
//! `sandbox-exec` if `/usr/bin/sandbox-exec` exists, else for the
//! unsandboxed mode. Every other OS gets a report in which nothing is
//! enforced.
//!
//! # Delegated cgroup root (PL-97 E3)
//! [`resolve_delegated_root`] is the one cgroup-root contract shared by the
//! host command port, the verify runner and `harw dod config suggest`:
//! `[jobs] cgroup_root` (trusted config layers only) → `HARW_CGROUP_ROOT`
//! → the own cgroup when the probe confirms it as delegated → none (with
//! [`NO_ROOT_HINT`]). A root is always an absolute path strictly below
//! [`CGROUP2_MOUNT`]; [`cgroup_path_below_mount`] converts it to the
//! mount-relative form of a Harw-DoD profile (and, without the leading
//! `/`, of the Warden's `CgroupId`).
//!
//! # Backend selection (PL-97 E1)
//! `select_backend` (Linux) turns a [`HostReport`] and
//! [`BackendPreferences`] into the `LinuxSandboxBackend` the executor uses.
//! It is the one resolver shared by the host command port and the
//! sandboxed verify runner. With [`BackendRequest::Auto`] it tries, in this
//! order:
//!
//! | Backend | Needs |
//! |---|---|
//! | [`BackendKind::BwrapTrampoline`] | trusted `bwrap`, user namespaces, a usable Landlock ABI (it keeps the job out of its read-write attempt directory), the `harw-job-exec` binary, a private plan directory, no unrelayed network need |
//! | [`BackendKind::Bwrap`] | trusted `bwrap`, user namespaces, no unrelayed network need |
//! | [`BackendKind::LandlockTrampoline`] | a usable Landlock ABI, the `harw-job-exec` binary, a private plan directory |
//! | [`BackendKind::None`] | nothing |
//!
//! Every step, taken or skipped, is recorded in `BackendSelection::reasons`
//! — the choice is never silent. "Trusted `bwrap`" means one at the fixed
//! root-owned paths of `harw-sandbox` (never `PATH`), unlike the
//! informational [`HostReport::bwrap_available`]. The trampoline is looked
//! up next to the running executable only (never `PATH`), unless an
//! explicit path is given. [`predicted_dimensions`] gives the per-dimension
//! table an operator sees for a choice.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use harw_job_core::{EnforcementState, SandboxProfileName, SandboxReport};
use serde::{Deserialize, Serialize};

/// Landlock support of the host (mirror of
/// `harw_job_linux::LandlockSupport`, so no Linux-crate type appears in this
/// crate's public API).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostLandlock {
    /// `landlock` is in `/sys/kernel/security/lsm`; `None` if unreadable.
    pub lsm_active: Option<bool>,
    /// Kernel Landlock ABI: `Some(n >= 1)` usable, `Some(0)` not
    /// implemented / not enabled, `None` undetermined.
    pub abi: Option<u8>,
}

impl HostLandlock {
    /// The usable ABI (`>= 1`), if any.
    #[must_use]
    pub fn usable_abi(&self) -> Option<u8> {
        self.abi.filter(|abi| *abi >= 1)
    }
}

/// Raw host facts: the input of [`HostReport::best_report_from`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFacts {
    /// `std::env::consts::OS` (`linux`, `macos`, ...).
    pub target_os: String,
    /// `std::env::consts::ARCH`.
    pub target_arch: String,
    /// Landlock support (Linux; default elsewhere).
    pub landlock: HostLandlock,
    /// A `bwrap` executable is on `PATH` (Linux).
    pub bwrap_available: bool,
    /// The cgroup v2 root is on a cgroup2 mount and writable (Linux).
    pub cgroup_v2_delegated: bool,
    /// Unprivileged user namespaces: `None` if unknown (Linux).
    pub user_namespaces: Option<bool>,
    /// `/usr/bin/sandbox-exec` exists (macOS).
    pub sandbox_exec_available: bool,
}

/// What this host can enforce for a job, plus the raw facts behind it.
/// Serializable so a runner can print it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostReport {
    /// `std::env::consts::OS`.
    pub target_os: String,
    /// `std::env::consts::ARCH`.
    pub target_arch: String,
    /// Landlock support.
    pub landlock: HostLandlock,
    /// A `bwrap` executable is on `PATH`.
    pub bwrap_available: bool,
    /// The cgroup v2 root is delegated (cgroup2 and writable).
    pub cgroup_v2_delegated: bool,
    /// Unprivileged user namespaces (`None`: unknown).
    pub user_namespaces: Option<bool>,
    /// `/usr/bin/sandbox-exec` exists (macOS).
    pub sandbox_exec_available: bool,
    /// Strongest achievable per-dimension report across all backends.
    pub best_report: SandboxReport,
}

impl HostReport {
    /// Probes this host (own cgroup as cgroup root). Never fails and has no
    /// side effects (see the module docs).
    #[must_use]
    pub fn probe() -> Self {
        Self::probe_with_cgroup_root(None)
    }

    /// As [`HostReport::probe`], but checks delegation of `cgroup_root`
    /// (e.g. `LinuxExecutorOptions::cgroup_root`) instead of the own
    /// cgroup when given.
    #[must_use]
    pub fn probe_with_cgroup_root(cgroup_root: Option<&Path>) -> Self {
        Self::from_facts(probe_facts(cgroup_root))
    }

    /// Builds the report from already collected facts (pure).
    #[must_use]
    pub fn from_facts(facts: HostFacts) -> Self {
        let best_report = Self::best_report_from(&facts);
        let HostFacts {
            target_os,
            target_arch,
            landlock,
            bwrap_available,
            cgroup_v2_delegated,
            user_namespaces,
            sandbox_exec_available,
        } = facts;
        Self {
            target_os,
            target_arch,
            landlock,
            bwrap_available,
            cgroup_v2_delegated,
            user_namespaces,
            sandbox_exec_available,
            best_report,
        }
    }

    /// The raw facts of this report.
    #[must_use]
    pub fn facts(&self) -> HostFacts {
        HostFacts {
            target_os: self.target_os.clone(),
            target_arch: self.target_arch.clone(),
            landlock: self.landlock,
            bwrap_available: self.bwrap_available,
            cgroup_v2_delegated: self.cgroup_v2_delegated,
            user_namespaces: self.user_namespaces,
            sandbox_exec_available: self.sandbox_exec_available,
        }
    }

    /// Strongest achievable report for `facts` (pure; table in the module
    /// docs).
    #[must_use]
    pub fn best_report_from(facts: &HostFacts) -> SandboxReport {
        match facts.target_os.as_str() {
            "linux" => linux_best(facts),
            "macos" => darwin_best(facts.sandbox_exec_available),
            _ => SandboxReport {
                filesystem: EnforcementState::Unsupported,
                network: EnforcementState::Unsupported,
                no_new_privs: EnforcementState::Unsupported,
                capabilities: EnforcementState::Unsupported,
                resource_limits: EnforcementState::NotEnforced,
            },
        }
    }
}

/// Strength rank for the per-dimension maximum.
const fn rank(state: EnforcementState) -> u8 {
    match state {
        EnforcementState::Unsupported => 0,
        EnforcementState::NotEnforced => 1,
        EnforcementState::Partial => 2,
        EnforcementState::Enforced => 3,
    }
}

fn stronger(left: EnforcementState, right: EnforcementState) -> EnforcementState {
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

fn merge(left: SandboxReport, right: SandboxReport) -> SandboxReport {
    SandboxReport {
        filesystem: stronger(left.filesystem, right.filesystem),
        network: stronger(left.network, right.network),
        no_new_privs: stronger(left.no_new_privs, right.no_new_privs),
        capabilities: stronger(left.capabilities, right.capabilities),
        resource_limits: stronger(left.resource_limits, right.resource_limits),
    }
}

fn linux_best(facts: &HostFacts) -> SandboxReport {
    let resource_limits = if facts.cgroup_v2_delegated {
        EnforcementState::Enforced
    } else {
        // rlimits are always available, cgroup limits are not.
        EnforcementState::Partial
    };
    // No filesystem/network backend: NO_NEW_PRIVS and the capability drop
    // are always possible on Linux.
    let mut best = SandboxReport {
        filesystem: EnforcementState::Unsupported,
        network: EnforcementState::Unsupported,
        no_new_privs: EnforcementState::Enforced,
        capabilities: EnforcementState::Enforced,
        resource_limits,
    };
    if let Some(abi) = facts.landlock.usable_abi() {
        let landlock = SandboxReport {
            filesystem: if abi >= 5 {
                EnforcementState::Enforced
            } else {
                EnforcementState::Partial
            },
            // TCP bind/connect only (ABI >= 4); UDP/raw stay reachable.
            network: if abi >= 4 {
                EnforcementState::Partial
            } else {
                EnforcementState::NotEnforced
            },
            no_new_privs: EnforcementState::Enforced,
            capabilities: EnforcementState::Enforced,
            resource_limits,
        };
        best = merge(best, landlock);
    }
    if facts.bwrap_available && facts.user_namespaces != Some(false) {
        let bwrap = SandboxReport {
            filesystem: EnforcementState::Partial,
            network: EnforcementState::Enforced,
            no_new_privs: EnforcementState::Enforced,
            capabilities: EnforcementState::Enforced,
            resource_limits,
        };
        best = merge(best, bwrap);
    }
    best
}

fn darwin_best(sandbox_exec_available: bool) -> SandboxReport {
    let sandbox = if sandbox_exec_available {
        harw_job_darwin::DarwinSandbox::sandbox_exec(std::env::temp_dir())
    } else {
        harw_job_darwin::DarwinSandbox::unsandboxed()
    };
    // Every profile maps to the same states today; take the strictest.
    sandbox.report_for(&SandboxProfileName::NoNetwork)
}

/// File name of the `harw-job-exec` trampoline binary.
pub const TRAMPOLINE_NAME: &str = "harw-job-exec";

/// The Linux sandbox backends, strongest first — the order
/// `select_backend` tries them in for [`BackendRequest::Auto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    /// Bubblewrap around the `harw-job-exec` trampoline: namespaces outside,
    /// Landlock, `NO_NEW_PRIVS`, capability drop and rlimits inside, cgroup
    /// joined before `bwrap` starts (`LinuxSandboxBackend::BwrapTrampoline`).
    BwrapTrampoline,
    /// Bubblewrap alone (`LinuxSandboxBackend::Bwrap`).
    Bwrap,
    /// The `harw-job-exec` trampoline with Landlock, without namespaces
    /// (`LinuxSandboxBackend::LandlockTrampoline`). Named `landlock` on
    /// command lines, in logs and on the wire.
    #[serde(rename = "landlock")]
    LandlockTrampoline,
    /// No sandbox backend (`LinuxSandboxBackend::None`).
    None,
}

impl BackendKind {
    /// Every kind, in selection order.
    pub const ALL: [Self; 4] = [
        Self::BwrapTrampoline,
        Self::Bwrap,
        Self::LandlockTrampoline,
        Self::None,
    ];

    /// The name used on command lines and in logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BwrapTrampoline => "bwrap-trampoline",
            Self::Bwrap => "bwrap",
            Self::LandlockTrampoline => "landlock",
            Self::None => "none",
        }
    }

    /// Whether the backend runs the job in a Bubblewrap namespace.
    #[must_use]
    pub const fn uses_bwrap(self) -> bool {
        matches!(self, Self::BwrapTrampoline | Self::Bwrap)
    }

    /// Whether the backend runs the `harw-job-exec` trampoline.
    #[must_use]
    pub const fn uses_trampoline(self) -> bool {
        matches!(self, Self::BwrapTrampoline | Self::LandlockTrampoline)
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A backend name that is none of `auto`, `bwrap-trampoline`, `bwrap`,
/// `landlock` or `none`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownBackend(pub String);

impl fmt::Display for UnknownBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown sandbox backend '{}' (expected auto, bwrap-trampoline, bwrap, landlock or none)",
            self.0
        )
    }
}

impl std::error::Error for UnknownBackend {}

impl FromStr for BackendKind {
    type Err = UnknownBackend;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text.trim())
            .ok_or_else(|| UnknownBackend(text.to_owned()))
    }
}

/// Which backend `select_backend` may choose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BackendRequest {
    /// The strongest available one ([`BackendKind::ALL`] order).
    #[default]
    Auto,
    /// Exactly this one. When it is not available the selection ends in
    /// [`BackendKind::None`] with the reason recorded; there is no fallback
    /// to another backend.
    Only(BackendKind),
}

impl fmt::Display for BackendRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Only(kind) => kind.fmt(f),
        }
    }
}

impl FromStr for BackendRequest {
    type Err = UnknownBackend;

    /// `auto`, or a [`BackendKind`] name.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.trim() == "auto" {
            Ok(Self::Auto)
        } else {
            text.parse().map(Self::Only)
        }
    }
}

/// Where `select_backend` takes an executable from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExecutableSource {
    /// The default lookup: `bwrap` at the fixed root-owned paths of
    /// `harw-sandbox`; the trampoline next to the running executable (or
    /// in [`BackendPreferences::exe_dir`]). `PATH` is never searched.
    #[default]
    Discover,
    /// Exactly this path (an explicit override): it must be absolute and
    /// an executable regular file. The caller vouches for its trust.
    Path(PathBuf),
    /// Not available.
    Disabled,
}

/// Inputs of `select_backend` besides the host facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendPreferences {
    /// Which backend may be chosen.
    pub request: BackendRequest,
    /// Where `bwrap` comes from.
    pub bwrap: ExecutableSource,
    /// Where the `harw-job-exec` trampoline comes from.
    pub trampoline: ExecutableSource,
    /// Directory of [`ExecutableSource::Discover`] for the trampoline.
    /// `None`: the directory of the running executable.
    pub exe_dir: Option<PathBuf>,
    /// Private (`0700`) directory for trampoline plan and report files;
    /// the trampoline backends need it to exist. `None`: they are skipped.
    pub plan_dir: Option<PathBuf>,
    /// The jobs need network access (e.g. `SandboxProfileName::WorkspaceBuild`).
    /// The Bubblewrap backends only offer proxy-only egress through a relay,
    /// which `select_backend` does not configure, so with this set they are
    /// skipped instead of refusing every such job later.
    pub needs_network: bool,
}

/// What `kind` can enforce on `host`, per dimension, for an operator
/// table: `namespace`, `filesystem`, `network`, `no_new_privs`,
/// `capabilities`, `resource_limits` and `cgroup`. Pure.
///
/// Network is the state for a policy that denies network. `cgroup` is the
/// job's own cgroup: `Enforced` when the process joins it before `exec`
/// (the trampoline backends), `Partial` when it is attached right after
/// spawn, `NotEnforced` without a delegated root
/// (`cgroup_root_configured`, confirmed by [`HostReport::cgroup_v2_delegated`]).
#[must_use]
pub fn predicted_dimensions(
    kind: BackendKind,
    host: &HostReport,
    cgroup_root_configured: bool,
) -> [(&'static str, EnforcementState); 7] {
    use EnforcementState::{Enforced, NotEnforced, Partial, Unsupported};
    let abi = host.landlock.usable_abi().unwrap_or(0);
    let landlock_filesystem = match abi {
        0 => Unsupported,
        1..=4 => Partial,
        _ => Enforced,
    };
    let landlock_network = match abi {
        0 => Unsupported,
        1..=3 => NotEnforced,
        _ => Partial,
    };
    let delegated = cgroup_root_configured && host.cgroup_v2_delegated;
    let (namespace, filesystem, network, privileges) = match kind {
        BackendKind::BwrapTrampoline => (
            Enforced,
            stronger(Partial, landlock_filesystem),
            Enforced,
            Enforced,
        ),
        BackendKind::Bwrap => (Enforced, Partial, Enforced, Enforced),
        BackendKind::LandlockTrampoline => {
            (NotEnforced, landlock_filesystem, landlock_network, Enforced)
        }
        BackendKind::None => (NotEnforced, NotEnforced, NotEnforced, NotEnforced),
    };
    let cgroup = match (delegated, kind.uses_trampoline()) {
        (false, _) => NotEnforced,
        (true, true) => Enforced,
        (true, false) => Partial,
    };
    [
        ("namespace", namespace),
        ("filesystem", filesystem),
        ("network", network),
        ("no_new_privs", privileges),
        ("capabilities", privileges),
        (
            "resource_limits",
            if delegated { Enforced } else { Partial },
        ),
        ("cgroup", cgroup),
    ]
}

/// The outcome of `select_backend`.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct BackendSelection {
    /// The backend to put into `LinuxExecutorOptions::sandbox`.
    pub backend: crate::coordinator::LinuxSandboxBackend,
    /// Its kind.
    pub chosen: BackendKind,
    /// One line per backend that was tried: why it was skipped, or that
    /// it was selected (with caveats).
    pub reasons: Vec<String>,
}

/// Chooses the sandbox backend for `host` (see the module docs). Never
/// fails and never silent: every candidate gets a line in
/// [`BackendSelection::reasons`].
///
/// Side effects: none besides the read-only lookups (`bwrap` discovery,
/// file metadata, `std::env::current_exe`).
#[cfg(target_os = "linux")]
#[must_use]
pub fn select_backend(host: &HostReport, prefs: &BackendPreferences) -> BackendSelection {
    let facts = SelectionFacts::resolve(host, prefs);
    let candidates: Vec<BackendKind> = match prefs.request {
        BackendRequest::Auto => BackendKind::ALL.to_vec(),
        BackendRequest::Only(kind) => vec![kind],
    };
    let mut reasons = Vec::new();
    for kind in candidates {
        match facts.backend(kind) {
            Ok((backend, note)) => {
                reasons.push(match note {
                    Some(note) => format!("{kind}: selected ({note})"),
                    None => format!("{kind}: selected"),
                });
                return BackendSelection {
                    backend,
                    chosen: kind,
                    reasons,
                };
            }
            Err(missing) => reasons.push(format!("{kind}: skipped: {}", missing.join("; "))),
        }
    }
    reasons.push(format!(
        "none: selected because '{}' was requested explicitly and there is no fallback; \
         jobs that request a sandbox run without one (`required` jobs are refused)",
        prefs.request
    ));
    BackendSelection {
        backend: crate::coordinator::LinuxSandboxBackend::None,
        chosen: BackendKind::None,
        reasons,
    }
}

/// The resolved inputs of one `select_backend` call.
#[cfg(target_os = "linux")]
struct SelectionFacts {
    linux: bool,
    user_namespaces: bool,
    landlock_abi: Option<u8>,
    needs_network: bool,
    #[cfg(feature = "bwrap")]
    bwrap: Result<harw_job_executor_bwrap::BwrapExecutor, String>,
    trampoline: Result<PathBuf, String>,
    plan_dir: Result<PathBuf, String>,
}

#[cfg(target_os = "linux")]
impl SelectionFacts {
    fn resolve(host: &HostReport, prefs: &BackendPreferences) -> Self {
        Self {
            linux: host.target_os == "linux",
            user_namespaces: host.user_namespaces != Some(false),
            landlock_abi: host.landlock.usable_abi(),
            needs_network: prefs.needs_network,
            #[cfg(feature = "bwrap")]
            bwrap: resolve_bwrap(&prefs.bwrap),
            trampoline: resolve_trampoline(&prefs.trampoline, prefs.exe_dir.as_deref()),
            plan_dir: resolve_plan_dir(prefs.plan_dir.as_deref()),
        }
    }

    /// The backend of `kind`, with an optional caveat, or everything that
    /// is missing for it.
    fn backend(
        &self,
        kind: BackendKind,
    ) -> Result<(crate::coordinator::LinuxSandboxBackend, Option<String>), Vec<String>> {
        use crate::coordinator::LinuxSandboxBackend;
        let mut missing = Vec::new();
        if !self.linux && kind != BackendKind::None {
            missing.push("not a Linux host".to_owned());
        }
        if kind.uses_bwrap() {
            if !self.user_namespaces {
                missing.push("unprivileged user namespaces are disabled".to_owned());
            }
            if self.needs_network {
                missing.push(
                    "the jobs need network access and bubblewrap only offers proxy-only \
                     egress through a relay, which is not configured"
                        .to_owned(),
                );
            }
        }
        #[cfg(feature = "bwrap")]
        let bwrap = match (&self.bwrap, kind.uses_bwrap()) {
            (Ok(bwrap), true) => Some(bwrap.clone()),
            (Err(why), true) => {
                missing.push(why.clone());
                None
            }
            (_, false) => None,
        };
        if cfg!(not(feature = "bwrap")) && kind.uses_bwrap() {
            missing.push("built without the `bwrap` feature".to_owned());
        }
        let trampoline = match (&self.trampoline, &self.plan_dir, kind.uses_trampoline()) {
            (Ok(trampoline), Ok(plan_dir), true) => Some((trampoline.clone(), plan_dir.clone())),
            (trampoline, plan_dir, true) => {
                for error in [trampoline.as_ref().err(), plan_dir.as_ref().err()]
                    .into_iter()
                    .flatten()
                {
                    missing.push(error.clone());
                }
                None
            }
            (_, _, false) => None,
        };
        if self.landlock_abi.is_none() {
            match kind {
                BackendKind::LandlockTrampoline => {
                    missing.push("no usable Landlock ABI".to_owned());
                }
                // Landlock is what keeps the job away from its attempt
                // directory (bound read-write for the plan and report).
                // Without it the job could rewrite its own report or plant
                // a FIFO in its place.
                BackendKind::BwrapTrampoline => missing.push(
                    "no usable Landlock ABI: the attempt directory (plan and report) \
                     would be writable by the job"
                        .to_owned(),
                ),
                BackendKind::Bwrap | BackendKind::None => {}
            }
        }
        if !missing.is_empty() {
            return Err(missing);
        }
        match (kind, trampoline) {
            #[cfg(feature = "bwrap")]
            (BackendKind::BwrapTrampoline, Some((trampoline, plan_dir))) => match bwrap {
                Some(bwrap) => Ok((
                    LinuxSandboxBackend::BwrapTrampoline {
                        bwrap,
                        trampoline,
                        plan_dir,
                    },
                    None,
                )),
                None => Err(vec!["no trusted bwrap".to_owned()]),
            },
            #[cfg(feature = "bwrap")]
            (BackendKind::Bwrap, _) => match bwrap {
                Some(bwrap) => Ok((LinuxSandboxBackend::Bwrap(bwrap), None)),
                None => Err(vec!["no trusted bwrap".to_owned()]),
            },
            (BackendKind::LandlockTrampoline, Some((trampoline, plan_dir))) => Ok((
                LinuxSandboxBackend::LandlockTrampoline {
                    trampoline,
                    plan_dir,
                },
                None,
            )),
            (BackendKind::None, _) => Ok((
                LinuxSandboxBackend::None,
                Some(
                    "no sandbox backend: jobs that request a sandbox run without one \
                     (`required` jobs are refused)"
                        .to_owned(),
                ),
            )),
            _ => Err(vec!["unavailable".to_owned()]),
        }
    }
}

/// A trusted `bwrap` per `source`.
#[cfg(all(target_os = "linux", feature = "bwrap"))]
fn resolve_bwrap(
    source: &ExecutableSource,
) -> Result<harw_job_executor_bwrap::BwrapExecutor, String> {
    match source {
        ExecutableSource::Discover => harw_job_executor_bwrap::BwrapExecutor::discover()
            .map_err(|error| format!("no trusted bwrap: {error}")),
        ExecutableSource::Path(path) => check_executable("bwrap", path)
            .map(|()| harw_job_executor_bwrap::BwrapExecutor::from_executable(path.clone())),
        ExecutableSource::Disabled => Err("bwrap is disabled".to_owned()),
    }
}

/// The trampoline binary per `source`.
#[cfg(target_os = "linux")]
fn resolve_trampoline(
    source: &ExecutableSource,
    exe_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    match source {
        ExecutableSource::Discover => {
            let dir = match exe_dir {
                Some(dir) => Some(dir.to_path_buf()),
                None => std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(Path::to_path_buf)),
            };
            let Some(dir) = dir.filter(|dir| dir.is_absolute()) else {
                return Err(format!(
                    "no {TRAMPOLINE_NAME}: the directory of the running executable is unknown"
                ));
            };
            let candidate = dir.join(TRAMPOLINE_NAME);
            check_executable(TRAMPOLINE_NAME, &candidate)
                .map(|()| candidate)
                .map_err(|why| format!("{why} (looked next to the running executable)"))
        }
        ExecutableSource::Path(path) => {
            check_executable(TRAMPOLINE_NAME, path).map(|()| path.clone())
        }
        ExecutableSource::Disabled => Err(format!("{TRAMPOLINE_NAME} is disabled")),
    }
}

/// An absolute executable regular file, or why not.
#[cfg(target_os = "linux")]
fn check_executable(name: &str, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    if !path.is_absolute() {
        return Err(format!("{name} path '{}' is not absolute", path.display()));
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => Ok(()),
        Ok(_) => Err(format!(
            "{name} '{}' is not an executable regular file",
            path.display()
        )),
        Err(error) => Err(format!("no {name} at '{}': {error}", path.display())),
    }
}

/// The plan directory: absolute, an existing directory, no group/other
/// access bits.
#[cfg(target_os = "linux")]
fn resolve_plan_dir(dir: Option<&Path>) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(dir) = dir else {
        return Err("no plan directory configured".to_owned());
    };
    if !dir.is_absolute() {
        return Err(format!(
            "plan directory '{}' is not absolute",
            dir.display()
        ));
    }
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() && meta.permissions().mode() & 0o077 == 0 => {
            Ok(dir.to_path_buf())
        }
        Ok(meta) if meta.is_dir() => Err(format!(
            "plan directory '{}' is not private (mode {:o}, expected 0700)",
            dir.display(),
            meta.permissions().mode() & 0o777
        )),
        Ok(_) => Err(format!(
            "plan directory '{}' is not a directory",
            dir.display()
        )),
        Err(error) => Err(format!("plan directory '{}': {error}", dir.display())),
    }
}

// ── Delegated cgroup root (PL-97 E3) ────────────────────────────────────────

/// Mount point of the cgroup v2 hierarchy. Every delegated root harw
/// accepts is an absolute path strictly below it; this is also the one
/// canonical prefix form shared with `[dod] allowed_cgroup_prefixes`.
pub const CGROUP2_MOUNT: &str = "/sys/fs/cgroup";

/// Environment variable naming the delegated cgroup v2 root of harw's jobs
/// (second step of [`resolve_delegated_root`]).
pub const CGROUP_ROOT_ENV: &str = "HARW_CGROUP_ROOT";

/// Deprecated alias of [`CGROUP_ROOT_ENV`], honoured only by the verify
/// runner ([`RootEnv::from_process_with_verify_alias`]).
pub const DEPRECATED_VERIFY_CGROUP_ROOT_ENV: &str = "HARW_VERIFY_CGROUP_ROOT";

/// Hint shown when no delegated root resolves.
pub const NO_ROOT_HINT: &str = "no delegated cgroup v2 root: set `[jobs] cgroup_root` (home or \
     profile config) or HARW_CGROUP_ROOT, start harw under `systemd-run --user --scope -p \
     Delegate=yes harw …`, or run it from a unit with Delegate=yes (harw-serve user unit, \
     harw-control.service); jobs run without their own cgroup";

/// Where [`resolve_delegated_root`] took the root from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootSource {
    /// `[jobs] cgroup_root` of a trusted config layer.
    Config,
    /// [`CGROUP_ROOT_ENV`] (or, for the verify runner, its deprecated alias).
    Env,
    /// The cgroup this process runs in, confirmed as delegated.
    OwnCgroupDelegated,
    /// Nothing resolved: jobs get no cgroup.
    None,
}

impl RootSource {
    /// Stable lowercase name (`config`, `env`, `own-cgroup-delegated`, `none`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Env => "env",
            Self::OwnCgroupDelegated => "own-cgroup-delegated",
            Self::None => "none",
        }
    }
}

impl fmt::Display for RootSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The outcome of [`resolve_delegated_root`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootResolution {
    /// The delegated root (absolute, strictly below [`CGROUP2_MOUNT`]),
    /// or `None` when nothing resolved.
    pub root: Option<PathBuf>,
    /// Where it came from.
    pub source: RootSource,
    /// An operator hint: [`NO_ROOT_HINT`] without a root, a deprecation
    /// note for the verify alias, else `None`.
    pub hint: Option<String>,
}

/// The config input of [`resolve_delegated_root`]: `[jobs] cgroup_root`
/// after the config merge (trusted layers only; an untrusted project
/// layer cannot set it, `harw_config` enforces that).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootConfig {
    /// `[jobs] cgroup_root`.
    pub cgroup_root: Option<PathBuf>,
}

/// The environment input of [`resolve_delegated_root`] (passed in, so tests
/// need no `set_var`). Empty values count as unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootEnv {
    /// [`CGROUP_ROOT_ENV`].
    pub cgroup_root: Option<std::ffi::OsString>,
    /// [`DEPRECATED_VERIFY_CGROUP_ROOT_ENV`]; only used when
    /// `cgroup_root` is unset.
    pub deprecated_verify_root: Option<std::ffi::OsString>,
}

impl RootEnv {
    /// [`CGROUP_ROOT_ENV`] of this process (the alias is ignored).
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            cgroup_root: std::env::var_os(CGROUP_ROOT_ENV),
            deprecated_verify_root: None,
        }
    }

    /// As [`RootEnv::from_process`], plus the deprecated
    /// [`DEPRECATED_VERIFY_CGROUP_ROOT_ENV`] alias (verify runner only).
    #[must_use]
    pub fn from_process_with_verify_alias() -> Self {
        Self {
            cgroup_root: std::env::var_os(CGROUP_ROOT_ENV),
            deprecated_verify_root: std::env::var_os(DEPRECATED_VERIFY_CGROUP_ROOT_ENV),
        }
    }
}

/// Name of the sub-cgroup harw's own process sits in when its unit sets
/// `DelegateSubgroup=harw-main` (systemd >= 254; older systemd ignores the
/// key with a warning). The delegated root is then the parent, which holds
/// no process, so the cgroup v2 "no internal processes" rule does not keep
/// the `memory`/`pids` controllers off for the job cgroups below it.
pub const DELEGATE_SUBGROUP: &str = "harw-main";

/// The own-cgroup input of [`resolve_delegated_root_with`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnCgroup {
    /// The candidate root: `/sys/fs/cgroup/<own cgroup v2 path>`, or its
    /// parent when the own cgroup is [`DELEGATE_SUBGROUP`]; `None` without
    /// cgroup v2 membership.
    pub path: Option<PathBuf>,
    /// The candidate is delegated to this process
    /// ([`own_cgroup_is_delegated`]).
    pub delegated: bool,
}

/// What [`OwnCgroup::probe`] knows about the candidate root; the input of
/// the pure [`own_cgroup_is_delegated`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OwnCgroupEvidence {
    /// cgroup2 and writable for this process (directory, `cgroup.procs`,
    /// `cgroup.subtree_control`; [`HostReport::cgroup_v2_delegated`]).
    pub writable: bool,
    /// The effective uid is 0 (writable says nothing then).
    pub privileged: bool,
    /// systemd marked the cgroup as delegated (`trusted.delegate` or
    /// `user.delegate` xattr = `1`).
    pub delegate_xattr: bool,
}

/// Whether the candidate own-cgroup root `path` is really delegated to
/// this process. Writable alone is no evidence: every cgroup below
/// `user@<uid>.service` belongs to the user (a terminal's `app-*.scope`
/// is writable but not delegated), and as euid 0 every cgroup passes. So:
///
/// - never as euid 0 (pass the root via `[jobs] cgroup_root` or
///   `HARW_CGROUP_ROOT` instead);
/// - accepted with systemd's delegation xattr on a writable cgroup;
/// - accepted without it only outside a user manager (no
///   `user@<uid>.service` component): there systemd hands a cgroup to a
///   unit's `User=` only with `Delegate=yes`, the rest stays root-owned.
#[must_use]
pub fn own_cgroup_is_delegated(path: &Path, evidence: &OwnCgroupEvidence) -> bool {
    if evidence.privileged || !evidence.writable {
        return false;
    }
    evidence.delegate_xattr || !below_user_manager(path)
}

/// `path` lies in a systemd user manager's subtree (`user@<uid>.service`).
fn below_user_manager(path: &Path) -> bool {
    path.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|name| {
            name.strip_prefix("user@")
                .and_then(|rest| rest.strip_suffix(".service"))
                .is_some()
        })
    })
}

/// The candidate root for the own cgroup `own`: its parent when `own` is
/// the [`DELEGATE_SUBGROUP`] leaf, else `own` itself.
#[must_use]
pub fn own_cgroup_root_candidate(own: &Path) -> PathBuf {
    match (own.file_name(), own.parent()) {
        (Some(name), Some(parent)) if name == DELEGATE_SUBGROUP => parent.to_path_buf(),
        _ => own.to_path_buf(),
    }
}

impl OwnCgroup {
    /// Reads `/proc/self/cgroup`, picks the candidate root
    /// ([`own_cgroup_root_candidate`]) and checks delegation of it
    /// ([`own_cgroup_is_delegated`]; read-only, see the module docs). Off
    /// Linux: nothing.
    #[must_use]
    pub fn probe() -> Self {
        let path = std::fs::read_to_string("/proc/self/cgroup")
            .ok()
            .and_then(|content| own_cgroup_from_proc(&content))
            .map(|own| own_cgroup_root_candidate(&own));
        let delegated = path.as_deref().is_some_and(|path| {
            let evidence = OwnCgroupEvidence {
                writable: HostReport::probe_with_cgroup_root(Some(path)).cgroup_v2_delegated,
                privileged: euid_is_root(),
                delegate_xattr: has_delegate_xattr(path),
            };
            own_cgroup_is_delegated(path, &evidence)
        });
        Self { path, delegated }
    }
}

#[cfg(target_os = "linux")]
fn euid_is_root() -> bool {
    rustix::process::geteuid().is_root()
}

#[cfg(not(target_os = "linux"))]
fn euid_is_root() -> bool {
    false
}

/// systemd's delegation marker on `path` (`trusted.delegate` needs
/// CAP_SYS_ADMIN to read, `user.delegate` does not).
#[cfg(target_os = "linux")]
fn has_delegate_xattr(path: &Path) -> bool {
    ["trusted.delegate", "user.delegate"]
        .into_iter()
        .any(|name| {
            let mut value = [0_u8; 8];
            rustix::fs::getxattr(path, name, &mut value[..])
                .ok()
                .and_then(|len| value.get(..len))
                .is_some_and(|value| value == b"1")
        })
}

#[cfg(not(target_os = "linux"))]
fn has_delegate_xattr(_path: &Path) -> bool {
    false
}

/// `/sys/fs/cgroup/<path>` from the unified (`0::`) line of a
/// `/proc/<pid>/cgroup` text; `None` without one.
fn own_cgroup_from_proc(content: &str) -> Option<PathBuf> {
    let relative = content
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim();
    if !relative.starts_with('/') {
        return None;
    }
    Some(Path::new(CGROUP2_MOUNT).join(relative.trim_start_matches('/')))
}

/// Why a configured cgroup root was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DelegatedRootError {
    /// The value is not valid UTF-8 or empty after trimming.
    Unreadable {
        /// Where it came from.
        source: RootSource,
    },
    /// Not an absolute path.
    NotAbsolute {
        /// Where it came from.
        source: RootSource,
        /// The refused value.
        path: PathBuf,
    },
    /// Contains `.` or `..` components.
    NotNormalized {
        /// Where it came from.
        source: RootSource,
        /// The refused value.
        path: PathBuf,
    },
    /// Not strictly below [`CGROUP2_MOUNT`].
    OutsideCgroupMount {
        /// Where it came from.
        source: RootSource,
        /// The refused value.
        path: PathBuf,
    },
}

impl fmt::Display for DelegatedRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { source } => {
                write!(f, "cgroup root from {source} is empty or not UTF-8")
            }
            Self::NotAbsolute { source, path } => write!(
                f,
                "cgroup root '{}' from {source} is not an absolute path",
                path.display()
            ),
            Self::NotNormalized { source, path } => write!(
                f,
                "cgroup root '{}' from {source} contains '.' or '..'",
                path.display()
            ),
            Self::OutsideCgroupMount { source, path } => write!(
                f,
                "cgroup root '{}' from {source} is not below the cgroup2 mount {CGROUP2_MOUNT}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for DelegatedRootError {}

/// Checks that `path` is an absolute, normalized path strictly below
/// [`CGROUP2_MOUNT`] and returns it without a trailing slash. Pure: it
/// does not check that the cgroup exists or is delegated.
///
/// # Errors
/// [`DelegatedRootError`] naming `source`.
pub fn validate_cgroup_root(
    path: &Path,
    source: RootSource,
) -> Result<PathBuf, DelegatedRootError> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(DelegatedRootError::NotAbsolute {
            source,
            path: path.to_path_buf(),
        });
    }
    let text = path.to_string_lossy();
    if text
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(DelegatedRootError::NotNormalized {
            source,
            path: path.to_path_buf(),
        });
    }
    let normalized: PathBuf = path.components().collect();
    let below = normalized
        .strip_prefix(CGROUP2_MOUNT)
        .ok()
        .filter(|rest| rest.components().any(|c| matches!(c, Component::Normal(_))));
    if below.is_none() {
        return Err(DelegatedRootError::OutsideCgroupMount {
            source,
            path: path.to_path_buf(),
        });
    }
    Ok(normalized)
}

/// The path of `root` relative to the cgroup2 mount, with a leading `/`
/// (`/sys/fs/cgroup/user.slice/harw.slice` → `/user.slice/harw.slice`):
/// the form of a Harw-DoD profile's `cgroup_paths` and of
/// `/proc/<pid>/cgroup`. Strip the leading `/` for the Warden's relative
/// `CgroupId` (`user.slice/harw.slice`). `None` when `root` is not a valid
/// root ([`validate_cgroup_root`]).
#[must_use]
pub fn cgroup_path_below_mount(root: &Path) -> Option<String> {
    let root = validate_cgroup_root(root, RootSource::Config).ok()?;
    let rest = root.strip_prefix(CGROUP2_MOUNT).ok()?;
    Some(format!("/{}", rest.to_string_lossy()))
}

/// Resolves the delegated cgroup v2 root of harw's jobs (PL-97 E3), in
/// this order:
///
/// 1. `[jobs] cgroup_root` ([`RootConfig`], trusted layers only);
/// 2. [`CGROUP_ROOT_ENV`] (verify runner: then the deprecated
///    [`DEPRECATED_VERIFY_CGROUP_ROOT_ENV`]);
/// 3. the cgroup this process runs in (its parent when it is the
///    [`DELEGATE_SUBGROUP`] leaf), when [`OwnCgroup::probe`] finds real
///    delegation evidence ([`own_cgroup_is_delegated`]: never as euid 0,
///    writable alone counts only outside a user manager);
/// 4. none, with [`NO_ROOT_HINT`].
///
/// A configured or environment root must be an absolute path strictly
/// below [`CGROUP2_MOUNT`]; anything else is refused (no fallback to a
/// later step). Whether it exists and is writable is checked where it is
/// used (`CgroupV2Fs::open`).
///
/// # Errors
/// [`DelegatedRootError`] for a malformed configured or environment root.
pub fn resolve_delegated_root(
    config: &RootConfig,
    env: &RootEnv,
) -> Result<RootResolution, DelegatedRootError> {
    resolve_delegated_root_with(config, env, OwnCgroup::probe)
}

/// [`resolve_delegated_root`] with an injected own-cgroup probe (called
/// only when steps 1 and 2 are empty).
///
/// # Errors
/// As [`resolve_delegated_root`].
pub fn resolve_delegated_root_with(
    config: &RootConfig,
    env: &RootEnv,
    own_cgroup: impl FnOnce() -> OwnCgroup,
) -> Result<RootResolution, DelegatedRootError> {
    if let Some(root) = &config.cgroup_root {
        let root = validate_cgroup_root(root, RootSource::Config)?;
        return Ok(RootResolution {
            root: Some(root),
            source: RootSource::Config,
            hint: None,
        });
    }
    let present = |value: &Option<std::ffi::OsString>| value.clone().filter(|v| !v.is_empty());
    let (value, hint) = match (
        present(&env.cgroup_root),
        present(&env.deprecated_verify_root),
    ) {
        (Some(value), _) => (Some(value), None),
        (None, Some(value)) => (
            Some(value),
            Some(format!(
                "{DEPRECATED_VERIFY_CGROUP_ROOT_ENV} is deprecated; set {CGROUP_ROOT_ENV} or \
                 `[jobs] cgroup_root` instead"
            )),
        ),
        (None, None) => (None, None),
    };
    if let Some(value) = value {
        let text = value
            .to_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or(DelegatedRootError::Unreadable {
                source: RootSource::Env,
            })?;
        let root = validate_cgroup_root(Path::new(text), RootSource::Env)?;
        return Ok(RootResolution {
            root: Some(root),
            source: RootSource::Env,
            hint,
        });
    }
    let own = own_cgroup();
    if own.delegated
        && let Some(path) = own.path
        && let Ok(root) = validate_cgroup_root(&path, RootSource::OwnCgroupDelegated)
    {
        return Ok(RootResolution {
            root: Some(root),
            source: RootSource::OwnCgroupDelegated,
            hint: None,
        });
    }
    Ok(RootResolution {
        root: None,
        source: RootSource::None,
        hint: Some(NO_ROOT_HINT.to_owned()),
    })
}

fn probe_facts(cgroup_root: Option<&Path>) -> HostFacts {
    let mut facts = HostFacts {
        target_os: std::env::consts::OS.to_owned(),
        target_arch: std::env::consts::ARCH.to_owned(),
        ..HostFacts::default()
    };
    probe_platform(&mut facts, cgroup_root);
    facts
}

#[cfg(target_os = "linux")]
fn probe_platform(facts: &mut HostFacts, cgroup_root: Option<&Path>) {
    let landlock = harw_job_linux::sandbox::landlock_support();
    facts.landlock = HostLandlock {
        lsm_active: landlock.lsm_active,
        abi: landlock.abi,
    };
    facts.user_namespaces = harw_job_linux::sandbox::userns_available();
    facts.bwrap_available = find_executable("bwrap", std::env::var_os("PATH").as_deref());
    let detection = match cgroup_root {
        Some(root) => harw_job_linux::cgroup::detect(root),
        None => harw_job_linux::cgroup::detect_own_cgroup(),
    };
    facts.cgroup_v2_delegated = detection.is_ok_and(|detection| detection.is_delegated());
}

#[cfg(target_os = "macos")]
fn probe_platform(facts: &mut HostFacts, _cgroup_root: Option<&Path>) {
    facts.sandbox_exec_available = Path::new(harw_job_darwin::SANDBOX_EXEC_PATH).is_file();
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_platform(_facts: &mut HostFacts, _cgroup_root: Option<&Path>) {}

/// Whether `name` is an executable regular file in one of the `path`
/// (`PATH`-syntax) directories. Relative `PATH` entries are ignored.
#[cfg(target_os = "linux")]
fn find_executable(name: &str, path: Option<&std::ffi::OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111) != 0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn linux(landlock_abi: Option<u8>, bwrap: bool, cgroup: bool) -> HostFacts {
        HostFacts {
            target_os: "linux".to_owned(),
            target_arch: "x86_64".to_owned(),
            landlock: HostLandlock {
                lsm_active: Some(landlock_abi.is_some()),
                abi: landlock_abi,
            },
            bwrap_available: bwrap,
            cgroup_v2_delegated: cgroup,
            user_namespaces: Some(true),
            sandbox_exec_available: false,
        }
    }

    #[test]
    fn test_probe_is_idempotent_and_leaves_thread_unrestricted() -> TestResult {
        let first = HostReport::probe();
        let second = HostReport::probe();
        assert_eq!(first, second);
        assert_eq!(first.target_os, std::env::consts::OS);
        assert_eq!(
            first.best_report,
            HostReport::best_report_from(&first.facts())
        );
        // The Landlock probe thread restricted itself to "no filesystem
        // access"; this thread must still write outside any ruleset.
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let file = temp.path().join("after-host-probe");
        std::fs::write(&file, b"ok").map_err(ctx("write after probe"))?;
        let content = std::fs::read(&file).map_err(ctx("read after probe"))?;
        assert_eq!(content, b"ok");
        Ok(())
    }

    #[test]
    fn test_report_serde_round_trip() -> TestResult {
        let report = HostReport::from_facts(linux(Some(5), true, true));
        let json = serde_json::to_string(&report).map_err(ctx("serialize"))?;
        let back: HostReport = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, report);
        Ok(())
    }

    #[test]
    fn test_best_report_no_backends() {
        let report = HostReport::best_report_from(&linux(None, false, false));
        assert_eq!(report.filesystem, EnforcementState::Unsupported);
        assert_eq!(report.network, EnforcementState::Unsupported);
        assert_eq!(report.no_new_privs, EnforcementState::Enforced);
        assert_eq!(report.capabilities, EnforcementState::Enforced);
        assert_eq!(report.resource_limits, EnforcementState::Partial);
        // ABI 0 (kernel says: no Landlock) is the same as no backend.
        assert_eq!(
            HostReport::best_report_from(&linux(Some(0), false, false)),
            report
        );
    }

    #[test]
    fn test_best_report_landlock_abis() {
        let v5 = HostReport::best_report_from(&linux(Some(5), false, false));
        assert_eq!(v5.filesystem, EnforcementState::Enforced);
        assert_eq!(v5.network, EnforcementState::Partial);
        let v4 = HostReport::best_report_from(&linux(Some(4), false, false));
        assert_eq!(v4.filesystem, EnforcementState::Partial);
        assert_eq!(v4.network, EnforcementState::Partial);
        let v3 = HostReport::best_report_from(&linux(Some(3), false, false));
        assert_eq!(v3.filesystem, EnforcementState::Partial);
        assert_eq!(v3.network, EnforcementState::NotEnforced);
    }

    #[test]
    fn test_best_report_bwrap_only() {
        let report = HostReport::best_report_from(&linux(None, true, false));
        assert_eq!(report.filesystem, EnforcementState::Partial);
        assert_eq!(report.network, EnforcementState::Enforced);
        assert_eq!(report.no_new_privs, EnforcementState::Enforced);
        assert_eq!(report.capabilities, EnforcementState::Enforced);
        // Without user namespaces bwrap does not count.
        let mut facts = linux(None, true, false);
        facts.user_namespaces = Some(false);
        let report = HostReport::best_report_from(&facts);
        assert_eq!(report.filesystem, EnforcementState::Unsupported);
        assert_eq!(report.network, EnforcementState::Unsupported);
    }

    #[test]
    fn test_best_report_takes_per_dimension_max() {
        let report = HostReport::best_report_from(&linux(Some(5), true, false));
        assert_eq!(report.filesystem, EnforcementState::Enforced);
        assert_eq!(report.network, EnforcementState::Enforced);
    }

    #[test]
    fn test_best_report_cgroup_delegated() {
        let report = HostReport::best_report_from(&linux(None, false, true));
        assert_eq!(report.resource_limits, EnforcementState::Enforced);
    }

    #[test]
    fn test_best_report_macos_and_other() {
        let mut facts = HostFacts {
            target_os: "macos".to_owned(),
            ..HostFacts::default()
        };
        let plain = HostReport::best_report_from(&facts);
        assert_eq!(plain.filesystem, EnforcementState::NotEnforced);
        assert_eq!(plain.no_new_privs, EnforcementState::Unsupported);
        facts.sandbox_exec_available = true;
        let sandboxed = HostReport::best_report_from(&facts);
        assert_eq!(sandboxed.filesystem, EnforcementState::Partial);
        assert_eq!(sandboxed.network, EnforcementState::Partial);
        let other = HostReport::best_report_from(&HostFacts {
            target_os: "windows".to_owned(),
            ..HostFacts::default()
        });
        assert!(!other.filesystem.is_enforced());
        assert!(!other.capabilities.is_enforced());
    }

    #[test]
    fn test_backend_names_round_trip() -> TestResult {
        for kind in BackendKind::ALL {
            let parsed: BackendKind = kind.as_str().parse().map_err(ctx("parse kind"))?;
            assert_eq!(parsed, kind);
            // One spelling everywhere: the wire form is the CLI name.
            let wire = serde_json::to_string(&kind).map_err(ctx("encode kind"))?;
            assert_eq!(wire, format!("\"{}\"", kind.as_str()));
            let decoded: BackendKind = serde_json::from_str(&wire).map_err(ctx("decode kind"))?;
            assert_eq!(decoded, kind);
            let request: BackendRequest = kind.to_string().parse().map_err(ctx("parse request"))?;
            assert_eq!(request, BackendRequest::Only(kind));
        }
        assert_eq!(
            "auto"
                .parse::<BackendRequest>()
                .map_err(ctx("parse auto"))?,
            BackendRequest::Auto
        );
        assert_eq!(BackendRequest::default().to_string(), "auto");
        let unknown = "landlock-trampoline".parse::<BackendRequest>();
        assert!(
            matches!(&unknown, Err(UnknownBackend(name)) if name == "landlock-trampoline"),
            "{unknown:?}"
        );
        assert!(BackendKind::BwrapTrampoline.uses_bwrap() && BackendKind::Bwrap.uses_bwrap());
        assert!(!BackendKind::LandlockTrampoline.uses_bwrap());
        assert!(BackendKind::LandlockTrampoline.uses_trampoline());
        assert!(!BackendKind::Bwrap.uses_trampoline() && !BackendKind::None.uses_trampoline());
        Ok(())
    }

    #[test]
    fn test_predicted_dimensions_per_backend() {
        use EnforcementState::{Enforced, NotEnforced, Partial, Unsupported};
        let full = HostReport::from_facts(linux(Some(5), true, true));
        let state = |kind, host: &HostReport, cgroup| -> Vec<EnforcementState> {
            predicted_dimensions(kind, host, cgroup)
                .iter()
                .map(|(_, state)| *state)
                .collect()
        };
        let names: Vec<&str> = predicted_dimensions(BackendKind::None, &full, false)
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            names,
            [
                "namespace",
                "filesystem",
                "network",
                "no_new_privs",
                "capabilities",
                "resource_limits",
                "cgroup"
            ]
        );
        assert_eq!(
            state(BackendKind::BwrapTrampoline, &full, true),
            [Enforced; 7]
        );
        assert_eq!(
            state(BackendKind::Bwrap, &full, true),
            [
                Enforced, Partial, Enforced, Enforced, Enforced, Enforced, Partial
            ]
        );
        assert_eq!(
            state(BackendKind::LandlockTrampoline, &full, true),
            [
                NotEnforced,
                Enforced,
                Partial,
                Enforced,
                Enforced,
                Enforced,
                Enforced
            ]
        );
        // Without a configured (delegated) root: rlimits only, no job cgroup.
        assert_eq!(
            state(BackendKind::BwrapTrampoline, &full, false),
            [
                Enforced,
                Enforced,
                Enforced,
                Enforced,
                Enforced,
                Partial,
                NotEnforced
            ]
        );
        // Landlock ABI 3: the trampoline layer is only partial.
        let old = HostReport::from_facts(linux(Some(3), true, false));
        assert_eq!(
            state(BackendKind::LandlockTrampoline, &old, true),
            [
                NotEnforced,
                Partial,
                NotEnforced,
                Enforced,
                Enforced,
                Partial,
                NotEnforced
            ]
        );
        let none = HostReport::from_facts(linux(None, true, false));
        assert_eq!(
            state(BackendKind::BwrapTrampoline, &none, false)[1],
            Partial,
            "mounts still confine the filesystem without Landlock"
        );
        assert_eq!(
            state(BackendKind::LandlockTrampoline, &none, false)[1],
            Unsupported
        );
        assert_eq!(
            state(BackendKind::None, &full, true),
            [
                NotEnforced,
                NotEnforced,
                NotEnforced,
                NotEnforced,
                NotEnforced,
                Enforced,
                Partial
            ]
        );
    }

    /// Fake executables and a private plan directory for the selection
    /// decision table.
    #[cfg(all(target_os = "linux", feature = "bwrap"))]
    struct SelectionFixture {
        _dir: tempfile::TempDir,
        bin: PathBuf,
        bwrap: PathBuf,
        trampoline: PathBuf,
        plan_dir: PathBuf,
        /// A directory with mode `0755` (not usable as a plan directory).
        open_dir: PathBuf,
    }

    #[cfg(all(target_os = "linux", feature = "bwrap"))]
    impl SelectionFixture {
        fn new() -> TestResult<Self> {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let bin = dir.path().join("bin");
            std::fs::create_dir(&bin).map_err(ctx("bin dir"))?;
            let mut tools = Vec::new();
            for name in ["bwrap", TRAMPOLINE_NAME] {
                let path = bin.join(name);
                std::fs::write(&path, b"#!/bin/sh\n").map_err(ctx("write tool"))?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .map_err(ctx("chmod tool"))?;
                tools.push(path);
            }
            let plan_dir = dir.path().join("plans");
            std::fs::create_dir(&plan_dir).map_err(ctx("plan dir"))?;
            std::fs::set_permissions(&plan_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(ctx("chmod plan dir"))?;
            let open_dir = dir.path().join("open");
            std::fs::create_dir(&open_dir).map_err(ctx("open dir"))?;
            std::fs::set_permissions(&open_dir, std::fs::Permissions::from_mode(0o755))
                .map_err(ctx("chmod open dir"))?;
            let [bwrap, trampoline] = <[PathBuf; 2]>::try_from(tools)
                .map_err(|_| crate::test_support::TestError::Missing("two tools"))?;
            Ok(Self {
                _dir: dir,
                bin,
                bwrap,
                trampoline,
                plan_dir,
                open_dir,
            })
        }

        /// Everything available, nothing discovered from the real host.
        fn prefs(&self) -> BackendPreferences {
            BackendPreferences {
                request: BackendRequest::Auto,
                bwrap: ExecutableSource::Path(self.bwrap.clone()),
                trampoline: ExecutableSource::Path(self.trampoline.clone()),
                exe_dir: None,
                plan_dir: Some(self.plan_dir.clone()),
                needs_network: false,
            }
        }
    }

    #[cfg(all(target_os = "linux", feature = "bwrap"))]
    #[test]
    fn test_select_backend_decision_table() -> TestResult {
        use crate::test_support::TestError;
        let fixture = SelectionFixture::new()?;
        let full = HostReport::from_facts(linux(Some(5), true, false));
        let no_landlock = HostReport::from_facts(linux(None, true, false));
        let mut no_userns_facts = linux(Some(5), true, false);
        no_userns_facts.user_namespaces = Some(false);
        let no_userns = HostReport::from_facts(no_userns_facts);
        let mut unknown_userns_facts = linux(Some(5), true, false);
        unknown_userns_facts.user_namespaces = None;
        let unknown_userns = HostReport::from_facts(unknown_userns_facts);
        let macos = HostReport::from_facts(HostFacts {
            target_os: "macos".to_owned(),
            ..linux(Some(5), true, false)
        });
        let base = fixture.prefs();
        let open_plan_dir = fixture.open_dir.clone();
        let with = |change: &dyn Fn(&mut BackendPreferences)| {
            let mut prefs = base.clone();
            change(&mut prefs);
            prefs
        };
        // (case, host, preferences, expected backend, expected skip count)
        let cases: Vec<(&str, &HostReport, BackendPreferences, BackendKind, usize)> = vec![
            (
                "everything",
                &full,
                base.clone(),
                BackendKind::BwrapTrampoline,
                0,
            ),
            (
                "unknown userns counts as available",
                &unknown_userns,
                base.clone(),
                BackendKind::BwrapTrampoline,
                0,
            ),
            (
                "no landlock: bubblewrap alone",
                &no_landlock,
                base.clone(),
                BackendKind::Bwrap,
                1,
            ),
            (
                "explicit composition without landlock: no fallback",
                &no_landlock,
                with(&|prefs| {
                    prefs.request = BackendRequest::Only(BackendKind::BwrapTrampoline);
                }),
                BackendKind::None,
                1,
            ),
            (
                "no trampoline",
                &full,
                with(&|prefs| prefs.trampoline = ExecutableSource::Disabled),
                BackendKind::Bwrap,
                1,
            ),
            (
                "plan dir not private",
                &full,
                with(&|prefs| prefs.plan_dir = Some(open_plan_dir.clone())),
                BackendKind::Bwrap,
                1,
            ),
            (
                "no bwrap",
                &full,
                with(&|prefs| prefs.bwrap = ExecutableSource::Disabled),
                BackendKind::LandlockTrampoline,
                2,
            ),
            (
                "no user namespaces",
                &no_userns,
                base.clone(),
                BackendKind::LandlockTrampoline,
                2,
            ),
            (
                "network without relay",
                &full,
                with(&|prefs| prefs.needs_network = true),
                BackendKind::LandlockTrampoline,
                2,
            ),
            (
                "no bwrap, no landlock",
                &no_landlock,
                with(&|prefs| prefs.bwrap = ExecutableSource::Disabled),
                BackendKind::None,
                3,
            ),
            (
                "relative trampoline override",
                &no_landlock,
                with(&|prefs| {
                    prefs.trampoline = ExecutableSource::Path(PathBuf::from(TRAMPOLINE_NAME));
                }),
                BackendKind::Bwrap,
                1,
            ),
            (
                "trampoline discovered in exe_dir",
                &full,
                with(&|prefs| {
                    prefs.trampoline = ExecutableSource::Discover;
                    prefs.exe_dir = Some(fixture.bin.clone());
                }),
                BackendKind::BwrapTrampoline,
                0,
            ),
            (
                "explicit landlock",
                &full,
                with(&|prefs| {
                    prefs.request = BackendRequest::Only(BackendKind::LandlockTrampoline);
                }),
                BackendKind::LandlockTrampoline,
                0,
            ),
            (
                "explicit bwrap without bwrap: no fallback",
                &full,
                with(&|prefs| {
                    prefs.request = BackendRequest::Only(BackendKind::Bwrap);
                    prefs.bwrap = ExecutableSource::Disabled;
                }),
                BackendKind::None,
                1,
            ),
            (
                "explicit none",
                &full,
                with(&|prefs| prefs.request = BackendRequest::Only(BackendKind::None)),
                BackendKind::None,
                0,
            ),
            ("not linux", &macos, base.clone(), BackendKind::None, 3),
        ];
        for (case, host, prefs, expected, skipped) in cases {
            let selection = select_backend(host, &prefs);
            if selection.chosen != expected || selection.backend.kind() != expected {
                return Err(TestError::Unexpected(format!(
                    "{case}: expected {expected}, got {} / {:?}: {:?}",
                    selection.chosen, selection.backend, selection.reasons
                )));
            }
            let skips = selection
                .reasons
                .iter()
                .filter(|reason| reason.contains(": skipped: "))
                .count();
            assert_eq!(skips, skipped, "{case}: {:?}", selection.reasons);
            // Never silent: one line per tried candidate, the last names
            // the choice.
            let last = selection
                .reasons
                .last()
                .ok_or(TestError::Missing("reasons"))?;
            assert!(
                last.starts_with(&format!("{expected}: selected")),
                "{case}: {last}"
            );
        }
        Ok(())
    }

    #[cfg(all(target_os = "linux", feature = "bwrap"))]
    #[test]
    fn test_select_backend_paths_and_reasons() -> TestResult {
        use crate::coordinator::LinuxSandboxBackend;
        use crate::test_support::TestError;
        let fixture = SelectionFixture::new()?;
        let full = HostReport::from_facts(linux(Some(5), true, false));
        let selection = select_backend(&full, &fixture.prefs());
        match &selection.backend {
            LinuxSandboxBackend::BwrapTrampoline {
                bwrap,
                trampoline,
                plan_dir,
            } => {
                assert_eq!(bwrap.executable(), fixture.bwrap.as_path());
                assert_eq!(trampoline, &fixture.trampoline);
                assert_eq!(plan_dir, &fixture.plan_dir);
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        // A skipped candidate names everything that is missing.
        let mut prefs = fixture.prefs();
        prefs.bwrap = ExecutableSource::Disabled;
        prefs.trampoline = ExecutableSource::Disabled;
        let mut facts = linux(None, true, false);
        facts.user_namespaces = Some(false);
        let selection = select_backend(&HostReport::from_facts(facts), &prefs);
        assert_eq!(selection.chosen, BackendKind::None);
        let first = selection
            .reasons
            .first()
            .ok_or(TestError::Missing("first reason"))?;
        assert!(first.starts_with("bwrap-trampoline: skipped: "), "{first}");
        assert!(first.contains("user namespaces are disabled"), "{first}");
        assert!(first.contains("bwrap is disabled"), "{first}");
        assert!(first.contains("harw-job-exec is disabled"), "{first}");
        let landlock = selection
            .reasons
            .get(2)
            .ok_or(TestError::Missing("landlock reason"))?;
        assert!(landlock.contains("no usable Landlock ABI"), "{landlock}");
        assert!(first.contains("attempt directory"), "{first}");
        // No Landlock: the composition is skipped (the job could write its
        // attempt directory), bubblewrap alone is chosen.
        let selection = select_backend(
            &HostReport::from_facts(linux(None, true, false)),
            &fixture.prefs(),
        );
        assert_eq!(selection.chosen, BackendKind::Bwrap);
        let first = selection
            .reasons
            .first()
            .ok_or(TestError::Missing("reason"))?;
        assert!(first.starts_with("bwrap-trampoline: skipped: "), "{first}");
        assert!(first.contains("no usable Landlock ABI"), "{first}");
        Ok(())
    }

    #[cfg(all(target_os = "linux", not(feature = "bwrap")))]
    #[test]
    fn test_select_backend_without_the_bwrap_feature_says_so() {
        let selection = select_backend(
            &HostReport::from_facts(linux(None, true, false)),
            &BackendPreferences {
                trampoline: ExecutableSource::Disabled,
                ..BackendPreferences::default()
            },
        );
        assert_eq!(selection.chosen, BackendKind::None);
        assert!(
            selection
                .reasons
                .first()
                .is_some_and(|reason| reason.contains("built without the `bwrap` feature")),
            "{:?}",
            selection.reasons
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_executable() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let tool = dir.path().join("harw-fake-tool");
        std::fs::write(&tool, b"#!/bin/sh\n").map_err(ctx("write tool"))?;
        let path = std::env::join_paths([dir.path()]).map_err(ctx("join paths"))?;
        assert!(!find_executable("harw-fake-tool", Some(&path)));
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod"))?;
        assert!(find_executable("harw-fake-tool", Some(&path)));
        assert!(!find_executable("harw-missing-tool", Some(&path)));
        assert!(!find_executable("harw-fake-tool", None));
        Ok(())
    }

    // ── PL-97 E3: delegated cgroup root ──

    fn own(path: Option<&str>, delegated: bool) -> OwnCgroup {
        OwnCgroup {
            path: path.map(PathBuf::from),
            delegated,
        }
    }

    fn env(primary: Option<&str>, alias: Option<&str>) -> RootEnv {
        RootEnv {
            cgroup_root: primary.map(Into::into),
            deprecated_verify_root: alias.map(Into::into),
        }
    }

    #[test]
    fn delegated_root_resolution_order_is_config_env_own_none() -> TestResult {
        let config = RootConfig {
            cgroup_root: Some(PathBuf::from("/sys/fs/cgroup/config.slice/")),
        };
        let own_ok = || own(Some("/sys/fs/cgroup/own.scope"), true);
        let unused = || -> OwnCgroup { own(Some("/sys/fs/cgroup/never"), true) };

        // 1. Config wins over env and the own cgroup (trailing slash dropped).
        let got =
            resolve_delegated_root_with(&config, &env(Some("/sys/fs/cgroup/env"), None), unused)
                .map_err(|e| crate::test_support::TestError::Unexpected(e.to_string()))?;
        assert_eq!(got.source, RootSource::Config);
        assert_eq!(got.root, Some(PathBuf::from("/sys/fs/cgroup/config.slice")));
        assert_eq!(got.hint, None);

        // 2. Env over the own cgroup.
        let got = resolve_delegated_root_with(
            &RootConfig::default(),
            &env(
                Some("/sys/fs/cgroup/env.slice"),
                Some("/sys/fs/cgroup/alias"),
            ),
            unused,
        )
        .map_err(|e| crate::test_support::TestError::Unexpected(e.to_string()))?;
        assert_eq!(got.source, RootSource::Env);
        assert_eq!(got.root, Some(PathBuf::from("/sys/fs/cgroup/env.slice")));
        assert_eq!(got.hint, None);

        // 2b. The deprecated alias only when the primary is unset or empty.
        let got = resolve_delegated_root_with(
            &RootConfig::default(),
            &env(Some(""), Some("/sys/fs/cgroup/alias.slice")),
            unused,
        )
        .map_err(|e| crate::test_support::TestError::Unexpected(e.to_string()))?;
        assert_eq!(got.source, RootSource::Env);
        assert_eq!(got.root, Some(PathBuf::from("/sys/fs/cgroup/alias.slice")));
        assert!(
            got.hint
                .as_deref()
                .is_some_and(|hint| hint.contains("deprecated")),
            "{got:?}"
        );

        // 3. The own cgroup, only when delegated.
        let got = resolve_delegated_root_with(&RootConfig::default(), &RootEnv::default(), own_ok)
            .map_err(|e| crate::test_support::TestError::Unexpected(e.to_string()))?;
        assert_eq!(got.source, RootSource::OwnCgroupDelegated);
        assert_eq!(got.root, Some(PathBuf::from("/sys/fs/cgroup/own.scope")));

        // 4. Nothing: none with the hint.
        for fallback in [
            own(Some("/sys/fs/cgroup/own.scope"), false),
            own(None, true),
            own(Some("/sys/fs/cgroup"), true),
        ] {
            let got =
                resolve_delegated_root_with(&RootConfig::default(), &RootEnv::default(), || {
                    fallback.clone()
                })
                .map_err(|e| crate::test_support::TestError::Unexpected(e.to_string()))?;
            assert_eq!(got.source, RootSource::None);
            assert_eq!(got.root, None);
            assert!(
                got.hint
                    .as_deref()
                    .is_some_and(|hint| hint.contains("systemd-run --user --scope -p Delegate=yes")),
                "{got:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn own_cgroup_needs_delegation_evidence_not_only_write_access() {
        let app_scope = Path::new(
            "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome\\x2dterminal-1.scope",
        );
        let serve = Path::new(
            "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service",
        );
        let control = Path::new("/sys/fs/cgroup/system.slice/harw-control.service");
        let session = Path::new("/sys/fs/cgroup/user.slice/user-1000.slice/session-3.scope");
        let writable = OwnCgroupEvidence {
            writable: true,
            ..OwnCgroupEvidence::default()
        };
        let marked = OwnCgroupEvidence {
            delegate_xattr: true,
            ..writable
        };
        let as_root = OwnCgroupEvidence {
            privileged: true,
            ..marked
        };

        // (a) Below user@<uid>.service everything is user-owned: a terminal's
        // app scope is writable but not delegated; the xattr decides.
        assert!(!own_cgroup_is_delegated(app_scope, &writable));
        assert!(!own_cgroup_is_delegated(serve, &writable));
        assert!(own_cgroup_is_delegated(serve, &marked));
        // A system unit with User= gets its cgroup only through Delegate=yes.
        assert!(own_cgroup_is_delegated(control, &writable));
        assert!(!own_cgroup_is_delegated(
            control,
            &OwnCgroupEvidence::default()
        ));
        // (b) As euid 0 (sudo) every cgroup is writable: never accepted.
        for path in [app_scope, serve, control, session] {
            assert!(
                !own_cgroup_is_delegated(path, &as_root),
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn own_cgroup_root_is_the_parent_of_the_delegate_subgroup() {
        let root = Path::new("/sys/fs/cgroup/system.slice/harw-control.service");
        assert_eq!(
            own_cgroup_root_candidate(&root.join(DELEGATE_SUBGROUP)),
            root
        );
        assert_eq!(own_cgroup_root_candidate(root), root);
        assert_eq!(
            own_cgroup_root_candidate(&root.join("harw-main2")),
            root.join("harw-main2")
        );
    }

    #[test]
    fn delegated_root_refuses_relative_unnormalized_and_outside_mount_roots() {
        for (bad, expected) in [
            ("relative/harw.slice", "NotAbsolute"),
            ("/sys/fs/cgroup/../etc", "NotNormalized"),
            ("/sys/fs/cgroup/./harw", "NotNormalized"),
            ("/tmp/harw.slice", "OutsideCgroupMount"),
            ("/sys/fs/cgroupx/harw", "OutsideCgroupMount"),
            ("/sys/fs/cgroup", "OutsideCgroupMount"),
            ("/sys/fs/cgroup/", "OutsideCgroupMount"),
        ] {
            let config = RootConfig {
                cgroup_root: Some(PathBuf::from(bad)),
            };
            // A refused config root never falls back to env or the own cgroup.
            let result = resolve_delegated_root_with(
                &config,
                &env(Some("/sys/fs/cgroup/env"), None),
                || own(Some("/sys/fs/cgroup/own"), true),
            );
            let error = format!("{result:?}");
            assert!(error.contains(expected), "{bad}: {error}");
            assert!(error.contains("Config"), "{bad}: {error}");
        }
        let result = resolve_delegated_root_with(
            &RootConfig::default(),
            &env(Some("harw.slice"), None),
            || own(None, false),
        );
        assert!(
            matches!(
                result,
                Err(DelegatedRootError::NotAbsolute {
                    source: RootSource::Env,
                    ..
                })
            ),
            "{result:?}"
        );
        let result =
            resolve_delegated_root_with(&RootConfig::default(), &env(Some("   "), None), || {
                own(None, false)
            });
        assert!(
            matches!(
                result,
                Err(DelegatedRootError::Unreadable {
                    source: RootSource::Env
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn host_cgroup_root_converts_to_the_dod_and_warden_forms() {
        assert_eq!(
            cgroup_path_below_mount(Path::new("/sys/fs/cgroup/user.slice/harw.slice/")).as_deref(),
            Some("/user.slice/harw.slice")
        );
        assert_eq!(cgroup_path_below_mount(Path::new("/sys/fs/cgroup")), None);
        assert_eq!(cgroup_path_below_mount(Path::new("user.slice")), None);
        assert_eq!(
            own_cgroup_from_proc("12:pids:/x\n0::/user.slice/harw.scope\n"),
            Some(PathBuf::from("/sys/fs/cgroup/user.slice/harw.scope"))
        );
        assert_eq!(own_cgroup_from_proc("4:memory:/x\n"), None);
        assert_eq!(
            RootSource::OwnCgroupDelegated.to_string(),
            "own-cgroup-delegated"
        );
    }
}
