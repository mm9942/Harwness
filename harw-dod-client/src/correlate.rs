//! Jobs ↔ findings correlation (PL-97 E4).
//!
//! A v2 export line carries `scope`, the cgroup path of the observed actor.
//! A harw job owns the cgroup `R/harw-job-<attempt>`; a finding belongs to
//! a job when its scope is that cgroup or a descendant of it, compared
//! **component by component** — `R/harw-job-1` never matches
//! `R/harw-job-10`. A leading cgroup-v2 mount point (`/sys/fs/cgroup`) is
//! ignored on both sides, so absolute and mount-relative spellings agree.
//! When job cgroups nest, a finding is attributed to the most specific job
//! only.
//!
//! Status: the client half of E4 landed early with D2 (this module and its
//! prefix tests). E4 itself adds `impl JobCgroup` for the TUI job rows, the
//! `job_id` filter and the badge, and re-checks the [`CGROUP_MOUNT`]
//! stripping once E3 has fixed the canonical cgroup-root spelling.

use serde::{Deserialize, Serialize};

use crate::findings::FindingLine;

/// The conventional cgroup-v2 mount point, stripped before comparing.
pub const CGROUP_MOUNT: &str = "/sys/fs/cgroup";

/// A job as seen by the correlator. Implemented by [`JobScope`]; consumers
/// (e.g. the TUI job rows) can implement it for their own row type.
pub trait JobCgroup {
    /// Stable job id.
    fn job_id(&self) -> &str;
    /// The job's cgroup path, if the runtime placed it in one.
    fn cgroup_path(&self) -> Option<&str>;
}

/// A minimal owned job description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobScope {
    /// Stable job id.
    pub job_id: String,
    /// The job's cgroup path.
    pub cgroup_path: Option<String>,
}

impl JobCgroup for JobScope {
    fn job_id(&self) -> &str {
        &self.job_id
    }

    fn cgroup_path(&self) -> Option<&str> {
        self.cgroup_path.as_deref()
    }
}

/// The findings attributed to one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFindings {
    /// Job id.
    pub job_id: String,
    /// Indices into the `findings` slice passed to [`correlate`], in order.
    pub findings: Vec<usize>,
    /// How many of them are `high` or `critical`.
    pub high_or_critical: u32,
}

/// Attributes findings to jobs by path-component prefix of the job cgroup.
///
/// Returns one entry per job with at least one finding, in job order.
/// Findings without `scope` and jobs without a cgroup path are ignored.
#[must_use]
pub fn correlate<J: JobCgroup>(findings: &[FindingLine], jobs: &[J]) -> Vec<JobFindings> {
    let job_components: Vec<Option<Vec<&str>>> = jobs
        .iter()
        .map(|job| {
            job.cgroup_path()
                .and_then(components)
                .filter(|parts| !parts.is_empty())
        })
        .collect();
    let mut hits: Vec<JobFindings> = jobs
        .iter()
        .map(|job| JobFindings {
            job_id: job.job_id().to_owned(),
            findings: Vec::new(),
            high_or_critical: 0,
        })
        .collect();
    for (index, finding) in findings.iter().enumerate() {
        let Some(scope) = finding.scope.as_deref().and_then(components) else {
            continue;
        };
        let best = job_components
            .iter()
            .enumerate()
            .filter_map(|(job, parts)| parts.as_ref().map(|parts| (job, parts)))
            .filter(|(_, parts)| scope.starts_with(parts))
            .max_by_key(|(_, parts)| parts.len())
            .map(|(job, _)| job);
        if let Some(entry) = best.and_then(|job| hits.get_mut(job)) {
            entry.findings.push(index);
            if finding.is_high_or_critical() {
                entry.high_or_critical = entry.high_or_critical.saturating_add(1);
            }
        }
    }
    hits.retain(|entry| !entry.findings.is_empty());
    hits
}

/// `true` when `scope` is `job_cgroup` or one of its descendants
/// (component-wise, mount point ignored).
#[must_use]
pub fn scope_within(job_cgroup: &str, scope: &str) -> bool {
    match (components(job_cgroup), components(scope)) {
        (Some(job), Some(scope)) => !job.is_empty() && scope.starts_with(&job),
        _ => false,
    }
}

/// Splits a cgroup path into its components, without the mount point.
/// `None` for paths containing `..` (never resolved, never matched).
fn components(path: &str) -> Option<Vec<&str>> {
    let mut parts: Vec<&str> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.contains(&"..") {
        return None;
    }
    let mount: Vec<&str> = CGROUP_MOUNT.split('/').filter(|p| !p.is_empty()).collect();
    if path.starts_with('/') && parts.starts_with(&mount) {
        parts.drain(..mount.len());
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::parse_finding_line;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn finding(id: &str, severity: &str, scope: Option<&str>) -> Result<FindingLine, String> {
        let scope = scope.map_or_else(String::new, |scope| format!(r#","scope":"{scope}""#));
        parse_finding_line(&format!(
            r#"{{"schema_version":2,"finding_id":"{id}","host":"h","rule_id":"r","severity":"{severity}","summary":"s","observed_at":"2026-10-28T10:00:00Z"{scope}}}"#
        ))
        .ok_or_else(|| format!("fixture {id} must parse"))
    }

    fn job(id: &str, cgroup: Option<&str>) -> JobScope {
        JobScope {
            job_id: id.to_owned(),
            cgroup_path: cgroup.map(str::to_owned),
        }
    }

    #[test]
    fn prefix_is_component_wise_never_substring() {
        assert!(scope_within(
            "/harw.slice/harw-job-1",
            "/harw.slice/harw-job-1"
        ));
        assert!(scope_within(
            "/harw.slice/harw-job-1",
            "/harw.slice/harw-job-1/sub/leaf"
        ));
        assert!(!scope_within(
            "/harw.slice/harw-job-1",
            "/harw.slice/harw-job-10"
        ));
        assert!(!scope_within("/harw.slice/harw-job-1", "/harw.slice"));
        assert!(!scope_within("/", "/harw.slice/harw-job-1"));
        assert!(!scope_within(
            "/harw.slice/harw-job-1",
            "/harw.slice/harw-job-1/../harw-job-2"
        ));
        // Mount point and trailing slashes do not matter.
        assert!(scope_within(
            "/sys/fs/cgroup/harw.slice/harw-job-1",
            "/harw.slice/harw-job-1/x"
        ));
        assert!(scope_within(
            "/harw.slice/harw-job-1/",
            "/sys/fs/cgroup/harw.slice/harw-job-1"
        ));
    }

    #[test]
    fn findings_are_attributed_to_the_most_specific_job() -> TestResult {
        let findings = vec![
            finding("f0", "high", Some("/harw.slice/harw-job-1"))?,
            finding("f1", "low", Some("/harw.slice/harw-job-10/child"))?,
            finding(
                "f2",
                "critical",
                Some("/sys/fs/cgroup/harw.slice/harw-job-1/x"),
            )?,
            finding("f3", "high", None)?,
            finding("f4", "medium", Some("/system.slice/sshd.service"))?,
            finding(
                "f5",
                "high",
                Some("/harw.slice/harw-job-2/nested/harw-job-3"),
            )?,
        ];
        let jobs = vec![
            job("job-1", Some("/sys/fs/cgroup/harw.slice/harw-job-1")),
            job("job-10", Some("/harw.slice/harw-job-10")),
            job("job-2", Some("/harw.slice/harw-job-2")),
            job("job-3", Some("/harw.slice/harw-job-2/nested/harw-job-3")),
            job("job-none", None),
        ];
        let hits = correlate(&findings, &jobs);
        assert_eq!(
            hits,
            vec![
                JobFindings {
                    job_id: "job-1".into(),
                    findings: vec![0, 2],
                    high_or_critical: 2
                },
                JobFindings {
                    job_id: "job-10".into(),
                    findings: vec![1],
                    high_or_critical: 0
                },
                JobFindings {
                    job_id: "job-3".into(),
                    findings: vec![5],
                    high_or_critical: 1
                },
            ]
        );
        assert!(correlate::<JobScope>(&findings, &[]).is_empty());
        Ok(())
    }
}
