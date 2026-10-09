//! `harw-container-model`: the vocabulary of managed container instances.
//!
//! # Responsibility
//! Plain, serde-only values shared by the executors (`harw-job-executor-oci`,
//! `harw-job-executor-k8s`), the DoD container sensor and placement offers.
//! No I/O, no engine client, no clock. The DoD ring may depend on this crate
//! because it sits in ring I.
//!
//! # Contract
//! - [`ImageDigest`] accepts only `name@sha256:<64 hex>`; a tag is refused
//!   (fail closed).
//! - [`ContainerInstance`] and [`PodInstance`] are *persistable identities*:
//!   an engine-assigned id survives a daemon restart where a bare PID does
//!   not. Act only on the immutable id, never on a name.
//! - [`OwnerLabels`] are set at create time and are what garbage collection
//!   and recovery compare against. Secrets never go into labels.
//! - [`Lifecycle`] only moves forward; [`Lifecycle::Lost`] is reachable from
//!   every non-terminal state.

#![forbid(unsafe_code)]

mod digest;
mod instance;
mod labels;
mod lifecycle;

pub use digest::{DigestError, ImageDigest};
pub use instance::{
    ContainerId, ContainerInstance, EngineKind, InstanceError, InstanceRef, PodInstance,
};
pub use labels::{
    LABEL_ATTEMPT, LABEL_EPOCH, LABEL_OWNER, LABEL_PROFILE, LABEL_TENANT, LABEL_WORK_ID,
    LabelError, OwnerLabels,
};
pub use lifecycle::Lifecycle;
