//! Minimal typed job specification (Job-Runtime-Doc §6, Eco-Doc §59).
//!
//! A [`JobSpec`] says *what* to run and *under which requirements*, never
//! *how* a platform enforces them. It carries no ambient authority: the
//! working directory is a [`WorkspacePath`] relative to a workspace root the
//! runner resolves, and the environment is an explicit allowlist.
//!
//! On the wire a spec always travels inside a versioned [`JobSpecEnvelope`]
//! (a typed payload, not `serde_json::Value`).

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use jiff::SignedDuration;
use serde::{Deserialize, Serialize};

use crate::ids::{IdempotencyKey, JobScopeId};

/// Why a spec (or a part of it) was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    /// The program name was empty.
    EmptyProgram,
    /// A string field contained a NUL byte (unrepresentable in an OS argv).
    NulByte {
        /// Which field.
        field: &'static str,
    },
    /// A workspace path was empty.
    EmptyPath,
    /// A workspace path was absolute (would escape the workspace root).
    AbsolutePath {
        /// The rejected path.
        path: String,
    },
    /// A workspace path contained a `..` component.
    ParentTraversal {
        /// The rejected path.
        path: String,
    },
    /// An environment variable name was empty or contained `=`.
    InvalidEnvName {
        /// The rejected name.
        name: String,
    },
    /// An environment variable name appeared twice.
    DuplicateEnvName {
        /// The duplicated name.
        name: String,
    },
    /// A resource request value was out of range.
    InvalidResource {
        /// Which resource.
        resource: &'static str,
        /// Human-readable constraint.
        constraint: &'static str,
    },
    /// The envelope version is not understood by this build.
    UnsupportedVersion {
        /// Version found in the envelope.
        found: u16,
        /// Version this build reads.
        supported: u16,
    },
    /// An identifier field (e.g. the scope) was rejected.
    InvalidId {
        /// Which field.
        field: &'static str,
        /// Rendered cause.
        reason: String,
    },
    /// The write scope version is not understood by this build. Refused,
    /// never ignored.
    UnsupportedWriteScopeVersion {
        /// Version found in the scope.
        found: u32,
        /// Version this build reads.
        supported: u32,
    },
    /// A write scope lists more paths than [`JobWriteScope::MAX_PATHS`].
    TooManyWriteScopePaths {
        /// Number of listed paths.
        count: usize,
        /// The maximum.
        max: usize,
    },
    /// A write scope path is not absolute, not normalised, not UTF-8, the
    /// host root, or resolves outside the workspace.
    InvalidWriteScopePath {
        /// The rejected path (lossy rendering).
        path: String,
        /// Human-readable constraint.
        reason: &'static str,
    },
    /// A write scope path appeared twice.
    DuplicateWriteScopePath {
        /// The duplicated path.
        path: String,
    },
    /// A job with a write scope does not use [`SandboxRequirement::Required`].
    WriteScopeNotRequired {
        /// The requirement the spec carries.
        requirement: SandboxRequirement,
    },
    /// The sandbox profile grants no workspace write access a write scope
    /// could narrow (a scope never widens the profile).
    WriteScopeWithoutWorkspaceWrite {
        /// The profile of the spec.
        profile: SandboxProfileName,
    },
    /// A write scope path lies outside the job's workspace (checked by the
    /// runtime, which knows the workspace root).
    WriteScopeOutsideWorkspace {
        /// The rejected path.
        path: String,
    },
    /// A write scope path does not exist and no entry of the scope covers an
    /// existing ancestor directory of it (checked by the runtime). The
    /// Landlock planner skips paths that do not exist, so such a path would
    /// grant nothing.
    WriteScopeUncovered {
        /// The uncovered path.
        path: String,
    },
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProgram => f.write_str("job program must not be empty"),
            Self::NulByte { field } => write!(f, "job spec field '{field}' contains a NUL byte"),
            Self::EmptyPath => f.write_str("workspace path must not be empty"),
            Self::AbsolutePath { path } => {
                write!(f, "workspace path '{path}' must be relative")
            }
            Self::ParentTraversal { path } => {
                write!(f, "workspace path '{path}' must not contain '..'")
            }
            Self::InvalidEnvName { name } => {
                write!(f, "environment variable name {name:?} is invalid")
            }
            Self::DuplicateEnvName { name } => {
                write!(f, "environment variable '{name}' is listed twice")
            }
            Self::InvalidResource {
                resource,
                constraint,
            } => write!(f, "resource request '{resource}' {constraint}"),
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "job spec envelope version {found} is not supported (this build reads {supported})"
            ),
            Self::InvalidId { field, reason } => {
                write!(f, "job spec field '{field}' is not a valid id: {reason}")
            }
            Self::UnsupportedWriteScopeVersion { found, supported } => write!(
                f,
                "job write scope version {found} is not supported (this build reads {supported})"
            ),
            Self::TooManyWriteScopePaths { count, max } => {
                write!(f, "job write scope lists {count} paths (at most {max})")
            }
            Self::InvalidWriteScopePath { path, reason } => {
                write!(f, "write scope path '{path}' {reason}")
            }
            Self::DuplicateWriteScopePath { path } => {
                write!(f, "write scope path '{path}' is listed twice")
            }
            Self::WriteScopeNotRequired { requirement } => write!(
                f,
                "a job with a write scope must use sandbox requirement 'required', not '{}'",
                requirement_name(*requirement)
            ),
            Self::WriteScopeWithoutWorkspaceWrite { profile } => write!(
                f,
                "sandbox profile '{}' grants no workspace write access for a write scope to narrow",
                profile_name(*profile)
            ),
            Self::WriteScopeOutsideWorkspace { path } => {
                write!(f, "write scope path '{path}' lies outside the job's workspace")
            }
            Self::WriteScopeUncovered { path } => write!(
                f,
                "write scope path '{path}' does not exist and no scope entry covers an existing \
                 parent directory of it (paths that do not exist get no Landlock rule)"
            ),
        }
    }
}

/// Wire name of a [`SandboxRequirement`] (diagnostics).
fn requirement_name(requirement: SandboxRequirement) -> &'static str {
    match requirement {
        SandboxRequirement::None => "none",
        SandboxRequirement::BestEffort => "best_effort",
        SandboxRequirement::Required => "required",
    }
}

/// Wire name of a [`SandboxProfileName`] (diagnostics).
fn profile_name(profile: SandboxProfileName) -> &'static str {
    match profile {
        SandboxProfileName::WorkspaceBuild => "workspace_build",
        SandboxProfileName::ReadOnlyAnalysis => "read_only_analysis",
        SandboxProfileName::NoNetwork => "no_network",
        SandboxProfileName::NetworkRestricted => "network_restricted",
    }
}

impl std::error::Error for SpecError {}

fn reject_nul(field: &'static str, value: &str) -> Result<(), SpecError> {
    if value.contains('\0') {
        Err(SpecError::NulByte { field })
    } else {
        Ok(())
    }
}

/// A path relative to the job's workspace root. `.` denotes the root.
///
/// Absolute paths (`/x`, `\x`, `C:...`) and `..` components are rejected on
/// construction and on deserialization, so the spec can never name a
/// location outside the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkspacePath(String);

impl WorkspacePath {
    /// Validates and wraps a relative workspace path.
    ///
    /// # Errors
    /// [`SpecError`] for an empty, absolute, NUL-containing, or `..`-containing
    /// path.
    pub fn new(path: impl Into<String>) -> Result<Self, SpecError> {
        let path = path.into();
        if path.is_empty() {
            return Err(SpecError::EmptyPath);
        }
        reject_nul("working_dir", &path)?;
        let bytes = path.as_bytes();
        let has_drive_prefix =
            bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        if path.starts_with('/') || path.starts_with('\\') || has_drive_prefix {
            return Err(SpecError::AbsolutePath { path });
        }
        if path.split(['/', '\\']).any(|component| component == "..") {
            return Err(SpecError::ParentTraversal { path });
        }
        Ok(Self(path))
    }

    /// The workspace root itself.
    #[must_use]
    pub fn root() -> Self {
        Self(".".to_owned())
    }

    /// The validated relative path text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspacePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for WorkspacePath {
    type Error = SpecError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<WorkspacePath> for String {
    fn from(value: WorkspacePath) -> Self {
        value.0
    }
}

/// CPU bandwidth of the whole job: at most `quota_us` of CPU time per
/// `period_us` (cgroup v2 `cpu.max`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuBandwidth {
    /// CPU time per period in microseconds, at least
    /// [`CpuBandwidth::MIN_QUOTA_US`]; above the period for more than one
    /// CPU.
    pub quota_us: u64,
    /// Period length in microseconds, within
    /// [`CpuBandwidth::MIN_PERIOD_US`]`..=`[`CpuBandwidth::MAX_PERIOD_US`].
    pub period_us: u64,
}

impl CpuBandwidth {
    /// Smallest quota the kernel accepts (1 ms).
    pub const MIN_QUOTA_US: u64 = 1_000;
    /// Shortest period the kernel accepts (1 ms).
    pub const MIN_PERIOD_US: u64 = 1_000;
    /// Longest period the kernel accepts (1 s).
    pub const MAX_PERIOD_US: u64 = 1_000_000;
    /// Default cgroup v2 period (100 ms).
    pub const DEFAULT_PERIOD_US: u64 = 100_000;

    /// A quota of `cpus` whole CPUs at the default period.
    #[must_use]
    pub fn cpus(cpus: u32) -> Self {
        Self {
            quota_us: u64::from(cpus).saturating_mul(Self::DEFAULT_PERIOD_US),
            period_us: Self::DEFAULT_PERIOD_US,
        }
    }
}

/// Platform-neutral resource request. Every field is optional; `None` means
/// "no explicit limit requested". Backends report whether they could enforce
/// it via `SandboxReport::resource_limits`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequest {
    /// Memory ceiling for the whole job in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_max: Option<u64>,
    /// Memory throttling threshold for the whole job in bytes (cgroup v2
    /// `memory.high`); at most `memory_max` when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_high: Option<u64>,
    /// Relative CPU weight in `1..=10_000` (100 is the neutral default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_weight: Option<u16>,
    /// CPU bandwidth limit of the whole job (cgroup v2 `cpu.max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_max: Option<CpuBandwidth>,
    /// Maximum number of processes/threads in the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids_max: Option<u32>,
    /// Relative I/O weight in `1..=10_000` (cgroup v2 `io.weight`; 100 is
    /// the neutral default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_weight: Option<u16>,
    /// Wall-clock timeout of one attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_timeout: Option<SignedDuration>,
    /// Address-space ceiling **per process** in bytes (`RLIMIT_AS`). Unlike
    /// `memory_max` (the whole job's cgroup) this also bounds a single
    /// runaway allocation in a process that has no cgroup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_space_max: Option<u64>,
    /// CPU time ceiling per process in seconds (`RLIMIT_CPU`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_time_max: Option<u64>,
    /// Largest file a process may create, in bytes (`RLIMIT_FSIZE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size_max: Option<u64>,
    /// Open file descriptors per process (`RLIMIT_NOFILE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_files_max: Option<u32>,
    /// Refuse to start the attempt when the per-process rlimits cannot be
    /// applied (no `prlimit`, no trampoline) instead of running without them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_rlimits: bool,
}

impl ResourceRequest {
    /// Whether any limit of the whole job is requested: `memory_max`,
    /// `memory_high`, `cpu_weight`, `cpu_max`, `pids_max` or `io_weight`
    /// (a resource domain such as a cgroup enforces them; per-process
    /// rlimits and the wall-clock timeout are not included).
    #[must_use]
    pub fn requests_job_limits(&self) -> bool {
        self.memory_max.is_some()
            || self.memory_high.is_some()
            || self.cpu_weight.is_some()
            || self.cpu_max.is_some()
            || self.pids_max.is_some()
            || self.io_weight.is_some()
    }

    /// Whether any per-process rlimit (`address_space_max`, `cpu_time_max`,
    /// `file_size_max`, `open_files_max`) is requested.
    #[must_use]
    pub fn requests_rlimits(&self) -> bool {
        self.address_space_max.is_some()
            || self.cpu_time_max.is_some()
            || self.file_size_max.is_some()
            || self.open_files_max.is_some()
    }

    /// Maximum permitted [`ResourceRequest::cpu_weight`] (and
    /// [`ResourceRequest::io_weight`]).
    pub const MAX_CPU_WEIGHT: u16 = 10_000;

    /// Checks every requested value is in range.
    ///
    /// # Errors
    /// [`SpecError::InvalidResource`] naming the first offending field.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.memory_max == Some(0) {
            return Err(SpecError::InvalidResource {
                resource: "memory_max",
                constraint: "must be greater than zero",
            });
        }
        if self
            .cpu_weight
            .is_some_and(|weight| !(1..=Self::MAX_CPU_WEIGHT).contains(&weight))
        {
            return Err(SpecError::InvalidResource {
                resource: "cpu_weight",
                constraint: "must be within 1..=10000",
            });
        }
        if self.pids_max == Some(0) {
            return Err(SpecError::InvalidResource {
                resource: "pids_max",
                constraint: "must be greater than zero",
            });
        }
        if self.memory_high == Some(0) {
            return Err(SpecError::InvalidResource {
                resource: "memory_high",
                constraint: "must be greater than zero",
            });
        }
        if let (Some(high), Some(max)) = (self.memory_high, self.memory_max) {
            if high > max {
                return Err(SpecError::InvalidResource {
                    resource: "memory_high",
                    constraint: "must not exceed memory_max",
                });
            }
        }
        if let Some(cpu) = self.cpu_max {
            if !(CpuBandwidth::MIN_PERIOD_US..=CpuBandwidth::MAX_PERIOD_US).contains(&cpu.period_us)
            {
                return Err(SpecError::InvalidResource {
                    resource: "cpu_max.period_us",
                    constraint: "must be within 1000..=1000000",
                });
            }
            if cpu.quota_us < CpuBandwidth::MIN_QUOTA_US {
                return Err(SpecError::InvalidResource {
                    resource: "cpu_max.quota_us",
                    constraint: "must be at least 1000",
                });
            }
        }
        if self
            .io_weight
            .is_some_and(|weight| !(1..=Self::MAX_CPU_WEIGHT).contains(&weight))
        {
            return Err(SpecError::InvalidResource {
                resource: "io_weight",
                constraint: "must be within 1..=10000",
            });
        }
        for (resource, zero) in [
            ("address_space_max", self.address_space_max == Some(0)),
            ("cpu_time_max", self.cpu_time_max == Some(0)),
            ("file_size_max", self.file_size_max == Some(0)),
            ("open_files_max", self.open_files_max == Some(0)),
        ] {
            if zero {
                return Err(SpecError::InvalidResource {
                    resource,
                    constraint: "must be greater than zero",
                });
            }
        }
        if self
            .wall_timeout
            .is_some_and(|timeout| timeout <= SignedDuration::ZERO)
        {
            return Err(SpecError::InvalidResource {
                resource: "wall_timeout",
                constraint: "must be positive",
            });
        }
        Ok(())
    }
}

/// How strictly the sandbox profile must be enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxRequirement {
    /// No sandbox requested.
    None,
    /// Apply what the platform supports; run even if partially enforced.
    #[default]
    BestEffort,
    /// Refuse to run unless every dimension is enforced.
    Required,
}

/// Named, platform-neutral sandbox profile. Backends translate a name into
/// their own mechanisms (Landlock, seatbelt, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfileName {
    /// Read/write inside the workspace, network allowed.
    WorkspaceBuild,
    /// Read-only access, no writes.
    ReadOnlyAnalysis,
    /// Workspace access, no network at all.
    NoNetwork,
    /// Workspace access, network restricted to an allowlist.
    NetworkRestricted,
}

/// A versioned per-job write scope (PL-97 E2, PR #153 W2): the only
/// locations inside the job's workspace the job may write.
///
/// # Description
/// The scope **replaces** the workspace write grant of the job's sandbox
/// profile and never widens it: the workspace becomes read-only except the
/// listed paths. Writes outside the workspace (`/tmp`, `/dev/null`) are
/// what the profile grants. A job with a write scope always runs under
/// [`SandboxRequirement::Required`] ([`JobSpecBuilder::write_scope`] sets
/// it, [`JobSpec::validate`] refuses anything else), so a runtime that
/// cannot enforce the filesystem dimension refuses the job before any
/// process starts. Profiles without workspace write access
/// ([`SandboxProfileName::ReadOnlyAnalysis`]) take no scope.
///
/// # Paths
/// Absolute, UTF-8, already normalised (no empty, `.` or `..` segment, no
/// trailing `/`), not `/` itself, no duplicates, at most
/// [`JobWriteScope::MAX_PATHS`]; checked here. The runtime, which knows the
/// workspace root, checks the rest when the attempt starts: every path lies
/// inside the workspace (also after resolving symlinks), and a path that
/// does not exist yet (a file the job will create) is covered by another
/// entry naming an existing ancestor directory, usually its parent. The
/// Landlock planner skips paths that do not exist, so such a path gets no
/// rule of its own; the covering directory entry is what grants the write.
///
/// # Versions
/// `version` is checked first: a scope of another version is refused with
/// [`SpecError::UnsupportedWriteScopeVersion`] at admission and at start,
/// never ignored. A scope with fields this build does not know fails to
/// deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobWriteScope {
    /// Schema version of the scope ([`JobWriteScope::CURRENT_VERSION`]).
    pub version: u32,
    /// Absolute paths inside the workspace the job may write (see the type
    /// docs).
    pub paths: Vec<PathBuf>,
}

impl JobWriteScope {
    /// The scope version this build writes and reads.
    pub const CURRENT_VERSION: u32 = 1;
    /// Most paths one scope may list (each becomes a Landlock rule).
    pub const MAX_PATHS: usize = 256;

    /// A current-version scope of `paths`, validated
    /// ([`JobWriteScope::validate`]).
    ///
    /// # Errors
    /// [`SpecError`] naming the first invalid path.
    pub fn new<I, P>(paths: I) -> Result<Self, SpecError>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let scope = Self {
            version: Self::CURRENT_VERSION,
            paths: paths.into_iter().map(Into::into).collect(),
        };
        scope.validate()?;
        Ok(scope)
    }

    /// Checks the version first, then every path (see the type docs).
    ///
    /// # Errors
    /// [`SpecError::UnsupportedWriteScopeVersion`],
    /// [`SpecError::TooManyWriteScopePaths`],
    /// [`SpecError::InvalidWriteScopePath`] or
    /// [`SpecError::DuplicateWriteScopePath`].
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.version != Self::CURRENT_VERSION {
            return Err(SpecError::UnsupportedWriteScopeVersion {
                found: self.version,
                supported: Self::CURRENT_VERSION,
            });
        }
        if self.paths.len() > Self::MAX_PATHS {
            return Err(SpecError::TooManyWriteScopePaths {
                count: self.paths.len(),
                max: Self::MAX_PATHS,
            });
        }
        let mut seen = BTreeSet::new();
        for path in &self.paths {
            check_write_scope_path(path)?;
            if !seen.insert(path.as_path()) {
                return Err(SpecError::DuplicateWriteScopePath {
                    path: path.to_string_lossy().into_owned(),
                });
            }
        }
        Ok(())
    }
}

/// The textual rules for one write scope path (raw segments, because
/// [`Path::components`] silently drops `.` and repeated separators).
fn check_write_scope_path(path: &Path) -> Result<(), SpecError> {
    let invalid = |reason: &'static str| {
        Err(SpecError::InvalidWriteScopePath {
            path: path.to_string_lossy().into_owned(),
            reason,
        })
    };
    let Some(text) = path.to_str() else {
        return invalid("is not valid UTF-8");
    };
    if text.contains('\0') {
        return invalid("contains a NUL byte");
    }
    let Some(rest) = text.strip_prefix('/') else {
        return invalid("must be absolute");
    };
    if rest.is_empty() {
        return invalid("must not be the host root");
    }
    if rest
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return invalid("must be normalised (no empty, '.' or '..' segment, no trailing '/')");
    }
    Ok(())
}

/// Minimal typed job specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    /// Program to execute (resolved by the runner, not by the shell).
    pub program: String,
    /// Arguments, passed verbatim (no shell interpretation).
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory relative to the workspace root.
    pub working_dir: WorkspacePath,
    /// Explicit environment allowlist; nothing is inherited implicitly.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Requested resource limits.
    #[serde(default)]
    pub resources: ResourceRequest,
    /// How strictly the sandbox profile must be enforced.
    #[serde(default)]
    pub sandbox: SandboxRequirement,
    /// Which sandbox profile applies.
    pub sandbox_profile: SandboxProfileName,
    /// Optional deduplication key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
    /// Generic authority scope the job runs in.
    pub scope: JobScopeId,
    /// Optional per-job write scope ([`JobWriteScope`]): narrows the
    /// profile's workspace write grant to the listed paths. `None` (and
    /// absent in old records): the profile applies unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_scope: Option<JobWriteScope>,
}

impl JobSpec {
    /// Checks every field of the spec.
    ///
    /// # Errors
    /// [`SpecError`] naming the first violated rule.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.program.is_empty() {
            return Err(SpecError::EmptyProgram);
        }
        reject_nul("program", &self.program)?;
        for arg in &self.args {
            reject_nul("args", arg)?;
        }
        let mut names = BTreeSet::new();
        for (name, value) in &self.env {
            if name.is_empty() || name.contains('=') || name.contains('\0') {
                return Err(SpecError::InvalidEnvName { name: name.clone() });
            }
            reject_nul("env", value)?;
            if !names.insert(name.as_str()) {
                return Err(SpecError::DuplicateEnvName { name: name.clone() });
            }
        }
        self.resources.validate()?;
        if let Some(scope) = &self.write_scope {
            scope.validate()?;
            if self.sandbox != SandboxRequirement::Required {
                return Err(SpecError::WriteScopeNotRequired {
                    requirement: self.sandbox,
                });
            }
            if self.sandbox_profile == SandboxProfileName::ReadOnlyAnalysis {
                return Err(SpecError::WriteScopeWithoutWorkspaceWrite {
                    profile: self.sandbox_profile,
                });
            }
        }
        Ok(())
    }

    /// Starts a builder for running `program` (Job-Runtime-Doc §16).
    ///
    /// Defaults: no arguments, the workspace root as working directory, an
    /// empty environment, no resource limits, sandbox profile
    /// [`SandboxProfileName::WorkspaceBuild`] at
    /// [`SandboxRequirement::BestEffort`], scope [`DEFAULT_SCOPE`], no write
    /// scope. Nothing is validated until [`JobSpecBuilder::build`].
    ///
    /// ```
    /// use harw_job_core::{JobSpec, SandboxProfileName, WorkspacePath};
    ///
    /// # fn main() -> Result<(), harw_job_core::SpecError> {
    /// let spec = JobSpec::command("cargo")
    ///     .arg("test")
    ///     .workspace(WorkspacePath::new("crates/app")?)
    ///     .sandbox(SandboxProfileName::WorkspaceBuild)
    ///     .build()?;
    /// assert_eq!(spec.args, ["test"]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn command(program: impl Into<String>) -> JobSpecBuilder {
        JobSpecBuilder {
            program: program.into(),
            args: Vec::new(),
            working_dir: WorkspacePath::root(),
            env: Vec::new(),
            resources: ResourceRequest::default(),
            sandbox: SandboxRequirement::default(),
            sandbox_profile: SandboxProfileName::WorkspaceBuild,
            idempotency_key: None,
            scope: None,
            write_scope: None,
        }
    }
}

/// Scope a [`JobSpecBuilder`] uses when none is set.
pub const DEFAULT_SCOPE: &str = "default";

/// Builder of a [`JobSpec`], created by [`JobSpec::command`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct JobSpecBuilder {
    program: String,
    args: Vec<String>,
    working_dir: WorkspacePath,
    env: Vec<(String, String)>,
    resources: ResourceRequest,
    sandbox: SandboxRequirement,
    sandbox_profile: SandboxProfileName,
    idempotency_key: Option<IdempotencyKey>,
    scope: Option<JobScopeId>,
    write_scope: Option<JobWriteScope>,
}

impl JobSpecBuilder {
    /// Appends one argument (passed verbatim, no shell).
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends several arguments.
    pub fn args<I, A>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the working directory relative to the workspace root.
    pub fn workspace(mut self, working_dir: WorkspacePath) -> Self {
        self.working_dir = working_dir;
        self
    }

    /// Adds one environment variable to the explicit allowlist.
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    /// Sets the resource request.
    pub fn resources(mut self, resources: ResourceRequest) -> Self {
        self.resources = resources;
        self
    }

    /// Sets the wall-clock timeout of one attempt.
    pub fn timeout(mut self, timeout: SignedDuration) -> Self {
        self.resources.wall_timeout = Some(timeout);
        self
    }

    /// Selects the sandbox profile.
    pub fn sandbox(mut self, profile: SandboxProfileName) -> Self {
        self.sandbox_profile = profile;
        self
    }

    /// Sets how strictly the sandbox profile must be enforced.
    pub fn sandbox_requirement(mut self, requirement: SandboxRequirement) -> Self {
        self.sandbox = requirement;
        self
    }

    /// Sets the deduplication key.
    pub fn idempotency_key(mut self, key: IdempotencyKey) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    /// Sets the authority scope.
    pub fn scope(mut self, scope: JobScopeId) -> Self {
        self.scope = Some(scope);
        self
    }

    /// Narrows the job's workspace writes to `scope` ([`JobWriteScope`])
    /// and sets [`SandboxRequirement::Required`]: a scoped job never runs on
    /// a backend that cannot enforce it. Lowering the requirement afterwards
    /// makes [`JobSpecBuilder::build`] fail.
    pub fn write_scope(mut self, scope: JobWriteScope) -> Self {
        self.write_scope = Some(scope);
        self.sandbox = SandboxRequirement::Required;
        self
    }

    /// Builds and validates the spec.
    ///
    /// # Errors
    /// [`SpecError`] naming the first violated rule (see
    /// [`JobSpec::validate`]).
    pub fn build(self) -> Result<JobSpec, SpecError> {
        let scope = match self.scope {
            Some(scope) => scope,
            None => JobScopeId::new(DEFAULT_SCOPE).map_err(|error| SpecError::InvalidId {
                field: "scope",
                reason: error.to_string(),
            })?,
        };
        let spec = JobSpec {
            program: self.program,
            args: self.args,
            working_dir: self.working_dir,
            env: self.env,
            resources: self.resources,
            sandbox: self.sandbox,
            sandbox_profile: self.sandbox_profile,
            idempotency_key: self.idempotency_key,
            scope,
            write_scope: self.write_scope,
        };
        spec.validate()?;
        Ok(spec)
    }
}

/// Versioned wire envelope of a [`JobSpec`] (Eco-Doc §59).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpecEnvelope {
    /// Schema version of `spec`.
    pub version: u16,
    /// The typed specification.
    pub spec: JobSpec,
}

impl JobSpecEnvelope {
    /// The envelope version this build writes and reads.
    pub const CURRENT_VERSION: u16 = 1;

    /// Wraps a validated spec in a current-version envelope.
    ///
    /// # Errors
    /// [`SpecError`] if the spec is invalid.
    pub fn new(spec: JobSpec) -> Result<Self, SpecError> {
        spec.validate()?;
        Ok(Self {
            version: Self::CURRENT_VERSION,
            spec,
        })
    }

    /// Unwraps the spec after checking version and validity. Always call
    /// this on a deserialized envelope before acting on it.
    ///
    /// # Errors
    /// [`SpecError::UnsupportedVersion`] or any validation error.
    pub fn into_spec(self) -> Result<JobSpec, SpecError> {
        if self.version != Self::CURRENT_VERSION {
            return Err(SpecError::UnsupportedVersion {
                found: self.version,
                supported: Self::CURRENT_VERSION,
            });
        }
        self.spec.validate()?;
        Ok(self.spec)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CpuBandwidth, JobSpec, JobSpecEnvelope, JobWriteScope, ResourceRequest,
        SandboxProfileName, SandboxRequirement, SpecError, WorkspacePath,
    };
    use std::path::PathBuf;
    use crate::ids::{IdempotencyKey, JobScopeId};
    use crate::test_support::{TestError, TestResult, ctx};
    use jiff::SignedDuration;

    fn spec() -> TestResult<JobSpec> {
        Ok(JobSpec {
            program: "cargo".into(),
            args: vec!["build".into(), "--locked".into()],
            working_dir: WorkspacePath::new("crates/app").map_err(ctx("path"))?,
            env: vec![("RUST_LOG".into(), "info".into())],
            resources: ResourceRequest {
                memory_max: Some(512 * 1024 * 1024),
                cpu_weight: Some(100),
                pids_max: Some(256),
                wall_timeout: Some(SignedDuration::from_secs(600)),
                ..ResourceRequest::default()
            },
            sandbox: SandboxRequirement::Required,
            sandbox_profile: SandboxProfileName::WorkspaceBuild,
            idempotency_key: Some(IdempotencyKey::new("build-42").map_err(ctx("key"))?),
            scope: JobScopeId::new("scope-a").map_err(ctx("scope"))?,
            write_scope: None,
        })
    }

    #[test]
    fn workspace_path_accepts_relative_paths() -> TestResult {
        for path in [".", "src", "a/b/c", "./a", "a..b", "..a", "a/.../b"] {
            assert_eq!(
                WorkspacePath::new(path)
                    .map_err(ctx("relative path"))?
                    .as_str(),
                path
            );
        }
        assert_eq!(WorkspacePath::root().as_str(), ".");
        Ok(())
    }

    #[test]
    fn workspace_path_rejects_escapes() {
        assert_eq!(WorkspacePath::new(""), Err(SpecError::EmptyPath));
        for path in ["/etc", "\\share", "C:\\x", "c:/x"] {
            assert_eq!(
                WorkspacePath::new(path),
                Err(SpecError::AbsolutePath { path: path.into() })
            );
        }
        for path in ["..", "../x", "a/../b", "a/..", "a\\..\\b"] {
            assert_eq!(
                WorkspacePath::new(path),
                Err(SpecError::ParentTraversal { path: path.into() })
            );
        }
        assert_eq!(
            WorkspacePath::new("a\0b"),
            Err(SpecError::NulByte {
                field: "working_dir"
            })
        );
    }

    #[test]
    fn workspace_path_is_validated_on_deserialization() {
        assert!(serde_json::from_str::<WorkspacePath>("\"../x\"").is_err());
        assert!(serde_json::from_str::<WorkspacePath>("\"/x\"").is_err());
        assert!(serde_json::from_str::<WorkspacePath>("\"x\"").is_ok());
    }

    #[test]
    fn valid_spec_passes_validation() -> TestResult {
        spec()?.validate().map_err(ctx("valid spec"))?;
        Ok(())
    }

    #[test]
    fn spec_validation_rejects_each_invalid_field() -> TestResult {
        let mut s = spec()?;
        s.program = String::new();
        assert_eq!(s.validate(), Err(SpecError::EmptyProgram));

        let mut s = spec()?;
        s.program = "a\0".into();
        assert_eq!(s.validate(), Err(SpecError::NulByte { field: "program" }));

        let mut s = spec()?;
        s.args.push("x\0y".into());
        assert_eq!(s.validate(), Err(SpecError::NulByte { field: "args" }));

        for name in ["", "A=B", "A\0"] {
            let mut s = spec()?;
            s.env.push((name.into(), "v".into()));
            assert_eq!(
                s.validate(),
                Err(SpecError::InvalidEnvName { name: name.into() })
            );
        }

        let mut s = spec()?;
        s.env.push(("OK".into(), "v\0".into()));
        assert_eq!(s.validate(), Err(SpecError::NulByte { field: "env" }));

        let mut s = spec()?;
        s.env.push(("RUST_LOG".into(), "debug".into()));
        assert_eq!(
            s.validate(),
            Err(SpecError::DuplicateEnvName {
                name: "RUST_LOG".into()
            })
        );
        Ok(())
    }

    #[test]
    fn resource_request_rejects_out_of_range_values() {
        assert_eq!(ResourceRequest::default().validate(), Ok(()));
        let cases = [
            (
                ResourceRequest {
                    memory_max: Some(0),
                    ..ResourceRequest::default()
                },
                "memory_max",
            ),
            (
                ResourceRequest {
                    cpu_weight: Some(0),
                    ..ResourceRequest::default()
                },
                "cpu_weight",
            ),
            (
                ResourceRequest {
                    cpu_weight: Some(10_001),
                    ..ResourceRequest::default()
                },
                "cpu_weight",
            ),
            (
                ResourceRequest {
                    pids_max: Some(0),
                    ..ResourceRequest::default()
                },
                "pids_max",
            ),
            (
                ResourceRequest {
                    wall_timeout: Some(SignedDuration::ZERO),
                    ..ResourceRequest::default()
                },
                "wall_timeout",
            ),
            (
                ResourceRequest {
                    wall_timeout: Some(SignedDuration::from_secs(-1)),
                    ..ResourceRequest::default()
                },
                "wall_timeout",
            ),
        ];
        for (request, field) in cases {
            assert!(
                matches!(
                    request.validate(),
                    Err(SpecError::InvalidResource { resource, .. }) if resource == field
                ),
                "{field}"
            );
        }
        let edge = ResourceRequest {
            memory_max: Some(1),
            cpu_weight: Some(ResourceRequest::MAX_CPU_WEIGHT),
            pids_max: Some(1),
            wall_timeout: Some(SignedDuration::from_nanos(1)),
            ..ResourceRequest::default()
        };
        assert_eq!(edge.validate(), Ok(()));
    }

    #[test]
    fn envelope_round_trips_through_serde() -> TestResult {
        let envelope = JobSpecEnvelope::new(spec()?).map_err(ctx("envelope"))?;
        assert_eq!(envelope.version, JobSpecEnvelope::CURRENT_VERSION);
        let json = serde_json::to_string(&envelope).map_err(ctx("serialize"))?;
        let back: JobSpecEnvelope = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, envelope);
        assert_eq!(back.into_spec().map_err(ctx("into_spec"))?, spec()?);
        Ok(())
    }

    #[test]
    fn envelope_minimal_json_uses_defaults() -> TestResult {
        let json = r#"{"version":1,"spec":{"program":"true","working_dir":".","sandbox_profile":"no_network","scope":"s"}}"#;
        let envelope: JobSpecEnvelope = serde_json::from_str(json).map_err(ctx("minimal"))?;
        let spec = envelope.into_spec().map_err(ctx("into_spec"))?;
        assert!(spec.args.is_empty());
        assert!(spec.env.is_empty());
        assert_eq!(spec.resources, ResourceRequest::default());
        assert_eq!(spec.sandbox, SandboxRequirement::BestEffort);
        assert_eq!(spec.idempotency_key, None);
        Ok(())
    }

    #[test]
    fn envelope_rejects_unknown_version_unknown_fields_and_invalid_specs() -> TestResult {
        let mut envelope = JobSpecEnvelope::new(spec()?).map_err(ctx("envelope"))?;
        envelope.version = 2;
        assert_eq!(
            envelope.into_spec(),
            Err(SpecError::UnsupportedVersion {
                found: 2,
                supported: 1
            })
        );

        let unknown = r#"{"version":1,"spec":{"program":"true","working_dir":".","sandbox_profile":"no_network","scope":"s","payload":{}}}"#;
        assert!(serde_json::from_str::<JobSpecEnvelope>(unknown).is_err());

        let escaping = r#"{"version":1,"spec":{"program":"true","working_dir":"../x","sandbox_profile":"no_network","scope":"s"}}"#;
        assert!(serde_json::from_str::<JobSpecEnvelope>(escaping).is_err());

        let mut invalid = spec()?;
        invalid.program = String::new();
        assert_eq!(
            JobSpecEnvelope::new(invalid.clone()),
            Err(SpecError::EmptyProgram)
        );
        let smuggled = JobSpecEnvelope {
            version: JobSpecEnvelope::CURRENT_VERSION,
            spec: invalid,
        };
        let Err(error) = smuggled.into_spec() else {
            return Err(TestError::Unexpected("invalid spec unwrapped".into()));
        };
        assert_eq!(error, SpecError::EmptyProgram);
        Ok(())
    }

    #[test]
    fn sandbox_enums_serialize_snake_case() -> TestResult {
        assert_eq!(
            serde_json::to_string(&SandboxProfileName::ReadOnlyAnalysis).map_err(ctx("profile"))?,
            "\"read_only_analysis\""
        );
        assert_eq!(
            serde_json::to_string(&SandboxRequirement::BestEffort).map_err(ctx("req"))?,
            "\"best_effort\""
        );
        Ok(())
    }

    #[test]
    fn spec_error_display_is_descriptive() {
        assert_eq!(
            SpecError::UnsupportedVersion {
                found: 9,
                supported: 1
            }
            .to_string(),
            "job spec envelope version 9 is not supported (this build reads 1)"
        );
        assert_eq!(
            SpecError::ParentTraversal {
                path: "../x".into()
            }
            .to_string(),
            "workspace path '../x' must not contain '..'"
        );
    }

    #[test]
    fn command_builder_applies_defaults() -> TestResult {
        let spec = JobSpec::command("true").build().map_err(ctx("build"))?;
        assert_eq!(spec.program, "true");
        assert!(spec.args.is_empty());
        assert_eq!(spec.working_dir, WorkspacePath::root());
        assert!(spec.env.is_empty());
        assert_eq!(spec.resources, ResourceRequest::default());
        assert_eq!(spec.sandbox, SandboxRequirement::BestEffort);
        assert_eq!(spec.sandbox_profile, SandboxProfileName::WorkspaceBuild);
        assert_eq!(spec.idempotency_key, None);
        assert_eq!(spec.scope.as_str(), super::DEFAULT_SCOPE);
        Ok(())
    }

    #[test]
    fn command_builder_sets_every_field() -> TestResult {
        let built = JobSpec::command("cargo")
            .arg("build")
            .args(["--locked"])
            .workspace(WorkspacePath::new("crates/app").map_err(ctx("path"))?)
            .env("RUST_LOG", "info")
            .resources(ResourceRequest {
                memory_max: Some(512 * 1024 * 1024),
                cpu_weight: Some(100),
                pids_max: Some(256),
                wall_timeout: None,
                ..ResourceRequest::default()
            })
            .timeout(SignedDuration::from_secs(600))
            .sandbox(SandboxProfileName::WorkspaceBuild)
            .sandbox_requirement(SandboxRequirement::Required)
            .idempotency_key(IdempotencyKey::new("build-42").map_err(ctx("key"))?)
            .scope(JobScopeId::new("scope-a").map_err(ctx("scope"))?)
            .build()
            .map_err(ctx("build"))?;
        assert_eq!(built, spec()?);
        Ok(())
    }

    #[test]
    fn command_builder_validates_on_build() {
        assert_eq!(JobSpec::command("").build(), Err(SpecError::EmptyProgram));
        assert_eq!(
            JobSpec::command("x").env("A=B", "v").build(),
            Err(SpecError::InvalidEnvName { name: "A=B".into() })
        );
        assert!(matches!(
            JobSpec::command("x").timeout(SignedDuration::ZERO).build(),
            Err(SpecError::InvalidResource {
                resource: "wall_timeout",
                ..
            })
        ));
    }

    #[test]
    fn rlimit_requests_are_validated_and_detected() {
        let none = ResourceRequest::default();
        assert!(!none.requests_rlimits());
        assert!(none.validate().is_ok());
        for request in [
            ResourceRequest {
                address_space_max: Some(1 << 30),
                ..ResourceRequest::default()
            },
            ResourceRequest {
                cpu_time_max: Some(60),
                ..ResourceRequest::default()
            },
            ResourceRequest {
                file_size_max: Some(1 << 20),
                ..ResourceRequest::default()
            },
            ResourceRequest {
                open_files_max: Some(256),
                ..ResourceRequest::default()
            },
        ] {
            assert!(request.requests_rlimits());
            assert!(request.validate().is_ok());
        }
        for request in [
            ResourceRequest {
                address_space_max: Some(0),
                ..ResourceRequest::default()
            },
            ResourceRequest {
                cpu_time_max: Some(0),
                ..ResourceRequest::default()
            },
            ResourceRequest {
                file_size_max: Some(0),
                ..ResourceRequest::default()
            },
            ResourceRequest {
                open_files_max: Some(0),
                ..ResourceRequest::default()
            },
        ] {
            assert!(request.validate().is_err(), "{request:?}");
        }
    }

    #[test]
    fn an_old_spec_without_rlimits_still_deserializes() {
        let old = r#"{"memory_max": 1024}"#;
        let parsed: Result<ResourceRequest, _> = serde_json::from_str(old);
        assert!(parsed.is_ok_and(|r| !r.requests_rlimits()));
    }

    fn scoped_spec() -> TestResult<JobSpec> {
        JobSpec::command("make")
            .sandbox(SandboxProfileName::NoNetwork)
            .write_scope(
                JobWriteScope::new(["/work/repo/src", "/work/repo/target"])
                    .map_err(ctx("scope"))?,
            )
            .build()
            .map_err(ctx("build"))
    }

    #[test]
    fn write_scope_round_trips_and_old_records_have_none() -> TestResult {
        let spec = scoped_spec()?;
        assert_eq!(spec.sandbox, SandboxRequirement::Required, "set by the builder");
        let scope = spec
            .write_scope
            .clone()
            .ok_or(TestError::Missing("write scope"))?;
        assert_eq!(scope.version, JobWriteScope::CURRENT_VERSION);
        assert_eq!(
            scope.paths,
            [
                PathBuf::from("/work/repo/src"),
                PathBuf::from("/work/repo/target")
            ]
        );
        let envelope = JobSpecEnvelope::new(spec.clone()).map_err(ctx("envelope"))?;
        let json = serde_json::to_string(&envelope).map_err(ctx("serialize"))?;
        assert!(
            json.contains(r#""write_scope":{"version":1,"paths":["/work/repo/src","/work/repo/target"]}"#),
            "{json}"
        );
        let back: JobSpecEnvelope = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back.into_spec().map_err(ctx("into_spec"))?, spec);

        // A record written before the field existed reads as unscoped, and
        // an unscoped spec serializes exactly as before (no new key).
        let old = r#"{"version":1,"spec":{"program":"true","working_dir":".","sandbox_profile":"no_network","scope":"s"}}"#;
        let old: JobSpecEnvelope = serde_json::from_str(old).map_err(ctx("old record"))?;
        let old = old.into_spec().map_err(ctx("old into_spec"))?;
        assert_eq!(old.write_scope, None);
        let unscoped = serde_json::to_string(&old).map_err(ctx("serialize unscoped"))?;
        assert!(!unscoped.contains("write_scope"), "{unscoped}");
        Ok(())
    }

    #[test]
    fn unknown_write_scope_version_is_refused_not_ignored() -> TestResult {
        let mut spec = scoped_spec()?;
        if let Some(scope) = spec.write_scope.as_mut() {
            scope.version = 2;
        }
        let expected = SpecError::UnsupportedWriteScopeVersion {
            found: 2,
            supported: 1,
        };
        assert_eq!(spec.validate(), Err(expected.clone()));
        // The version is checked before the paths.
        if let Some(scope) = spec.write_scope.as_mut() {
            scope.paths.push(PathBuf::from("relative"));
        }
        assert_eq!(spec.validate(), Err(expected.clone()));

        // At admission: a deserialized record with a newer scope version.
        let json = r#"{"version":1,"spec":{"program":"true","working_dir":".","sandbox":"required","sandbox_profile":"no_network","scope":"s","write_scope":{"version":7,"paths":["/w/a"]}}}"#;
        let envelope: JobSpecEnvelope = serde_json::from_str(json).map_err(ctx("parse"))?;
        assert_eq!(
            envelope.into_spec(),
            Err(SpecError::UnsupportedWriteScopeVersion {
                found: 7,
                supported: 1
            })
        );
        // A scope with unknown fields does not deserialize at all.
        let shaped = r#"{"version":1,"spec":{"program":"true","working_dir":".","sandbox":"required","sandbox_profile":"no_network","scope":"s","write_scope":{"version":2,"paths":[],"globs":["**"]}}}"#;
        assert!(serde_json::from_str::<JobSpecEnvelope>(shaped).is_err());
        assert_eq!(
            expected.to_string(),
            "job write scope version 2 is not supported (this build reads 1)"
        );
        Ok(())
    }

    #[test]
    fn write_scope_paths_must_be_absolute_and_normalised() {
        for (path, reason) in [
            ("relative/x", "must be absolute"),
            ("/", "must not be the host root"),
            ("/w/./a", "must be normalised"),
            ("/w/../etc", "must be normalised"),
            ("/w//a", "must be normalised"),
            ("/w/a/", "must be normalised"),
            ("/w/a\0b", "contains a NUL byte"),
        ] {
            assert!(
                matches!(
                    JobWriteScope::new([path]),
                    Err(SpecError::InvalidWriteScopePath { reason: found, .. })
                        if found.starts_with(reason)
                ),
                "{path:?}"
            );
        }
        assert_eq!(
            JobWriteScope::new(["/w/a", "/w/a"]),
            Err(SpecError::DuplicateWriteScopePath {
                path: "/w/a".into()
            })
        );
        let many = (0..=JobWriteScope::MAX_PATHS).map(|index| format!("/w/{index}"));
        assert_eq!(
            JobWriteScope::new(many),
            Err(SpecError::TooManyWriteScopePaths {
                count: JobWriteScope::MAX_PATHS + 1,
                max: JobWriteScope::MAX_PATHS
            })
        );
        // An empty scope is valid: the whole workspace becomes read-only.
        assert!(JobWriteScope::new(Vec::<PathBuf>::new()).is_ok());
    }

    #[test]
    fn write_scope_requires_required_and_a_writable_profile() -> TestResult {
        let scope = JobWriteScope::new(["/w/a"]).map_err(ctx("scope"))?;
        for requirement in [SandboxRequirement::None, SandboxRequirement::BestEffort] {
            assert_eq!(
                JobSpec::command("true")
                    .write_scope(scope.clone())
                    .sandbox_requirement(requirement)
                    .build(),
                Err(SpecError::WriteScopeNotRequired { requirement })
            );
        }
        let read_only = JobSpec::command("true")
            .sandbox(SandboxProfileName::ReadOnlyAnalysis)
            .write_scope(scope.clone())
            .build();
        assert_eq!(
            read_only,
            Err(SpecError::WriteScopeWithoutWorkspaceWrite {
                profile: SandboxProfileName::ReadOnlyAnalysis
            })
        );
        assert_eq!(
            SpecError::WriteScopeNotRequired {
                requirement: SandboxRequirement::BestEffort
            }
            .to_string(),
            "a job with a write scope must use sandbox requirement 'required', not 'best_effort'"
        );
        for profile in [
            SandboxProfileName::WorkspaceBuild,
            SandboxProfileName::NoNetwork,
            SandboxProfileName::NetworkRestricted,
        ] {
            JobSpec::command("true")
                .sandbox(profile)
                .write_scope(scope.clone())
                .build()
                .map_err(ctx("scoped spec"))?;
        }
        Ok(())
    }

    #[test]
    fn job_limits_are_validated_and_detected() {
        assert!(!ResourceRequest::default().requests_job_limits());
        let all = ResourceRequest {
            memory_max: Some(1 << 30),
            memory_high: Some(1 << 29),
            cpu_weight: Some(200),
            cpu_max: Some(CpuBandwidth::cpus(2)),
            pids_max: Some(64),
            io_weight: Some(50),
            ..ResourceRequest::default()
        };
        assert_eq!(all.validate(), Ok(()));
        for single in [
            ResourceRequest {
                memory_high: Some(1),
                ..ResourceRequest::default()
            },
            ResourceRequest {
                cpu_max: Some(CpuBandwidth::cpus(1)),
                ..ResourceRequest::default()
            },
            ResourceRequest {
                io_weight: Some(1),
                ..ResourceRequest::default()
            },
        ] {
            assert!(single.requests_job_limits(), "{single:?}");
            assert!(!single.requests_rlimits(), "{single:?}");
        }
        let cases = [
            (
                ResourceRequest {
                    memory_high: Some(0),
                    ..ResourceRequest::default()
                },
                "memory_high",
            ),
            (
                ResourceRequest {
                    memory_max: Some(10),
                    memory_high: Some(11),
                    ..ResourceRequest::default()
                },
                "memory_high",
            ),
            (
                ResourceRequest {
                    cpu_max: Some(CpuBandwidth {
                        quota_us: 50_000,
                        period_us: 999,
                    }),
                    ..ResourceRequest::default()
                },
                "cpu_max.period_us",
            ),
            (
                ResourceRequest {
                    cpu_max: Some(CpuBandwidth {
                        quota_us: 50_000,
                        period_us: 1_000_001,
                    }),
                    ..ResourceRequest::default()
                },
                "cpu_max.period_us",
            ),
            (
                ResourceRequest {
                    cpu_max: Some(CpuBandwidth {
                        quota_us: 999,
                        period_us: 100_000,
                    }),
                    ..ResourceRequest::default()
                },
                "cpu_max.quota_us",
            ),
            (
                ResourceRequest {
                    io_weight: Some(0),
                    ..ResourceRequest::default()
                },
                "io_weight",
            ),
            (
                ResourceRequest {
                    io_weight: Some(10_001),
                    ..ResourceRequest::default()
                },
                "io_weight",
            ),
        ];
        for (request, field) in cases {
            assert!(
                matches!(
                    request.validate(),
                    Err(SpecError::InvalidResource { resource, .. }) if resource == field
                ),
                "{field}: {request:?}"
            );
        }
    }

    #[test]
    fn new_job_limits_round_trip_and_old_requests_stay_unchanged() -> TestResult {
        let request = ResourceRequest {
            memory_high: Some(4096),
            cpu_max: Some(CpuBandwidth {
                quota_us: 20_000,
                period_us: 100_000,
            }),
            io_weight: Some(10),
            ..ResourceRequest::default()
        };
        let json = serde_json::to_string(&request).map_err(ctx("serialize"))?;
        assert_eq!(
            json,
            r#"{"memory_high":4096,"cpu_max":{"quota_us":20000,"period_us":100000},"io_weight":10}"#
        );
        let back: ResourceRequest = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, request);
        let old = ResourceRequest {
            memory_max: Some(1),
            ..ResourceRequest::default()
        };
        assert_eq!(
            serde_json::to_string(&old).map_err(ctx("serialize old"))?,
            r#"{"memory_max":1}"#
        );
        Ok(())
    }
}
