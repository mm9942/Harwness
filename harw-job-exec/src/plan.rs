//! The versioned launch plan the supervisor hands to the trampoline.
//!
//! The plan is one JSON document. Parsing first reads only `version`; an
//! unknown version is rejected with [`ExecError::UnsupportedVersion`]
//! before any other field is interpreted, so a future `ExecPlanV2` can
//! change every other field without being misread by an old trampoline.
//! Unknown fields are rejected (`deny_unknown_fields`): a plan field this
//! trampoline does not understand could be a control it would silently
//! skip.

use std::path::PathBuf;

use harw_job_core::{EnforcementState, SandboxRequirement};
use harw_job_linux::{RlimitSet, SandboxPolicy};
use serde::{Deserialize, Serialize};

use crate::error::ExecError;

/// The only plan version this trampoline understands.
pub const PLAN_VERSION: u32 = 1;

/// Upper bound for a serialized plan (64 KiB). Checked by the parent before
/// writing and by the trampoline before parsing.
pub const MAX_PLAN_BYTES: usize = 64 * 1024;

/// Maximum length of a cgroup child name (mirrors `harw-job-linux`).
const MAX_CGROUP_NAME: usize = 200;

/// The job cgroup the trampoline moves itself into before anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupJoin {
    /// The delegated cgroup v2 root (absolute path under the cgroup2 mount)
    /// the supervisor's `CgroupV2Fs` is rooted at.
    pub root: PathBuf,
    /// The job cgroup's child name beneath `root` (one path component, as
    /// created by `CgroupBackend::create`).
    pub relative: String,
    /// What the supervisor's `CgroupHandle::limits_enforcement` reported
    /// for the cgroup limits. `None` (unknown) counts as
    /// [`EnforcementState::NotEnforced`] in the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<EnforcementState>,
}

/// Launch plan, version 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecPlanV1 {
    /// Always [`PLAN_VERSION`].
    pub version: u32,
    /// Job cgroup to join, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup: Option<CgroupJoin>,
    /// rlimits to apply to the trampoline (and so to the job).
    #[serde(default)]
    pub rlimits: RlimitSet,
    /// Sandbox policy to apply, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxPolicy>,
    /// How strictly the achieved report must match; checked before `exec`.
    pub sandbox_requirement: SandboxRequirement,
    /// Program to `exec` (a bare name is looked up in `PATH`).
    pub program: PathBuf,
    /// Arguments after `argv[0]`. UTF-8 only: the plan is JSON.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Reads only the version field (unknown fields allowed).
#[derive(Deserialize)]
struct VersionProbe {
    version: u64,
}

impl ExecPlanV1 {
    /// A plan that just runs `program`: no cgroup, no rlimits, no sandbox,
    /// requirement [`SandboxRequirement::None`].
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            version: PLAN_VERSION,
            cgroup: None,
            rlimits: RlimitSet::new(),
            sandbox: None,
            sandbox_requirement: SandboxRequirement::None,
            program: program.into(),
            args: Vec::new(),
        }
    }

    /// Sets the arguments (builder style).
    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the cgroup to join (builder style).
    #[must_use]
    pub fn with_cgroup(mut self, cgroup: CgroupJoin) -> Self {
        self.cgroup = Some(cgroup);
        self
    }

    /// Sets the rlimits (builder style).
    #[must_use]
    pub fn with_rlimits(mut self, rlimits: RlimitSet) -> Self {
        self.rlimits = rlimits;
        self
    }

    /// Sets the sandbox policy and requirement (builder style).
    #[must_use]
    pub fn with_sandbox(mut self, policy: SandboxPolicy, requirement: SandboxRequirement) -> Self {
        self.sandbox = Some(policy);
        self.sandbox_requirement = requirement;
        self
    }

    /// Checks every semantic rule of the plan.
    ///
    /// # Errors
    /// [`ExecError::UnsupportedVersion`] for a version other than
    /// [`PLAN_VERSION`]; [`ExecError::InvalidPlan`] for an empty program, a
    /// NUL byte in program or arguments, an invalid cgroup join, invalid
    /// rlimits, or [`SandboxRequirement::Required`] without a sandbox
    /// policy (contradictory: nothing could ever satisfy it).
    pub fn validate(&self) -> Result<(), ExecError> {
        if self.version != PLAN_VERSION {
            return Err(ExecError::UnsupportedVersion {
                found: u64::from(self.version),
            });
        }
        let invalid = |reason: String| Err(ExecError::InvalidPlan { reason });
        if self.program.as_os_str().is_empty() {
            return invalid("program is empty".to_owned());
        }
        if self.program.as_os_str().as_encoded_bytes().contains(&0) {
            return invalid("program contains a NUL byte".to_owned());
        }
        if let Some(index) = self.args.iter().position(|arg| arg.contains('\0')) {
            return invalid(format!("argument {index} contains a NUL byte"));
        }
        if let Some(cgroup) = &self.cgroup {
            validate_cgroup(cgroup)?;
        }
        self.rlimits
            .validate()
            .map_err(|error| ExecError::InvalidPlan {
                reason: error.to_string(),
            })?;
        if self.sandbox_requirement == SandboxRequirement::Required && self.sandbox.is_none() {
            return invalid("sandbox requirement `required` without a sandbox policy".to_owned());
        }
        Ok(())
    }

    /// Validates and serializes the plan.
    ///
    /// # Errors
    /// As [`ExecPlanV1::validate`]; [`ExecError::PlanParse`] when a path is
    /// not UTF-8; [`ExecError::PlanTooLarge`] above [`MAX_PLAN_BYTES`].
    pub fn to_json(&self) -> Result<Vec<u8>, ExecError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|error| ExecError::PlanParse {
            message: error.to_string(),
        })?;
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(ExecError::PlanTooLarge {
                limit: MAX_PLAN_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Parses and validates a serialized plan.
    ///
    /// # Errors
    /// [`ExecError::PlanTooLarge`] above [`MAX_PLAN_BYTES`];
    /// [`ExecError::PlanParse`] for malformed JSON, a missing version or
    /// unknown fields; [`ExecError::UnsupportedVersion`]; the errors of
    /// [`ExecPlanV1::validate`].
    pub fn from_json(bytes: &[u8]) -> Result<Self, ExecError> {
        if bytes.len() > MAX_PLAN_BYTES {
            return Err(ExecError::PlanTooLarge {
                limit: MAX_PLAN_BYTES,
            });
        }
        let parse_error = |error: serde_json::Error| ExecError::PlanParse {
            message: error.to_string(),
        };
        let probe: VersionProbe = serde_json::from_slice(bytes).map_err(parse_error)?;
        if probe.version != u64::from(PLAN_VERSION) {
            return Err(ExecError::UnsupportedVersion {
                found: probe.version,
            });
        }
        let plan: Self = serde_json::from_slice(bytes).map_err(parse_error)?;
        plan.validate()?;
        Ok(plan)
    }
}

fn validate_cgroup(cgroup: &CgroupJoin) -> Result<(), ExecError> {
    let invalid = |reason: String| Err(ExecError::InvalidPlan { reason });
    if !cgroup.root.is_absolute() {
        return invalid(format!(
            "cgroup root {} is not absolute",
            cgroup.root.display()
        ));
    }
    let name = cgroup.relative.as_str();
    let valid_chars = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if name.is_empty() || name.len() > MAX_CGROUP_NAME || name.starts_with('.') || !valid_chars {
        return invalid(format!(
            "cgroup name {name:?} must be one component of [A-Za-z0-9_.-], not starting with '.', at most {MAX_CGROUP_NAME} bytes"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use harw_job_core::SandboxProfileName;
    use harw_job_linux::{RlimitResource, RlimitValue};

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn full_plan() -> ExecPlanV1 {
        ExecPlanV1::new("/bin/sh")
            .with_args(["-c", "exit 7"])
            .with_cgroup(CgroupJoin {
                root: PathBuf::from("/sys/fs/cgroup/user.slice/harw"),
                relative: "job-attempt-1".to_owned(),
                limits: Some(EnforcementState::Partial),
            })
            .with_rlimits(RlimitSet::new().with(RlimitResource::Nofile, RlimitValue::fixed(256)))
            .with_sandbox(
                SandboxPolicy::from_profile(
                    SandboxProfileName::ReadOnlyAnalysis,
                    Path::new("/work/repo"),
                ),
                SandboxRequirement::BestEffort,
            )
    }

    fn expect_err(result: Result<ExecPlanV1, ExecError>) -> TestResult<ExecError> {
        match result {
            Ok(plan) => Err(TestError::Unexpected(format!("accepted {plan:?}"))),
            Err(error) => Ok(error),
        }
    }

    #[test]
    fn test_plan_round_trips_through_json() -> TestResult {
        let plan = full_plan();
        let json = plan.to_json().map_err(ctx("serialize"))?;
        let text = String::from_utf8(json.clone()).map_err(ctx("utf-8"))?;
        assert!(text.contains(r#""version":1"#), "{text}");
        assert!(
            text.contains(r#""sandbox_requirement":"best_effort""#),
            "{text}"
        );
        let back = ExecPlanV1::from_json(&json).map_err(ctx("parse"))?;
        assert_eq!(back, plan);

        let minimal = ExecPlanV1::new("true");
        let back = ExecPlanV1::from_json(&minimal.to_json().map_err(ctx("serialize"))?)
            .map_err(ctx("parse minimal"))?;
        assert_eq!(back, minimal);
        Ok(())
    }

    #[test]
    fn test_plan_rejects_other_versions_before_other_fields() -> TestResult {
        let error = expect_err(ExecPlanV1::from_json(
            br#"{"version":2,"something_new":true}"#,
        ))?;
        assert!(
            matches!(error, ExecError::UnsupportedVersion { found: 2 }),
            "{error}"
        );
        let error = expect_err(ExecPlanV1::from_json(br#"{"program":"/bin/true"}"#))?;
        assert!(matches!(error, ExecError::PlanParse { .. }), "{error}");
        let error = expect_err(ExecPlanV1::from_json(b"not json"))?;
        assert!(matches!(error, ExecError::PlanParse { .. }), "{error}");

        let mut plan = ExecPlanV1::new("/bin/true");
        plan.version = 0;
        assert!(matches!(
            plan.validate(),
            Err(ExecError::UnsupportedVersion { found: 0 })
        ));
        Ok(())
    }

    #[test]
    fn test_plan_rejects_unknown_fields() -> TestResult {
        let error = expect_err(ExecPlanV1::from_json(
            br#"{"version":1,"sandbox_requirement":"none","program":"/bin/true","seccomp":"strict"}"#,
        ))?;
        assert!(matches!(error, ExecError::PlanParse { .. }), "{error}");
        Ok(())
    }

    #[test]
    fn test_plan_size_cap() -> TestResult {
        let huge = ExecPlanV1::new("/bin/true").with_args(["x".repeat(MAX_PLAN_BYTES)]);
        assert!(matches!(
            huge.to_json(),
            Err(ExecError::PlanTooLarge {
                limit: MAX_PLAN_BYTES
            })
        ));
        let oversized = vec![b' '; MAX_PLAN_BYTES + 1];
        let error = expect_err(ExecPlanV1::from_json(&oversized))?;
        assert!(matches!(error, ExecError::PlanTooLarge { .. }), "{error}");
        Ok(())
    }

    #[test]
    fn test_plan_validation_rules() -> TestResult {
        let invalid =
            |plan: ExecPlanV1| matches!(plan.validate(), Err(ExecError::InvalidPlan { .. }));
        assert!(invalid(ExecPlanV1::new("")));
        assert!(invalid(ExecPlanV1::new("/bin/true").with_args(["a\0b"])));
        let mut required = ExecPlanV1::new("/bin/true");
        required.sandbox_requirement = SandboxRequirement::Required;
        assert!(invalid(required));
        for (root, name) in [
            ("/sys/fs/cgroup/harw", ""),
            ("/sys/fs/cgroup/harw", ".."),
            ("/sys/fs/cgroup/harw", "a/b"),
            ("/sys/fs/cgroup/harw", ".hidden"),
            ("relative/root", "job"),
        ] {
            let plan = ExecPlanV1::new("/bin/true").with_cgroup(CgroupJoin {
                root: PathBuf::from(root),
                relative: name.to_owned(),
                limits: None,
            });
            assert!(invalid(plan), "{root} {name}");
        }
        let bad_rlimit = ExecPlanV1::new("/bin/true").with_rlimits(RlimitSet::new().with(
            RlimitResource::Nofile,
            RlimitValue {
                soft: Some(10),
                hard: Some(5),
            },
        ));
        assert!(invalid(bad_rlimit));
        full_plan().validate().map_err(ctx("full plan is valid"))?;
        Ok(())
    }
}
