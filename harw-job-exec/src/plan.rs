//! The versioned launch plan the supervisor hands to the trampoline.
//!
//! The plan is one JSON document. Parsing first reads only `version`; an
//! unknown version is rejected with [`ExecError::UnsupportedVersion`]
//! before any other field is interpreted, so a future `ExecPlanV2` can
//! change every other field without being misread by an old trampoline.
//! Unknown fields are rejected (`deny_unknown_fields`): a plan field this
//! trampoline does not understand could be a control it would silently
//! skip.
//!
//! # Cgroup membership
//! [`ExecPlanV1::cgroup`] is a [`CgroupMembership`]: either the trampoline
//! moves itself into the job cgroup ([`CgroupJoin`], the original and
//! still default form), or the supervisor already placed it there (e.g. a
//! Bubblewrap child spawned into the cgroup, where the trampoline cannot
//! reach cgroupfs) and the trampoline only verifies `/proc/self/cgroup`
//! ([`CgroupVerify`]). The JSON of a [`CgroupJoin`] is unchanged, so plans
//! written before `Verify` existed still parse.

use std::path::{Path, PathBuf};

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

/// Where `/proc/self/cgroup` is read for [`CgroupVerify`].
pub const PROC_SELF_CGROUP: &str = "/proc/self/cgroup";

/// The job cgroup the trampoline moves itself into before anything else
/// ([`CgroupMembership::Attach`]).
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

/// A cgroup membership the trampoline only checks
/// ([`CgroupMembership::Verify`]): the supervisor already placed the
/// process into the job cgroup, so the trampoline needs no cgroupfs access.
/// If the unified-hierarchy entry of `/proc/self/cgroup` names another
/// cgroup, the job is refused ([`ExecError::CgroupMismatch`], exit
/// [`crate::exit_code::SANDBOX_REFUSED`]) and never runs an instruction
/// outside its cgroup.
///
/// # Preconditions for the supervisor
/// The check proves membership only when the trampoline shares the
/// supervisor's cgroup namespace and is itself attached:
/// - **No fresh cgroup namespace.** Inside one the kernel prints paths
///   relative to the namespace root, which is whatever cgroup the process
///   was in when the namespace was created; `0::/` then means the job
///   cgroup or the supervisor's own one. A Bubblewrap launcher must list
///   `--unshare-user --unshare-ipc --unshare-pid --unshare-net
///   --unshare-uts` instead of `--unshare-all`, which includes
///   `--unshare-cgroup-try`.
/// - **The process that becomes the trampoline is attached.** Writing a pid
///   to `cgroup.procs` moves only that process, never descendants that
///   already exist. Attaching the launcher's primary pid after spawn
///   (`LinuxJobGroup::spawn_in_cgroup`) misses a child the launcher has
///   already cloned. Hold the launcher before the trampoline starts (e.g.
///   bwrap `--block-fd`) until its whole process tree is attached, or
///   attach before it clones. Otherwise this check refuses the job:
///   fail-closed, but the launch then fails at random.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupVerify {
    /// The job cgroup exactly as `CgroupHandle::proc_path` reports it: the
    /// absolute path the supervisor itself sees in the `0::<path>` line of
    /// `/proc/<pid>/cgroup`, i.e. relative to the supervisor's
    /// cgroup-namespace root (e.g. `/user.slice/…/harw.slice/harw-job-<attempt>`).
    /// Compared component-wise with the `0::<path>` line of the
    /// trampoline's `/proc/self/cgroup`.
    ///
    /// Validation rejects `/` (a job cgroup is never the root of the
    /// hierarchy, and inside a cgroup namespace `/` only means "at the
    /// namespace root") and `.` or `..` components (only a view from
    /// another cgroup namespace prints those), besides relative paths, NUL
    /// and newlines.
    pub expected_proc_path: PathBuf,
    /// As [`CgroupJoin::limits`]: what the supervisor's
    /// `CgroupHandle::limits_enforcement` reported for the cgroup limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<EnforcementState>,
}

impl CgroupVerify {
    /// Checks the content of `/proc/self/cgroup` against
    /// [`CgroupVerify::expected_proc_path`]. Pure.
    ///
    /// # Errors
    /// [`ExecError::InvalidPlan`] when [`CgroupVerify::expected_proc_path`]
    /// fails plan validation (checked here too, so a value built without
    /// [`ExecPlanV1::validate`] never passes); [`ExecError::CgroupMismatch`]
    /// when the content has no single unified-hierarchy entry
    /// ([`unified_cgroup_path`]) or it names another cgroup.
    pub fn check(&self, proc_self_cgroup: &str) -> Result<(), ExecError> {
        validate_verify(self)?;
        let found = match unified_cgroup_path(proc_self_cgroup) {
            Some(path) if Path::new(path) == self.expected_proc_path => return Ok(()),
            Some(path) => path.to_owned(),
            None => format!("no single cgroup v2 entry (`0::<path>`) in {PROC_SELF_CGROUP}"),
        };
        Err(ExecError::CgroupMismatch {
            expected: self.expected_proc_path.clone(),
            found,
        })
    }
}

/// How the trampoline establishes the job's cgroup membership — the first
/// control of the §24 order.
///
/// Serialized untagged: an attach is the unchanged [`CgroupJoin`] object
/// (`root`, `relative`, `limits`), a verify the [`CgroupVerify`] object
/// (`expected_proc_path`, `limits`). An object with fields of both is
/// rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CgroupMembership {
    /// Move into the job cgroup (`CgroupBackend::attach_self`).
    Attach(CgroupJoin),
    /// Only verify that the process already is in the job cgroup.
    Verify(CgroupVerify),
}

impl CgroupMembership {
    /// The supervisor-reported state of the cgroup limits.
    #[must_use]
    pub fn limits(&self) -> Option<EnforcementState> {
        match self {
            Self::Attach(join) => join.limits,
            Self::Verify(verify) => verify.limits,
        }
    }
}

impl From<CgroupJoin> for CgroupMembership {
    fn from(join: CgroupJoin) -> Self {
        Self::Attach(join)
    }
}

impl From<CgroupVerify> for CgroupMembership {
    fn from(verify: CgroupVerify) -> Self {
        Self::Verify(verify)
    }
}

/// The cgroup v2 path in the content of `/proc/self/cgroup`: the path of
/// the single unified-hierarchy line `0::<path>`. Pure.
///
/// Cgroup v1 lines (`<id>:<controllers>:<path>`, e.g. on hybrid hosts) are
/// ignored. `None` when there is no unified entry (a v1-only host), more
/// than one, or its path is not absolute — never a guess.
#[must_use]
pub fn unified_cgroup_path(proc_self_cgroup: &str) -> Option<&str> {
    let mut found = None;
    for line in proc_self_cgroup.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some("0"), Some(""), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if found.is_some() || !path.starts_with('/') {
            return None;
        }
        found = Some(path);
    }
    found
}

/// Launch plan, version 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecPlanV1 {
    /// Always [`PLAN_VERSION`].
    pub version: u32,
    /// How the job's cgroup membership is established, if at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup: Option<CgroupMembership>,
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

    /// Sets the cgroup membership (builder style): a [`CgroupJoin`] to
    /// attach, or a [`CgroupVerify`] to only check.
    #[must_use]
    pub fn with_cgroup(mut self, cgroup: impl Into<CgroupMembership>) -> Self {
        self.cgroup = Some(cgroup.into());
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
    /// NUL byte in program or arguments, an invalid cgroup membership, invalid
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

fn validate_cgroup(cgroup: &CgroupMembership) -> Result<(), ExecError> {
    match cgroup {
        CgroupMembership::Attach(join) => validate_join(join),
        CgroupMembership::Verify(verify) => validate_verify(verify),
    }
}

/// Fail-closed rules for [`CgroupVerify::expected_proc_path`].
fn validate_verify(verify: &CgroupVerify) -> Result<(), ExecError> {
    let path = &verify.expected_proc_path;
    let bytes = path.as_os_str().as_encoded_bytes();
    // Raw segments: `Path::components` silently drops `.`.
    let dot_segment = bytes
        .split(|byte| *byte == b'/')
        .any(|segment| segment == b"." || segment == b"..");
    let reason = if !path.is_absolute() {
        "must be absolute"
    } else if bytes.contains(&0) || bytes.contains(&b'\n') {
        "must not contain NUL or a newline"
    } else if dot_segment {
        "must not contain `.` or `..` components"
    } else if path.parent().is_none() {
        "must not be `/`: a job cgroup is never the root of the hierarchy"
    } else {
        return Ok(());
    };
    Err(ExecError::InvalidPlan {
        reason: format!("expected cgroup path {path:?} {reason}"),
    })
}

fn validate_join(cgroup: &CgroupJoin) -> Result<(), ExecError> {
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
        for expected in [
            "relative/harw-job-1",
            "",
            "/harw-job-1\n0::/",
            "/a\0b",
            "/",
            "//",
            "/a/../b",
            "/../harw-job-1",
            "/a/./b",
            "/a/.",
        ] {
            let plan = ExecPlanV1::new("/bin/true").with_cgroup(CgroupVerify {
                expected_proc_path: PathBuf::from(expected),
                limits: None,
            });
            assert!(invalid(plan), "{expected:?}");
        }
        let job = ExecPlanV1::new("/bin/true").with_cgroup(CgroupVerify {
            expected_proc_path: PathBuf::from("/user.slice/harw.slice/harw-job-1"),
            limits: None,
        });
        job.validate().map_err(ctx("absolute job path is valid"))?;
        Ok(())
    }

    #[test]
    fn test_verify_rejects_the_root_cgroup() -> TestResult {
        // Inside a cgroup namespace every process at the namespace root
        // reads `0::/`, wherever it really is: `/` proves nothing.
        let root = CgroupVerify {
            expected_proc_path: PathBuf::from("/"),
            limits: Some(EnforcementState::Enforced),
        };
        let plan = ExecPlanV1::new("/bin/true").with_cgroup(root.clone());
        match plan.validate() {
            Err(ExecError::InvalidPlan { reason }) => {
                assert!(reason.contains("must not be `/`"), "{reason}");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert!(matches!(
            ExecPlanV1::from_json(br#"{"version":1,"cgroup":{"expected_proc_path":"/"},"sandbox_requirement":"none","program":"/bin/true"}"#),
            Err(ExecError::InvalidPlan { .. })
        ));
        // Even content that would "match" never passes the check itself.
        assert!(matches!(
            root.check("0::/\n"),
            Err(ExecError::InvalidPlan { .. })
        ));
        Ok(())
    }

    /// The exact JSON a parent wrote before `CgroupMembership::Verify`
    /// existed (cgroup as a bare `CgroupJoin` object).
    const PRE_VERIFY_PLAN: &str = concat!(
        r#"{"version":1,"#,
        r#""cgroup":{"root":"/sys/fs/cgroup/user.slice/harw","relative":"job-attempt-1","limits":"partial"},"#,
        r#""rlimits":{"limits":{"Nofile":{"soft":256,"hard":256}}},"#,
        r#""sandbox_requirement":"none","program":"/bin/sh","args":["-c","exit 7"]}"#
    );

    #[test]
    fn test_pre_verify_plan_json_still_parses_and_serializes_unchanged() -> TestResult {
        let plan = ExecPlanV1::from_json(PRE_VERIFY_PLAN.as_bytes()).map_err(ctx("parse"))?;
        assert_eq!(
            plan.cgroup,
            Some(CgroupMembership::Attach(CgroupJoin {
                root: PathBuf::from("/sys/fs/cgroup/user.slice/harw"),
                relative: "job-attempt-1".to_owned(),
                limits: Some(EnforcementState::Partial),
            }))
        );
        assert_eq!(plan.args, vec!["-c", "exit 7"]);
        // An attach still serializes to the old shape, byte for byte, so an
        // older trampoline reads plans of a newer parent.
        let json = plan.to_json().map_err(ctx("serialize"))?;
        assert_eq!(
            String::from_utf8(json).map_err(ctx("utf-8"))?,
            PRE_VERIFY_PLAN
        );

        let without_limits = br#"{"version":1,"cgroup":{"root":"/sys/fs/cgroup/harw","relative":"job"},"sandbox_requirement":"none","program":"/bin/true"}"#;
        let plan = ExecPlanV1::from_json(without_limits).map_err(ctx("parse without limits"))?;
        assert!(
            matches!(
                &plan.cgroup,
                Some(CgroupMembership::Attach(CgroupJoin { limits: None, .. }))
            ),
            "{plan:?}"
        );
        Ok(())
    }

    #[test]
    fn test_verify_plan_round_trips_and_is_unambiguous() -> TestResult {
        let verify = CgroupVerify {
            expected_proc_path: PathBuf::from("/user.slice/harw.slice/harw-job-7"),
            limits: Some(EnforcementState::Enforced),
        };
        let plan = ExecPlanV1::new("/bin/true").with_cgroup(verify.clone());
        let json = plan.to_json().map_err(ctx("serialize"))?;
        let text = String::from_utf8(json.clone()).map_err(ctx("utf-8"))?;
        assert!(
            text.contains(
                r#""cgroup":{"expected_proc_path":"/user.slice/harw.slice/harw-job-7","limits":"enforced"}"#
            ),
            "{text}"
        );
        let back = ExecPlanV1::from_json(&json).map_err(ctx("parse"))?;
        assert_eq!(back.cgroup, Some(CgroupMembership::Verify(verify)));
        assert_eq!(
            back.cgroup.as_ref().and_then(CgroupMembership::limits),
            Some(EnforcementState::Enforced)
        );

        // Fields of both forms, or neither, are never guessed.
        for cgroup in [
            r#"{"root":"/sys/fs/cgroup/harw","relative":"job","expected_proc_path":"/harw/job"}"#,
            r#"{"expected_proc_path":"/harw/job","verify":true}"#,
            r#"{"limits":"enforced"}"#,
        ] {
            let document = format!(
                r#"{{"version":1,"cgroup":{cgroup},"sandbox_requirement":"none","program":"/bin/true"}}"#
            );
            let error = expect_err(ExecPlanV1::from_json(document.as_bytes()))?;
            assert!(
                matches!(error, ExecError::PlanParse { .. }),
                "{cgroup}: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_unified_cgroup_path_parsing() {
        assert_eq!(
            unified_cgroup_path("0::/user.slice/harw.slice/harw-job-1\n"),
            Some("/user.slice/harw.slice/harw-job-1")
        );
        assert_eq!(unified_cgroup_path("0::/\n"), Some("/"));
        // Hybrid host: v1 lines are ignored, the unified line is used.
        let hybrid = "12:memory:/user.slice\n1:name=systemd:/user.slice/session-2.scope\n0::/user.slice/session-2.scope\n";
        assert_eq!(
            unified_cgroup_path(hybrid),
            Some("/user.slice/session-2.scope")
        );
        // Everything after the second colon is the path.
        assert_eq!(unified_cgroup_path("0::/odd:name"), Some("/odd:name"));
        // A v1-only host, garbage, duplicates and relative paths: no answer.
        for content in [
            "",
            "12:memory:/user.slice\n",
            "not a cgroup file",
            "0::/a\n0::/b\n",
            "0::relative\n",
            "0:cpu:/x\n",
        ] {
            assert_eq!(unified_cgroup_path(content), None, "{content:?}");
        }
    }

    #[test]
    fn test_verify_check_against_proc_content() -> TestResult {
        let verify = CgroupVerify {
            expected_proc_path: PathBuf::from("/harw.slice/harw-job-1"),
            limits: None,
        };
        verify
            .check("0::/harw.slice/harw-job-1\n")
            .map_err(ctx("same path"))?;
        verify
            .check("1:name=systemd:/x\n0::/harw.slice/harw-job-1/\n")
            .map_err(ctx("component-wise equal"))?;
        for (content, found) in [
            ("0::/harw.slice/harw-job-10\n", "/harw.slice/harw-job-10"),
            ("0::/harw.slice\n", "/harw.slice"),
            // At a cgroup-namespace root, or seen from one: never a match.
            ("0::/\n", "/"),
            (
                "0::/../harw.slice/harw-job-1\n",
                "/../harw.slice/harw-job-1",
            ),
            (
                "0::/harw.slice/harw-job-1/child\n",
                "/harw.slice/harw-job-1/child",
            ),
            (
                "12:memory:/harw.slice/harw-job-1\n",
                "no single cgroup v2 entry",
            ),
        ] {
            match verify.check(content) {
                Err(ExecError::CgroupMismatch {
                    expected,
                    found: observed,
                }) => {
                    assert_eq!(expected, verify.expected_proc_path);
                    assert!(observed.contains(found), "{observed} vs {found}");
                }
                other => {
                    return Err(TestError::Unexpected(format!("{content:?}: {other:?}")));
                }
            }
        }
        Ok(())
    }
}
