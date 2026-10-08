//! `harw-cloud-ops` — operations of the cloud home stack (status, enrollments,
//! cloudctl control plane) for the UIA operation registry.

pub mod cloudctl;
pub mod enrollments;
pub mod status;

use std::sync::Arc;

use harw_operations::registry::{OperationRegistry, RegistryError};
use harw_operations::Operation;

/// Number of operations registered by [`register_cloud`]:
/// 1 status + 1 enrollments + 5 cloudctl.
pub const CLOUD_OP_COUNT: usize = 7;

/// Register the seven cloud-stack operations in an [`OperationRegistry`]:
/// `cloud.status`, `cloud.enrollments.list` and the five
/// `cloud.cloudctl.{up,down,restart,enroll,revoke}` operations.
///
/// # Errors
/// The first [`RegistryError`] (name or web-route collision).
///
/// # Example
/// ```rust
/// use harw_operations::registry::OperationRegistry;
///
/// # fn main() -> Result<(), harw_operations::registry::RegistryError> {
/// let mut registry = OperationRegistry::new();
/// let added = harw_cloud_ops::register_cloud(&mut registry)?;
/// assert_eq!(added, 7);
/// assert!(registry.find_by_name("cloud.status").is_some());
/// assert!(registry.find_by_name("cloud.enrollments.list").is_some());
/// assert!(registry.find_by_name("cloud.cloudctl.up").is_some());
/// # Ok(())
/// # }
/// ```
pub fn register_cloud(registry: &mut OperationRegistry) -> Result<usize, RegistryError> {
    let ops: [Arc<dyn Operation>; CLOUD_OP_COUNT] = [
        Arc::new(status::CloudStatusOperation),
        Arc::new(enrollments::CloudEnrollmentsListOperation),
        Arc::new(cloudctl::CloudctlUpOperation),
        Arc::new(cloudctl::CloudctlDownOperation),
        Arc::new(cloudctl::CloudctlRestartOperation),
        Arc::new(cloudctl::CloudctlEnrollOperation),
        Arc::new(cloudctl::CloudctlRevokeOperation),
    ];
    for op in ops {
        registry.try_register(op)?;
    }
    Ok(CLOUD_OP_COUNT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_cloud_adds_seven_operations() {
        let mut registry = OperationRegistry::new();
        let added = register_cloud(&mut registry).expect("registration must succeed");
        assert_eq!(added, 7);
        assert_eq!(added, CLOUD_OP_COUNT);
        assert!(registry.find_by_name("cloud.status").is_some());
        assert!(registry.find_by_name("cloud.enrollments.list").is_some());
        assert!(registry.find_by_name("cloud.cloudctl.up").is_some());
        assert!(registry.find_by_name("cloud.cloudctl.down").is_some());
        assert!(registry.find_by_name("cloud.cloudctl.restart").is_some());
        assert!(registry.find_by_name("cloud.cloudctl.enroll").is_some());
        assert!(registry.find_by_name("cloud.cloudctl.revoke").is_some());
    }
}
