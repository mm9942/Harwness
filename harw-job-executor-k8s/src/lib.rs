//! Kubernetes executor for the job runtime.
//!
//! One attempt is one restricted pod (`restartPolicy: Never`, no
//! service-account token, non-root, read-only root, all capabilities
//! dropped, `emptyDir` scratch only, image by digest). The pod is created
//! behind a **scheduling gate**, so nothing runs until the executor has read
//! the admitted pod back from the API (admission webhooks may have changed
//! it), mapped it to a [`harw_job_core::SandboxReport`] and checked a
//! `SandboxRequirement::Required` against it; only then the gate is lifted.
//! This needs Kubernetes 1.30 or newer (scheduling gates are GA there).
//!
//! Identity is the pod's immutable UID; recovery verifies it and the owner
//! labels before it reports a pod as alive, and a recreated pod of the same
//! name has another UID. The pod name doubles as the container name and is
//! stored in [`harw_container_model::PodInstance::container`].
//!
//! Limits of a pod spec: egress cannot be denied from the pod, so the network
//! dimension is at most `Partial` unless the operator attests that the
//! namespace's NetworkPolicy does ([`K8sConfig::network_attested`]); CPU
//! weight and pids ceilings are not expressible per pod (`Partial`). Pod
//! logs do not separate stderr; all output arrives as stdout.

#![forbid(unsafe_code)]

mod config;
mod error;
mod executor;
mod offer;
mod pod;
mod transport;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use config::K8sConfig;
pub use error::K8sError;
pub use executor::K8sExecutor;
pub use pod::{NetworkIntent, PodDoc, Requested, report_from_pod};
pub use transport::{HttpTransport, KubeTransport, Reply, Request};
