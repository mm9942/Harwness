//! Read-back verification: compare what was asked with what the engine
//! reports after the container exists.
//!
//! A requested flag proves nothing. The engine may ignore it (a missing
//! cgroup controller, an old version) or another layer may override it. The
//! tool layer inspects the container, converts the JSON into
//! [`InspectFacts`], and [`verify`] answers per dimension. A fact the engine
//! did not report is [`Enforcement::Unverifiable`], never `Enforced`.

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
    /// Memory limit at or below the profile ceiling.
    Memory,
    /// Process limit at or below the profile ceiling.
    Pids,
    /// Workspace mounted with the access the profile demands.
    Workspace,
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

/// Facts read from the engine's inspect output. `None` means "not reported".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InspectFacts {
    /// Privileged mode.
    pub privileged: Option<bool>,
    /// Added capabilities.
    pub cap_add: Option<Vec<String>>,
    /// Dropped capabilities.
    pub cap_drop: Option<Vec<String>>,
    /// Network mode.
    pub network_mode: Option<String>,
    /// Read-only root filesystem.
    pub read_only_rootfs: Option<bool>,
    /// Security options (`no-new-privileges`, ...).
    pub security_opt: Option<Vec<String>>,
    /// Memory limit in bytes (`0` = unlimited).
    pub memory_bytes: Option<i64>,
    /// Process limit (`0` or `-1` = unlimited).
    pub pids_limit: Option<i64>,
    /// Whether the workspace mount is read-only.
    pub workspace_read_only: Option<bool>,
}

/// Per-dimension result of [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readback {
    entries: Vec<(Dimension, Enforcement)>,
}

impl Readback {
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
        self.entries.iter().all(|(_, e)| *e == Enforcement::Enforced)
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
    let capabilities = match &facts.cap_drop {
        Some(drop) if drop.iter().any(|c| c.eq_ignore_ascii_case("all")) => Enforcement::Enforced,
        Some(_) => Enforcement::NotEnforced,
        None => Enforcement::Unverifiable,
    };
    let no_new_privileges = match &facts.security_opt {
        Some(opts) if opts.iter().any(|o| is_no_new_privileges(o)) => Enforcement::Enforced,
        Some(_) => Enforcement::NotEnforced,
        None => Enforcement::Unverifiable,
    };
    let workspace = match facts.workspace_read_only {
        Some(ro) if ro == expected.workspace_read_only => Enforcement::Enforced,
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
            (
                Dimension::Memory,
                limit(facts.memory_bytes, expected.memory_bytes),
            ),
            (Dimension::Pids, limit(facts.pids_limit, expected.pids_limit)),
            (Dimension::Workspace, workspace),
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
    use crate::test_support::{TestResult, ensure};

    fn expected(ro: bool) -> Expected {
        Expected {
            workspace_read_only: ro,
            memory_bytes: 1024 * 1024 * 1024,
            pids_limit: 256,
        }
    }

    fn good() -> InspectFacts {
        InspectFacts {
            privileged: Some(false),
            cap_add: Some(vec![]),
            cap_drop: Some(vec!["ALL".to_owned()]),
            network_mode: Some("none".to_owned()),
            read_only_rootfs: Some(true),
            security_opt: Some(vec!["no-new-privileges".to_owned()]),
            memory_bytes: Some(1024 * 1024 * 1024),
            pids_limit: Some(256),
            workspace_read_only: Some(true),
        }
    }

    #[test]
    fn matching_facts_are_all_enforced() -> TestResult {
        let r = verify(&expected(true), &good());
        ensure(r.all_enforced(), "all enforced")?;
        ensure(r.failures().is_empty(), "no failures")?;
        ensure(r.entries().len() == 8, "eight dimensions")
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
            Dimension::Workspace,
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
        f.security_opt = Some(vec![]);
        f.memory_bytes = Some(0);
        f.pids_limit = Some(-1);
        f.workspace_read_only = Some(false);
        let r = verify(&expected(true), &f);
        ensure(r.failures().len() == 8, "all eight fail")?;
        ensure(
            r.get(Dimension::Network) == Some(Enforcement::NotEnforced),
            "network",
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
        ensure(verify(&expected(true), &f).all_enforced(), "stricter is fine")?;
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
    fn workspace_access_must_match_the_profile() -> TestResult {
        let f = good();
        ensure(
            verify(&expected(false), &f).get(Dimension::Workspace) == Some(Enforcement::NotEnforced),
            "ro mount where rw expected is a mismatch",
        )?;
        let mut rw = good();
        rw.workspace_read_only = Some(false);
        ensure(
            verify(&expected(false), &rw).get(Dimension::Workspace) == Some(Enforcement::Enforced),
            "rw matches rw",
        )
    }

    #[test]
    fn no_new_privileges_spellings() -> TestResult {
        for ok in ["no-new-privileges", "no-new-privileges=true", "no-new-privileges:true"] {
            ensure(is_no_new_privileges(ok), ok)?;
        }
        for bad in ["no-new-privileges=false", "seccomp=unconfined", ""] {
            ensure(!is_no_new_privileges(bad), bad)?;
        }
        Ok(())
    }
}
