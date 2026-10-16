//! The sensor.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use harw_container_model::ContainerId;
use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use harw_types::SensorId;
use jiff::Timestamp;

/// Most scopes examined per poll.
pub const MAX_SCOPES: usize = 64;
/// Metrics per scope (the six names in the crate docs, at most five emitted).
const METRICS_PER_SCOPE: usize = 5;
/// Upper bound of samples per poll (plus the truncation flag).
pub const MAX_CARDINALITY: usize = MAX_SCOPES * METRICS_PER_SCOPE + 1;
/// Effective-capability count from which a container counts as privileged
/// (a default engine profile keeps about 14).
pub const PRIVILEGED_CAP_BITS: u32 = 32;

const SCOPE_PREFIXES: [&str; 3] = ["libpod-", "docker-", "crio-"];

/// What the sensor looks at.
#[derive(Debug, Clone)]
pub struct ContainerSensorConfig {
    /// Directory holding the container scopes (inside the read scope).
    pub cgroup_root: PathBuf,
    /// procfs root (inside the read scope).
    pub proc_root: PathBuf,
    /// The container ids this runner started; everything else is unowned.
    pub expected: Vec<ContainerId>,
}

/// The container sensor.
#[derive(Debug)]
pub struct ContainerSensor {
    handle: SensorHandle<Bound>,
    config: ContainerSensorConfig,
}

impl ContainerSensor {
    /// The capability this sensor claims.
    pub const CAPABILITY: Capability = Capability::ReadContainerScopes;

    /// Canonical sensor kind.
    pub const KIND: &'static str = "container";

    /// Creates the sensor over a bound handle.
    #[must_use]
    pub fn new(handle: SensorHandle<Bound>, config: ContainerSensorConfig) -> Self {
        Self { handle, config }
    }

    /// A handle for this sensor over `scope`.
    #[must_use]
    pub fn handle_for(id: SensorId, scope: ReadScope) -> SensorHandle<Bound> {
        SensorHandle::new(id, Self::CAPABILITY).bind(scope)
    }
}

fn map_err(error: ReadFsError) -> SensorError {
    match error {
        ReadFsError::Scope(inner) => inner,
        ReadFsError::TooLarge { .. }
        | ReadFsError::GlobPatternAbsolute { .. }
        | ReadFsError::GlobPatternTraversal { .. }
        | ReadFsError::GlobLimitExceeded { .. } => SensorError::MalformedSource,
    }
}

/// The id a scope directory name carries, if it is a container scope.
fn scope_id(path: &Path) -> Option<ContainerId> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".scope")?;
    let hex = SCOPE_PREFIXES
        .iter()
        .find_map(|prefix| stem.strip_prefix(prefix))?;
    ContainerId::new(hex).ok()
}

fn sample(id: &SensorId, now: Timestamp, metric: &str, label: &str, value: f64) -> HostSample {
    HostSample {
        sensor: id.clone(),
        observed_at: now,
        metric: Cow::Owned(format!("{metric}_{label}")),
        value,
    }
}

fn relative(root: &Path) -> Option<String> {
    root.to_str().map(|s| s.trim_start_matches('/').to_owned())
}

/// First pid of a scope; `Ok(None)` when unreadable or empty.
fn first_pid(scope: &ReadScope, dir: &Path) -> Result<Option<u32>, SensorError> {
    match harw_dod_readfs::read_first_line(scope, &dir.join("cgroup.procs")) {
        Ok(line) => Ok(line.trim().parse::<u32>().ok().filter(|pid| *pid > 0)),
        Err(ReadFsError::Scope(SensorError::Io(_))) => Ok(None),
        Err(other) => Err(map_err(other)),
    }
}

/// The hardening facts of `/proc/<pid>/status`.
struct Facts {
    cap_eff_bits: u32,
    seccomp: f64,
    no_new_privs: f64,
}

fn read_facts(scope: &ReadScope, proc_root: &Path, pid: u32) -> Result<Option<Facts>, SensorError> {
    let path = proc_root.join(pid.to_string()).join("status");
    let pairs = match harw_dod_readfs::read_key_values(scope, &path, ':') {
        Ok(pairs) => pairs,
        Err(ReadFsError::Scope(SensorError::Io(_))) => return Ok(None),
        Err(other) => return Err(map_err(other)),
    };
    let find = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let (Some(cap), Some(seccomp), Some(nnp)) =
        (find("CapEff"), find("Seccomp"), find("NoNewPrivs"))
    else {
        // A kernel that does not report a field is unverifiable, not clean.
        return Ok(None);
    };
    let (Ok(cap), Ok(seccomp), Ok(nnp)) = (
        u64::from_str_radix(cap, 16),
        seccomp.parse::<u8>(),
        nnp.parse::<u8>(),
    ) else {
        return Err(SensorError::MalformedSource);
    };
    Ok(Some(Facts {
        cap_eff_bits: cap.count_ones(),
        seccomp: f64::from(seccomp),
        no_new_privs: f64::from(nnp),
    }))
}

impl Sensor for ContainerSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let root = relative(&self.config.cgroup_root).ok_or(SensorError::MalformedSource)?;
        let found =
            harw_dod_readfs::glob::glob(scope, &format!("{root}/*.scope")).map_err(map_err)?;
        let mut scopes: Vec<(ContainerId, PathBuf)> = found
            .into_iter()
            .filter_map(|path| scope_id(&path).map(|id| (id, path)))
            .collect();
        if scopes.is_empty() {
            return Err(SensorError::SourceUnavailable);
        }
        scopes.sort_by(|a, b| a.0.cmp(&b.0));
        let truncated = scopes.len() > MAX_SCOPES;
        scopes.truncate(MAX_SCOPES);

        let sensor = self.handle.id();
        let mut samples = Vec::new();
        if truncated {
            samples.push(sample(
                sensor,
                now,
                "container_scopes_truncated",
                "all",
                1.0,
            ));
        }
        for (id, dir) in &scopes {
            let label: String = id.as_str().chars().take(12).collect();
            if !self.config.expected.contains(id) {
                samples.push(sample(sensor, now, "container_unowned", &label, 1.0));
                continue;
            }
            let facts = match first_pid(scope, dir)? {
                Some(pid) => read_facts(scope, &self.config.proc_root, pid)?,
                None => None,
            };
            let Some(facts) = facts else {
                samples.push(sample(sensor, now, "container_unverifiable", &label, 1.0));
                continue;
            };
            let privileged = f64::from(u8::from(facts.cap_eff_bits >= PRIVILEGED_CAP_BITS));
            samples.push(sample(
                sensor,
                now,
                "container_cap_eff_bits",
                &label,
                f64::from(facts.cap_eff_bits),
            ));
            samples.push(sample(
                sensor,
                now,
                "container_privileged",
                &label,
                privileged,
            ));
            samples.push(sample(
                sensor,
                now,
                "container_seccomp_mode",
                &label,
                facts.seccomp,
            ));
            samples.push(sample(
                sensor,
                now,
                "container_no_new_privs",
                &label,
                facts.no_new_privs,
            ));
        }
        Ok(SensorReading {
            samples,
            events: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
            fs::create_dir_all(dir.path().join("cg")).map_err(ctx("cg"))?;
            fs::create_dir_all(dir.path().join("proc")).map_err(ctx("proc"))?;
            Ok(Self { dir })
        }

        fn scope(
            &self,
            kind: &str,
            id: &str,
            pid: Option<u32>,
            status: Option<&str>,
        ) -> TestResult {
            let cg = self
                .dir
                .path()
                .join("cg")
                .join(format!("{kind}-{id}.scope"));
            fs::create_dir_all(&cg).map_err(ctx("scope"))?;
            if let Some(pid) = pid {
                fs::write(cg.join("cgroup.procs"), format!("{pid}\n")).map_err(ctx("procs"))?;
                if let Some(status) = status {
                    let p = self.dir.path().join("proc").join(pid.to_string());
                    fs::create_dir_all(&p).map_err(ctx("pid"))?;
                    fs::write(p.join("status"), status).map_err(ctx("status"))?;
                }
            }
            Ok(())
        }

        fn sensor(&self, expected: &[&str]) -> TestResult<ContainerSensor> {
            let scope =
                ReadScope::from_roots([self.dir.path().join("cg"), self.dir.path().join("proc")]);
            let config = ContainerSensorConfig {
                cgroup_root: self.dir.path().join("cg"),
                proc_root: self.dir.path().join("proc"),
                expected: expected
                    .iter()
                    .map(|id| ContainerId::new(id).map_err(ctx("id")))
                    .collect::<TestResult<_>>()?,
            };
            let handle = ContainerSensor::handle_for(SensorId::from_str("container-0"), scope);
            Ok(ContainerSensor::new(handle, config))
        }
    }

    fn value(reading: &SensorReading, metric: &str) -> Option<f64> {
        reading
            .samples
            .iter()
            .find(|s| s.metric == metric)
            .map(|s| s.value)
    }

    fn status(cap_eff: &str, seccomp: u8, nnp: u8) -> String {
        format!("Name:\tx\nCapEff:\t{cap_eff}\nSeccomp:\t{seccomp}\nNoNewPrivs:\t{nnp}\n")
    }

    #[test]
    fn hardened_and_privileged_containers_are_told_apart() -> TestResult {
        let fx = Fixture::new()?;
        fx.scope(
            "libpod",
            A,
            Some(100),
            Some(&status("0000000000000000", 2, 1)),
        )?;
        fx.scope(
            "docker",
            B,
            Some(200),
            Some(&status("000001ffffffffff", 0, 0)),
        )?;
        let reading = fx
            .sensor(&[A, B])?
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("poll"))?;
        assert_eq!(
            value(&reading, "container_privileged_aaaaaaaaaaaa"),
            Some(0.0)
        );
        assert_eq!(
            value(&reading, "container_seccomp_mode_aaaaaaaaaaaa"),
            Some(2.0)
        );
        assert_eq!(
            value(&reading, "container_no_new_privs_aaaaaaaaaaaa"),
            Some(1.0)
        );
        assert_eq!(
            value(&reading, "container_privileged_bbbbbbbbbbbb"),
            Some(1.0)
        );
        assert_eq!(
            value(&reading, "container_seccomp_mode_bbbbbbbbbbbb"),
            Some(0.0)
        );
        assert_eq!(
            value(&reading, "container_cap_eff_bits_bbbbbbbbbbbb"),
            Some(41.0)
        );
        Ok(())
    }

    #[test]
    fn unknown_scopes_are_unowned_and_never_read() -> TestResult {
        let fx = Fixture::new()?;
        fx.scope("libpod", A, Some(100), Some(&status("0", 2, 1)))?;
        fx.scope(
            "libpod",
            C,
            Some(300),
            Some(&status("000001ffffffffff", 0, 0)),
        )?;
        let reading = fx
            .sensor(&[A])?
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("poll"))?;
        assert_eq!(value(&reading, "container_unowned_cccccccccccc"), Some(1.0));
        assert!(value(&reading, "container_privileged_cccccccccccc").is_none());
        assert!(value(&reading, "container_privileged_aaaaaaaaaaaa").is_some());
        Ok(())
    }

    #[test]
    fn a_missing_pid_or_status_is_unverifiable_not_clean() -> TestResult {
        let fx = Fixture::new()?;
        fx.scope("libpod", A, None, None)?;
        fx.scope("libpod", B, Some(200), None)?;
        let reading = fx
            .sensor(&[A, B])?
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("poll"))?;
        assert_eq!(
            value(&reading, "container_unverifiable_aaaaaaaaaaaa"),
            Some(1.0)
        );
        assert_eq!(
            value(&reading, "container_unverifiable_bbbbbbbbbbbb"),
            Some(1.0)
        );
        assert!(value(&reading, "container_privileged_aaaaaaaaaaaa").is_none());
        Ok(())
    }

    #[test]
    fn a_status_without_the_fields_is_unverifiable_and_a_bad_one_malformed() -> TestResult {
        let fx = Fixture::new()?;
        fx.scope("libpod", A, Some(100), Some("Name:\tx\n"))?;
        let reading = fx
            .sensor(&[A])?
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("poll"))?;
        assert_eq!(
            value(&reading, "container_unverifiable_aaaaaaaaaaaa"),
            Some(1.0)
        );
        let fx = Fixture::new()?;
        fx.scope("libpod", A, Some(100), Some(&status("zz", 2, 1)))?;
        let result = fx.sensor(&[A])?.poll(Timestamp::UNIX_EPOCH);
        assert!(matches!(result, Err(SensorError::MalformedSource)));
        Ok(())
    }

    #[test]
    fn too_many_scopes_are_cut_and_flagged() -> TestResult {
        let fx = Fixture::new()?;
        for n in 0..(MAX_SCOPES + 5) {
            fx.scope("libpod", &format!("{n:064x}"), None, None)?;
        }
        let reading = fx
            .sensor(&[])?
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("poll"))?;
        assert_eq!(value(&reading, "container_scopes_truncated_all"), Some(1.0));
        assert!(reading.samples.len() <= MAX_CARDINALITY);
        Ok(())
    }

    #[test]
    fn non_container_directories_and_bad_ids_are_ignored() -> TestResult {
        let fx = Fixture::new()?;
        fs::create_dir_all(fx.dir.path().join("cg").join("user.slice")).map_err(ctx("dir"))?;
        fs::create_dir_all(fx.dir.path().join("cg").join("libpod-nothex.scope"))
            .map_err(ctx("dir"))?;
        let result = fx.sensor(&[])?.poll(Timestamp::UNIX_EPOCH);
        assert!(matches!(result, Err(SensorError::SourceUnavailable)));
        Ok(())
    }

    #[test]
    fn paths_outside_the_scope_are_refused() -> TestResult {
        let fx = Fixture::new()?;
        fx.scope("libpod", A, Some(100), Some(&status("0", 2, 1)))?;
        let other = tempfile::tempdir().map_err(ctx("other"))?;
        let scope = ReadScope::from_roots([fx.dir.path().join("cg")]);
        let config = ContainerSensorConfig {
            cgroup_root: fx.dir.path().join("cg"),
            proc_root: other.path().to_path_buf(),
            expected: vec![ContainerId::new(A).map_err(ctx("id"))?],
        };
        let sensor = ContainerSensor::new(
            ContainerSensor::handle_for(SensorId::from_str("container-0"), scope),
            config,
        );
        let reading = sensor.poll(Timestamp::UNIX_EPOCH);
        // procfs is outside the scope: it cannot be read, so it is not clean.
        match reading {
            Ok(r) => assert_eq!(value(&r, "container_unverifiable_aaaaaaaaaaaa"), Some(1.0)),
            Err(SensorError::OutsideScope) => {}
            Err(other) => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }
}
