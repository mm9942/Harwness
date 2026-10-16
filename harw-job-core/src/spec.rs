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
        }
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

/// Platform-neutral resource request. Every field is optional; `None` means
/// "no explicit limit requested". Backends report whether they could enforce
/// it via `SandboxReport::resource_limits`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequest {
    /// Memory ceiling for the whole job in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_max: Option<u64>,
    /// Relative CPU weight in `1..=10_000` (100 is the neutral default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_weight: Option<u16>,
    /// Maximum number of processes/threads in the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids_max: Option<u32>,
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
    /// Whether any per-process rlimit (`address_space_max`, `cpu_time_max`,
    /// `file_size_max`, `open_files_max`) is requested.
    #[must_use]
    pub fn requests_rlimits(&self) -> bool {
        self.address_space_max.is_some()
            || self.cpu_time_max.is_some()
            || self.file_size_max.is_some()
            || self.open_files_max.is_some()
    }

    /// Maximum permitted [`ResourceRequest::cpu_weight`].
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
        self.resources.validate()
    }

    /// Starts a builder for running `program` (Job-Runtime-Doc §16).
    ///
    /// Defaults: no arguments, the workspace root as working directory, an
    /// empty environment, no resource limits, sandbox profile
    /// [`SandboxProfileName::WorkspaceBuild`] at
    /// [`SandboxRequirement::BestEffort`], scope [`DEFAULT_SCOPE`]. Nothing
    /// is validated until [`JobSpecBuilder::build`].
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
        JobSpec, JobSpecEnvelope, ResourceRequest, SandboxProfileName, SandboxRequirement,
        SpecError, WorkspacePath,
    };
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
}
