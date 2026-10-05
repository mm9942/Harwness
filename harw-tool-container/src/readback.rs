//! Read-back verification: compare what was asked with what the engine
//! reports after the container exists.
//!
//! A requested flag proves nothing. The engine may ignore it (a missing
//! cgroup controller, an old version) or another layer may override it. The
//! tool layer inspects the container, converts the JSON into
//! [`InspectFacts`], and [`verify`] answers per dimension. A fact the engine
//! did not report is [`Enforcement::Unverifiable`], never `Enforced`.
//!
//! Field names were checked against `podman inspect` of a running container
//! (Podman 4.9.3): `HostConfig.{NetworkMode, ReadonlyRootfs, CapAdd, CapDrop,
//! SecurityOpt, Memory, MemorySwap, PidsLimit, Privileged}`, top-level
//! `EffectiveCaps`/`BoundingCaps`, `Mounts[].{Destination, RW}`,
//! `Config.Timeout` and `Config.Labels`. Docker's inspect output is not covered.

use crate::mount::MountKind;
use crate::plan::Expected;

/// A security-relevant property of a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Dimension {
    /// Not privileged and no capability added.
    Privilege,
    /// Network mode `none`.
    Network,
    /// Read-only root filesystem.
    RootFs,
    /// All capabilities dropped.
    Capabilities,
    /// `no-new-privileges`.
    NoNewPrivileges,
    /// Memory and memory-plus-swap at or below the profile ceiling.
    Memory,
    /// Process limit at or below the profile ceiling.
    Pids,
    /// Exactly the planned mounts, each with its source and access, and no
    /// other.
    Mounts,
    /// Wall-time limit at or below the profile ceiling (`Config.Timeout`).
    Timeout,
    /// The inspected container is the one `create` returned.
    Identity,
}

/// Outcome of one dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// The engine reports the requested property.
    Enforced,
    /// The engine reports something weaker.
    NotEnforced,
    /// The engine did not report the property.
    Unverifiable,
}

/// One mount as the engine reports it (inspect `Mounts[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedMount {
    /// `Type`: `bind`, `volume`, ...
    pub kind: String,
    /// `Name` of a volume.
    pub name: Option<String>,
    /// `Source`: the host path of a bind (a volume's is its data directory).
    pub source: Option<String>,
    /// `Destination` inside the container.
    pub destination: String,
    /// `RW`: writable.
    pub rw: bool,
}

/// Facts read from the engine's inspect output. `None` means "not reported".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InspectFacts {
    /// The container id the engine reports (inspect `Id`).
    pub id: Option<String>,
    /// Privileged mode.
    pub privileged: Option<bool>,
    /// Added capabilities.
    pub cap_add: Option<Vec<String>>,
    /// Dropped capabilities as the engine lists them. This is the requested
    /// configuration, never proof (Podman also expands `--cap-drop=all` into
    /// the explicit default set); it is not used for the verdict, only
    /// [`Self::effective_caps`] is.
    pub cap_drop: Option<Vec<String>>,
    /// Capabilities the container's init process actually has (Podman:
    /// inspect `EffectiveCaps`). `Some(empty)` means none. Podman 4.9.3
    /// renders an empty set as JSON `null`, so the parse layer must map a
    /// *present* `null` to `Some(empty)` and an *absent* key to `None`.
    pub effective_caps: Option<Vec<String>>,
    /// Network mode.
    pub network_mode: Option<String>,
    /// Read-only root filesystem.
    pub read_only_rootfs: Option<bool>,
    /// Security options (`no-new-privileges`, ...).
    pub security_opt: Option<Vec<String>>,
    /// Memory limit in bytes (`0` = unlimited).
    pub memory_bytes: Option<i64>,
    /// Memory plus swap limit in bytes. Podman defaults it to twice the
    /// memory limit, so a plain `--memory` doubles the real ceiling.
    pub memory_swap_bytes: Option<i64>,
    /// Process limit (`0` or `-1` = unlimited).
    pub pids_limit: Option<i64>,
    /// Every mount the engine reports (inspect `Mounts[]`).
    pub mounts: Option<Vec<ObservedMount>>,
    /// Wall-time limit in seconds the engine applied (Podman: inspect
    /// `Config.Timeout`). `Some(0)` means no limit.
    pub timeout_s: Option<u32>,
}

/// Per-dimension result of [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readback {
    entries: Vec<(Dimension, Enforcement)>,
}

impl Readback {
    /// Adds one more dimension (used for facts only the plan can judge).
    pub(crate) fn with_entry(mut self, dimension: Dimension, enforcement: Enforcement) -> Self {
        self.entries.push((dimension, enforcement));
        self
    }

    /// All entries in a fixed order.
    #[must_use]
    pub fn entries(&self) -> &[(Dimension, Enforcement)] {
        &self.entries
    }

    /// Outcome of one dimension.
    #[must_use]
    pub fn get(&self, dimension: Dimension) -> Option<Enforcement> {
        self.entries
            .iter()
            .find(|(d, _)| *d == dimension)
            .map(|(_, e)| *e)
    }

    /// `true` only when every dimension is [`Enforcement::Enforced`].
    #[must_use]
    pub fn all_enforced(&self) -> bool {
        self.entries
            .iter()
            .all(|(_, e)| *e == Enforcement::Enforced)
    }

    /// Dimensions that are not enforced (weaker or unknown).
    #[must_use]
    pub fn failures(&self) -> Vec<Dimension> {
        self.entries
            .iter()
            .filter(|(_, e)| *e != Enforcement::Enforced)
            .map(|(d, _)| *d)
            .collect()
    }
}

fn tri(value: Option<bool>) -> Enforcement {
    match value {
        Some(true) => Enforcement::Enforced,
        Some(false) => Enforcement::NotEnforced,
        None => Enforcement::Unverifiable,
    }
}

fn limit(reported: Option<i64>, ceiling: i64) -> Enforcement {
    match reported {
        Some(v) if v > 0 && v <= ceiling => Enforcement::Enforced,
        Some(_) => Enforcement::NotEnforced,
        None => Enforcement::Unverifiable,
    }
}

/// The reported mounts must be exactly the planned ones: every expected
/// destination once, with the planned kind, source (path, or volume name)
/// and access, and nothing else. An unexpected host mount, or a mount that is
/// writable although the plan asked for read-only, fails.
fn verify_mounts(expected: &Expected, observed: Option<&[ObservedMount]>) -> Enforcement {
    let Some(observed) = observed else {
        return Enforcement::Unverifiable;
    };
    if observed.len() != expected.mounts.len() {
        return Enforcement::NotEnforced;
    }
    for want in &expected.mounts {
        let mut at = observed
            .iter()
            .filter(|o| o.destination == want.destination);
        let (Some(got), None) = (at.next(), at.next()) else {
            return Enforcement::NotEnforced;
        };
        let kind_ok = match want.kind {
            MountKind::Bind => got.kind == "bind" && got.source.as_deref() == Some(&want.source),
            MountKind::Volume => got.kind == "volume" && got.name.as_deref() == Some(&want.source),
        };
        if !kind_ok || got.rw == want.read_only {
            return Enforcement::NotEnforced;
        }
    }
    Enforcement::Enforced
}

/// Compares the plan's expectations with the engine's facts.
#[must_use]
pub fn verify(expected: &Expected, facts: &InspectFacts) -> Readback {
    let privilege = match (&facts.privileged, &facts.cap_add) {
        (Some(false), Some(add)) if add.is_empty() => Enforcement::Enforced,
        (Some(true), _) => Enforcement::NotEnforced,
        (_, Some(add)) if !add.is_empty() => Enforcement::NotEnforced,
        _ => Enforcement::Unverifiable,
    };
    let network = match facts.network_mode.as_deref() {
        Some("none") => Enforcement::Enforced,
        Some(_) => Enforcement::NotEnforced,
        None => Enforcement::Unverifiable,
    };
    // Only the effective set is proof. `CapDrop` is the requested host
    // configuration: if the runtime ignored the flag it would still read
    // `["ALL"]`, so without an effective-capability observation the result
    // stays unverifiable (fail closed), whatever the drop list says.
    let capabilities = match &facts.effective_caps {
        Some(caps) if caps.is_empty() => Enforcement::Enforced,
        Some(_) => Enforcement::NotEnforced,
        None => Enforcement::Unverifiable,
    };
    let no_new_privileges = match &facts.security_opt {
        Some(opts) if opts.iter().any(|o| is_no_new_privileges(o)) => Enforcement::Enforced,
        Some(_) => Enforcement::NotEnforced,
        None => Enforcement::Unverifiable,
    };
    let memory = match (
        limit(facts.memory_bytes, expected.memory_bytes),
        limit(facts.memory_swap_bytes, expected.memory_bytes),
    ) {
        (Enforcement::Enforced, Enforcement::Enforced) => Enforcement::Enforced,
        (Enforcement::NotEnforced, _) | (_, Enforcement::NotEnforced) => Enforcement::NotEnforced,
        _ => Enforcement::Unverifiable,
    };
    let mounts = verify_mounts(expected, facts.mounts.as_deref());
    let timeout = match facts.timeout_s {
        Some(t) if t > 0 && t <= expected.timeout_s => Enforcement::Enforced,
        Some(_) => Enforcement::NotEnforced,
        None => Enforcement::Unverifiable,
    };
    Readback {
        entries: vec![
            (Dimension::Privilege, privilege),
            (Dimension::Network, network),
            (Dimension::RootFs, tri(facts.read_only_rootfs)),
            (Dimension::Capabilities, capabilities),
            (Dimension::NoNewPrivileges, no_new_privileges),
            (Dimension::Memory, memory),
            (
                Dimension::Pids,
                limit(facts.pids_limit, expected.pids_limit),
            ),
            (Dimension::Mounts, mounts),
            (Dimension::Timeout, timeout),
        ],
    }
}

/// `no-new-privileges`, `no-new-privileges=true` or `no-new-privileges:true`.
fn is_no_new_privileges(option: &str) -> bool {
    matches!(
        option,
        "no-new-privileges" | "no-new-privileges=true" | "no-new-privileges:true"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mount::ExpectedMount;
    use crate::test_support::{TestResult, ensure};

    fn workspace(ro: bool) -> ExpectedMount {
        ExpectedMount {
            kind: MountKind::Bind,
            source: "/srv/ws".to_owned(),
            destination: "/workspace".to_owned(),
            read_only: ro,
        }
    }

    fn seen_bind(src: &str, dst: &str, rw: bool) -> ObservedMount {
        ObservedMount {
            kind: "bind".to_owned(),
            name: None,
            source: Some(src.to_owned()),
            destination: dst.to_owned(),
            rw,
        }
    }

    fn expected(ro: bool) -> Expected {
        Expected {
            mounts: vec![workspace(ro)],
            memory_bytes: 1024 * 1024 * 1024,
            pids_limit: 256,
            timeout_s: 300,
        }
    }

    fn good() -> InspectFacts {
        InspectFacts {
            id: None,
            privileged: Some(false),
            cap_add: Some(vec![]),
            cap_drop: Some(vec!["ALL".to_owned()]),
            effective_caps: Some(vec![]),
            network_mode: Some("none".to_owned()),
            read_only_rootfs: Some(true),
            security_opt: Some(vec!["no-new-privileges".to_owned()]),
            memory_bytes: Some(1024 * 1024 * 1024),
            memory_swap_bytes: Some(1024 * 1024 * 1024),
            pids_limit: Some(256),
            mounts: Some(vec![seen_bind("/srv/ws", "/workspace", false)]),
            timeout_s: Some(300),
        }
    }

    #[test]
    fn matching_facts_are_all_enforced() -> TestResult {
        let r = verify(&expected(true), &good());
        ensure(r.all_enforced(), "all enforced")?;
        ensure(r.failures().is_empty(), "no failures")?;
        ensure(r.entries().len() == 9, "nine dimensions")
    }

    #[test]
    fn missing_facts_are_unverifiable_not_enforced() -> TestResult {
        let r = verify(&expected(true), &InspectFacts::default());
        ensure(!r.all_enforced(), "not enforced")?;
        for d in [
            Dimension::Privilege,
            Dimension::Network,
            Dimension::RootFs,
            Dimension::Capabilities,
            Dimension::NoNewPrivileges,
            Dimension::Memory,
            Dimension::Pids,
            Dimension::Mounts,
        ] {
            ensure(r.get(d) == Some(Enforcement::Unverifiable), "unverifiable")?;
        }
        Ok(())
    }

    #[test]
    fn weaker_facts_are_reported_per_dimension() -> TestResult {
        let mut f = good();
        f.privileged = Some(true);
        f.network_mode = Some("host".to_owned());
        f.read_only_rootfs = Some(false);
        f.cap_drop = Some(vec!["NET_RAW".to_owned()]);
        f.effective_caps = Some(vec!["CAP_NET_RAW".to_owned()]);
        f.security_opt = Some(vec![]);
        f.memory_bytes = Some(0);
        f.pids_limit = Some(-1);
        f.mounts = Some(vec![seen_bind("/etc", "/workspace", true)]);
        f.timeout_s = Some(0);
        let r = verify(&expected(true), &f);
        ensure(r.failures().len() == 9, "all nine fail")?;
        ensure(
            r.get(Dimension::Network) == Some(Enforcement::NotEnforced),
            "network",
        )
    }

    #[test]
    fn a_missing_or_overlong_timeout_fails_closed() -> TestResult {
        let mut f = good();
        f.timeout_s = None;
        ensure(
            verify(&expected(true), &f).get(Dimension::Timeout) == Some(Enforcement::Unverifiable),
            "not reported",
        )?;
        f.timeout_s = Some(0);
        ensure(
            verify(&expected(true), &f).get(Dimension::Timeout) == Some(Enforcement::NotEnforced),
            "no limit",
        )?;
        f.timeout_s = Some(301);
        ensure(
            verify(&expected(true), &f).get(Dimension::Timeout) == Some(Enforcement::NotEnforced),
            "above the ceiling",
        )?;
        f.timeout_s = Some(60);
        ensure(
            verify(&expected(true), &f).get(Dimension::Timeout) == Some(Enforcement::Enforced),
            "below the ceiling",
        )
    }

    #[test]
    fn an_added_capability_defeats_privilege() -> TestResult {
        let mut f = good();
        f.cap_add = Some(vec!["SYS_ADMIN".to_owned()]);
        let r = verify(&expected(true), &f);
        ensure(
            r.get(Dimension::Privilege) == Some(Enforcement::NotEnforced),
            "cap_add",
        )
    }

    #[test]
    fn limits_may_be_stricter_but_never_looser() -> TestResult {
        let mut f = good();
        f.memory_bytes = Some(512 * 1024 * 1024);
        f.pids_limit = Some(100);
        ensure(
            verify(&expected(true), &f).all_enforced(),
            "stricter is fine",
        )?;
        f.memory_bytes = Some(2 * 1024 * 1024 * 1024);
        f.pids_limit = Some(257);
        let r = verify(&expected(true), &f);
        ensure(
            r.get(Dimension::Memory) == Some(Enforcement::NotEnforced)
                && r.get(Dimension::Pids) == Some(Enforcement::NotEnforced),
            "looser fails",
        )
    }

    #[test]
    fn podman_style_cap_drop_list_is_not_proof_but_empty_effective_caps_is() -> TestResult {
        // Real Podman 4.9.3 reports `--cap-drop=all` as the default set, not "ALL".
        let podman_list: Vec<String> = ["CAP_CHOWN", "CAP_KILL", "CAP_SETUID"]
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
        let mut f = good();
        f.cap_drop = Some(podman_list.clone());
        f.effective_caps = Some(vec![]);
        ensure(
            verify(&expected(true), &f).get(Dimension::Capabilities) == Some(Enforcement::Enforced),
            "empty effective set is the proof",
        )?;
        f.effective_caps = None;
        ensure(
            verify(&expected(true), &f).get(Dimension::Capabilities)
                == Some(Enforcement::Unverifiable),
            "a default-set list without the effective set is unverifiable",
        )?;
        f.cap_drop = Some(vec!["ALL".to_owned()]);
        ensure(
            verify(&expected(true), &f).get(Dimension::Capabilities)
                == Some(Enforcement::Unverifiable),
            "a requested ALL is not proof without the effective set",
        )?;
        ensure(
            !verify(&expected(true), &f).all_enforced(),
            "so the readback is not all enforced",
        )
    }

    #[test]
    fn swap_must_not_exceed_the_memory_ceiling() -> TestResult {
        let mut f = good();
        f.memory_swap_bytes = Some(2 * 1024 * 1024 * 1024);
        ensure(
            verify(&expected(true), &f).get(Dimension::Memory) == Some(Enforcement::NotEnforced),
            "podman default of twice the memory",
        )?;
        f.memory_swap_bytes = None;
        ensure(
            verify(&expected(true), &f).get(Dimension::Memory) == Some(Enforcement::Unverifiable),
            "unknown swap",
        )
    }

    #[test]
    fn mount_access_must_match_the_plan() -> TestResult {
        let f = good();
        ensure(
            verify(&expected(false), &f).get(Dimension::Mounts) == Some(Enforcement::NotEnforced),
            "ro mount where rw expected is a mismatch",
        )?;
        let mut rw = good();
        rw.mounts = Some(vec![seen_bind("/srv/ws", "/workspace", true)]);
        ensure(
            verify(&expected(false), &rw).get(Dimension::Mounts) == Some(Enforcement::Enforced),
            "rw matches rw",
        )?;
        ensure(
            verify(&expected(true), &rw).get(Dimension::Mounts) == Some(Enforcement::NotEnforced),
            "a writable mount where read-only was planned",
        )
    }

    #[test]
    fn an_unexpected_wrong_or_missing_mount_fails() -> TestResult {
        let mut f = good();
        f.mounts = Some(vec![
            seen_bind("/srv/ws", "/workspace", false),
            seen_bind("/home/u", "/data", true),
        ]);
        ensure(
            verify(&expected(true), &f).get(Dimension::Mounts) == Some(Enforcement::NotEnforced),
            "an extra host mount",
        )?;
        f.mounts = Some(vec![seen_bind("/srv/elsewhere", "/workspace", false)]);
        ensure(
            verify(&expected(true), &f).get(Dimension::Mounts) == Some(Enforcement::NotEnforced),
            "the right destination from another host path",
        )?;
        f.mounts = Some(vec![]);
        ensure(
            verify(&expected(true), &f).get(Dimension::Mounts) == Some(Enforcement::NotEnforced),
            "the planned mount is missing",
        )?;
        f.mounts = None;
        ensure(
            verify(&expected(true), &f).get(Dimension::Mounts) == Some(Enforcement::Unverifiable),
            "not reported",
        )
    }

    #[test]
    fn a_volume_is_matched_by_name_and_two_mounts_may_not_share_a_destination() -> TestResult {
        let mut want = expected(true);
        want.mounts.push(ExpectedMount {
            kind: MountKind::Volume,
            source: "harw-cache".to_owned(),
            destination: "/cache".to_owned(),
            read_only: false,
        });
        let volume = |name: &str, rw: bool| ObservedMount {
            kind: "volume".to_owned(),
            name: Some(name.to_owned()),
            source: Some("/var/lib/containers/storage/volumes/x/_data".to_owned()),
            destination: "/cache".to_owned(),
            rw,
        };
        let mut f = good();
        f.mounts = Some(vec![
            seen_bind("/srv/ws", "/workspace", false),
            volume("harw-cache", true),
        ]);
        ensure(
            verify(&want, &f).get(Dimension::Mounts) == Some(Enforcement::Enforced),
            "the volume by name",
        )?;
        f.mounts = Some(vec![
            seen_bind("/srv/ws", "/workspace", false),
            volume("someone-elses", true),
        ]);
        ensure(
            verify(&want, &f).get(Dimension::Mounts) == Some(Enforcement::NotEnforced),
            "another volume under the planned destination",
        )?;
        f.mounts = Some(vec![
            seen_bind("/srv/ws", "/workspace", false),
            seen_bind("/srv/ws", "/workspace", false),
        ]);
        ensure(
            verify(&expected(true), &f).get(Dimension::Mounts) == Some(Enforcement::NotEnforced),
            "the same destination twice",
        )
    }

    #[test]
    fn no_new_privileges_spellings() -> TestResult {
        for ok in [
            "no-new-privileges",
            "no-new-privileges=true",
            "no-new-privileges:true",
        ] {
            ensure(is_no_new_privileges(ok), ok)?;
        }
        for bad in ["no-new-privileges=false", "seccomp=unconfined", ""] {
            ensure(!is_no_new_privileges(bad), bad)?;
        }
        Ok(())
    }
}
