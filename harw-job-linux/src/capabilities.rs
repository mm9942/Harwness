//! Linux capability reduction through rustix (Job-Runtime-Doc §4.2: no
//! second capability crate).
//!
//! # Scope: the calling thread
//! Capability sets, the bounding set and `NO_NEW_PRIVS` are **per thread**
//! on Linux. [`drop_capabilities`] changes the calling thread only; a job
//! inherits them from the thread that calls `execve`. The intended caller is
//! a single-threaded trampoline right before `exec`.
//!
//! # What "kept" means
//! Kept capabilities are merely *not dropped*. For a non-root process,
//! permitted capabilities do not survive `execve` unless they are in the
//! ambient set or granted by file capabilities; this crate never raises
//! ambient capabilities (it clears the ambient set).

use harw_job_core::EnforcementState;
use rustix::thread::{
    CapabilitySet, CapabilitySets, capabilities, capability_is_in_bounding_set,
    clear_ambient_capability_set, remove_capability_from_bounding_set, set_capabilities,
};

use crate::error::{SandboxComponent, SandboxError};

/// Parses a capability name (`CAP_NET_BIND_SERVICE`, `net_bind_service`).
///
/// # Errors
/// [`SandboxError::UnknownCapability`].
pub fn parse_capability(name: &str) -> Result<CapabilityName, SandboxError> {
    let upper = name.trim().to_ascii_uppercase();
    let bare = upper.strip_prefix("CAP_").unwrap_or(&upper);
    CapabilitySet::all()
        .iter_names()
        .find(|(flag_name, _)| *flag_name == bare)
        .map(|(flag_name, _)| CapabilityName(flag_name))
        .ok_or_else(|| SandboxError::UnknownCapability {
            name: name.to_owned(),
        })
}

/// A validated capability name (canonical form without `CAP_` prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapabilityName(&'static str);

impl CapabilityName {
    /// Canonical name, e.g. `NET_BIND_SERVICE`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.0
    }

    fn to_set(self) -> CapabilitySet {
        CapabilitySet::from_name(self.0).unwrap_or(CapabilitySet::empty())
    }
}

/// Validates all names in `keep` (no side effects).
///
/// # Errors
/// [`SandboxError::UnknownCapability`] for the first unknown name.
pub fn parse_keep_list(keep: &[String]) -> Result<Vec<CapabilityName>, SandboxError> {
    keep.iter().map(|name| parse_capability(name)).collect()
}

/// Names of the calling thread's effective capabilities (diagnostics).
///
/// # Errors
/// [`SandboxError::Os`] if `capget` fails.
pub fn effective_capabilities() -> Result<Vec<CapabilityName>, SandboxError> {
    let sets = capabilities(None).map_err(|errno| SandboxError::Os {
        operation: "capget",
        errno: errno.raw_os_error(),
    })?;
    Ok(sets
        .effective
        .iter_names()
        .map(|(name, _)| CapabilityName(name))
        .collect())
}

/// Drops every capability not in `keep` from the calling thread: ambient
/// set cleared, bounding set reduced, effective/permitted/inheritable masked.
///
/// Returns [`EnforcementState::Enforced`] when all sets were reduced. If the
/// bounding set could not be reduced (no `CAP_SETPCAP`, the normal case for
/// unprivileged processes) the result is still `Enforced` when
/// `no_new_privs_enforced` is true — `NO_NEW_PRIVS` stops `execve` from
/// gaining capabilities through setuid or file capabilities, which is all
/// the bounding set would add — and [`EnforcementState::Partial`] otherwise.
///
/// # Errors
/// [`SandboxError::UnknownCapability`] before any change;
/// [`SandboxError::PermissionDenied`] / [`SandboxError::Os`] if `capset`
/// fails.
pub fn drop_capabilities(
    keep: &[String],
    no_new_privs_enforced: bool,
) -> Result<EnforcementState, SandboxError> {
    let keep_set = parse_keep_list(keep)?
        .into_iter()
        .fold(CapabilitySet::empty(), |acc, name| acc | name.to_set());

    // Ambient set: EINVAL on kernels before 4.3 (no ambient set to clear).
    if let Err(errno) = clear_ambient_capability_set() {
        if errno != rustix::io::Errno::INVAL {
            return Err(SandboxError::Os {
                operation: "PR_CAP_AMBIENT_CLEAR_ALL",
                errno: errno.raw_os_error(),
            });
        }
    }

    let mut bounding_complete = true;
    for (_, flag) in CapabilitySet::all().iter_names() {
        if keep_set.contains(flag) {
            continue;
        }
        // Err: capability unknown to the running kernel → nothing to drop.
        if !capability_is_in_bounding_set(flag).unwrap_or(false) {
            continue;
        }
        match remove_capability_from_bounding_set(flag) {
            Ok(()) => {}
            Err(errno) if errno == rustix::io::Errno::PERM => bounding_complete = false,
            Err(errno) if errno == rustix::io::Errno::INVAL => {}
            Err(errno) => {
                return Err(SandboxError::Os {
                    operation: "PR_CAPBSET_DROP",
                    errno: errno.raw_os_error(),
                });
            }
        }
    }

    let current = capabilities(None).map_err(|errno| SandboxError::Os {
        operation: "capget",
        errno: errno.raw_os_error(),
    })?;
    let reduced = CapabilitySets {
        effective: current.effective & keep_set,
        permitted: current.permitted & keep_set,
        inheritable: current.inheritable & keep_set,
    };
    if reduced != current {
        set_capabilities(None, reduced).map_err(|errno| {
            if errno == rustix::io::Errno::PERM {
                SandboxError::PermissionDenied {
                    component: SandboxComponent::Capabilities,
                }
            } else {
                SandboxError::Os {
                    operation: "capset",
                    errno: errno.raw_os_error(),
                }
            }
        })?;
    }

    Ok(if bounding_complete || no_new_privs_enforced {
        EnforcementState::Enforced
    } else {
        EnforcementState::Partial
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_parse_capability_accepts_both_spellings() -> TestResult {
        let long = parse_capability("CAP_NET_BIND_SERVICE").map_err(ctx("long"))?;
        let short = parse_capability("net_bind_service").map_err(ctx("short"))?;
        assert_eq!(long, short);
        assert_eq!(long.as_str(), "NET_BIND_SERVICE");
        assert!(matches!(
            parse_capability("CAP_DOES_NOT_EXIST"),
            Err(SandboxError::UnknownCapability { .. })
        ));
        assert!(parse_capability("").is_err());
        Ok(())
    }

    #[test]
    fn test_parse_keep_list_fails_on_first_unknown() {
        let names = vec!["CAP_CHOWN".to_owned(), "bogus".to_owned()];
        assert!(matches!(
            parse_keep_list(&names),
            Err(SandboxError::UnknownCapability { name }) if name == "bogus"
        ));
    }

    #[test]
    fn test_effective_capabilities_readable() -> TestResult {
        // Read-only capget of the calling thread; changes nothing.
        let names = effective_capabilities().map_err(ctx("capget"))?;
        assert!(names.iter().all(|name| !name.as_str().is_empty()));
        Ok(())
    }
}
