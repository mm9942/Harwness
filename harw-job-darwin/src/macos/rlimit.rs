//! Applying an [`RlimitSet`] to the calling process (macOS).

use rustix::process::{Resource, Rlimit, setrlimit};

use crate::error::ProcessError;
use crate::rlimit::{RlimitResource, RlimitSet};

const fn to_rustix(resource: RlimitResource) -> Resource {
    match resource {
        RlimitResource::Cpu => Resource::Cpu,
        RlimitResource::FileSize => Resource::Fsize,
        RlimitResource::Core => Resource::Core,
        RlimitResource::OpenFiles => Resource::Nofile,
        RlimitResource::Processes => Resource::Nproc,
        RlimitResource::Stack => Resource::Stack,
    }
}

/// Applies every limit in `limits` to the **calling** process with
/// `setrlimit(2)`.
///
/// Darwin has no `prlimit`; limits cannot be set on another process. This
/// is meant for an exec trampoline that runs as the job process and calls
/// this right before `exec` (a `pre_exec` hook would need `unsafe`). Limits
/// are applied in a fixed order; on the first failure the earlier ones stay
/// applied, so a trampoline must not `exec` after an error.
///
/// # Errors
/// [`ProcessError::PermissionDenied`] when raising a hard limit without
/// privilege, [`ProcessError::Os`] (`EINVAL`) for values the kernel rejects
/// (e.g. `RLIMIT_NOFILE` above `kern.maxfilesperproc`).
pub fn apply_rlimits_to_current_process(limits: &RlimitSet) -> Result<(), ProcessError> {
    for (resource, value) in limits.iter() {
        setrlimit(
            to_rustix(resource),
            Rlimit {
                current: value.soft,
                maximum: value.hard,
            },
        )
        .map_err(|errno| ProcessError::from_errno("setrlimit", 0, errno.raw_os_error()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apply_rlimits_to_current_process;
    use crate::error::ProcessError;
    use crate::rlimit::{RlimitResource, RlimitSet};
    use crate::test_support::{TestResult, ctx};
    use rustix::process::{Resource, getrlimit};

    #[test]
    fn test_apply_current_values_is_accepted() -> TestResult {
        // Re-applying the current limits changes nothing for the test process
        // but exercises the full setrlimit path.
        // RLIMIT_CORE, not RLIMIT_NOFILE: macOS rejects NOFILE soft limits
        // above OPEN_MAX even when getrlimit reported them.
        let current = getrlimit(Resource::Core);
        let mut set = RlimitSet::new();
        set.set(RlimitResource::Core, current.current, current.maximum)
            .map_err(ctx("current core pair"))?;
        apply_rlimits_to_current_process(&set).map_err(ctx("apply"))?;
        assert_eq!(getrlimit(Resource::Core), current);
        Ok(())
    }

    #[test]
    fn test_empty_set_is_a_no_op() -> TestResult {
        apply_rlimits_to_current_process(&RlimitSet::new()).map_err(ctx("empty"))?;
        Ok(())
    }

    #[test]
    fn test_raising_hard_limit_above_current_is_refused() -> TestResult {
        let current = getrlimit(Resource::Core);
        let Some(hard) = current.maximum else {
            // Unlimited hard limit: nothing to raise above.
            return Ok(());
        };
        let Some(raised) = hard.checked_add(1) else {
            return Ok(());
        };
        let mut set = RlimitSet::new();
        set.set(RlimitResource::Core, Some(0), Some(raised))
            .map_err(ctx("raised pair"))?;
        let result = apply_rlimits_to_current_process(&set);
        // Root may raise it; an unprivileged test process must be refused.
        if !rustix::process::geteuid().is_root() {
            assert!(
                matches!(result, Err(ProcessError::PermissionDenied { .. })),
                "{result:?}"
            );
        }
        Ok(())
    }
}
