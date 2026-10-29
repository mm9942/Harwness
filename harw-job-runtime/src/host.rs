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
        if kind == BackendKind::BwrapTrampoline && self.landlock_abi.is_some_and(|abi| abi < 5) {
            missing.push(
                "Landlock ABI 5 or later required: the attempt directory needs full filesystem enforcement"
                    .to_owned(),
            );
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
    fn test_composition_requires_full_filesystem_abi() -> TestResult {
        let fixture = SelectionFixture::new()?;
        for abi in 1..5 {
            let host = HostReport::from_facts(linux(Some(abi), true, false));
            let mut prefs = fixture.prefs();
            assert_eq!(select_backend(&host, &prefs).chosen, BackendKind::Bwrap);
            prefs.request = BackendRequest::Only(BackendKind::BwrapTrampoline);
            let explicit = select_backend(&host, &prefs);
            assert_eq!(explicit.chosen, BackendKind::None);
            assert!(
                explicit
                    .reasons
                    .iter()
                    .any(|reason| reason.contains("ABI 5"))
            );
        }
        Ok(())
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
}
